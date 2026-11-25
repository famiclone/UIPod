//! Lenient, zero-panic streaming XML / XHTML / HTML5 pull-tokenizer.

use crate::encoding::decode_entities;

#[derive(Debug, Clone)]
pub enum MarkupToken<'a> {
    StartTag {
        name_lower: String,
        attrs_raw: &'a str,
        self_closing: bool,
    },
    EndTag {
        name_lower: String,
    },
    Text(&'a str),
}

pub struct MarkupTokenizer<'a> {
    src: &'a str,
    pos: usize,
}

impl<'a> MarkupTokenizer<'a> {
    pub fn new(src: &'a str) -> Self {
        Self { src, pos: 0 }
    }
}

impl<'a> Iterator for MarkupTokenizer<'a> {
    type Item = MarkupToken<'a>;

    fn next(&mut self) -> Option<Self::Item> {
        let bytes = self.src.as_bytes();
        while self.pos < bytes.len() {
            let rem = &self.src[self.pos..];
            let Some(lt_rel) = rem.find('<') else {
                self.pos = bytes.len();
                return Some(MarkupToken::Text(rem));
            };
            if lt_rel > 0 {
                let txt = &rem[..lt_rel];
                self.pos += lt_rel;
                return Some(MarkupToken::Text(txt));
            }

            // At `<`
            let after_lt = &rem[1..];
            if let Some(after_comment) = after_lt.strip_prefix("!--") {
                if let Some(end_rel) = after_comment.find("-->") {
                    self.pos += 4 + end_rel + 3;
                } else {
                    self.pos = bytes.len();
                }
                continue;
            }
            if let Some(after_cdata) = after_lt.strip_prefix("![CDATA[") {
                if let Some(end_rel) = after_cdata.find("]]>") {
                    let cdata = &after_cdata[..end_rel];
                    self.pos += 9 + end_rel + 3;
                    if !cdata.is_empty() {
                        return Some(MarkupToken::Text(cdata));
                    }
                } else {
                    self.pos = bytes.len();
                }
                continue;
            }
            if after_lt.starts_with('?') || after_lt.starts_with('!') {
                if let Some(gt_rel) = find_tag_end(after_lt) {
                    self.pos += 1 + gt_rel + 1;
                } else {
                    self.pos = bytes.len();
                }
                continue;
            }

            let Some(gt_rel) = find_tag_end(after_lt) else {
                // Stray `<` at end of document
                self.pos += 1;
                return Some(MarkupToken::Text("<"));
            };
            let inside = after_lt[..gt_rel].trim();
            self.pos += 1 + gt_rel + 1;

            if inside.is_empty() {
                continue;
            }

            if let Some(end_body) = inside.strip_prefix('/') {
                let raw_name = end_body
                    .trim()
                    .split(|c: char| c.is_ascii_whitespace() || c == '/')
                    .next()
                    .unwrap_or("");
                let name_lower = strip_ns_lower(raw_name);
                if !name_lower.is_empty() {
                    return Some(MarkupToken::EndTag { name_lower });
                }
                continue;
            }

            let self_closing = inside.ends_with('/');
            let body_no_slash = if self_closing {
                inside[..inside.len() - 1].trim_end()
            } else {
                inside
            };

            let name_end = body_no_slash
                .find(|c: char| c.is_ascii_whitespace() || c == '/')
                .unwrap_or(body_no_slash.len());
            let raw_name = &body_no_slash[..name_end];
            let name_lower = strip_ns_lower(raw_name);
            if name_lower.is_empty() {
                continue;
            }

            // Skip `<script>` and `<style>` raw text blocks cleanly
            if name_lower == "script" || name_lower == "style" {
                if !self_closing {
                    skip_until_closing_tag(self.src, &mut self.pos, &name_lower);
                }
                continue;
            }

            let attrs_raw = body_no_slash[name_end..].trim();
            return Some(MarkupToken::StartTag {
                name_lower,
                attrs_raw,
                self_closing,
            });
        }
        None
    }
}

