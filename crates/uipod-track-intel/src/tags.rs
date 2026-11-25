//! Lightweight zero-decode audio tag & `TBPM` header inspector (`< 0.3 ms`).
//!
//! Extracts embedded `TBPM` / `BPM` / `TEMPO` / `tmpo` tags plus basic `Title`, `Artist`,
//! `Album`, and `Genre` strings directly from ID3v2 (`2.2`/`2.3`/`2.4`), FLAC `VORBIS_COMMENT`,
//! Ogg Vorbis/Opus comment headers, and MP4/M4A `ilst` (`tmpo`) atoms without decoding audio.

use std::io::{Read, Seek, SeekFrom};

#[derive(Clone, Debug, Default)]
pub struct ExtractedTags {
    pub title: String,
    pub artist: String,
    pub album: String,
    pub genre: String,
    pub tbpm: Option<f32>,
}

pub fn parse_bpm_str(raw: &str) -> Option<f32> {
    let trimmed = raw
        .trim_matches(|c: char| c == '\0' || c.is_whitespace())
        .trim_end_matches(|c: char| c.eq_ignore_ascii_case(&'b')
            || c.eq_ignore_ascii_case(&'p')
            || c.eq_ignore_ascii_case(&'m')
            || c.is_whitespace());
    let token = trimmed
        .split(|c: char| c.is_whitespace() || c == '/' || c == ';')
        .next()?
        .replace(',', ".");
    let val: f32 = token.parse().ok()?;
    if val.is_finite() && (25.0..=400.0).contains(&val) {
        Some(val)
    } else {
        None
    }
}

/// Read embedded tags and `TBPM` from any seekable reader without touching audio frames.
pub fn read_tags_from_reader<R: Read + Seek>(mut r: R) -> ExtractedTags {
    let mut magic = [0u8; 12];
    if r.seek(SeekFrom::Start(0)).is_err() || r.read(&mut magic).unwrap_or(0) < 10 {
        return ExtractedTags::default();
    }

    if &magic[0..3] == b"ID3" {
        return parse_id3v2(&mut r, &magic);
    }
    if &magic[0..4] == b"fLaC" {
        return parse_flac_blocks(&mut r);
    }
    if &magic[0..4] == b"OggS" {
        return parse_ogg_comments(&mut r);
    }
    if &magic[4..8] == b"ftyp" {
        return parse_mp4_atoms(&mut r);
    }

    ExtractedTags::default()
}

fn synchsafe_u32(b: &[u8]) -> usize {
    (((b[0] & 0x7F) as usize) << 21)
        | (((b[1] & 0x7F) as usize) << 14)
        | (((b[2] & 0x7F) as usize) << 7)
        | ((b[3] & 0x7F) as usize)
}

fn decode_id3_text(payload: &[u8]) -> String {
    if payload.is_empty() {
        return String::new();
    }
    let enc = payload[0];
    let body = &payload[1..];
    let raw = match enc {
        0 => body
            .iter()
            .take_while(|&&c| c != 0)
            .map(|&c| c as char)
            .collect::<String>(),
        1 => {
            // UTF-16 with BOM
            let (le, start) = if body.len() >= 2 && body[0] == 0xFF && body[1] == 0xFE {
                (true, 2)
            } else if body.len() >= 2 && body[0] == 0xFE && body[1] == 0xFF {
                (false, 2)
            } else {
                (true, 0)
            };
            let mut u16s = Vec::with_capacity(body.len() / 2);
            let mut i = start;
            while i + 1 < body.len() {
                let w = if le {
                    u16::from_le_bytes([body[i], body[i + 1]])
                } else {
                    u16::from_be_bytes([body[i], body[i + 1]])
                };
                if w == 0 {
                    break;
                }
                u16s.push(w);
                i += 2;
            }
            String::from_utf16_lossy(&u16s)
        }
        2 => {
            // UTF-16BE without BOM
            let mut u16s = Vec::with_capacity(body.len() / 2);
            let mut i = 0;
            while i + 1 < body.len() {
                let w = u16::from_be_bytes([body[i], body[i + 1]]);
                if w == 0 {
                    break;
                }
                u16s.push(w);
                i += 2;
            }
            String::from_utf16_lossy(&u16s)
        }
        _ => {
            let end = body.iter().position(|&b| b == 0).unwrap_or(body.len());
            String::from_utf8_lossy(&body[..end]).into_owned()
        }
    };
    raw.trim().to_string()
}

