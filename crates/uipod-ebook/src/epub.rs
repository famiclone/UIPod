//! Pure-Rust `.epub` (`EPUB 2` & `EPUB 3`) parser:
//! - `META-INF/container.xml` -> `.opf` manifest & reading `spine`
//! - Multi-format TOC: EPUB 3 `<nav epub:type="toc">` -> EPUB 2 `toc.ncx` -> spine `<h1..h3>` fallback
//! - XHTML/HTML5 chapter stream with bold/italic/code/blockquote/headings, embedded illustrations
//!   (`<img src>` and `<image xlink:href>`), and cross-spine / same-file footnotes (`noteref` / `#id`).

use std::collections::HashMap;

use crate::encoding::{decode_base64, decode_bytes_auto};
use crate::image_dither::probe_image_dimensions;
use crate::xml_html::{collapse_inline_ws, get_attr, MarkupToken, MarkupTokenizer};
use crate::zip_archive::{normalize_zip_path, resolve_relative_href, ZipArchive};
use crate::{
    DocBuilder, DocFormat, EbookDoc, EbookError, FootnoteDef, InlineRun, InlineStyle, TocEntry,
};

struct ManifestItem {
    href: String,
    media_type: String,
    properties: String,
}

#[derive(Clone)]
pub(crate) struct RawTocItem {
    pub title: String,
    pub level: u8,
    pub target_path: String,
    pub fragment: Option<String>,
}

/// Maximum cumulative compressed image payload retained per `.epub` (`32 MiB`) to prevent OOM spikes.
const MAX_TOTAL_EPUB_IMAGE_BYTES: usize = 32 * 1024 * 1024;

