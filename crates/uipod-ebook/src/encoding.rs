//! Character-set transcoding (`UTF-8`, `UTF-16LE/BE`, `Windows-1251`, `KOI8-U`, `KOI8-R`,
//! `Windows-1252`), XML/HTML5 entity decoding, and tolerant Base64 decoding.

/// Exact upper-half (`0x80..=0xFF`) lookup table for `Windows-1251` (full Cyrillic + Ukrainian `Ґ/ґ, Є/є, І/і, Ї/ї`).
const CP1251_HIGH: [char; 128] = [
    '\u{0402}', '\u{0403}', '\u{201A}', '\u{0453}', '\u{201E}', '\u{2026}', '\u{2020}', '\u{2021}',
    '\u{20AC}', '\u{2030}', '\u{0409}', '\u{2039}', '\u{040A}', '\u{040C}', '\u{040B}', '\u{040F}',
    '\u{0452}', '\u{2018}', '\u{2019}', '\u{201C}', '\u{201D}', '\u{2022}', '\u{2013}', '\u{2014}',
    '\u{0098}', '\u{2122}', '\u{0459}', '\u{203A}', '\u{045A}', '\u{045C}', '\u{045B}', '\u{045F}',
    '\u{00A0}', '\u{040E}', '\u{045E}', '\u{0408}', '\u{00A4}', '\u{0490}', '\u{00A6}', '\u{00A7}',
    '\u{0401}', '\u{00A9}', '\u{0404}', '\u{00AB}', '\u{00AC}', '\u{00AD}', '\u{00AE}', '\u{0407}',
    '\u{00B0}', '\u{00B1}', '\u{0406}', '\u{0456}', '\u{0491}', '\u{00B5}', '\u{00B6}', '\u{00B7}',
    '\u{0451}', '\u{2116}', '\u{0454}', '\u{00BB}', '\u{0458}', '\u{0405}', '\u{0455}', '\u{0457}',
    '\u{0410}', '\u{0411}', '\u{0412}', '\u{0413}', '\u{0414}', '\u{0415}', '\u{0416}', '\u{0417}',
    '\u{0418}', '\u{0419}', '\u{041A}', '\u{041B}', '\u{041C}', '\u{041D}', '\u{041E}', '\u{041F}',
    '\u{0420}', '\u{0421}', '\u{0422}', '\u{0423}', '\u{0424}', '\u{0425}', '\u{0426}', '\u{0427}',
    '\u{0428}', '\u{0429}', '\u{042A}', '\u{042B}', '\u{042C}', '\u{042D}', '\u{042E}', '\u{042F}',
    '\u{0430}', '\u{0431}', '\u{0432}', '\u{0433}', '\u{0434}', '\u{0435}', '\u{0436}', '\u{0437}',
    '\u{0438}', '\u{0439}', '\u{043A}', '\u{043B}', '\u{043C}', '\u{043D}', '\u{043E}', '\u{043F}',
    '\u{0440}', '\u{0441}', '\u{0442}', '\u{0443}', '\u{0444}', '\u{0445}', '\u{0446}', '\u{0447}',
    '\u{0448}', '\u{0449}', '\u{044A}', '\u{044B}', '\u{044C}', '\u{044D}', '\u{044E}', '\u{044F}',
];

