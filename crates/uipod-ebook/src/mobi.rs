//! Pure-Rust DRM-free `.mobi` / `.prc` / `.azw` (PalmDB + PalmDOC `LZ77` + MOBI/EXTH) parser:
//! - Decompresses PalmDOC `LZ77` and uncompressed text records (stripping trailing multibyte overlap bytes)
//! - Gracefully reports DRM-encrypted or Huff/CDIC books via [`EbookError::DrmProtected`] / [`EbookError::UnsupportedCompression`]
//! - Extracts embedded `recindex` image records + EXTH cover art
//! - Resolves exact `filepos="0000123456"` byte offsets into canonical `TocEntry` & `FootnoteDef` popups!

use std::collections::HashMap;

use crate::encoding::{decode_bytes_auto, decode_entities, TextEncoding};
use crate::epub::{format_footnote_badge, push_merged_run, resolve_style};
use crate::image_dither::probe_image_dimensions;
use crate::xml_html::{collapse_inline_ws, get_attr, MarkupToken, MarkupTokenizer};
use crate::{
    DocBuilder, DocFormat, EbookDoc, EbookError, FootnoteDef, InlineRun, TocEntry,
};

const MAX_MOBI_TEXT_BYTES: usize = 16 * 1024 * 1024;

pub fn parse_mobi(file_name: &str, data: &[u8]) -> Result<EbookDoc, EbookError> {
    if data.len() < 86 {
        return Err(EbookError::InvalidFormat("File too short for PalmDB/MOBI".into()));
    }

    let pdb_name = String::from_utf8_lossy(&data[0..32])
        .trim_end_matches('\0')
        .trim()
        .to_string();
    let magic_type = &data[60..68];
    let is_mobi_magic = magic_type == b"BOOKMOBI" || magic_type == b"TEXtREAd";
    if !is_mobi_magic {
        return Err(EbookError::InvalidFormat(
            "Invalid PalmDB type (expected BOOKMOBI or TEXtREAd)".into(),
        ));
    }

    let num_records = read_u16_be(data, 76).ok_or_else(|| EbookError::InvalidFormat("Truncated PDB".into()))? as usize;
    if num_records < 2 || 78 + num_records * 8 > data.len() {
        return Err(EbookError::InvalidFormat("Invalid PalmDB record table".into()));
    }

    let mut rec_offsets = Vec::with_capacity(num_records + 1);
    for i in 0..num_records {
        let off = read_u32_be(data, 78 + i * 8).unwrap_or(0) as usize;
        rec_offsets.push(off.min(data.len()));
    }
    rec_offsets.push(data.len());

    let record_slice = |idx: usize| -> Option<&[u8]> {
        if idx >= num_records {
            return None;
        }
        let s = rec_offsets[idx];
        let e = rec_offsets[idx + 1];
        if s <= e && e <= data.len() {
            Some(&data[s..e])
        } else {
            None
        }
    };

    let rec0 = record_slice(0).ok_or_else(|| EbookError::InvalidFormat("Empty MOBI Record 0".into()))?;
    if rec0.len() < 16 {
        return Err(EbookError::InvalidFormat("Truncated PalmDOC header".into()));
    }

    let compression = read_u16_be(rec0, 0).unwrap_or(0);
    let text_rec_count = read_u16_be(rec0, 8).unwrap_or(0) as usize;
    let encryption_type = read_u16_be(rec0, 12).unwrap_or(0);

    if encryption_type != 0 {
        return Err(EbookError::DrmProtected);
    }
    if compression == 17480 {
        return Err(EbookError::UnsupportedCompression("MOBI Huff/CDIC".into()));
    }
    if compression != 1 && compression != 2 {
        return Err(EbookError::InvalidFormat(format!(
            "Unknown PalmDOC compression ({compression})"
        )));
    }

    // Parse optional MOBI + EXTH headers inside Record 0 (starting at offset 16)
    let mut text_enc = TextEncoding::Windows1252;
    let mut first_image_rec: Option<usize> = None;
    let mut extra_data_flags: u16 = 0;
    let mut full_title = String::new();
    let mut author = String::new();
    let mut exth_cover_offset: Option<usize> = None;

    if rec0.len() >= 36 && &rec0[16..20] == b"MOBI" {
        let mobi_hdr_len = read_u32_be(rec0, 20).unwrap_or(0) as usize;
        let codepage = read_u32_be(rec0, 28).unwrap_or(1252);
        text_enc = match codepage {
            65001 => TextEncoding::Utf8,
            1251 => TextEncoding::Windows1251,
            _ => TextEncoding::Windows1252,
        };
        if rec0.len() >= 92 {
            let full_name_off = read_u32_be(rec0, 84).unwrap_or(0) as usize;
            let full_name_len = read_u32_be(rec0, 88).unwrap_or(0) as usize;
            if full_name_len > 0 && full_name_off + full_name_len <= rec0.len() {
                full_title = decode_bytes_auto(
                    &rec0[full_name_off..full_name_off + full_name_len],
                    Some(text_enc),
                )
                .trim()
                .to_string();
            }
        }
        if rec0.len() >= 112 {
            let img_idx = read_u32_be(rec0, 108).unwrap_or(0xFFFF_FFFF);
            if img_idx != 0xFFFF_FFFF && (img_idx as usize) < num_records {
                first_image_rec = Some(img_idx as usize);
            }
        }
        if mobi_hdr_len >= 0xF4 && rec0.len() >= 16 + 0xF4 {
            extra_data_flags = read_u16_be(rec0, 16 + 0xF2).unwrap_or(0);
        }

        let exth_flags = if rec0.len() >= 132 {
            read_u32_be(rec0, 128).unwrap_or(0)
        } else {
            0
        };
        let exth_off = 16 + mobi_hdr_len;
        if (exth_flags & 0x40) != 0 && exth_off + 12 <= rec0.len() && &rec0[exth_off..exth_off + 4] == b"EXTH" {
            let exth_count = read_u32_be(rec0, exth_off + 8).unwrap_or(0) as usize;
            let mut cur = exth_off + 12;
            for _ in 0..exth_count {
                if cur + 8 > rec0.len() {
                    break;
                }
                let rtype = read_u32_be(rec0, cur).unwrap_or(0);
                let rlen = read_u32_be(rec0, cur + 4).unwrap_or(0) as usize;
                if rlen < 8 || cur + rlen > rec0.len() {
                    break;
                }
                let payload = &rec0[cur + 8..cur + rlen];
                match rtype {
                    100 if author.is_empty() => {
                        author = decode_bytes_auto(payload, Some(text_enc)).trim().to_string();
                    }
                    503 => {
                        let upd = decode_bytes_auto(payload, Some(text_enc)).trim().to_string();
                        if !upd.is_empty() {
                            full_title = upd;
                        }
                    }
                    201 if payload.len() == 4 => {
                        let co = u32::from_be_bytes([payload[0], payload[1], payload[2], payload[3]]);
                        if co != 0xFFFF_FFFF {
                            exth_cover_offset = Some(co as usize);
                        }
                    }
                    _ => {}
                }
                cur += rlen;
            }
        }
    }

    // 2. Decompress all text records (`1..=text_rec_count`) into a single raw byte stream
    let mut raw_text = Vec::new();
    let max_rec = text_rec_count.min(num_records.saturating_sub(1));
    for r_idx in 1..=max_rec {
        let Some(raw_rec) = record_slice(r_idx) else {
            continue;
        };
        let trimmed = strip_mobi_trailing_data(raw_rec, extra_data_flags);
        if compression == 1 {
            raw_text.extend_from_slice(trimmed);
        } else {
            decompress_palmdoc_lz77(trimmed, &mut raw_text);
        }
        if raw_text.len() >= MAX_MOBI_TEXT_BYTES {
            raw_text.truncate(MAX_MOBI_TEXT_BYTES);
            break;
        }
    }

    let fallback_title = if !pdb_name.is_empty() {
        pdb_name
    } else {
        crate::clean_stem_title(file_name)
    };
    let doc_title = if full_title.is_empty() {
        fallback_title
    } else {
        full_title
    };

    // Pure `TEXtREAd` plain-text PalmDOC without HTML tags
    if magic_type == b"TEXtREAd" && !looks_like_mobi_html(&raw_text) {
        let text = decode_bytes_auto(&raw_text, Some(text_enc));
        let mut doc = crate::plain_md::parse_plain_or_markdown(file_name, text.as_bytes(), false)?;
        doc.title = doc_title;
        doc.format = DocFormat::Mobi;
        return Ok(doc);
    }

    let decoded_html = decode_bytes_auto(&raw_text, Some(text_enc));

    // Build byte-offset mapping from raw decompressed MOBI byte offset (`filepos`) -> UTF-8 byte index in `decoded_html`
    // When `text_enc == Utf8`, raw `filepos` equals `decoded_html` byte offset (clamped to char boundary);
    // when single-byte (`CP1252`/`CP1251`), raw byte `N` corresponds to character `N`!
    let is_single_byte = text_enc != TextEncoding::Utf8;
    let char_to_str_byte: Vec<usize> = if is_single_byte {
        decoded_html.char_indices().map(|(bi, _)| bi).collect()
    } else {
        Vec::new()
    };
    let map_filepos_to_str_idx = |fp: usize| -> usize {
        if is_single_byte {
            if char_to_str_byte.is_empty() {
                0
            } else if fp >= char_to_str_byte.len() {
                decoded_html.len()
            } else {
                char_to_str_byte[fp]
            }
        } else {
            let mut b = fp.min(decoded_html.len());
            while b > 0 && !decoded_html.is_char_boundary(b) {
                b -= 1;
            }
            b
        }
    };

    let mut builder = DocBuilder::new(doc_title, author, DocFormat::Mobi);
    let mut rec_img_cache: HashMap<usize, usize> = HashMap::new();

    let mut load_mobi_image_rec = |rec_num: usize, builder: &mut DocBuilder| -> Option<usize> {
        if let Some(&idx) = rec_img_cache.get(&rec_num) {
            return Some(idx);
        }
        let bytes = record_slice(rec_num)?;
        let (w, h) = probe_image_dimensions(bytes)?;
        let idx = builder.register_image(bytes.to_vec(), w, h);
        rec_img_cache.insert(rec_num, idx);
        Some(idx)
    };

    if let (Some(first_img), Some(cov_off)) = (first_image_rec, exth_cover_offset) {
        if let Some(idx) = load_mobi_image_rec(first_img + cov_off, &mut builder) {
            builder.push_image(idx, String::new());
        }
    }

    // Collect all `<a filepos="...">Label</a>` links first so we can distinguish TOC clusters from inline footnotes
    // and extract footnote snippets at target `filepos`!
    let mut all_filepos_links: Vec<(usize, usize, String)> = Vec::new(); // (link_str_pos, target_str_pos, label)
    {
        let mut cur_fp: Option<(usize, usize)> = None;
        let mut lbl_buf = String::new();
        let mut tok_iter = MarkupTokenizer::new(&decoded_html);
        while let Some(tok) = tok_iter.next() {
            match tok {
                MarkupToken::StartTag {
                    name_lower,
                    attrs_raw,
                    self_closing,
                } if name_lower == "a" && !self_closing => {
                    if let Some(fp_str) = get_attr(attrs_raw, "filepos") {
                        if let Ok(fp_val) = fp_str.trim().parse::<usize>() {
                            let target_idx = map_filepos_to_str_idx(fp_val);
                            let link_pos = attrs_raw.as_ptr() as usize - decoded_html.as_ptr() as usize;
                            cur_fp = Some((link_pos, target_idx));
                            lbl_buf.clear();
                        }
                    }
                }
                MarkupToken::Text(t) => {
                    if cur_fp.is_some() {
                        lbl_buf.push_str(t);
                    }
                }
                MarkupToken::EndTag { name_lower } if name_lower == "a" => {
                    if let Some((link_pos, target_idx)) = cur_fp.take() {
                        let clean = collapse_inline_ws(&lbl_buf, true).trim().to_string();
                        if !clean.is_empty() {
                            all_filepos_links.push((link_pos, target_idx, clean));
                        }
                    }
                }
                _ => {}
            }
        }
    }

    // Stream HTML into blocks while recording `(str_byte_pos, canonical_offset)`checkpoints
    let mut pos_checkpoints: Vec<(usize, usize)> = Vec::new();
    let mut fallback_toc: Vec<TocEntry> = Vec::new();
    let mut raw_mobi_toc: Vec<(String, usize)> = Vec::new();
    let mut fn_by_target: HashMap<usize, usize> = HashMap::new();

    let mut heading_level = 0u8;
    let mut bold_depth = 0usize;
    let mut italic_depth = 0usize;
    let mut code_depth = 0usize;
    let mut quote_depth = 0usize;
    let mut runs: Vec<InlineRun> = Vec::new();
    let mut active_fp_target: Option<usize> = None;
    let mut fp_link_txt = String::new();

    let flush_para = |runs: &mut Vec<InlineRun>,
                      heading_level: u8,
                      quote_depth: usize,
                      fallback_toc: &mut Vec<TocEntry>,
                      builder: &mut DocBuilder| {
        if runs.is_empty() {
            return;
        }
        let plain: String = runs.iter().map(|r| r.text.as_str()).collect();
        if plain.trim().is_empty() {
            runs.clear();
            return;
        }
        let off = builder.current_offset();
        if (1..=3).contains(&heading_level) {
            fallback_toc.push(TocEntry {
                title: plain.trim().to_string(),
                level: heading_level - 1,
                offset: off,
            });
        }
        let drained = std::mem::take(runs);
        builder.push_paragraph(drained, heading_level, quote_depth > 0, false, heading_level == 1);
    };

    for tok in MarkupTokenizer::new(&decoded_html) {
        match tok {
            MarkupToken::StartTag {
                name_lower,
                attrs_raw,
                self_closing,
            } => {
                let approx_pos = if !attrs_raw.is_empty() {
                    (attrs_raw.as_ptr() as usize).saturating_sub(decoded_html.as_ptr() as usize)
                } else {
                    pos_checkpoints.last().map(|p| p.0).unwrap_or(0)
                };
                if pos_checkpoints.last().map(|p| p.0 != approx_pos).unwrap_or(true) {
                    pos_checkpoints.push((approx_pos, builder.current_offset()));
                }

                match name_lower.as_str() {
                    "h1" | "h2" | "h3" | "h4" | "h5" | "h6" => {
                        flush_para(&mut runs, heading_level, quote_depth, &mut fallback_toc, &mut builder);
                        if !self_closing {
                            heading_level = match name_lower.as_str() {
                                "h1" => 1,
                                "h2" => 2,
                                _ => 3,
                            };
                        }
                    }
                    "p" | "div" | "li" | "tr" | "dt" | "dd" => {
                        flush_para(&mut runs, heading_level, quote_depth, &mut fallback_toc, &mut builder);
                    }
                    "pagebreak" => {
                        flush_para(&mut runs, heading_level, quote_depth, &mut fallback_toc, &mut builder);
                        builder.push_page_break();
                    }
                    "br" => {
                        flush_para(&mut runs, heading_level, quote_depth, &mut fallback_toc, &mut builder);
                    }
                    "hr" => {
                        flush_para(&mut runs, heading_level, quote_depth, &mut fallback_toc, &mut builder);
                        builder.push_spacer();
                    }
                    "blockquote" if !self_closing => {
                        flush_para(&mut runs, heading_level, quote_depth, &mut fallback_toc, &mut builder);
                        quote_depth += 1;
                    }
                    "b" | "strong" if !self_closing => bold_depth += 1,
                    "i" | "em" if !self_closing => italic_depth += 1,
                    "code" | "tt" if !self_closing => code_depth += 1,
                    "img" => {
                        if let (Some(first_img), Some(rec_attr)) =
                            (first_image_rec, get_attr(attrs_raw, "recindex"))
                        {
                            if let Ok(one_based) = rec_attr.trim().parse::<usize>() {
                                if one_based >= 1 {
                                    let rec_num = first_img + one_based - 1;
                                    if exth_cover_offset != Some(one_based - 1) || builder.current_offset() > 4 {
                                        flush_para(
                                            &mut runs,
                                            heading_level,
                                            quote_depth,
                                            &mut fallback_toc,
                                            &mut builder,
                                        );
                                        if let Some(img_idx) = load_mobi_image_rec(rec_num, &mut builder) {
                                            let alt = get_attr(attrs_raw, "alt").unwrap_or_default();
                                            builder.push_image(img_idx, collapse_inline_ws(&alt, true));
                                        }
                                    }
                                }
                            }
                        }
                    }
                    "a" if !self_closing => {
                        if let Some(fp_str) = get_attr(attrs_raw, "filepos") {
                            if let Ok(fp_val) = fp_str.trim().parse::<usize>() {
                                active_fp_target = Some(map_filepos_to_str_idx(fp_val));
                                fp_link_txt.clear();
                            }
                        }
                    }
                    _ => {}
                }
            }
            MarkupToken::Text(t) => {
                let txt_pos = (t.as_ptr() as usize).saturating_sub(decoded_html.as_ptr() as usize);
                if pos_checkpoints.last().map(|p| p.0 != txt_pos).unwrap_or(true) {
                    pos_checkpoints.push((txt_pos, builder.current_offset()));
                }
                if active_fp_target.is_some() {
                    fp_link_txt.push_str(t);
                    continue;
                }
                let clean = collapse_inline_ws(t, runs.is_empty());
                if clean.is_empty() || (runs.is_empty() && clean.trim().is_empty()) {
                    continue;
                }
                push_merged_run(
                    &mut runs,
                    clean,
                    resolve_style(bold_depth, italic_depth, code_depth),
                );
            }
            MarkupToken::EndTag { name_lower } => match name_lower.as_str() {
                "a" => {
                    if let Some(target_str_idx) = active_fp_target.take() {
                        let raw_lbl = collapse_inline_ws(&fp_link_txt, true).trim().to_string();
                        let is_compact_fn = !raw_lbl.is_empty()
                            && raw_lbl.chars().count() <= 7
                            && raw_lbl.chars().any(|c| c.is_ascii_digit() || c == '*');
                        if is_compact_fn {
                            let snippet = extract_mobi_footnote_at(&decoded_html, target_str_idx);
                            if !snippet.is_empty() {
                                let fn_idx = *fn_by_target.entry(target_str_idx).or_insert_with(|| {
                                    let next_n = builder.footnotes.len() + 1;
                                    let badge = format_footnote_badge(&raw_lbl, next_n);
                                    let idx = builder.footnotes.len();
                                    builder.footnotes.push(FootnoteDef {
                                        index: idx,
                                        label: badge.clone(),
                                        title: badge,
                                        paragraphs: snippet,
                                    });
                                    idx
                                });
                                let badge = builder.footnotes[fn_idx].label.clone();
                                runs.push(InlineRun {
                                    text: badge,
                                    style: crate::InlineStyle::FootnoteRef,
                                    footnote_idx: Some(fn_idx),
                                });
                            } else {
                                push_merged_run(
                                    &mut runs,
                                    raw_lbl,
                                    resolve_style(bold_depth, italic_depth, code_depth),
                                );
                            }
                        } else if !raw_lbl.is_empty() {
                            raw_mobi_toc.push((raw_lbl.clone(), target_str_idx));
                            push_merged_run(
                                &mut runs,
                                raw_lbl,
                                resolve_style(bold_depth, italic_depth, code_depth),
                            );
                        }
                    }
                }
                "h1" | "h2" | "h3" | "h4" | "h5" | "h6" => {
                    flush_para(&mut runs, heading_level, quote_depth, &mut fallback_toc, &mut builder);
                    heading_level = 0;
                }
                "p" | "div" | "li" | "tr" | "dt" | "dd" => {
                    flush_para(&mut runs, heading_level, quote_depth, &mut fallback_toc, &mut builder);
                }
                "blockquote" => {
                    flush_para(&mut runs, heading_level, quote_depth, &mut fallback_toc, &mut builder);
                    quote_depth = quote_depth.saturating_sub(1);
                }
                "b" | "strong" => bold_depth = bold_depth.saturating_sub(1),
                "i" | "em" => italic_depth = italic_depth.saturating_sub(1),
                "code" | "tt" => code_depth = code_depth.saturating_sub(1),
                _ => {}
            },
        }
    }
    flush_para(&mut runs, heading_level, quote_depth, &mut fallback_toc, &mut builder);

    // Resolve inline MOBI `<a filepos="...">` TOC entries to canonical offsets via binary search
    let _ = all_filepos_links;
    if raw_mobi_toc.len() >= 2 {
        pos_checkpoints.sort_by_key(|p| p.0);
        for (title, target_str_idx) in raw_mobi_toc {
            let idx = match pos_checkpoints.binary_search_by_key(&target_str_idx, |p| p.0) {
                Ok(i) => i,
                Err(i) => i.min(pos_checkpoints.len().saturating_sub(1)),
            };
            if let Some(&(_, canon_off)) = pos_checkpoints.get(idx) {
                builder.toc.push(TocEntry {
                    title,
                    level: 0,
                    offset: canon_off,
                });
            }
        }
    }
    if builder.toc.is_empty() {
        builder.toc = fallback_toc;
    }
    builder.dedup_and_clamp_toc();

    Ok(builder.finish())
}