pub fn parse_epub(file_name: &str, data: &[u8]) -> Result<EbookDoc, EbookError> {
    let zip = ZipArchive::parse(data).map_err(|e| EbookError::InvalidFormat(e.to_string()))?;

    // Detect Adobe Adept / Readium LCP / FairPlay DRM encryption (`META-INF/encryption.xml`),
    // ignoring benign IDPF/Adobe font-obfuscation entries.
    if is_epub_drm_encrypted(&zip) {
        return Err(EbookError::DrmProtected);
    }

    // 1. Locate `.opf` package via `META-INF/container.xml` (or fallback `.opf` scan)
    let opf_path = find_opf_path(&zip)
        .ok_or_else(|| EbookError::InvalidFormat("Missing EPUB OPF package".into()))?;
    let opf_bytes = zip
        .read_by_path(&opf_path)
        .ok_or_else(|| EbookError::InvalidFormat("Cannot read EPUB OPF file".into()))?;
    let opf_xml = decode_bytes_auto(&opf_bytes, None);

    // 2. Parse `.opf` metadata, manifest, spine, and cover
    let mut book_title = String::new();
    let mut book_author = String::new();
    let mut cover_meta_id: Option<String> = None;
    let mut manifest: HashMap<String, ManifestItem> = HashMap::new();
    let mut spine_ids: Vec<String> = Vec::new();
    let mut ncx_id: Option<String> = None;

    let mut cur_meta_tag: Option<String> = None;
    let mut meta_buf = String::new();

    for tok in MarkupTokenizer::new(&opf_xml) {
        match tok {
            MarkupToken::StartTag {
                name_lower,
                attrs_raw,
                self_closing,
            } => match name_lower.as_str() {
                "title" if book_title.is_empty() && !self_closing => {
                    cur_meta_tag = Some("title".into());
                    meta_buf.clear();
                }
                "creator" if book_author.is_empty() && !self_closing => {
                    cur_meta_tag = Some("creator".into());
                    meta_buf.clear();
                }
                "meta" => {
                    if let (Some(n), Some(c)) =
                        (get_attr(attrs_raw, "name"), get_attr(attrs_raw, "content"))
                    {
                        if n.eq_ignore_ascii_case("cover") && !c.trim().is_empty() {
                            cover_meta_id = Some(c.trim().to_string());
                        }
                    }
                }
                "item" => {
                    if let (Some(id), Some(href)) =
                        (get_attr(attrs_raw, "id"), get_attr(attrs_raw, "href"))
                    {
                        let (mut resolved, _) = resolve_relative_href(&opf_path, &href);
                        if zip.find_index(&resolved).is_none() {
                            let (root_rel, _) = resolve_relative_href("", &href);
                            if zip.find_index(&root_rel).is_some() {
                                resolved = root_rel;
                            }
                        }
                        let media_type = get_attr(attrs_raw, "media-type")
                            .unwrap_or_default()
                            .to_ascii_lowercase();
                        let properties = get_attr(attrs_raw, "properties")
                            .unwrap_or_default()
                            .to_ascii_lowercase();
                        manifest.insert(
                            id,
                            ManifestItem {
                                href: resolved,
                                media_type,
                                properties,
                            },
                        );
                    }
                }
                "spine" => {
                    if let Some(toc_attr) = get_attr(attrs_raw, "toc") {
                        if !toc_attr.trim().is_empty() {
                            ncx_id = Some(toc_attr.trim().to_string());
                        }
                    }
                }
                "itemref" => {
                    if let Some(idref) = get_attr(attrs_raw, "idref") {
                        let linear = get_attr(attrs_raw, "linear").unwrap_or_default();
                        if !linear.eq_ignore_ascii_case("no") || spine_ids.is_empty() {
                            spine_ids.push(idref);
                        }
                    }
                }
                _ => {}
            },
            MarkupToken::Text(t) => {
                if cur_meta_tag.is_some() {
                    meta_buf.push_str(t);
                }
            }
            MarkupToken::EndTag { name_lower } => {
                if let Some(ref active) = cur_meta_tag {
                    if active == &name_lower {
                        let clean = collapse_inline_ws(&meta_buf, true).trim().to_string();
                        if !clean.is_empty() {
                            if active == "title" && book_title.is_empty() {
                                book_title = clean;
                            } else if active == "creator" && book_author.is_empty() {
                                book_author = clean;
                            }
                        }
                        cur_meta_tag = None;
                    }
                }
            }
        }
    }

    // Fallback: include non-linear items at end if spine was empty
    if spine_ids.is_empty() {
        for (id, item) in &manifest {
            if item.media_type.contains("html") || item.media_type.contains("xml") {
                spine_ids.push(id.clone());
            }
        }
    }

    let spine_paths: Vec<String> = spine_ids
        .iter()
        .filter_map(|id| manifest.get(id).map(|m| m.href.clone()))
        .collect();

    // 3. Pre-load chapter XHTML texts & index element `#id` snippet texts for cross-file footnotes!
    let mut chapter_texts: HashMap<String, String> = HashMap::new();
    let mut id_note_snippets: HashMap<(String, String), Vec<String>> = HashMap::new();

    // Load spine chapters + any manifest XHTML files (e.g. non-linear `notes.xhtml`)
    for item in manifest.values() {
        let is_html = item.media_type.contains("html")
            || item.href.ends_with(".xhtml")
            || item.href.ends_with(".html")
            || item.href.ends_with(".htm");
        if is_html && !chapter_texts.contains_key(&item.href) {
            if let Some(raw) = zip.read_by_path(&item.href) {
                let txt = decode_bytes_auto(&raw, None);
                harvest_footnote_candidates(&item.href, &txt, &mut id_note_snippets);
                chapter_texts.insert(item.href.clone(), txt);
            }
        }
    }

    // 4. Extract TOC (EPUB 3 `<nav epub:type="toc">` -> EPUB 2 `toc.ncx` -> spine headings fallback)
    let mut raw_toc: Vec<RawTocItem> = Vec::new();
    if let Some(nav_item) = manifest
        .values()
        .find(|m| m.properties.split_whitespace().any(|p| p == "nav"))
    {
        if let Some(nav_html) = chapter_texts
            .get(&nav_item.href)
            .cloned()
            .or_else(|| zip.read_by_path(&nav_item.href).map(|b| decode_bytes_auto(&b, None)))
        {
            raw_toc = parse_epub3_nav_toc(&nav_item.href, &nav_html);
        }
    }
    if raw_toc.is_empty() {
        let ncx_path = ncx_id
            .as_ref()
            .and_then(|id| manifest.get(id))
            .map(|m| m.href.clone())
            .or_else(|| {
                manifest
                    .values()
                    .find(|m| {
                        m.media_type.contains("dtbncx")
                            || m.href.to_ascii_lowercase().ends_with(".ncx")
                    })
                    .map(|m| m.href.clone())
            });
        if let Some(np) = ncx_path {
            if let Some(ncx_bytes) = zip.read_by_path(&np) {
                let ncx_xml = decode_bytes_auto(&ncx_bytes, None);
                raw_toc = parse_epub2_ncx_toc(&np, &ncx_xml);
            }
        }
    }

    let fallback_title = crate::clean_stem_title(file_name);
    let mut builder = DocBuilder::new(
        if book_title.is_empty() {
            fallback_title
        } else {
            book_title
        },
        book_author,
        DocFormat::Epub,
    );

    let mut image_map: HashMap<String, usize> = HashMap::new();

    // Optional cover image if the first spine item doesn't already embed it
    let cover_href = manifest
        .values()
        .find(|m| m.properties.split_whitespace().any(|p| p == "cover-image"))
        .map(|m| m.href.clone())
        .or_else(|| {
            cover_meta_id
                .as_ref()
                .and_then(|cid| manifest.get(cid))
                .map(|m| m.href.clone())
        });

    let first_spine_mentions_cover = match (&cover_href, spine_paths.first()) {
        (Some(ch), Some(sp0)) => {
            let cover_leaf = ch.rsplit('/').next().unwrap_or(ch);
            chapter_texts
                .get(sp0)
                .map(|t| !cover_leaf.is_empty() && t.contains(cover_leaf))
                .unwrap_or(false)
        }
        _ => false,
    };

    if let Some(ch) = cover_href {
        if !first_spine_mentions_cover {
            if let Some(img_idx) = load_zip_image(&zip, &ch, &mut builder, &mut image_map) {
                builder.push_image(img_idx, String::new());
            }
        }
    }

    // 5. Parse spine chapters sequentially, recording `(spine_path, Option<id>) -> canonical_offset`
    let mut anchor_offsets: HashMap<(String, Option<String>), usize> = HashMap::new();
    let mut fallback_heading_toc: Vec<TocEntry> = Vec::new();
    let mut footnote_lookup: HashMap<(String, String), usize> = HashMap::new();

    for (sp_idx, sp_path) in spine_paths.iter().enumerate() {
        if sp_idx > 0 {
            builder.push_page_break();
        }
        let chap_start = builder.current_offset();
        anchor_offsets.insert((sp_path.clone(), None), chap_start);

        let Some(xhtml) = chapter_texts.get(sp_path) else {
            continue;
        };

        parse_xhtml_chapter(
            sp_path,
            xhtml,
            &zip,
            &id_note_snippets,
            &mut image_map,
            &mut anchor_offsets,
            &mut footnote_lookup,
            &mut fallback_heading_toc,
            &mut builder,
        );
    }
    drop(chapter_texts);
    drop(id_note_snippets);

    // 6. Resolve TOC entries to exact canonical offsets
    if !raw_toc.is_empty() {
        for item in raw_toc {
            let exact = anchor_offsets
                .get(&(item.target_path.clone(), item.fragment.clone()))
                .copied()
                .or_else(|| {
                    anchor_offsets
                        .get(&(item.target_path.clone(), None))
                        .copied()
                });
            if let Some(off) = exact {
                builder.toc.push(TocEntry {
                    title: item.title,
                    level: item.level.min(4),
                    offset: off,
                });
            }
        }
    }
    if builder.toc.is_empty() {
        builder.toc = fallback_heading_toc;
    }
    builder.dedup_and_clamp_toc();

    Ok(builder.finish())
}