/// Exact upper-half (`0x80..=0xFF`) lookup table for `KOI8-U` / `KOI8-R`.
/// (`ukrainian == true` maps `0xA4/0xB4 -> є/Є`, `0xA6/0xB6 -> і/І`, `0xA7/0xB7 -> ї/Ї`, `0xAD/0xBD -> ґ/Ґ`).
const KOI8R_HIGH: [char; 128] = [
    '\u{2500}', '\u{2502}', '\u{250C}', '\u{2510}', '\u{2514}', '\u{2518}', '\u{251C}', '\u{2524}',
    '\u{252C}', '\u{2534}', '\u{253C}', '\u{2580}', '\u{2584}', '\u{2588}', '\u{258C}', '\u{2590}',
    '\u{2591}', '\u{2592}', '\u{2593}', '\u{2320}', '\u{25A0}', '\u{2219}', '\u{221A}', '\u{2248}',
    '\u{2264}', '\u{2265}', '\u{00A0}', '\u{2321}', '\u{00B0}', '\u{00B2}', '\u{00B7}', '\u{00F7}',
    '\u{2550}', '\u{2551}', '\u{2552}', '\u{0451}', '\u{2553}', '\u{2554}', '\u{2555}', '\u{2556}',
    '\u{2557}', '\u{2558}', '\u{2559}', '\u{255A}', '\u{255B}', '\u{255C}', '\u{255D}', '\u{255E}',
    '\u{255F}', '\u{2560}', '\u{2561}', '\u{0401}', '\u{2562}', '\u{2563}', '\u{2564}', '\u{2565}',
    '\u{2566}', '\u{2567}', '\u{2568}', '\u{2569}', '\u{256A}', '\u{256B}', '\u{256C}', '\u{00A9}',
    '\u{044E}', '\u{0430}', '\u{0431}', '\u{0446}', '\u{0434}', '\u{0435}', '\u{0444}', '\u{0433}',
    '\u{0445}', '\u{0438}', '\u{0439}', '\u{043A}', '\u{043B}', '\u{043C}', '\u{043D}', '\u{043E}',
    '\u{043F}', '\u{044F}', '\u{0440}', '\u{0441}', '\u{0442}', '\u{0443}', '\u{0436}', '\u{0432}',
    '\u{044C}', '\u{044B}', '\u{0437}', '\u{0448}', '\u{044D}', '\u{0449}', '\u{0447}', '\u{044A}',
    '\u{042E}', '\u{0410}', '\u{0411}', '\u{0426}', '\u{0414}', '\u{0415}', '\u{0424}', '\u{0413}',
    '\u{0425}', '\u{0418}', '\u{0419}', '\u{041A}', '\u{041B}', '\u{041C}', '\u{041D}', '\u{041E}',
    '\u{041F}', '\u{042F}', '\u{0420}', '\u{0421}', '\u{0422}', '\u{0423}', '\u{0416}', '\u{0412}',
    '\u{042C}', '\u{042B}', '\u{0417}', '\u{0428}', '\u{042D}', '\u{0429}', '\u{0427}', '\u{042A}',
];

