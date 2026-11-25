//! Pure-Rust FictionBook 2 (`.fb2` and `.fb2.zip`) parser with automatic `Windows-1251` /
//! `KOI8-U` / `KOI8-R` / `UTF-8` transcoding, `<binary>` Base64 illustrations, `<body name="notes">`
//! footnotes, and hierarchical `<section><title>` Table of Contents extraction.

use std::collections::HashMap;

use crate::encoding::{decode_base64, decode_bytes_auto};
use crate::epub::{format_footnote_badge, push_merged_run, resolve_style};
use crate::image_dither::probe_image_dimensions;
use crate::xml_html::{collapse_inline_ws, get_attr, MarkupToken, MarkupTokenizer};
use crate::zip_archive::ZipArchive;
use crate::{
    DocBuilder, DocFormat, EbookDoc, EbookError, FootnoteDef, InlineRun, InlineStyle, TocEntry,
};

pub fn parse_fb2_or_zip(file_name: &str, data: &[u8]) -> Result<EbookDoc, EbookError> {
    // If PK\x03\x04 ZIP header or `.fb2.zip`, extract the `.fb2` payload first
    if data.starts_with(b"PK\x03\x04") || file_name.to_ascii_lowercase().ends_with(".zip") {
        let zip = ZipArchive::parse(data).map_err(|e| EbookError::InvalidFormat(e.to_string()))?;
        let fb2_idx = zip
            .entries()
            .iter()
            .position(|e| e.name.to_ascii_lowercase().ends_with(".fb2"))
            .or_else(|| {
                zip.entries()
                    .iter()
                    .position(|e| e.name.to_ascii_lowercase().ends_with(".xml"))
            })
            .or(if !zip.entries().is_empty() {
                Some(0)
            } else {
                None
            })
            .ok_or_else(|| EbookError::InvalidFormat("Empty .fb2.zip archive".into()))?;
        let inner = zip
            .read_by_index(fb2_idx)
            .map_err(|e| EbookError::InvalidFormat(e.to_string()))?;
        return parse_fb2_xml_bytes(file_name, &inner);
    }
    parse_fb2_xml_bytes(file_name, data)
}