fn find_opf_path(zip: &ZipArchive<'_>) -> Option<String> {
    if let Some(bytes) = zip.read_by_path("META-INF/container.xml") {
        let xml = decode_bytes_auto(&bytes, None);
        for tok in MarkupTokenizer::new(&xml) {
            if let MarkupToken::StartTag {
                name_lower,
                attrs_raw,
                ..
            } = tok
            {
                if name_lower == "rootfile" {
                    if let Some(fp) = get_attr(attrs_raw, "full-path") {
                        let norm = normalize_zip_path(&fp);
                        if !norm.is_empty() {
                            return Some(norm);
                        }
                    }
                }
            }
        }
    }
    zip.entries()
        .iter()
        .find(|e| e.name.to_ascii_lowercase().ends_with(".opf"))
        .map(|e| e.name.clone())
}

pub(crate) fn parse_epub3_nav_toc(nav_path: &str, html: &str) -> Vec<RawTocItem> {
    let mut out = Vec::new();
    let mut in_toc_nav = false;
    let mut nav_depth = 0usize;
    let mut ol_depth = 0usize;
    let mut active_href: Option<String> = None;
    let mut link_text = String::new();

    for tok in MarkupTokenizer::new(html) {
        match tok {
            MarkupToken::StartTag {
                name_lower,
                attrs_raw,
                self_closing,
            } => {
                if name_lower == "nav" && !self_closing {
                    let et = get_attr(attrs_raw, "epub:type")
                        .or_else(|| get_attr(attrs_raw, "type"))
                        .or_else(|| get_attr(attrs_raw, "role"))
                        .unwrap_or_default()
                        .to_ascii_lowercase();
                    if in_toc_nav {
                        nav_depth += 1;
                    } else if et.contains("toc") {
                        in_toc_nav = true;
                        nav_depth = 1;
                        ol_depth = 0;
                    }
                } else if in_toc_nav {
                    if name_lower == "ol" || name_lower == "ul" {
                        if !self_closing {
                            ol_depth += 1;
                        }
                    } else if name_lower == "a" && !self_closing {
                        active_href = get_attr(attrs_raw, "href");
                        link_text.clear();
                    }
                }
            }
            MarkupToken::Text(t) => {
                if active_href.is_some() {
                    link_text.push_str(t);
                }
            }
            MarkupToken::EndTag { name_lower } => {
                if in_toc_nav {
                    if name_lower == "a" {
                        if let Some(href) = active_href.take() {
                            let title = collapse_inline_ws(&link_text, true).trim().to_string();
                            if !title.is_empty() && !href.trim().is_empty() {
                                let (target_path, fragment) =
                                    resolve_relative_href(nav_path, &href);
                                out.push(RawTocItem {
                                    title,
                                    level: ol_depth.saturating_sub(1).min(4) as u8,
                                    target_path,
                                    fragment,
                                });
                            }
                        }
                    } else if name_lower == "ol" || name_lower == "ul" {
                        ol_depth = ol_depth.saturating_sub(1);
                    } else if name_lower == "nav" {
                        nav_depth = nav_depth.saturating_sub(1);
                        if nav_depth == 0 {
                            break;
                        }
                    }
                }
            }
        }
    }
    out
}

