//! Plain-text (`.txt`, `.lrc`, `.log`, `.cfg`, `.ini`, `.json`, `.cue`, `.nfo`) and
//! Markdown (`.md` + structured UIPod manuals with `#`/`##`/`###` headings) parser.

use std::collections::HashMap;

use crate::encoding::decode_bytes_auto;
use crate::epub::format_footnote_badge;
use crate::{
    DocBuilder, DocFormat, EbookDoc, EbookError, FootnoteDef, InlineRun, InlineStyle, TocEntry,
};

pub fn parse_plain_or_markdown(
    file_name: &str,
    data: &[u8],
    force_markdown: bool,
) -> Result<EbookDoc, EbookError> {
    let raw = decode_bytes_auto(data, None);
    let lower = file_name.to_ascii_lowercase();
    let is_md_ext = force_markdown || lower.ends_with(".md") || lower.ends_with(".markdown");
    let has_md_headings = raw
        .lines()
        .take(200)
        .any(|l| l.starts_with("# ") || l.starts_with("## ") || l.starts_with("### "));
    let structured = is_md_ext || has_md_headings;

    let doc_title = crate::clean_stem_title(file_name);
    let mut builder = DocBuilder::new(
        doc_title,
        String::new(),
        if is_md_ext {
            DocFormat::Markdown
        } else {
            DocFormat::PlainText
        },
    );

    // Pre-scan Markdown footnote definitions (`[^id]: text...`) when in structured mode
    let mut md_notes: HashMap<String, Vec<String>> = HashMap::new();
    if structured {
        for line in raw.lines() {
            let trimmed = line.trim_end_matches('\r');
            if let Some((id, body)) = parse_md_footnote_def(trimmed) {
                md_notes.entry(id).or_default().push(body);
            }
        }
    }

    let mut fn_map: HashMap<String, usize> = HashMap::new();
    let mut in_code_fence = false;

    for raw_line in raw.lines() {
        let line = raw_line.trim_end_matches('\r');
        if structured {
            let trimmed_start = line.trim_start();
            if trimmed_start.starts_with("```") {
                in_code_fence = !in_code_fence;
                continue;
            }
            if in_code_fence {
                builder.push_paragraph(
                    vec![InlineRun {
                        text: line.replace('\t', "    "),
                        style: InlineStyle::Code,
                        footnote_idx: None,
                    }],
                    0,
                    false,
                    true,
                    false,
                );
                continue;
            }
            if parse_md_footnote_def(line).is_some() {
                continue;
            }
        }

        if line.trim().is_empty() {
            builder.push_spacer();
            continue;
        }

        let expanded = line.replace('\t', "    ");
        if structured {
            if let Some((h_lvl, h_body)) = split_md_heading(&expanded) {
                let off = builder.current_offset();
                builder.toc.push(TocEntry {
                    title: h_body.to_string(),
                    level: h_lvl - 1,
                    offset: off,
                });
                let runs = parse_md_inline_runs(h_body, &md_notes, &mut fn_map, &mut builder.footnotes);
                builder.push_paragraph(runs, h_lvl, false, false, false);
                continue;
            }
            if let Some(q_body) = expanded.trim_start().strip_prefix("> ") {
                let runs = parse_md_inline_runs(q_body, &md_notes, &mut fn_map, &mut builder.footnotes);
                builder.push_paragraph(runs, 0, true, false, false);
                continue;
            }
            if is_md_ext {
                let norm_bullet = if let Some(rest) = expanded.trim_start().strip_prefix("- ") {
                    format!("• {rest}")
                } else if let Some(rest) = expanded.trim_start().strip_prefix("* ") {
                    format!("• {rest}")
                } else {
                    expanded
                };
                let runs = parse_md_inline_runs(
                    &norm_bullet,
                    &md_notes,
                    &mut fn_map,
                    &mut builder.footnotes,
                );
                builder.push_paragraph(runs, 0, false, false, false);
                continue;
            }
        }

        builder.push_paragraph(
            vec![InlineRun {
                text: expanded,
                style: InlineStyle::Regular,
                footnote_idx: None,
            }],
            0,
            false,
            false,
            false,
        );
    }

    if builder.blocks.is_empty() {
        builder.push_paragraph(
            vec![InlineRun {
                text: String::new(),
                style: InlineStyle::Regular,
                footnote_idx: None,
            }],
            0,
            false,
            false,
            false,
        );
    }

    builder.dedup_and_clamp_toc();
    Ok(builder.finish())
}