fn looks_like_mobi_html(bytes: &[u8]) -> bool {
    let head = &bytes[..bytes.len().min(2048)];
    let s = String::from_utf8_lossy(head).to_ascii_lowercase();
    s.contains("<html") || s.contains("<body") || s.contains("<p") || s.contains("<mbp:")
}

fn extract_mobi_footnote_at(html: &str, str_pos: usize) -> Vec<String> {
    if str_pos >= html.len() {
        return Vec::new();
    }
    let slice_end = (str_pos + 2400).min(html.len());
    let mut end = slice_end;
    while end > str_pos && !html.is_char_boundary(end) {
        end -= 1;
    }
    let window = &html[str_pos..end];
    let mut paras = Vec::new();
    let mut cur = String::new();
    let mut blocks_done = 0usize;

    for tok in MarkupTokenizer::new(window) {
        match tok {
            MarkupToken::StartTag { name_lower, .. } => {
                if name_lower == "pagebreak" {
                    break;
                }
                if matches!(name_lower.as_str(), "p" | "div" | "br") && !cur.trim().is_empty() {
                    paras.push(collapse_inline_ws(&cur, true).trim().to_string());
                    cur.clear();
                    blocks_done += 1;
                    if blocks_done >= 2 {
                        break;
                    }
                }
            }
            MarkupToken::Text(t) => {
                cur.push_str(&decode_entities(t));
            }
            MarkupToken::EndTag { name_lower } => {
                if matches!(name_lower.as_str(), "p" | "div") && !cur.trim().is_empty() {
                    paras.push(collapse_inline_ws(&cur, true).trim().to_string());
                    cur.clear();
                    blocks_done += 1;
                    if blocks_done >= 2 {
                        break;
                    }
                }
            }
        }
    }
    if paras.is_empty() && !cur.trim().is_empty() {
        paras.push(collapse_inline_ws(&cur, true).trim().to_string());
    }
    paras
}