fn parse_fb2_xml_bytes(file_name: &str, raw_bytes: &[u8]) -> Result<EbookDoc, EbookError> {
    let xml = decode_bytes_auto(raw_bytes, None);
    if xml.trim().is_empty() {
        return Err(EbookError::InvalidFormat("Empty FB2 document".into()));
    }

    // Pass 1: Extract `<description><title-info>`, `<binary id="...">` Base64 images,
    // and `<body name="notes"|"comments">` footnote definitions!
    let mut book_title = String::new();
    let mut author_parts: Vec<String> = Vec::new();
    let mut cover_binary_id: Option<String> = None;
    let mut binaries: HashMap<String, Vec<u8>> = HashMap::new();
    let mut notes_map: HashMap<String, (String, Vec<String>)> = HashMap::new();

    {
        let mut in_title_info = false;
        let mut in_coverpage = false;
        let mut in_author = false;
        let mut cur_meta_field: Option<&'static str> = None;
        let mut meta_buf = String::new();

        let mut cur_binary_id: Option<String> = None;
        let mut binary_b64 = String::new();

        let mut in_notes_body = false;
        let mut note_sec_stack: Vec<(Option<String>, String, Vec<String>)> = Vec::new();
        let mut in_note_title = false;
        let mut note_para_buf = String::new();

        for tok in MarkupTokenizer::new(&xml) {
            match tok {
                MarkupToken::StartTag {
                    name_lower,
                    attrs_raw,
                    self_closing,
                } => {
                    if let Some(ref _bid) = cur_binary_id {
                        continue;
                    }
                    match name_lower.as_str() {
                        "title-info" if !self_closing => in_title_info = true,
                        "coverpage" if in_title_info && !self_closing => in_coverpage = true,
                        "image" if in_coverpage => {
                            if cover_binary_id.is_none() {
                                if let Some(h) = get_attr(attrs_raw, "href") {
                                    let id = h.trim().trim_start_matches('#').to_string();
                                    if !id.is_empty() {
                                        cover_binary_id = Some(id);
                                    }
                                }
                            }
                        }
                        "book-title" if in_title_info && !self_closing => {
                            cur_meta_field = Some("book-title");
                            meta_buf.clear();
                        }
                        "author" if in_title_info && !self_closing => {
                            in_author = true;
                        }
                        "first-name" | "middle-name" | "last-name" | "nickname"
                            if in_author && !self_closing =>
                        {
                            cur_meta_field = Some("author-part");
                            meta_buf.clear();
                        }
                        "binary" if !self_closing => {
                            if let Some(id) = get_attr(attrs_raw, "id") {
                                let clean = id.trim().trim_start_matches('#').to_string();
                                if !clean.is_empty() {
                                    cur_binary_id = Some(clean);
                                    binary_b64.clear();
                                }
                            }
                        }
                        "body" if !self_closing => {
                            let body_name = get_attr(attrs_raw, "name")
                                .unwrap_or_default()
                                .to_ascii_lowercase();
                            if body_name == "notes"
                                || body_name == "comments"
                                || body_name == "footnotes"
                            {
                                in_notes_body = true;
                            }
                        }
                        "section" if in_notes_body && !self_closing => {
                            let id = get_attr(attrs_raw, "id")
                                .map(|s| s.trim().trim_start_matches('#').to_string())
                                .filter(|s| !s.is_empty());
                            note_sec_stack.push((id, String::new(), Vec::new()));
                        }
                        "title" if in_notes_body && !note_sec_stack.is_empty() && !self_closing => {
                            in_note_title = true;
                            note_para_buf.clear();
                        }
                        "p" | "v" | "subtitle"
                            if in_notes_body && !note_sec_stack.is_empty() && !self_closing =>
                        {
                            note_para_buf.clear();
                        }
                        _ => {}
                    }
                }
                MarkupToken::Text(t) => {
                    if cur_binary_id.is_some() {
                        binary_b64.push_str(t);
                    } else if cur_meta_field.is_some() {
                        meta_buf.push_str(t);
                    } else if in_notes_body && !note_sec_stack.is_empty() {
                        note_para_buf.push_str(t);
                    }
                }
                MarkupToken::EndTag { name_lower } => {
                    if name_lower == "binary" {
                        if let Some(id) = cur_binary_id.take() {
                            if let Some(bytes) = decode_base64(&binary_b64) {
                                binaries.insert(id, bytes);
                            }
                            binary_b64.clear();
                        }
                        continue;
                    }
                    match name_lower.as_str() {
                        "book-title" if cur_meta_field == Some("book-title") => {
                            let clean = collapse_inline_ws(&meta_buf, true).trim().to_string();
                            if !clean.is_empty() && book_title.is_empty() {
                                book_title = clean;
                            }
                            cur_meta_field = None;
                        }
                        "first-name" | "middle-name" | "last-name" | "nickname"
                            if cur_meta_field == Some("author-part") =>
                        {
                            let clean = collapse_inline_ws(&meta_buf, true).trim().to_string();
                            if !clean.is_empty() && author_parts.len() < 4 {
                                author_parts.push(clean);
                            }
                            cur_meta_field = None;
                        }
                        "author" => in_author = false,
                        "coverpage" => in_coverpage = false,
                        "title-info" => {
                            in_title_info = false;
                            in_author = false;
                            in_coverpage = false;
                        }
                        "title" if in_notes_body => {
                            in_note_title = false;
                            let clean = collapse_inline_ws(&note_para_buf, true).trim().to_string();
                            if let Some((_, ref mut t_slot, _)) = note_sec_stack.last_mut() {
                                if !clean.is_empty() {
                                    *t_slot = clean;
                                }
                            }
                            note_para_buf.clear();
                        }
                        "p" | "v" | "subtitle" if in_notes_body && !in_note_title => {
                            let clean = collapse_inline_ws(&note_para_buf, true).trim().to_string();
                            if !clean.is_empty() {
                                if let Some((_, _, ref mut paras)) = note_sec_stack.last_mut() {
                                    paras.push(clean);
                                }
                            }
                            note_para_buf.clear();
                        }
                        "section" if in_notes_body => {
                            if let Some((Some(id), title, paras)) = note_sec_stack.pop() {
                                if !paras.is_empty() || !title.is_empty() {
                                    notes_map.insert(id, (title, paras));
                                }
                            }
                        }
                        "body" if in_notes_body => {
                            in_notes_body = false;
                            note_sec_stack.clear();
                        }
                        _ => {}
                    }
                }
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
        author_parts.join(" "),
        DocFormat::Fb2,
    );

    let mut registered_imgs: HashMap<String, usize> = HashMap::new();
    let mut register_fb2_img = |id: &str, builder: &mut DocBuilder| -> Option<usize> {
        let key = id.trim().trim_start_matches('#');
        if let Some(&idx) = registered_imgs.get(key) {
            return Some(idx);
        }
        let bytes = binaries
            .get(key)
            .cloned()
            .or_else(|| {
                let lower = key.to_ascii_lowercase();
                binaries
                    .iter()
                    .find(|(k, _)| k.to_ascii_lowercase() == lower)
                    .map(|(_, v)| v.clone())
            })?;
        let (w, h) = probe_image_dimensions(&bytes)?;
        let idx = builder.register_image(bytes, w, h);
        registered_imgs.insert(key.to_string(), idx);
        Some(idx)
    };

    if let Some(ref cid) = cover_binary_id {
        if let Some(idx) = register_fb2_img(cid, &mut builder) {
            builder.push_image(idx, String::new());
        }
    }

    // Pass 2: Stream main `<body>` (ignoring `<body name="notes"|"comments">`)
    let mut in_main_body = false;
    let mut section_depth = 0usize;
    let mut in_title = false;
    let mut title_lines: Vec<String> = Vec::new();
    let mut section_start_offsets: Vec<usize> = Vec::new();

    let mut epigraph_or_cite_depth = 0usize;
    let mut poem_depth = 0usize;
    let mut bold_depth = 0usize;
    let mut italic_depth = 0usize;
    let mut code_depth = 0usize;
    let mut is_subtitle = false;

    let mut runs: Vec<InlineRun> = Vec::new();
    let mut active_note_id: Option<String> = None;
    let mut note_link_txt = String::new();
    let mut fn_id_to_idx: HashMap<String, usize> = HashMap::new();

    let flush_runs = |runs: &mut Vec<InlineRun>,
                      in_title: bool,
                      is_subtitle: bool,
                      section_depth: usize,
                      quote: bool,
                      code: bool,
                      title_lines: &mut Vec<String>,
                      builder: &mut DocBuilder| {
        if runs.is_empty() {
            return;
        }
        let plain: String = runs.iter().map(|r| r.text.as_str()).collect();
        let trimmed = plain.trim();
        if trimmed.is_empty() {
            runs.clear();
            return;
        }
        if in_title {
            title_lines.push(trimmed.to_string());
        }
        let h_level = if in_title {
            if section_depth <= 1 {
                1
            } else if section_depth == 2 {
                2
            } else {
                3
            }
        } else if is_subtitle {
            2
        } else {
            0
        };
        let drained = std::mem::take(runs);
        builder.push_paragraph(drained, h_level, quote, code, in_title && section_depth <= 1);
    };

    for tok in MarkupTokenizer::new(&xml) {
        match tok {
            MarkupToken::StartTag {
                name_lower,
                attrs_raw,
                self_closing,
            } => {
                if name_lower == "body" && !self_closing {
                    let bname = get_attr(attrs_raw, "name")
                        .unwrap_or_default()
                        .to_ascii_lowercase();
                    in_main_body =
                        bname != "notes" && bname != "comments" && bname != "footnotes";
                    continue;
                }
                if !in_main_body {
                    continue;
                }

                match name_lower.as_str() {
                    "section" if !self_closing => {
                        flush_runs(
                            &mut runs,
                            in_title,
                            is_subtitle,
                            section_depth,
                            epigraph_or_cite_depth > 0 || poem_depth > 0,
                            code_depth > 0,
                            &mut title_lines,
                            &mut builder,
                        );
                        if section_depth == 0 && builder.current_offset() > 2 {
                            builder.push_page_break();
                        }
                        section_depth += 1;
                        section_start_offsets.push(builder.current_offset());
                    }
                    "title" if !self_closing => {
                        flush_runs(
                            &mut runs,
                            in_title,
                            is_subtitle,
                            section_depth,
                            epigraph_or_cite_depth > 0 || poem_depth > 0,
                            code_depth > 0,
                            &mut title_lines,
                            &mut builder,
                        );
                        in_title = true;
                        title_lines.clear();
                    }
                    "subtitle" if !self_closing => {
                        flush_runs(
                            &mut runs,
                            in_title,
                            is_subtitle,
                            section_depth,
                            epigraph_or_cite_depth > 0 || poem_depth > 0,
                            code_depth > 0,
                            &mut title_lines,
                            &mut builder,
                        );
                        is_subtitle = true;
                    }
                    "epigraph" | "cite" | "annotation" if !self_closing => {
                        flush_runs(
                            &mut runs,
                            in_title,
                            is_subtitle,
                            section_depth,
                            epigraph_or_cite_depth > 0 || poem_depth > 0,
                            code_depth > 0,
                            &mut title_lines,
                            &mut builder,
                        );
                        epigraph_or_cite_depth += 1;
                    }
                    "poem" if !self_closing => {
                        flush_runs(
                            &mut runs,
                            in_title,
                            is_subtitle,
                            section_depth,
                            epigraph_or_cite_depth > 0 || poem_depth > 0,
                            code_depth > 0,
                            &mut title_lines,
                            &mut builder,
                        );
                        poem_depth += 1;
                    }
                    "stanza" => {
                        flush_runs(
                            &mut runs,
                            in_title,
                            is_subtitle,
                            section_depth,
                            epigraph_or_cite_depth > 0 || poem_depth > 0,
                            code_depth > 0,
                            &mut title_lines,
                            &mut builder,
                        );
                    }
                    "p" | "v" | "text-author" => {
                        flush_runs(
                            &mut runs,
                            in_title,
                            is_subtitle,
                            section_depth,
                            epigraph_or_cite_depth > 0 || poem_depth > 0,
                            code_depth > 0,
                            &mut title_lines,
                            &mut builder,
                        );
                        if name_lower == "text-author" && !self_closing {
                            italic_depth += 1;
                        }
                    }
                    "empty-line" => {
                        flush_runs(
                            &mut runs,
                            in_title,
                            is_subtitle,
                            section_depth,
                            epigraph_or_cite_depth > 0 || poem_depth > 0,
                            code_depth > 0,
                            &mut title_lines,
                            &mut builder,
                        );
                        builder.push_spacer();
                    }
                    "strong" | "b" if !self_closing => bold_depth += 1,
                    "emphasis" | "i" if !self_closing => italic_depth += 1,
                    "code" if !self_closing => code_depth += 1,
                    "image" => {
                        if let Some(href) = get_attr(attrs_raw, "href") {
                            let id = href.trim().trim_start_matches('#');
                            if cover_binary_id.as_deref() != Some(id) || builder.current_offset() > 4 {
                                flush_runs(
                                    &mut runs,
                                    in_title,
                                    is_subtitle,
                                    section_depth,
                                    epigraph_or_cite_depth > 0 || poem_depth > 0,
                                    code_depth > 0,
                                    &mut title_lines,
                                    &mut builder,
                                );
                                let alt = get_attr(attrs_raw, "alt").unwrap_or_default();
                                if let Some(idx) = register_fb2_img(id, &mut builder) {
                                    builder.push_image(idx, collapse_inline_ws(&alt, true));
                                }
                            }
                        }
                    }
                    "a" if !self_closing => {
                        let atype = get_attr(attrs_raw, "type")
                            .unwrap_or_default()
                            .to_ascii_lowercase();
                        if let Some(href) = get_attr(attrs_raw, "href") {
                            let target = href.trim().trim_start_matches('#').to_string();
                            if !target.is_empty() && (atype == "note" || notes_map.contains_key(&target)) {
                                active_note_id = Some(target);
                                note_link_txt.clear();
                            }
                        }
                    }
                    _ => {}
                }
            }
            MarkupToken::Text(t) => {
                if !in_main_body {
                    continue;
                }
                if active_note_id.is_some() {
                    note_link_txt.push_str(t);
                    continue;
                }
                let clean = collapse_inline_ws(t, runs.is_empty());
                if clean.is_empty() || (runs.is_empty() && clean.trim().is_empty()) {
                    continue;
                }
                let style = resolve_style(
                    bold_depth,
                    italic_depth + usize::from(epigraph_or_cite_depth > 0),
                    code_depth,
                );
                push_merged_run(&mut runs, clean, style);
            }
            MarkupToken::EndTag { name_lower } => {
                if name_lower == "body" {
                    flush_runs(
                        &mut runs,
                        in_title,
                        is_subtitle,
                        section_depth,
                        epigraph_or_cite_depth > 0 || poem_depth > 0,
                        code_depth > 0,
                        &mut title_lines,
                        &mut builder,
                    );
                    in_main_body = false;
                    section_depth = 0;
                    section_start_offsets.clear();
                    continue;
                }
                if !in_main_body {
                    continue;
                }

                match name_lower.as_str() {
                    "a" => {
                        if let Some(note_id) = active_note_id.take() {
                            let raw_lbl = collapse_inline_ws(&note_link_txt, true).trim().to_string();
                            if let Some((ntitle, nparas)) = notes_map.get(&note_id) {
                                let fn_idx = *fn_id_to_idx.entry(note_id).or_insert_with(|| {
                                    let next_n = builder.footnotes.len() + 1;
                                    let badge = format_footnote_badge(
                                        if raw_lbl.is_empty() { ntitle } else { &raw_lbl },
                                        next_n,
                                    );
                                    let idx = builder.footnotes.len();
                                    let heading = if ntitle.is_empty() {
                                        badge.clone()
                                    } else {
                                        format!("{badge}  {ntitle}")
                                    };
                                    builder.footnotes.push(FootnoteDef {
                                        index: idx,
                                        label: badge,
                                        title: heading,
                                        paragraphs: if nparas.is_empty() {
                                            vec![ntitle.clone()]
                                        } else {
                                            nparas.clone()
                                        },
                                    });
                                    idx
                                });
                                let badge = builder.footnotes[fn_idx].label.clone();
                                runs.push(InlineRun {
                                    text: badge,
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
                        }
                    }
                    "title" if in_title => {
                        flush_runs(
                            &mut runs,
                            in_title,
                            is_subtitle,
                            section_depth,
                            epigraph_or_cite_depth > 0 || poem_depth > 0,
                            code_depth > 0,
                            &mut title_lines,
                            &mut builder,
                        );
                        in_title = false;
                        if section_depth > 0 && !title_lines.is_empty() {
                            let flat_title = title_lines.join(" — ");
                            let sec_off = section_start_offsets
                                .last()
                                .copied()
                                .unwrap_or_else(|| builder.current_offset());
                            builder.toc.push(TocEntry {
                                title: flat_title,
                                level: (section_depth - 1).min(4) as u8,
                                offset: sec_off,
                            });
                            title_lines.clear();
                        }
                    }
                    "subtitle" => {
                        flush_runs(
                            &mut runs,
                            in_title,
                            is_subtitle,
                            section_depth,
                            epigraph_or_cite_depth > 0 || poem_depth > 0,
                            code_depth > 0,
                            &mut title_lines,
                            &mut builder,
                        );
                        is_subtitle = false;
                    }
                    "p" | "v" => {
                        flush_runs(
                            &mut runs,
                            in_title,
                            is_subtitle,
                            section_depth,
                            epigraph_or_cite_depth > 0 || poem_depth > 0,
                            code_depth > 0,
                            &mut title_lines,
                            &mut builder,
                        );
                    }
                    "text-author" => {
                        flush_runs(
                            &mut runs,
                            in_title,
                            is_subtitle,
                            section_depth,
                            epigraph_or_cite_depth > 0 || poem_depth > 0,
                            code_depth > 0,
                            &mut title_lines,
                            &mut builder,
                        );
                        italic_depth = italic_depth.saturating_sub(1);
                    }
                    "stanza" => {
                        flush_runs(
                            &mut runs,
                            in_title,
                            is_subtitle,
                            section_depth,
                            epigraph_or_cite_depth > 0 || poem_depth > 0,
                            code_depth > 0,
                            &mut title_lines,
                            &mut builder,
                        );
                        builder.push_spacer();
                    }
                    "epigraph" | "cite" | "annotation" => {
                        flush_runs(
                            &mut runs,
                            in_title,
                            is_subtitle,
                            section_depth,
                            epigraph_or_cite_depth > 0 || poem_depth > 0,
                            code_depth > 0,
                            &mut title_lines,
                            &mut builder,
                        );
                        epigraph_or_cite_depth = epigraph_or_cite_depth.saturating_sub(1);
                    }
                    "poem" => {
                        flush_runs(
                            &mut runs,
                            in_title,
                            is_subtitle,
                            section_depth,
                            epigraph_or_cite_depth > 0 || poem_depth > 0,
                            code_depth > 0,
                            &mut title_lines,
                            &mut builder,
                        );
                        poem_depth = poem_depth.saturating_sub(1);
                    }
                    "section" => {
                        flush_runs(
                            &mut runs,
                            in_title,
                            is_subtitle,
                            section_depth,
                            epigraph_or_cite_depth > 0 || poem_depth > 0,
                            code_depth > 0,
                            &mut title_lines,
                            &mut builder,
                        );
                        section_depth = section_depth.saturating_sub(1);
                        let _ = section_start_offsets.pop();
                    }
                    "strong" | "b" => bold_depth = bold_depth.saturating_sub(1),
                    "emphasis" | "i" => italic_depth = italic_depth.saturating_sub(1),
                    "code" => code_depth = code_depth.saturating_sub(1),
                    _ => {}
                }
            }
        }
    }

    builder.dedup_and_clamp_toc();
    Ok(builder.finish())
}
