//! Zero-dependency JSON decoder for Last.fm API 2.0 responses.

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LastFmApiError {
    pub code: i32,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LastFmSession {
    pub username: String,
    pub session_key: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScrobbleBatchResult {
    pub accepted: u32,
    pub ignored: u32,
}

fn skip_ws(b: &[u8], mut i: usize) -> usize {
    while i < b.len() && matches!(b[i], b' ' | b'\t' | b'\r' | b'\n') {
        i += 1;
    }
    i
}

fn parse_json_string(b: &[u8], start_quote: usize) -> Option<(String, usize)> {
    if start_quote >= b.len() || b[start_quote] != b'"' {
        return None;
    }
    let mut i = start_quote + 1;
    let mut out = String::new();
    while i < b.len() {
        let ch = b[i];
        if ch == b'"' {
            return Some((out, i + 1));
        }
        if ch == b'\\' {
            i += 1;
            if i >= b.len() {
                return None;
            }
            match b[i] {
                b'"' => out.push('"'),
                b'\\' => out.push('\\'),
                b'/' => out.push('/'),
                b'b' => out.push('\u{0008}'),
                b'f' => out.push('\u{000c}'),
                b'n' => out.push('\n'),
                b'r' => out.push('\r'),
                b't' => out.push('\t'),
                b'u' => {
                    if i + 4 >= b.len() {
                        return None;
                    }
                    let hex = std::str::from_utf8(&b[i + 1..i + 5]).ok()?;
                    let cp = u16::from_str_radix(hex, 16).ok()?;
                    i += 4;
                    if (0xD800..=0xDBFF).contains(&cp)
                        && i + 6 < b.len()
                        && b[i + 1] == b'\\'
                        && b[i + 2] == b'u'
                    {
                        if let Ok(hex2) = std::str::from_utf8(&b[i + 3..i + 7]) {
                            if let Ok(cp2) = u16::from_str_radix(hex2, 16) {
                                if (0xDC00..=0xDFFF).contains(&cp2) {
                                    let combined = 0x10000
                                        + (((cp as u32 - 0xD800) << 10) | (cp2 as u32 - 0xDC00));
                                    if let Some(c) = char::from_u32(combined) {
                                        out.push(c);
                                        i += 6;
                                        i += 1;
                                        continue;
                                    }
                                }
                            }
                        }
                    }
                    if let Some(c) = char::from_u32(cp as u32) {
                        out.push(c);
                    }
                }
                other => out.push(other as char),
            }
            i += 1;
        } else {
            // Decode contiguous non-escaped UTF-8 span directly
            let span_start = i;
            while i < b.len() && b[i] != b'"' && b[i] != b'\\' {
                i += 1;
            }
            out.push_str(&String::from_utf8_lossy(&b[span_start..i]));
        }
    }
    None
}

/// Find `"key"\s*:\s*` anywhere in `json` (outside string values at top/nested scan) and return byte index of value.
fn find_key_value_offset(json: &[u8], target_key: &str) -> Option<usize> {
    let mut i = 0;
    while i < json.len() {
        i = skip_ws(json, i);
        if i >= json.len() {
            break;
        }
        if json[i] == b'"' {
            let (s, next_i) = parse_json_string(json, i)?;
            let after_str = skip_ws(json, next_i);
            if after_str < json.len() && json[after_str] == b':' {
                let val_pos = skip_ws(json, after_str + 1);
                if s == target_key {
                    return Some(val_pos);
                }
                i = val_pos;
                continue;
            }
            i = next_i;
        } else {
            i += 1;
        }
    }
    None
}

fn extract_string_field(json: &[u8], key: &str) -> Option<String> {
    let pos = find_key_value_offset(json, key)?;
    if pos < json.len() && json[pos] == b'"' {
        parse_json_string(json, pos).map(|(s, _)| s)
    } else {
        None
    }
}

fn extract_i32_or_str_field(json: &[u8], key: &str) -> Option<i32> {
    let pos = find_key_value_offset(json, key)?;
    if pos >= json.len() {
        return None;
    }
    if json[pos] == b'"' {
        let (s, _) = parse_json_string(json, pos)?;
        s.trim().parse::<i32>().ok()
    } else {
        let mut end = pos;
        if end < json.len() && json[end] == b'-' {
            end += 1;
        }
        while end < json.len() && json[end].is_ascii_digit() {
            end += 1;
        }
        if end == pos {
            return None;
        }
        std::str::from_utf8(&json[pos..end]).ok()?.parse::<i32>().ok()
    }
}

/// Extract the JSON slice `{ ... }` for a named object field like `"session": { ... }`.
fn extract_object_slice<'a>(json: &'a [u8], key: &str) -> Option<&'a [u8]> {
    let pos = find_key_value_offset(json, key)?;
    if pos >= json.len() || json[pos] != b'{' {
        return None;
    }
    let mut depth = 0usize;
    let mut i = pos;
    while i < json.len() {
        match json[i] {
            b'"' => {
                let (_, next_i) = parse_json_string(json, i)?;
                i = next_i;
                continue;
            }
            b'{' => depth += 1,
            b'}' => {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    return Some(&json[pos..=i]);
                }
            }
            _ => {}
        }
        i += 1;
    }
    None
}

