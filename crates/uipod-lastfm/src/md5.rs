//! Pure-Rust RFC 1321 MD5, Last.fm API 2.0 `api_sig` signing, and URL form encoding.

use std::collections::BTreeMap;

const S: [u32; 64] = [
    7, 12, 17, 22, 7, 12, 17, 22, 7, 12, 17, 22, 7, 12, 17, 22, 5, 9, 14, 20, 5, 9, 14, 20, 5, 9,
    14, 20, 5, 9, 14, 20, 4, 11, 16, 23, 4, 11, 16, 23, 4, 11, 16, 23, 4, 11, 16, 23, 6, 10, 15,
    21, 6, 10, 15, 21, 6, 10, 15, 21, 6, 10, 15, 21,
];

const K: [u32; 64] = [
    0xd76aa478, 0xe8c7b756, 0x242070db, 0xc1bdceee, 0xf57c0faf, 0x4787c62a, 0xa8304613, 0xfd469501,
    0x698098d8, 0x8b44f7af, 0xffff5bb1, 0x895cd7be, 0x6b901122, 0xfd987193, 0xa679438e, 0x49b40821,
    0xf61e2562, 0xc040b340, 0x265e5a51, 0xe9b6c7aa, 0xd62f105d, 0x02441453, 0xd8a1e681, 0xe7d3fbc8,
    0x21e1cde6, 0xc33707d6, 0xf4d50d87, 0x455a14ed, 0xa9e3e905, 0xfcefa3f8, 0x676f02d9, 0x8d2a4c8a,
    0xfffa3942, 0x8771f681, 0x6d9d6122, 0xfde5380c, 0xa4beea44, 0x4bdecfa9, 0xf6bb4b60, 0xbebfbc70,
    0x289b7ec6, 0xeaa127fa, 0xd4ef3085, 0x04881d05, 0xd9d4d039, 0xe6db99e5, 0x1fa27cf8, 0xc4ac5665,
    0xf4292244, 0x432aff97, 0xab9423a7, 0xfc93a039, 0x655b59c3, 0x8f0ccc92, 0xffeff47d, 0x85845dd1,
    0x6fa87e4f, 0xfe2ce6e0, 0xa3014314, 0x4e0811a1, 0xf7537e82, 0xbd3af235, 0x2ad7d2bb, 0xeb86d391,
];

/// Compute RFC 1321 MD5 digest of `data` as a 32-character lowercase hex string.
pub fn md5_hex(data: &[u8]) -> String {
    let mut a0: u32 = 0x67452301;
    let mut b0: u32 = 0xefcdab89;
    let mut c0: u32 = 0x98badcfe;
    let mut d0: u32 = 0x10325476;

    let bit_len = (data.len() as u64).wrapping_mul(8);
    let mut padded = Vec::with_capacity(((data.len() + 9 + 63) / 64) * 64);
    padded.extend_from_slice(data);
    padded.push(0x80);
    while padded.len() % 64 != 56 {
        padded.push(0);
    }
    padded.extend_from_slice(&bit_len.to_le_bytes());

    for chunk in padded.chunks_exact(64) {
        let mut m = [0u32; 16];
        for (i, word) in m.iter_mut().enumerate() {
            let off = i * 4;
            *word = u32::from_le_bytes([chunk[off], chunk[off + 1], chunk[off + 2], chunk[off + 3]]);
        }

        let mut a = a0;
        let mut b = b0;
        let mut c = c0;
        let mut d = d0;

        for i in 0..64 {
            let (f, g) = match i {
                0..=15 => ((b & c) | ((!b) & d), i),
                16..=31 => ((d & b) | ((!d) & c), (5 * i + 1) % 16),
                32..=47 => (b ^ c ^ d, (3 * i + 5) % 16),
                _ => (c ^ (b | (!d)), (7 * i) % 16),
            };
            let temp = d;
            d = c;
            c = b;
            let sum = a.wrapping_add(f).wrapping_add(K[i]).wrapping_add(m[g]);
            b = b.wrapping_add(sum.rotate_left(S[i]));
            a = temp;
        }

        a0 = a0.wrapping_add(a);
        b0 = b0.wrapping_add(b);
        c0 = c0.wrapping_add(c);
        d0 = d0.wrapping_add(d);
    }

    let mut digest = [0u8; 16];
    digest[0..4].copy_from_slice(&a0.to_le_bytes());
    digest[4..8].copy_from_slice(&b0.to_le_bytes());
    digest[8..12].copy_from_slice(&c0.to_le_bytes());
    digest[12..16].copy_from_slice(&d0.to_le_bytes());

    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(32);
    for &b in &digest {
        out.push(HEX[(b >> 4) as usize] as char);
        out.push(HEX[(b & 0x0f) as usize] as char);
    }
    out
}