/// Exact `0x80..=0x9F` lookup table for `Windows-1252` (`0xA0..=0xFF` map 1:1 to Unicode Latin-1).
const CP1252_C1: [char; 32] = [
    '\u{20AC}', '\u{FFFD}', '\u{201A}', '\u{0192}', '\u{201E}', '\u{2026}', '\u{2020}', '\u{2021}',
    '\u{02C6}', '\u{2030}', '\u{0160}', '\u{2039}', '\u{0152}', '\u{FFFD}', '\u{017D}', '\u{FFFD}',
    '\u{FFFD}', '\u{2018}', '\u{2019}', '\u{201C}', '\u{201D}', '\u{2022}', '\u{2013}', '\u{2014}',
    '\u{02DC}', '\u{2122}', '\u{0161}', '\u{203A}', '\u{0153}', '\u{FFFD}', '\u{017E}', '\u{0178}',
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TextEncoding {
    Utf8,
    Utf16Le,
    Utf16Be,
    Windows1251,
    Koi8R,
    Koi8U,
    Windows1252,
}

impl TextEncoding {
    pub fn from_label(label: &str) -> Option<Self> {
        let norm: String = label
            .trim()
            .chars()
            .filter(|c| c.is_ascii_alphanumeric())
            .map(|c| c.to_ascii_lowercase())
            .collect();
        match norm.as_str() {
            "utf8" => Some(Self::Utf8),
            "utf16" | "utf16le" | "ucs2le" => Some(Self::Utf16Le),
            "utf16be" | "ucs2be" => Some(Self::Utf16Be),
            "windows1251" | "cp1251" | "1251" | "xcp1251" | "cyrillic" => Some(Self::Windows1251),
            "koi8u" => Some(Self::Koi8U),
            "koi8r" | "koi8" => Some(Self::Koi8R),
            "windows1252" | "cp1252" | "1252" | "iso88591" | "latin1" | "usascii" | "ascii" => {
                Some(Self::Windows1252)
            }
            _ => None,
        }
    }

    pub const fn to_u8(self) -> u8 {
        match self {
            Self::Utf8 => 0,
            Self::Utf16Le => 1,
            Self::Utf16Be => 2,
            Self::Windows1251 => 3,
            Self::Koi8R => 4,
            Self::Koi8U => 5,
            Self::Windows1252 => 6,
        }
    }

    pub const fn from_u8(v: u8) -> Self {
        match v {
            1 => Self::Utf16Le,
            2 => Self::Utf16Be,
            3 => Self::Windows1251,
            4 => Self::Koi8R,
            5 => Self::Koi8U,
            6 => Self::Windows1252,
            _ => Self::Utf8,
        }
    }
}

pub fn detect_xml_encoding(bytes: &[u8]) -> TextEncoding {
    if bytes.starts_with(&[0xEF, 0xBB, 0xBF]) {
        return TextEncoding::Utf8;
    }
    if bytes.starts_with(&[0xFF, 0xFE]) {
        return TextEncoding::Utf16Le;
    }
    if bytes.starts_with(&[0xFE, 0xFF]) {
        return TextEncoding::Utf16Be;
    }
    if let Some(enc) = sniff_xml_or_html_charset(bytes) {
        return enc;
    }
    if std::str::from_utf8(&bytes[..bytes.len().min(8192)]).is_ok() {
        TextEncoding::Utf8
    } else {
        TextEncoding::Windows1251
    }
}

/// Sniffs `encoding="..."` or `charset=...` inside the first 1024 ASCII bytes of an XML/HTML file.
fn sniff_xml_or_html_charset(bytes: &[u8]) -> Option<TextEncoding> {
    let head_len = bytes.len().min(1024);
    let mut ascii = String::with_capacity(head_len);
    for &b in &bytes[..head_len] {
        ascii.push(if b.is_ascii() {
            (b as char).to_ascii_lowercase()
        } else {
            ' '
        });
    }

    for needle in ["encoding=", "charset="] {
        let mut rest = ascii.as_str();
        while let Some(pos) = rest.find(needle) {
            let after = rest[pos + needle.len()..].trim_start();
            let val = if let Some(q) = after.strip_prefix('"') {
                q.split('"').next().unwrap_or("")
            } else if let Some(q) = after.strip_prefix('\'') {
                q.split('\'').next().unwrap_or("")
            } else {
                after
                    .split(|c: char| c.is_ascii_whitespace() || c == ';' || c == '>' || c == '?')
                    .next()
                    .unwrap_or("")
            };
            if let Some(enc) = TextEncoding::from_label(val) {
                return Some(enc);
            }
            rest = &rest[pos + needle.len()..];
        }
    }
    None
}

/// Decodes an arbitrary byte buffer into clean UTF-8, honoring BOMs, XML/HTML `encoding="..."`
/// declarations, explicit hint, and Cyrillic `Windows-1251` fallback when invalid UTF-8 is encountered.
pub fn decode_bytes_auto(bytes: &[u8], explicit: Option<TextEncoding>) -> String {
    if bytes.is_empty() {
        return String::new();
    }
    if bytes.starts_with(&[0xEF, 0xBB, 0xBF]) {
        return String::from_utf8_lossy(&bytes[3..]).into_owned();
    }
    if bytes.starts_with(&[0xFF, 0xFE]) {
        return decode_utf16(&bytes[2..], true);
    }
    if bytes.starts_with(&[0xFE, 0xFF]) {
        return decode_utf16(&bytes[2..], false);
    }

    let detected = explicit.or_else(|| sniff_xml_or_html_charset(bytes));
    match detected {
        Some(TextEncoding::Utf8) => {
            if let Ok(s) = std::str::from_utf8(bytes) {
                return s.to_string();
            }
            // Many legacy `.fb2` files declare `encoding="utf-8"` after re-saving as CP1251 or vice-versa;
            // if high bytes look strongly like Cyrillic CP1251, decode as CP1251, else lossy UTF-8.
            if looks_like_cp1251(bytes) {
                return decode_single_byte(bytes, TextEncoding::Windows1251);
            }
            String::from_utf8_lossy(bytes).into_owned()
        }
        Some(TextEncoding::Utf16Le) => decode_utf16(bytes, true),
        Some(TextEncoding::Utf16Be) => decode_utf16(bytes, false),
        Some(enc) => {
            // Guard against files that say `encoding="windows-1251"` in prolog after already being UTF-8!
            if matches!(
                enc,
                TextEncoding::Windows1251 | TextEncoding::Koi8R | TextEncoding::Koi8U
            ) {
                if let Ok(valid_utf8) = std::str::from_utf8(bytes) {
                    if valid_utf8.chars().any(|c| ('\u{0400}'..='\u{04FF}').contains(&c)) {
                        return valid_utf8.to_string();
                    }
                }
            }
            decode_single_byte(bytes, enc)
        }
        None => {
            if let Ok(s) = std::str::from_utf8(bytes) {
                return s.to_string();
            }
            if looks_like_cp1251(bytes) {
                decode_single_byte(bytes, TextEncoding::Windows1251)
            } else {
                decode_single_byte(bytes, TextEncoding::Windows1252)
            }
        }
    }
}

fn looks_like_cp1251(bytes: &[u8]) -> bool {
    let mut high = 0usize;
    let mut cyr_range = 0usize;
    for &b in bytes.iter().take(16 * 1024) {
        if b >= 0x80 {
            high += 1;
            if b >= 0xC0
                || matches!(
                    b,
                    0xA5 | 0xA8 | 0xAA | 0xAF | 0xB2 | 0xB3 | 0xB4 | 0xB8 | 0xBA | 0xBF
                )
            {
                cyr_range += 1;
            }
        }
    }
    high >= 4 && cyr_range * 10 >= high * 7
}

fn decode_utf16(bytes: &[u8], little_endian: bool) -> String {
    let units = bytes.chunks_exact(2).map(|pair| {
        if little_endian {
            u16::from_le_bytes([pair[0], pair[1]])
        } else {
            u16::from_be_bytes([pair[0], pair[1]])
        }
    });
    char::decode_utf16(units)
        .map(|r| r.unwrap_or('\u{FFFD}'))
        .collect()
}

pub fn decode_single_byte(bytes: &[u8], enc: TextEncoding) -> String {
    let mut out = String::with_capacity(bytes.len() + bytes.len() / 2);
    for &b in bytes {
        if b < 0x80 {
            out.push(b as char);
        } else {
            let idx = (b - 0x80) as usize;
            let ch = match enc {
                TextEncoding::Windows1251 => CP1251_HIGH[idx],
                TextEncoding::Koi8R => KOI8R_HIGH[idx],
                TextEncoding::Koi8U => match b {
                    0xA4 => 'є',
                    0xA6 => 'і',
                    0xA7 => 'ї',
                    0xAD => 'ґ',
                    0xB4 => 'Є',
                    0xB6 => 'І',
                    0xB7 => 'Ї',
                    0xBD => 'Ґ',
                    _ => KOI8R_HIGH[idx],
                },
                TextEncoding::Windows1252 => {
                    if b < 0xA0 {
                        CP1252_C1[(b - 0x80) as usize]
                    } else {
                        b as char
                    }
                }
                _ => '\u{FFFD}',
            };
            out.push(ch);
        }
    }
    out
}

/// Decodes XML and common HTML5 named + numeric character entities (`&#160;`, `&#x2014;`, `&mdash;`, etc.).
pub fn decode_entities(input: &str) -> String {
    if !input.contains('&') {
        return input.to_string();
    }
    let mut out = String::with_capacity(input.len());
    let mut rest = input;
    while let Some(amp) = rest.find('&') {
        out.push_str(&rest[..amp]);
        let after = &rest[amp + 1..];
        let after_bytes = after.as_bytes();
        let max_scan = after_bytes.len().min(32);
        let mut semi_opt = None;
        for (idx, &b) in after_bytes[..max_scan].iter().enumerate() {
            if b == b';' {
                semi_opt = Some(idx);
                break;
            }
            if b == b'&' || b.is_ascii_whitespace() {
                break;
            }
        }
        if let Some(semi) = semi_opt {
            let token = &after[..semi];
            if let Some(decoded) = resolve_entity(token) {
                out.push(decoded);
                rest = &after[semi + 1..];
                continue;
            }
        }
        out.push('&');
        rest = after;
    }
    out.push_str(rest);
    out
}

fn resolve_entity(token: &str) -> Option<char> {
    if let Some(hex) = token.strip_prefix("#x").or_else(|| token.strip_prefix("#X")) {
        let code = u32::from_str_radix(hex, 16).ok()?;
        return map_numeric_entity(code);
    }
    if let Some(dec) = token.strip_prefix('#') {
        let code = dec.parse::<u32>().ok()?;
        return map_numeric_entity(code);
    }
    match token {
        "amp" | "AMP" => Some('&'),
        "lt" | "LT" => Some('<'),
        "gt" | "GT" => Some('>'),
        "quot" | "QUOT" => Some('"'),
        "apos" | "APOS" => Some('\''),
        "nbsp" | "NBSP" => Some('\u{00A0}'),
        "ensp" | "emsp" | "thinsp" => Some(' '),
        "shy" => Some('\u{00AD}'),
        "ndash" => Some('–'),
        "mdash" => Some('—'),
        "horbar" => Some('―'),
        "lsquo" | "sbquo" => Some('‘'),
        "rsquo" => Some('’'),
        "ldquo" | "bdquo" => Some('“'),
        "rdquo" => Some('”'),
        "laquo" => Some('«'),
        "raquo" => Some('»'),
        "lsaquo" => Some('‹'),
        "rsaquo" => Some('›'),
        "hellip" => Some('…'),
        "bull" => Some('•'),
        "middot" => Some('·'),
        "copy" | "COPY" => Some('©'),
        "reg" | "REG" => Some('®'),
        "trade" => Some('™'),
        "sect" => Some('§'),
        "para" => Some('¶'),
        "deg" => Some('°'),
        "plusmn" => Some('±'),
        "times" => Some('×'),
        "divide" => Some('÷'),
        "euro" => Some('€'),
        "pound" => Some('£'),
        "yen" => Some('¥'),
        "cent" => Some('¢'),
        "numero" => Some('№'),
        "dagger" => Some('†'),
        "Dagger" => Some('‡'),
        "prime" => Some('′'),
        "Prime" => Some('″'),
        _ => None,
    }
}

fn map_numeric_entity(code: u32) -> Option<char> {
    if (0x80..=0x9F).contains(&code) {
        return Some(CP1252_C1[(code - 0x80) as usize]);
    }
    if code == 0 {
        return Some('\u{FFFD}');
    }
    char::from_u32(code)
}

/// Decodes Base64 text (skipping ASCII whitespace, newlines, and optional `data:...;base64,` prefix).
pub fn decode_base64(raw: &str) -> Option<Vec<u8>> {
    let payload = match raw.find(";base64,") {
        Some(idx) => &raw[idx + 8..],
        None => raw,
    };

    #[inline]
    fn b64_val(b: u8) -> u8 {
        match b {
            b'A'..=b'Z' => b - b'A',
            b'a'..=b'z' => b - b'a' + 26,
            b'0'..=b'9' => b - b'0' + 52,
            b'+' | b'-' => 62,
            b'/' | b'_' => 63,
            _ => 255,
        }
    }

    let mut out = Vec::with_capacity((payload.len() * 3) / 4);
    let mut quad = [0u8; 4];
    let mut qlen = 0usize;
    for b in payload.bytes() {
        if b.is_ascii_whitespace() {
            continue;
        }
        if b == b'=' {
            break;
        }
        let v = b64_val(b);
        if v == 255 {
            continue;
        }
        quad[qlen] = v;
        qlen += 1;
        if qlen == 4 {
            out.push((quad[0] << 2) | (quad[1] >> 4));
            out.push((quad[1] << 4) | (quad[2] >> 2));
            out.push((quad[2] << 6) | quad[3]);
            qlen = 0;
        }
    }
    if qlen == 2 {
        out.push((quad[0] << 2) | (quad[1] >> 4));
    } else if qlen == 3 {
        out.push((quad[0] << 2) | (quad[1] >> 4));
        out.push((quad[1] << 4) | (quad[2] >> 2));
    }
    if out.is_empty() && !payload.trim().is_empty() {
        None
    } else {
        Some(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transcodes_windows_1251_ukrainian_xml() {
        // `<?xml version="1.0" encoding="windows-1251"?>` + `Ґрунт Єдність Історія Їжак`
        let mut raw = b"<?xml version=\"1.0\" encoding=\"windows-1251\"?>".to_vec();
        // Ґ=0xA5, Є=0xAA, І=0xB2, і=0xB3, Ї=0xAF, ї=0xBF, ґ=0xB4, є=0xBA
        raw.extend_from_slice(&[0xA5, 0xB4, b' ', 0xAA, 0xBA, b' ', 0xB2, 0xB3, b' ', 0xAF, 0xBF]);
        let decoded = decode_bytes_auto(&raw, None);
        assert!(decoded.ends_with("Ґґ Єє Іі Її"));
    }

    #[test]
    fn decodes_entities_and_base64() {
        assert_eq!(
            decode_entities("Chapter&nbsp;1 &mdash; &laquo;Test&#x2019;s&raquo;"),
            "Chapter\u{00A0}1 — «Test’s»"
        );
        // Regression test: multi-byte UTF-8 (Ukrainian Cyrillic + CJK) straddling byte 16 after `&`
        assert_eq!(
            decode_entities("&nbsp;Привіт світе! Simon & Schuster — Історія & 「日本語・한국어・简体中文」"),
            "\u{00A0}Привіт світе! Simon & Schuster — Історія & 「日本語・한국어・简体中文」"
        );
        assert_eq!(decode_base64("SGVs\nbG8h").unwrap(), b"Hello!");
    }
}