fn parse_id3v2<R: Read + Seek>(r: &mut R, hdr12: &[u8; 12]) -> ExtractedTags {
    let mut out = ExtractedTags::default();
    let ver = hdr12[3];
    let flags = hdr12[5];
    let tag_size = synchsafe_u32(&hdr12[6..10]).min(256 * 1024);
    if tag_size < 6 || r.seek(SeekFrom::Start(10)).is_err() {
        return out;
    }

    let mut buf = vec![0u8; tag_size];
    let n = r.read(&mut buf).unwrap_or(0);
    buf.truncate(n);

    let mut pos = 0usize;
    // Skip extended header if present on v2.3/v2.4
    if (flags & 0x40) != 0 && buf.len() >= 4 {
        let ext = if ver >= 4 {
            synchsafe_u32(&buf[0..4])
        } else {
            (u32::from_be_bytes([buf[0], buf[1], buf[2], buf[3]]) as usize).saturating_add(4)
        };
        pos = ext.min(buf.len());
    }

    while pos < buf.len() {
        if ver == 2 {
            if pos + 6 > buf.len() || buf[pos] == 0 {
                break;
            }
            let id = &buf[pos..pos + 3];
            let fsize = ((buf[pos + 3] as usize) << 16)
                | ((buf[pos + 4] as usize) << 8)
                | (buf[pos + 5] as usize);
            pos += 6;
            if fsize == 0 || pos + fsize > buf.len() {
                break;
            }
            let val = decode_id3_text(&buf[pos..pos + fsize]);
            match id {
                b"TT2" => out.title = val,
                b"TP1" => out.artist = val,
                b"TAL" => out.album = val,
                b"TCO" => out.genre = val,
                b"TBP" => out.tbpm = parse_bpm_str(&val),
                _ => {}
            }
            pos += fsize;
        } else {
            if pos + 10 > buf.len() || buf[pos] == 0 {
                break;
            }
            let id = &buf[pos..pos + 4];
            let fsize = if ver >= 4 {
                synchsafe_u32(&buf[pos + 4..pos + 8])
            } else {
                u32::from_be_bytes([buf[pos + 4], buf[pos + 5], buf[pos + 6], buf[pos + 7]]) as usize
            };
            let fflags = u16::from_be_bytes([buf[pos + 8], buf[pos + 9]]);
            pos += 10;
            if fsize == 0 || pos + fsize > buf.len() {
                break;
            }
            // Skip compressed/encrypted frames
            if (fflags & 0x00C0) == 0 {
                let slice = if (fflags & 0x0001) != 0 && fsize > 4 {
                    &buf[pos + 4..pos + fsize]
                } else {
                    &buf[pos..pos + fsize]
                };
                let val = decode_id3_text(slice);
                match id {
                    b"TIT2" => out.title = val,
                    b"TPE1" => out.artist = val,
                    b"TALB" => out.album = val,
                    b"TCON" => out.genre = val,
                    b"TBPM" => out.tbpm = parse_bpm_str(&val),
                    _ => {}
                }
            }
            pos += fsize;
        }
    }
    out
}