pub(crate) fn parse_epub2_ncx_toc(ncx_path: &str, xml: &str) -> Vec<RawTocItem> {
    struct NavBuilder {
        title: String,
        src: Option<String>,
        level: u8,
    }
    let mut out: Vec<(usize, RawTocItem)> = Vec::new();
    let mut stack: Vec<(usize, NavBuilder)> = Vec::new();
    let mut in_nav_label = false;
    let mut in_text = false;
    let mut text_buf = String::new();

    for tok in MarkupTokenizer::new(xml) {
        match tok {
            MarkupToken::StartTag {
                name_lower,
                attrs_raw,
                self_closing,
            } => match name_lower.as_str() {
                "navpoint" if !self_closing => {
                    let lvl = stack.len().min(4) as u8;
                    let slot = out.len();
                    out.push((
                        slot,
                        RawTocItem {
                            title: String::new(),
                            level: lvl,
                            target_path: String::new(),
                            fragment: None,
                        },
                    ));
                    stack.push((
                        slot,
                        NavBuilder {
                            title: String::new(),
                            src: None,
                            level: lvl,
                        },
                    ));
                }
                "navlabel" if !self_closing && !stack.is_empty() => {
                    in_nav_label = true;
                }
                "text" if in_nav_label && !self_closing => {
                    in_text = true;
                    text_buf.clear();
                }
                "content" if !stack.is_empty() => {
                    if let Some(src) = get_attr(attrs_raw, "src") {
                        if let Some((_, top)) = stack.last_mut() {
                            if top.src.is_none() {
                                top.src = Some(src);
                            }
                        }
                    }
                }
                _ => {}
            },
            MarkupToken::Text(t) => {
                if in_text {
                    text_buf.push_str(t);
                }
            }
            MarkupToken::EndTag { name_lower } => match name_lower.as_str() {
                "text" if in_text => {
                    in_text = false;
                    let clean = collapse_inline_ws(&text_buf, true).trim().to_string();
                    if let Some((_, top)) = stack.last_mut() {
                        if top.title.is_empty() {
                            top.title = clean;
                        }
                    }
                }
                "navlabel" => {
                    in_nav_label = false;
                    in_text = false;
                }
                "navpoint" => {
                    if let Some((slot, done)) = stack.pop() {
                        if let Some(src) = done.src {
                            let (tp, frag) = resolve_relative_href(ncx_path, &src);
                            out[slot].1 = RawTocItem {
                                title: done.title,
                                level: done.level,
                                target_path: tp,
                                fragment: frag,
                            };
                        }
                    }
                }
                _ => {}
            },
        }
    }

    out.into_iter()
        .map(|(_, item)| item)
        .filter(|it| !it.title.is_empty() && !it.target_path.is_empty())
        .collect()
}