/// RFC 3986 percent-encoder suitable for `application/x-www-form-urlencoded` bodies and query params.
pub fn form_urlencode(input: &str) -> String {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    let mut out = String::with_capacity(input.len() + 8);
    for &b in input.as_bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') {
            out.push(b as char);
        } else {
            out.push('%');
            out.push(HEX[(b >> 4) as usize] as char);
            out.push(HEX[(b & 0x0f) as usize] as char);
        }
    }
    out
}

/// Compute Last.fm API 2.0 `api_sig`:
/// Sort all parameters alphabetically by UTF-8 key (excluding `format` and `callback`),
/// concatenate raw UTF-8 `<key><value>` pairs, append `api_secret`, and hash with MD5.
pub fn sign_lastfm_params(params: &BTreeMap<String, String>, api_secret: &str) -> String {
    let mut raw = Vec::with_capacity(256);
    for (k, v) in params {
        if k == "format" || k == "callback" {
            continue;
        }
        raw.extend_from_slice(k.as_bytes());
        raw.extend_from_slice(v.as_bytes());
    }
    raw.extend_from_slice(api_secret.as_bytes());
    md5_hex(&raw)
}

/// Build a complete signed `application/x-www-form-urlencoded` body with `&api_sig=...&format=json`.
pub fn build_signed_post_body(params: &BTreeMap<String, String>, api_secret: &str) -> Vec<u8> {
    let sig = sign_lastfm_params(params, api_secret);
    let mut body = String::with_capacity(256);
    for (k, v) in params {
        if !body.is_empty() {
            body.push('&');
        }
        body.push_str(&form_urlencode(k));
        body.push('=');
        body.push_str(&form_urlencode(v));
    }
    if !body.is_empty() {
        body.push('&');
    }
    body.push_str("api_sig=");
    body.push_str(&sig);
    body.push_str("&format=json");
    body.into_bytes()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_md5_vectors() {
        assert_eq!(md5_hex(b""), "d41d8cd98f00b204e9800998ecf8427e");
        assert_eq!(md5_hex(b"abc"), "900150983cd24fb0d6963f7d28e17f72");
        assert_eq!(
            md5_hex(b"The quick brown fox jumps over the lazy dog"),
            "9e107d9d372bb6826bd81d3542a419d6"
        );
    }

    #[test]
    fn test_lastfm_signature_excludes_format_and_hashes_raw_utf8() {
        let mut p = BTreeMap::new();
        p.insert("method".to_string(), "track.scrobble".to_string());
        p.insert("api_key".to_string(), "mykey".to_string());
        p.insert("artist[0]".to_string(), "Океан Ельзи".to_string());
        p.insert("track[0]".to_string(), "Без бою".to_string());
        p.insert("timestamp[0]".to_string(), "1700000000".to_string());
        p.insert("format".to_string(), "json".to_string());

        let expected_concat =
            "api_keymykeyartist[0]Океан Ельзиmethodtrack.scrobbletimestamp[0]1700000000track[0]Без боюmysecret";
        assert_eq!(
            sign_lastfm_params(&p, "mysecret"),
            md5_hex(expected_concat.as_bytes())
        );
    }
}