fn parse_vorbis_comment_block(data: &[u8], out: &mut ExtractedTags) {
    if data.len() < 8 {
        return;
    }
    let vendor_len = u32::from_le_bytes([data[0], data[1], data[2], data[3]]) as usize;
    let mut pos = 4usize.saturating_add(vendor_len);
    if pos + 4 > data.len() {
        return;
    }
    let count = u32::from_le_bytes([data[pos], data[pos + 1], data[pos + 2], data[pos + 3]])
        .min(256) as usize;
    pos += 4;

    for _ in 0..count {
        if pos + 4 > data.len() {
            break;
        }
        let clen =
            u32::from_le_bytes([data[pos], data[pos + 1], data[pos + 2], data[pos + 3]]) as usize;
        pos += 4;
        if pos + clen > data.len() {
            break;
        }
        if let Ok(kv) = std::str::from_utf8(&data[pos..pos + clen]) {
            if let Some((k, v)) = kv.split_once('=') {
                let vt = v.trim();
                if k.eq_ignore_ascii_case("TITLE") && out.title.is_empty() {
                    out.title = vt.to_string();
                } else if k.eq_ignore_ascii_case("ARTIST") && out.artist.is_empty() {
                    out.artist = vt.to_string();
                } else if k.eq_ignore_ascii_case("ALBUM") && out.album.is_empty() {
                    out.album = vt.to_string();
                } else if k.eq_ignore_ascii_case("GENRE") && out.genre.is_empty() {
                    out.genre = vt.to_string();
                } else if (k.eq_ignore_ascii_case("BPM")
                    || k.eq_ignore_ascii_case("TBPM")
                    || k.eq_ignore_ascii_case("TEMPO"))
                    && out.tbpm.is_none()
                {
                    out.tbpm = parse_bpm_str(vt);
                }
            }
        }
        pos += clen;
    }
}

fn parse_flac_blocks<R: Read + Seek>(r: &mut R) -> ExtractedTags {
    let mut out = ExtractedTags::default();
    if r.seek(SeekFrom::Start(4)).is_err() {
        return out;
    }
    for _ in 0..16 {
        let mut bh = [0u8; 4];
        if r.read_exact(&mut bh).is_err() {
            break;
        }
        let is_last = (bh[0] & 0x80) != 0;
        let btype = bh[0] & 0x7F;
        let blen = ((bh[1] as usize) << 16) | ((bh[2] as usize) << 8) | (bh[3] as usize);
        if btype == 4 && blen <= 256 * 1024 {
            let mut body = vec![0u8; blen];
            if r.read_exact(&mut body).is_ok() {
                parse_vorbis_comment_block(&body, &mut out);
            }
            break;
        } else if r.seek(SeekFrom::Current(blen as i64)).is_err() {
            break;
        }
        if is_last {
            break;
        }
    }
    out
}

fn parse_ogg_comments<R: Read + Seek>(r: &mut R) -> ExtractedTags {
    let mut out = ExtractedTags::default();
    if r.seek(SeekFrom::Start(0)).is_err() {
        return out;
    }
    let mut buf = vec![0u8; 64 * 1024];
    let n = r.read(&mut buf).unwrap_or(0);
    buf.truncate(n);

    // Locate "\x03vorbis" or "OpusTags" inside the initial Ogg pages
    for i in 0..buf.len().saturating_sub(8) {
        if &buf[i..i + 7] == b"\x03vorbis" {
            parse_vorbis_comment_block(&buf[i + 7..], &mut out);
            break;
        } else if &buf[i..i + 8] == b"OpusTags" {
            parse_vorbis_comment_block(&buf[i + 8..], &mut out);
            break;
        }
    }
    out
}

fn parse_mp4_atoms<R: Read + Seek>(r: &mut R) -> ExtractedTags {
    let mut out = ExtractedTags::default();
    if r.seek(SeekFrom::Start(0)).is_err() {
        return out;
    }
    // Scan first 128 KB for `tmpo` integer BPM atom inside `moov/udta/meta/ilst`
    let mut buf = vec![0u8; 128 * 1024];
    let n = r.read(&mut buf).unwrap_or(0);
    buf.truncate(n);

    for i in 4..buf.len().saturating_sub(24) {
        if &buf[i..i + 4] == b"tmpo" {
            let atom_len =
                u32::from_be_bytes([buf[i - 4], buf[i - 3], buf[i - 2], buf[i - 1]]) as usize;
            let end = (i - 4 + atom_len).min(buf.len());
            if end >= i + 22 && &buf[i + 8..i + 12] == b"data" {
                let bpm_u16 = u16::from_be_bytes([buf[i + 20], buf[i + 21]]);
                if (25..=400).contains(&bpm_u16) {
                    out.tbpm = Some(bpm_u16 as f32);
                }
            }
            break;
        }
    }
    out
}
