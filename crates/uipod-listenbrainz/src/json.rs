//! Zero-dependency JSON serializer and decoder for ListenBrainz REST API (`/1/*`).

use crate::queue::QueuedScrobble;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LbApiError {
    pub code: i32,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValidateTokenResponse {
    pub valid: bool,
    pub user_name: String,
    pub message: String,
}

pub struct NowPlayingMeta<'a> {
    pub artist: &'a str,
    pub track: &'a str,
    pub album: &'a str,
    pub album_artist: &'a str,
    pub track_number: u32,
    pub duration_ms: u64,
}

pub fn json_escape_str(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 4);
    for ch in s.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{0008}' => out.push_str("\\b"),
            '\u{000c}' => out.push_str("\\f"),
            c if (c as u32) < 0x20 => {
                let code = c as u32;
                out.push_str(&format!("\\u{code:04x}"));
            }
            c => out.push(c),
        }
    }
    out
}

fn push_track_metadata_json(
    out: &mut String,
    artist: &str,
    track: &str,
    album: &str,
    album_artist: &str,
    track_number: u32,
    duration_ms: u64,
) {
    out.push_str("\"track_metadata\":{");
    out.push_str("\"artist_name\":\"");
    out.push_str(&json_escape_str(artist.trim()));
    out.push_str("\",\"track_name\":\"");
    out.push_str(&json_escape_str(track.trim()));
    out.push('"');

    let alb = album.trim();
    if !alb.is_empty() {
        out.push_str(",\"release_name\":\"");
        out.push_str(&json_escape_str(alb));
        out.push('"');
    }

    out.push_str(",\"additional_info\":{");
    out.push_str("\"media_player\":\"UIPod\",\"submission_client\":\"UIPod\"");
    if duration_ms > 0 {
        out.push_str(&format!(",\"duration_ms\":{duration_ms}"));
    }
    if track_number > 0 {
        out.push_str(&format!(",\"tracknumber\":{track_number}"));
    }
    let aa = album_artist.trim();
    if !aa.is_empty() && aa != artist.trim() {
        out.push_str(",\"release_artist_name\":\"");
        out.push_str(&json_escape_str(aa));
        out.push('"');
    }
    out.push_str("}}");
}

/// Build `POST /1/submit-listens` body for live `playing_now`.
/// CRITICAL: `listened_at` MUST NOT be present anywhere in `playing_now` payloads.
pub fn encode_playing_now_json(np: &NowPlayingMeta<'_>) -> String {
    let mut out = String::with_capacity(256);
    out.push_str("{\"listen_type\":\"playing_now\",\"payload\":[{");
    push_track_metadata_json(
        &mut out,
        np.artist,
        np.track,
        np.album,
        np.album_artist,
        np.track_number,
        np.duration_ms,
    );
    out.push_str("}]}");
    out
}

/// Build `POST /1/submit-listens` body for `single` (1 item) or `import` (>1 items).
/// CRITICAL: `listened_at` (Unix timestamp in seconds) MUST be present on every item.
pub fn encode_scrobbles_json(batch: &[QueuedScrobble]) -> String {
    let listen_type = if batch.len() == 1 {
        "single"
    } else {
        "import"
    };
    let mut out = String::with_capacity(128 + batch.len() * 224);
    out.push_str("{\"listen_type\":\"");
    out.push_str(listen_type);
    out.push_str("\",\"payload\":[");
    for (idx, s) in batch.iter().enumerate() {
        if idx > 0 {
            out.push(',');
        }
        out.push_str(&format!("{{\"listened_at\":{},", s.timestamp));
        push_track_metadata_json(
            &mut out,
            &s.artist,
            &s.track,
            &s.album,
            &s.album_artist,
            s.track_number,
            s.duration_ms,
        );
        out.push('}');
    }
    out.push_str("]}");
    out
}

// ---- Minimal JSON Decoder ---------------------------------------------------

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
                                        i += 7;
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
            let span_start = i;
            while i < b.len() && b[i] != b'"' && b[i] != b'\\' {
                i += 1;
            }
            out.push_str(&String::from_utf8_lossy(&b[span_start..i]));
        }
    }
    None
}

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

fn extract_bool_field(json: &[u8], key: &str) -> Option<bool> {
    let pos = find_key_value_offset(json, key)?;
    let rem = &json[pos..];
    if rem.starts_with(b"true") {
        Some(true)
    } else if rem.starts_with(b"false") {
        Some(false)
    } else {
        None
    }
}