fn find_tag_end(s: &str) -> Option<usize> {
    let mut quote: Option<u8> = None;
    for (i, b) in s.bytes().enumerate() {
        match quote {
            Some(q) if b == q => quote = None,
            Some(_) => {}
            None if b == b'"' || b == b'\'' => quote = Some(b),
            None if b == b'>' => return Some(i),
            _ => {}
        }
    }
    None
}

fn strip_ns_lower(tag: &str) -> String {
    let local = match tag.rfind(':') {
        Some(colon) => &tag[colon + 1..],
        None => tag,
    };
    local.to_ascii_lowercase()
}

fn skip_until_closing_tag(src: &str, pos: &mut usize, tag_lower: &str) {
    let rem = &src[*pos..];
    let lower = rem.to_ascii_lowercase();
    let needle = format!("</{tag_lower}");
    if let Some(rel) = lower.find(&needle) {
        let after = &rem[rel..];
        if let Some(gt) = after.find('>') {
            *pos += rel + gt + 1;
            return;
        }
    }
    *pos = src.len();
}

/// Extracts an attribute from `attrs_raw` matching `target_name` (either exact lowercased name
/// like `"epub:type"` or local namespace-stripped name like `"href"` matching `l:href` / `xlink:href`).
pub fn get_attr(attrs_raw: &str, target_name: &str) -> Option<String> {
    let target_lower = target_name.to_ascii_lowercase();
    let match_any_ns = !target_lower.contains(':');
    let bytes = attrs_raw.as_bytes();
    let mut i = 0;

    while i < bytes.len() {
        while i < bytes.len() && (bytes[i].is_ascii_whitespace() || bytes[i] == b'/') {
            i += 1;
        }
        if i >= bytes.len() {
            break;
        }
        let name_start = i;
        while i < bytes.len()
            && !bytes[i].is_ascii_whitespace()
            && bytes[i] != b'='
            && bytes[i] != b'/'
        {
            i += 1;
        }
        let raw_attr = &attrs_raw[name_start..i];
        while i < bytes.len() && bytes[i].is_ascii_whitespace() {
            i += 1;
        }
        let val = if i < bytes.len() && bytes[i] == b'=' {
            i += 1;
            while i < bytes.len() && bytes[i].is_ascii_whitespace() {
                i += 1;
            }
            if i < bytes.len() && (bytes[i] == b'"' || bytes[i] == b'\'') {
                let q = bytes[i];
                i += 1;
                let v_start = i;
                while i < bytes.len() && bytes[i] != q {
                    i += 1;
                }
                let v = &attrs_raw[v_start..i];
                if i < bytes.len() {
                    i += 1;
                }
                v
            } else {
                let v_start = i;
                while i < bytes.len() && !bytes[i].is_ascii_whitespace() {
                    i += 1;
                }
                &attrs_raw[v_start..i]
            }
        } else {
            ""
        };

        let attr_lower = raw_attr.to_ascii_lowercase();
        let is_match = if attr_lower == target_lower {
            true
        } else if match_any_ns {
            match attr_lower.rfind(':') {
                Some(c) => &attr_lower[c + 1..] == target_lower,
                None => false,
            }
        } else {
            false
        };
        if is_match {
            return Some(decode_entities(val));
        }
    }
    None
}

/// Normalizes inline whitespace (collapsing runs of `\r`, `\n`, `\t`, `' '` into single spaces while preserving `\u{00A0}`).
pub fn collapse_inline_ws(raw: &str, trim_leading: bool) -> String {
    let decoded = decode_entities(raw);
    let mut out = String::with_capacity(decoded.len());
    let mut prev_ws = trim_leading;
    for ch in decoded.chars() {
        if matches!(ch, ' ' | '\t' | '\r' | '\n') {
            if !prev_ws {
                out.push(' ');
                prev_ws = true;
            }
        } else {
            out.push(ch);
            prev_ws = false;
        }
    }
    out
}