/// Scans an XHTML file for `<aside id="..." epub:type="footnote">`, `<li id="...">`, `<div id="...">`,
/// or `<p id="...">` elements so footnote popups can display rich note paragraphs even when notes live
/// at the end of a chapter or in a separate `notes.xhtml` file.
pub(crate) fn harvest_footnote_candidates(
    file_path: &str,
    xhtml: &str,
    out: &mut HashMap<(String, String), Vec<String>>,
) {
    struct Capture {
        id: String,
        tag: String,
        depth: usize,
        paras: Vec<String>,
        cur: String,
    }
    let mut cap: Option<Capture> = None;

    for tok in MarkupTokenizer::new(xhtml) {
        match tok {
            MarkupToken::StartTag {
                name_lower,
                attrs_raw,
                self_closing,
            } => {
                if let Some(ref mut c) = cap {
                    if !self_closing && name_lower == c.tag {
                        c.depth += 1;
                    }
                    if matches!(name_lower.as_str(), "p" | "div" | "br" | "li") {
                        let trimmed = collapse_inline_ws(&c.cur, true).trim().to_string();
                        if !trimmed.is_empty() {
                            c.paras.push(trimmed);
                            c.cur.clear();
                        }
                    }
                } else if !self_closing
                    && matches!(
                        name_lower.as_str(),
                        "aside" | "li" | "div" | "p" | "dd" | "section"
                    )
                {
                    if let Some(id) = get_attr(attrs_raw, "id") {
                        let clean_id = id.trim().to_string();
                        if !clean_id.is_empty() {
                            cap = Some(Capture {
                                id: clean_id,
                                tag: name_lower,
                                depth: 1,
                                paras: Vec::new(),
                                cur: String::new(),
                            });
                        }
                    }
                }
            }
            MarkupToken::Text(t) => {
                if let Some(ref mut c) = cap {
                    if c.paras.len() < 12 && c.cur.len() < 4096 {
                        c.cur.push_str(t);
                    }
                }
            }
            MarkupToken::EndTag { name_lower } => {
                let mut finished = None;
                if let Some(ref mut c) = cap {
                    if matches!(name_lower.as_str(), "p" | "div" | "li") {
                        let trimmed = collapse_inline_ws(&c.cur, true).trim().to_string();
                        if !trimmed.is_empty() {
                            c.paras.push(trimmed);
                            c.cur.clear();
                        }
                    }
                    if name_lower == c.tag {
                        c.depth = c.depth.saturating_sub(1);
                        if c.depth == 0 {
                            finished = cap.take();
                        }
                    }
                }
                if let Some(mut f) = finished {
                    let trimmed = collapse_inline_ws(&f.cur, true).trim().to_string();
                    if !trimmed.is_empty() {
                        f.paras.push(trimmed);
                    }
                    if !f.paras.is_empty() {
                        out.insert((file_path.to_string(), f.id), f.paras);
                    }
                }
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn parse_xhtml_chapter(
    chapter_path: &str,
    xhtml: &str,
    zip: &ZipArchive<'_>,
    id_note_snippets: &HashMap<(String, String), Vec<String>>,
    image_map: &mut HashMap<String, usize>,
    anchor_offsets: &mut HashMap<(String, Option<String>), usize>,
    footnote_lookup: &mut HashMap<(String, String), usize>,
    fallback_toc: &mut Vec<TocEntry>,
    builder: &mut DocBuilder,
) {
    let mut in_head = false;
    let mut skip_aside_depth = 0usize;

    let mut bold_depth = 0usize;
    let mut italic_depth = 0usize;
    let mut code_depth = 0usize;
    let mut quote_depth = 0usize;
    let mut pre_depth = 0usize;
    let mut heading_level = 0u8;
    let mut list_li_pending = false;

    let mut runs: Vec<InlineRun> = Vec::new();
    let mut active_noteref: Option<(String, String)> = None;
    let mut noteref_text = String::new();

    let flush_para = |runs: &mut Vec<InlineRun>,
                      heading_level: u8,
                      quote_depth: usize,
                      pre_depth: usize,
                      fallback_toc: &mut Vec<TocEntry>,
                      builder: &mut DocBuilder| {
        if runs.is_empty() {
            return;
        }
        let joined: String = runs.iter().map(|r| r.text.as_str()).collect();
        if joined.trim().is_empty() {
            runs.clear();
            return;
        }
        let off = builder.current_offset();
        if (1..=3).contains(&heading_level) {
            fallback_toc.push(TocEntry {
                title: joined.trim().to_string(),
                level: heading_level - 1,
                offset: off,
            });
        }
        let drain_runs = std::mem::take(runs);
        builder.push_paragraph(
            drain_runs,
            heading_level,
            quote_depth > 0,
            pre_depth > 0,
            heading_level == 1,
        );
    };

    for tok in MarkupTokenizer::new(xhtml) {
        match tok {
            MarkupToken::StartTag {
                name_lower,
                attrs_raw,
                self_closing,
            } => {
                if name_lower == "head" && !self_closing {
                    in_head = true;
                    continue;
                }
                if in_head {
                    continue;
                }
                if skip_aside_depth > 0 {
                    if !self_closing && name_lower == "aside" {
                        skip_aside_depth += 1;
                    }
                    continue;
                }

                // Hide `<aside epub:type="footnote">` from inline body flow since it opens as a popup card!
                if name_lower == "aside" && !self_closing {
                    let et = get_attr(attrs_raw, "epub:type")
                        .or_else(|| get_attr(attrs_raw, "role"))
                        .unwrap_or_default()
                        .to_ascii_lowercase();
                    if et.contains("footnote") || et.contains("endnote") || et.contains("note") {
                        skip_aside_depth = 1;
                        continue;
                    }
                }

                // Record `#id` / `name` fragment target at exact canonical offset
                if let Some(id) = get_attr(attrs_raw, "id").or_else(|| get_attr(attrs_raw, "name")) {
                    let clean_id = id.trim().to_string();
                    if !clean_id.is_empty() {
                        // If a block tag starts with an `id`, flush preceding paragraph first so the anchor lands at the start of the new block!
                        if is_block_tag(&name_lower) {
                            flush_para(
                                &mut runs,
                                heading_level,
                                quote_depth,
                                pre_depth,
                                fallback_toc,
                                builder,
                            );
                        }
                        anchor_offsets
                            .entry((chapter_path.to_string(), Some(clean_id)))
                            .or_insert_with(|| builder.current_offset());
                    }
                }

                match name_lower.as_str() {
                    "h1" | "h2" | "h3" | "h4" | "h5" | "h6" => {
                        flush_para(
                            &mut runs,
                            heading_level,
                            quote_depth,
                            pre_depth,
                            fallback_toc,
                            builder,
                        );
                        if !self_closing {
                            heading_level = match name_lower.as_str() {
                                "h1" => 1,
                                "h2" => 2,
                                _ => 3,
                            };
                        }
                    }
                    "p" | "div" | "section" | "article" | "header" | "footer" | "tr" | "dt"
                    | "dd" | "figcaption" => {
                        flush_para(
                            &mut runs,
                            heading_level,
                            quote_depth,
                            pre_depth,
                            fallback_toc,
                            builder,
                        );
                    }
                    "li" => {
                        flush_para(
                            &mut runs,
                            heading_level,
                            quote_depth,
                            pre_depth,
                            fallback_toc,
                            builder,
                        );
                        list_li_pending = true;
                    }
                    "blockquote" if !self_closing => {
                        flush_para(
                            &mut runs,
                            heading_level,
                            quote_depth,
                            pre_depth,
                            fallback_toc,
                            builder,
                        );
                        quote_depth += 1;
                    }
                    "pre" if !self_closing => {
                        flush_para(
                            &mut runs,
                            heading_level,
                            quote_depth,
                            pre_depth,
                            fallback_toc,
                            builder,
                        );
                        pre_depth += 1;
                    }
                    "hr" => {
                        flush_para(
                            &mut runs,
                            heading_level,
                            quote_depth,
                            pre_depth,
                            fallback_toc,
                            builder,
                        );
                        builder.push_spacer();
                    }
                    "br" => {
                        if pre_depth > 0 {
                            flush_para(
                                &mut runs,
                                heading_level,
                                quote_depth,
                                pre_depth,
                                fallback_toc,
                                builder,
                            );
                        } else if !runs.is_empty() {
                            flush_para(
                                &mut runs,
                                heading_level,
                                quote_depth,
                                pre_depth,
                                fallback_toc,
                                builder,
                            );
                        }
                    }
                    "b" | "strong" if !self_closing => bold_depth += 1,
                    "i" | "em" | "cite" | "dfn" if !self_closing => italic_depth += 1,
                    "code" | "tt" | "kbd" | "samp" if !self_closing => code_depth += 1,
                    "img" | "image" => {
                        let src_opt = get_attr(attrs_raw, "src")
                            .or_else(|| get_attr(attrs_raw, "href"));
                        if let Some(src) = src_opt {
                            let alt = get_attr(attrs_raw, "alt").unwrap_or_default();
                            flush_para(
                                &mut runs,
                                heading_level,
                                quote_depth,
                                pre_depth,
                                fallback_toc,
                                builder,
                            );
                            if let Some(img_idx) = resolve_and_load_epub_image(
                                zip,
                                chapter_path,
                                &src,
                                builder,
                                image_map,
                            ) {
                                builder.push_image(img_idx, collapse_inline_ws(&alt, true));
                            }
                        }
                    }
                    "a" if !self_closing => {
                        if let Some(href) = get_attr(attrs_raw, "href") {
                            let et = get_attr(attrs_raw, "epub:type")
                                .or_else(|| get_attr(attrs_raw, "role"))
                                .or_else(|| get_attr(attrs_raw, "class"))
                                .unwrap_or_default()
                                .to_ascii_lowercase();
                            let (t_path, frag_opt) = resolve_relative_href(chapter_path, &href);
                            if let Some(frag) = frag_opt {
                                let has_note_target =
                                    id_note_snippets.contains_key(&(t_path.clone(), frag.clone()));
                                let looks_noteref = et.contains("note")
                                    || et.contains("fn")
                                    || frag.to_ascii_lowercase().starts_with("fn")
                                    || frag.to_ascii_lowercase().starts_with("note")
                                    || (has_note_target && t_path != chapter_path);
                                if has_note_target && (looks_noteref || et.is_empty()) {
                                    active_noteref = Some((t_path, frag));
                                    noteref_text.clear();
                                }
                            }
                        }
                    }
                    _ => {}
                }
            }
            MarkupToken::Text(t) => {
                if in_head || skip_aside_depth > 0 {
                    continue;
                }
                if active_noteref.is_some() {
                    noteref_text.push_str(t);
                    continue;
                }
                if pre_depth > 0 {
                    let dec = crate::encoding::decode_entities(t);
                    for (li, line) in dec.split('\n').enumerate() {
                        if li > 0 {
                            flush_para(
                                &mut runs,
                                heading_level,
                                quote_depth,
                                pre_depth,
                                fallback_toc,
                                builder,
                            );
                        }
                        let clean = line.trim_end_matches('\r').replace('\t', "    ");
                        if !clean.is_empty() {
                            runs.push(InlineRun {
                                text: clean,
                                style: InlineStyle::Code,
                                footnote_idx: None,
                            });
                        }
                    }
                    continue;
                }

                let trim_lead = runs.is_empty() && !list_li_pending;
                let clean = collapse_inline_ws(t, trim_lead);
                if clean.is_empty() || (runs.is_empty() && clean.trim().is_empty()) {
                    continue;
                }
                if list_li_pending {
                    list_li_pending = false;
                    runs.push(InlineRun {
                        text: "• ".to_string(),
                        style: InlineStyle::Bold,
                        footnote_idx: None,
                    });
                }
                let style = resolve_style(bold_depth, italic_depth, code_depth);
                push_merged_run(&mut runs, clean, style);
            }
            MarkupToken::EndTag { name_lower } => {
                if name_lower == "head" {
                    in_head = false;
                    continue;
                }
                if in_head {
                    continue;
                }
                if skip_aside_depth > 0 {
                    if name_lower == "aside" {
                        skip_aside_depth -= 1;
                    }
                    continue;
                }

                match name_lower.as_str() {
                    "a" => {
                        if let Some((t_path, frag)) = active_noteref.take() {
                            let raw_lbl = collapse_inline_ws(&noteref_text, true).trim().to_string();
                            // Distinguish compact footnote markers (`[1]`, `1`, `*`, `†`) from long TOC links!
                            let is_compact_marker = raw_lbl.chars().count() <= 8;
                            if is_compact_marker {
                                if let Some(paras) = id_note_snippets.get(&(t_path.clone(), frag.clone())) {
                                    let fn_idx = *footnote_lookup
                                        .entry((t_path, frag))
                                        .or_insert_with(|| {
                                            let next_n = builder.footnotes.len() + 1;
                                            let badge = format_footnote_badge(&raw_lbl, next_n);
                                            let idx = builder.footnotes.len();
                                            builder.footnotes.push(FootnoteDef {
                                                index: idx,
                                                label: badge.clone(),
                                                title: badge,
                                                paragraphs: paras.clone(),
                                            });
                                            idx
                                        });
                                    let badge_txt = builder.footnotes[fn_idx].label.clone();
                                    runs.push(InlineRun {
                                        text: badge_txt,
                                        style: InlineStyle::FootnoteRef,
                                        footnote_idx: Some(fn_idx),
                                    });
                                } else if !raw_lbl.is_empty() {
                                    push_merged_run(
                                        &mut runs,
                                        raw_lbl,
                                        resolve_style(bold_depth, italic_depth, code_depth),
                                    );
                                }
                            } else if !raw_lbl.is_empty() {
                                push_merged_run(
                                    &mut runs,
                                    raw_lbl,
                                    resolve_style(bold_depth, italic_depth, code_depth),
                                );
                            }
                        }
                    }
                    "h1" | "h2" | "h3" | "h4" | "h5" | "h6" => {
                        flush_para(
                            &mut runs,
                            heading_level,
                            quote_depth,
                            pre_depth,
                            fallback_toc,
                            builder,
                        );
                        heading_level = 0;
                    }
                    "p" | "div" | "section" | "article" | "li" | "tr" | "dt" | "dd"
                    | "figcaption" => {
                        list_li_pending = false;
                        flush_para(
                            &mut runs,
                            heading_level,
                            quote_depth,
                            pre_depth,
                            fallback_toc,
                            builder,
                        );
                    }
                    "blockquote" => {
                        flush_para(
                            &mut runs,
                            heading_level,
                            quote_depth,
                            pre_depth,
                            fallback_toc,
                            builder,
                        );
                        quote_depth = quote_depth.saturating_sub(1);
                    }
                    "pre" => {
                        flush_para(
                            &mut runs,
                            heading_level,
                            quote_depth,
                            pre_depth,
                            fallback_toc,
                            builder,
                        );
                        pre_depth = pre_depth.saturating_sub(1);
                    }
                    "b" | "strong" => bold_depth = bold_depth.saturating_sub(1),
                    "i" | "em" | "cite" | "dfn" => italic_depth = italic_depth.saturating_sub(1),
                    "code" | "tt" | "kbd" | "samp" => code_depth = code_depth.saturating_sub(1),
                    _ => {}
                }
            }
        }
    }

    flush_para(
        &mut runs,
        heading_level,
        quote_depth,
        pre_depth,
        fallback_toc,
        builder,
    );
}

pub(crate) fn is_block_tag(name: &str) -> bool {
    matches!(
        name,
        "h1" | "h2"
            | "h3"
            | "h4"
            | "h5"
            | "h6"
            | "p"
            | "div"
            | "section"
            | "article"
            | "blockquote"
            | "pre"
            | "li"
            | "figure"
    )
}

pub(crate) fn resolve_style(bold: usize, italic: usize, code: usize) -> InlineStyle {
    if code > 0 {
        InlineStyle::Code
    } else if bold > 0 && italic > 0 {
        InlineStyle::BoldItalic
    } else if bold > 0 {
        InlineStyle::Bold
    } else if italic > 0 {
        InlineStyle::Italic
    } else {
        InlineStyle::Regular
    }
}

pub(crate) fn push_merged_run(runs: &mut Vec<InlineRun>, text: String, style: InlineStyle) {
    if text.is_empty() {
        return;
    }
    if let Some(last) = runs.last_mut() {
        if last.style == style && last.footnote_idx.is_none() {
            if last.text.ends_with(' ') && text.starts_with(' ') {
                last.text.push_str(text.trim_start_matches(' '));
            } else {
                last.text.push_str(&text);
            }
            return;
        }
    }
    runs.push(InlineRun {
        text,
        style,
        footnote_idx: None,
    });
}

pub(crate) fn format_footnote_badge(raw: &str, fallback_num: usize) -> String {
    let trimmed = raw
        .trim()
        .trim_start_matches('[')
        .trim_end_matches(']')
        .trim_start_matches('(')
        .trim_end_matches(')')
        .trim();
    if trimmed.is_empty() {
        format!("[{fallback_num}]")
    } else {
        format!("[{trimmed}]")
    }
}

fn resolve_and_load_epub_image(
    zip: &ZipArchive<'_>,
    base_file: &str,
    src: &str,
    builder: &mut DocBuilder,
    image_map: &mut HashMap<String, usize>,
) -> Option<usize> {
    if src.starts_with("data:image/") {
        let bytes = decode_base64(src)?;
        let (w, h) = probe_image_dimensions(&bytes)?;
        return Some(builder.register_image(bytes, w, h));
    }
    let (norm_path, _) = resolve_relative_href(base_file, src);
    load_zip_image(zip, &norm_path, builder, image_map)
}

fn is_epub_drm_encrypted(zip: &ZipArchive<'_>) -> bool {
    let Some(enc_bytes) = zip.read_by_path("META-INF/encryption.xml") else {
        return false;
    };
    is_epub_drm_encrypted_bytes(&enc_bytes)
}

pub(crate) fn is_epub_drm_encrypted_bytes(enc_bytes: &[u8]) -> bool {
    let xml = decode_bytes_auto(&enc_bytes, None);
    let mut in_enc_data = false;
    let mut cur_algo = String::new();
    let mut cur_uri = String::new();

    for tok in MarkupTokenizer::new(&xml) {
        match tok {
            MarkupToken::StartTag {
                name_lower,
                attrs_raw,
                self_closing,
            } => match name_lower.as_str() {
                "encrypteddata" if !self_closing => {
                    in_enc_data = true;
                    cur_algo.clear();
                    cur_uri.clear();
                }
                "encryptionmethod" if in_enc_data => {
                    if let Some(alg) = get_attr(attrs_raw, "algorithm") {
                        cur_algo = alg.to_ascii_lowercase();
                    }
                }
                "cipherreference" if in_enc_data => {
                    if let Some(u) = get_attr(attrs_raw, "uri") {
                        cur_uri = u.to_ascii_lowercase();
                    }
                }
                _ => {}
            },
            MarkupToken::EndTag { name_lower } if name_lower == "encrypteddata" => {
                if in_enc_data {
                    in_enc_data = false;
                    let is_font_obfuscation = cur_algo.contains("idpf.org/2008/embedding")
                        || cur_algo.contains("adobe.com/pdf/enc#rc")
                        || cur_uri.ends_with(".otf")
                        || cur_uri.ends_with(".ttf")
                        || cur_uri.ends_with(".woff")
                        || cur_uri.ends_with(".woff2");
                    if !is_font_obfuscation {
                        return true;
                    }
                }
            }
            _ => {}
        }
    }
    false
}

fn load_zip_image(
    zip: &ZipArchive<'_>,
    norm_path: &str,
    builder: &mut DocBuilder,
    image_map: &mut HashMap<String, usize>,
) -> Option<usize> {
    if let Some(&idx) = image_map.get(norm_path) {
        return Some(idx);
    }
    let used_bytes: usize = builder.images.iter().map(|img| img.raw_bytes.len()).sum();
    if used_bytes >= MAX_TOTAL_EPUB_IMAGE_BYTES {
        return None;
    }
    let bytes = zip.read_by_path(norm_path)?;
    if used_bytes.saturating_add(bytes.len()) > MAX_TOTAL_EPUB_IMAGE_BYTES {
        return None;
    }
    let (w, h) = probe_image_dimensions(&bytes)?;
    let idx = builder.register_image(bytes, w, h);
    image_map.insert(norm_path.to_string(), idx);
    Some(idx)
}