/// Strips MOBI `extra_data_flags` trailing entries (multibyte UTF-8 overlap + indexing trailers)
/// from the end of a compressed text record before PalmDOC LZ77 decompression.
fn strip_mobi_trailing_data(rec: &[u8], extra_flags: u16) -> &[u8] {
    if extra_flags == 0 || rec.is_empty() {
        return rec;
    }
    let mut end = rec.len();
    for bit in (1..16).rev() {
        if (extra_flags & (1 << bit)) != 0 {
            let sz = read_mobi_backwards_varint(&rec[..end]);
            end = end.saturating_sub(sz);
        }
    }
    if (extra_flags & 1) != 0 && end > 0 {
        let overlap = ((rec[end - 1] & 0x03) as usize) + 1;
        end = end.saturating_sub(overlap);
    }
    &rec[..end]
}

fn read_mobi_backwards_varint(slice: &[u8]) -> usize {
    let mut val = 0usize;
    let mut shift = 0usize;
    for &b in slice.iter().rev().take(4) {
        val |= ((b & 0x7F) as usize) << shift;
        if (b & 0x80) != 0 {
            return val.min(slice.len());
        }
        shift += 7;
    }
    0
}

/// Standard PalmDOC LZ77 decompression (`compression == 2`).
pub fn decompress_palmdoc_lz77(src: &[u8], out: &mut Vec<u8>) {
    let mut i = 0usize;
    while i < src.len() {
        let c = src[i];
        i += 1;
        match c {
            0x00 => out.push(0x00),
            0x01..=0x08 => {
                let n = (c as usize).min(src.len() - i);
                out.extend_from_slice(&src[i..i + n]);
                i += n;
            }
            0x09..=0x7F => out.push(c),
            0x80..=0xBF => {
                if i >= src.len() {
                    break;
                }
                let next = src[i];
                i += 1;
                let pair = (((c as u16) << 8) | (next as u16)) & 0x3FFF;
                let dist = (pair >> 3) as usize;
                let len = ((pair & 0x07) as usize) + 3;
                if dist == 0 || dist > out.len() {
                    continue;
                }
                for _ in 0..len {
                    let b = out[out.len() - dist];
                    out.push(b);
                }
            }
            0xC0..=0xFF => {
                out.push(b' ');
                out.push(c ^ 0x80);
            }
        }
    }
}

#[inline]
fn read_u16_be(data: &[u8], off: usize) -> Option<u16> {
    let b = data.get(off..off + 2)?;
    Some(u16::from_be_bytes([b[0], b[1]]))
}

#[inline]
fn read_u32_be(data: &[u8], off: usize) -> Option<u32> {
    let b = data.get(off..off + 4)?;
    Some(u32::from_be_bytes([b[0], b[1], b[2], b[3]]))
}