pub fn check_api_error(body: &[u8]) -> Result<(), LastFmApiError> {
    if let Some(code) = extract_i32_or_str_field(body, "error") {
        if code != 0 {
            let message = extract_string_field(body, "message")
                .unwrap_or_else(|| format!("Last.fm API error {code}"));
            return Err(LastFmApiError { code, message });
        }
    }
    Ok(())
}

pub fn decode_auth_token(body: &[u8]) -> Result<String, LastFmApiError> {
    check_api_error(body)?;
    extract_string_field(body, "token")
        .filter(|t| !t.trim().is_empty())
        .ok_or_else(|| LastFmApiError {
            code: -1,
            message: "Missing token in Last.fm response".to_string(),
        })
}

pub fn decode_session(body: &[u8]) -> Result<LastFmSession, LastFmApiError> {
    check_api_error(body)?;
    let sess = extract_object_slice(body, "session").unwrap_or(body);
    let username = extract_string_field(sess, "name").unwrap_or_default();
    let session_key = extract_string_field(sess, "key").unwrap_or_default();
    if session_key.trim().is_empty() {
        return Err(LastFmApiError {
            code: -1,
            message: "Missing session key in Last.fm response".to_string(),
        });
    }
    Ok(LastFmSession {
        username,
        session_key,
    })
}

pub fn decode_scrobble_response(body: &[u8]) -> Result<ScrobbleBatchResult, LastFmApiError> {
    check_api_error(body)?;
    let scope = extract_object_slice(body, "@attr").unwrap_or(body);
    let accepted = extract_i32_or_str_field(scope, "accepted").unwrap_or(0).max(0) as u32;
    let ignored = extract_i32_or_str_field(scope, "ignored").unwrap_or(0).max(0) as u32;
    Ok(ScrobbleBatchResult { accepted, ignored })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_decode_token_and_error() {
        let ok = br#"{"token":"abcdef0123456789"}"#;
        assert_eq!(decode_auth_token(ok).unwrap(), "abcdef0123456789");

        let err = br#"{"message":"Unauthorized Token - This token has not been authorized","error":14}"#;
        let e = decode_session(err).unwrap_err();
        assert_eq!(e.code, 14);
        assert!(e.message.contains("Unauthorized Token"));
    }

    #[test]
    fn test_decode_session_and_scrobbles() {
        let sess_json = br#"{"session":{"name":"trimpod_user","key":"d580d57f32848f5dcf574d1ce18d78b2","subscriber":0}}"#;
        let sess = decode_session(sess_json).unwrap();
        assert_eq!(sess.username, "trimpod_user");
        assert_eq!(sess.session_key, "d580d57f32848f5dcf574d1ce18d78b2");

        let scrob_json = br#"{"scrobbles":{"@attr":{"accepted":2,"ignored":"1"},"scrobble":[]}}"#;
        let res = decode_scrobble_response(scrob_json).unwrap();
        assert_eq!(res.accepted, 2);
        assert_eq!(res.ignored, 1);
    }
}