fn extract_i32_field(json: &[u8], key: &str) -> Option<i32> {
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

pub fn extract_error_message(body: &[u8]) -> Option<String> {
    extract_string_field(body, "error")
        .or_else(|| extract_string_field(body, "message"))
        .filter(|s| !s.trim().is_empty())
}

pub fn decode_validate_token(body: &[u8]) -> Result<ValidateTokenResponse, LbApiError> {
    if let Some(valid) = extract_bool_field(body, "valid") {
        let user_name = extract_string_field(body, "user_name").unwrap_or_default();
        let message = extract_string_field(body, "message")
            .or_else(|| extract_string_field(body, "error"))
            .unwrap_or_default();
        return Ok(ValidateTokenResponse {
            valid,
            user_name,
            message,
        });
    }

    let code = extract_i32_field(body, "code").unwrap_or(-1);
    let message = extract_error_message(body)
        .unwrap_or_else(|| "Invalid ListenBrainz response".to_string());
    Err(LbApiError { code, message })
}

pub fn decode_submit_response(body: &[u8]) -> Result<(), LbApiError> {
    if let Some(status) = extract_string_field(body, "status") {
        if status.eq_ignore_ascii_case("ok") {
            return Ok(());
        }
    }
    if let Some(err) = extract_string_field(body, "error") {
        let code = extract_i32_field(body, "code").unwrap_or(400);
        return Err(LbApiError { code, message: err });
    }
    // Compatible servers (e.g. Maloja) may return `{}` or empty `200 OK` body
    if body.iter().all(|b| matches!(b, b' ' | b'\t' | b'\r' | b'\n' | b'{' | b'}')) {
        return Ok(());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_playing_now_excludes_listened_at_and_empty_optionals() {
        let np = NowPlayingMeta {
            artist: "Aphex Twin",
            track: "Xtal",
            album: "",
            album_artist: "",
            track_number: 0,
            duration_ms: 0,
        };
        let json = encode_playing_now_json(&np);
        assert!(json.contains("\"listen_type\":\"playing_now\""));
        assert!(!json.contains("listened_at"));
        assert!(!json.contains("release_name"));
        assert!(!json.contains("duration_ms"));
        assert!(!json.contains("tracknumber"));
        assert!(json.contains("\"artist_name\":\"Aphex Twin\""));
        assert!(json.contains("\"track_name\":\"Xtal\""));
        assert!(json.contains("\"media_player\":\"UIPod\""));
    }

    #[test]
    fn test_single_and_import_include_listened_at() {
        let s1 = QueuedScrobble {
            artist: "Burial".to_string(),
            track: "Archangel".to_string(),
            album: "Untrue".to_string(),
            album_artist: "Burial".to_string(),
            track_number: 2,
            duration_ms: 238_000,
            timestamp: 1_763_890_800,
        };
        let single = encode_scrobbles_json(std::slice::from_ref(&s1));
        assert!(single.contains("\"listen_type\":\"single\""));
        assert!(single.contains("\"listened_at\":1763890800"));
        assert!(single.contains("\"release_name\":\"Untrue\""));
        assert!(single.contains("\"duration_ms\":238000"));
        assert!(single.contains("\"tracknumber\":2"));
        // Same artist == album_artist -> omitted
        assert!(!single.contains("release_artist_name"));

        let s2 = QueuedScrobble {
            artist: "Madvillain".to_string(),
            track: "All Caps".to_string(),
            album: "Madvillainy".to_string(),
            album_artist: "MF DOOM & Madlib".to_string(),
            track_number: 0,
            duration_ms: 130_000,
            timestamp: 1_763_891_100,
        };
        let import = encode_scrobbles_json(&[s1, s2]);
        assert!(import.contains("\"listen_type\":\"import\""));
        assert!(import.contains("\"listened_at\":1763890800"));
        assert!(import.contains("\"listened_at\":1763891100"));
        assert!(import.contains("\"release_artist_name\":\"MF DOOM & Madlib\""));
    }

    #[test]
    fn test_decode_validate_token_responses() {
        let ok = br#"{
            "code": 200,
            "message": "Token valid.",
            "valid": true,
            "user_name": "famiclone_ua"
        }"#;
        let res = decode_validate_token(ok).unwrap();
        assert!(res.valid);
        assert_eq!(res.user_name, "famiclone_ua");

        let invalid = br#"{"code": 200, "message": "Token invalid.", "valid": false}"#;
        let res_inv = decode_validate_token(invalid).unwrap();
        assert!(!res_inv.valid);
        assert!(res_inv.user_name.is_empty());
    }

    #[test]
    fn test_decode_submit_response() {
        assert!(decode_submit_response(br#"{"status":"ok"}"#).is_ok());
        let err = decode_submit_response(br#"{"code":400,"error":"listen_type is invalid"}"#)
            .unwrap_err();
        assert_eq!(err.code, 400);
        assert!(err.message.contains("listen_type"));
    }
}