fn split_md_heading(line: &str) -> Option<(u8, &str)> {
    if let Some(r) = line.strip_prefix("# ") {
        Some((1, r.trim()))
    } else if let Some(r) = line.strip_prefix("## ") {
        Some((2, r.trim()))
    } else if let Some(r) = line.strip_prefix("### ") {
        Some((3, r.trim()))
    } else {
        None
    }
}

fn parse_md_footnote_def(line: &str) -> Option<(String, String)> {
    let rest = line.strip_prefix("[^")?;
    let (id, after) = rest.split_once("]:")?;
    if id.is_empty() || id.len() > 24 {
        return None;
    }
    Some((id.trim().to_string(), after.trim().to_string()))
}

fn parse_md_inline_runs(
    line: &str,
    md_notes: &HashMap<String, Vec<String>>,
    fn_map: &mut HashMap<String, usize>,
    footnotes: &mut Vec<FootnoteDef>,
) -> Vec<InlineRun> {
    let mut runs = Vec::new();
    let mut cur = String::new();
    let mut rest = line;

    let flush = |cur: &mut String, runs: &mut Vec<InlineRun>| {
        if !cur.is_empty() {
            runs.push(InlineRun {
                text: std::mem::take(cur),
                style: InlineStyle::Regular,
                footnote_idx: None,
            });
        }
    };

    while !rest.is_empty() {
        // Markdown footnote ref `[^1]`
        if let Some(after_br) = rest.strip_prefix("[^") {
            if let Some(end_idx) = after_br.find(']') {
                let id = &after_br[..end_idx];
                if let Some(paras) = md_notes.get(id) {
                    flush(&mut cur, &mut runs);
                    let fn_idx = *fn_map.entry(id.to_string()).or_insert_with(|| {
                        let next_n = footnotes.len() + 1;
                        let badge = format_footnote_badge(id, next_n);
                        let idx = footnotes.len();
                        footnotes.push(FootnoteDef {
                            index: idx,
                            label: badge.clone(),
                            title: badge,
                            paragraphs: paras.clone(),
                        });
                        idx
                    });
                    runs.push(InlineRun {
                        text: footnotes[fn_idx].label.clone(),
                        style: InlineStyle::FootnoteRef,
                        footnote_idx: Some(fn_idx),
                    });
                    rest = &after_br[end_idx + 1..];
                    continue;
                }
            }
        }
        // Inline code `` `...` ``
        if let Some(after_tick) = rest.strip_prefix('`') {
            if let Some(end) = after_tick.find('`') {
                if end > 0 {
                    flush(&mut cur, &mut runs);
                    runs.push(InlineRun {
                        text: after_tick[..end].to_string(),
                        style: InlineStyle::Code,
                        footnote_idx: None,
                    });
                    rest = &after_tick[end + 1..];
                    continue;
                }
            }
        }
        // Bold `**...**`
        if let Some(after_b) = rest.strip_prefix("**") {
            if let Some(end) = after_b.find("**") {
                if end > 0 {
                    flush(&mut cur, &mut runs);
                    runs.push(InlineRun {
                        text: after_b[..end].to_string(),
                        style: InlineStyle::Bold,
                        footnote_idx: None,
                    });
                    rest = &after_b[end + 2..];
                    continue;
                }
            }
        }
        // Italic `*...*`
        if let Some(after_i) = rest.strip_prefix('*') {
            if !after_i.starts_with(' ') {
                if let Some(end) = after_i.find('*') {
                    if end > 0 {
                        flush(&mut cur, &mut runs);
                        runs.push(InlineRun {
                            text: after_i[..end].to_string(),
                            style: InlineStyle::Italic,
                            footnote_idx: None,
                        });
                        rest = &after_i[end + 1..];
                        continue;
                    }
                }
            }
        }
        // Markdown link `[Label](url)` -> render `Label`
        if let Some(after_l) = rest.strip_prefix('[') {
            if let Some(close_b) = after_l.find("](") {
                let after_url = &after_l[close_b + 2..];
                if let Some(close_p) = after_url.find(')') {
                    let lbl = &after_l[..close_b];
                    cur.push_str(lbl);
                    rest = &after_url[close_p + 1..];
                    continue;
                }
            }
        }

        let mut chars = rest.chars();
        if let Some(ch) = chars.next() {
            cur.push(ch);
            rest = chars.as_str();
        }
    }
    flush(&mut cur, &mut runs);
    runs
}
