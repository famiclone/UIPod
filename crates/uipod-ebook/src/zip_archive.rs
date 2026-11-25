//! Bounds-checked pure-Rust slice-based ZIP reader (`Stored` & `Deflate` via `flate2`),
//! shared across `.epub` and `.fb2.zip` on Linux, Android, and `wasm32`.

use std::io::Read;

use flate2::read::DeflateDecoder;

const EOCD_SIG: u32 = 0x0605_4b50;
const CEN_SIG: u32 = 0x0201_4b50;
const LOC_SIG: u32 = 0x0403_4b50;
/// Cap individual decompressed archive entries at `24 MiB` to stay well within handheld RAM limits.
const MAX_ENTRY_UNCOMPRESSED: usize = 24 * 1024 * 1024;

#[derive(Debug, Clone)]
pub struct ZipEntryMeta {
    pub name: String,
    pub method: u16,
    pub compressed_size: usize,
    pub uncompressed_size: usize,
    pub local_header_offset: usize,
}

pub struct ZipArchive<'a> {
    data: &'a [u8],
    entries: Vec<ZipEntryMeta>,
}

impl<'a> ZipArchive<'a> {
    pub fn parse(data: &'a [u8]) -> Result<Self, &'static str> {
        if data.len() < 22 {
            return Err("File too small to be a ZIP archive");
        }
        let max_comment = 65_535usize;
        let min_pos = data.len().saturating_sub(22 + max_comment);
        let mut eocd_off = None;
        for pos in (min_pos..=data.len() - 22).rev() {
            if read_u32_le(data, pos) == Some(EOCD_SIG) {
                eocd_off = Some(pos);
                break;
            }
        }
        let eocd = eocd_off.ok_or("Missing ZIP End of Central Directory")?;
        let cd_size = read_u32_le(data, eocd + 12).ok_or("Truncated EOCD")? as usize;
        let cd_off = read_u32_le(data, eocd + 16).ok_or("Truncated EOCD")? as usize;
        let cd_end = cd_off
            .checked_add(cd_size)
            .filter(|&end| end <= data.len())
            .ok_or("Invalid ZIP Central Directory bounds")?;

        let mut entries = Vec::new();
        let mut cur = cd_off;
        while cur + 46 <= cd_end {
            if read_u32_le(data, cur) != Some(CEN_SIG) {
                break;
            }
            let method = read_u16_le(data, cur + 10).unwrap_or(0);
            let comp_sz = read_u32_le(data, cur + 20).unwrap_or(0) as usize;
            let uncomp_sz = read_u32_le(data, cur + 24).unwrap_or(0) as usize;
            let name_len = read_u16_le(data, cur + 28).unwrap_or(0) as usize;
            let extra_len = read_u16_le(data, cur + 30).unwrap_or(0) as usize;
            let comment_len = read_u16_le(data, cur + 32).unwrap_or(0) as usize;
            let local_off = read_u32_le(data, cur + 42).unwrap_or(0) as usize;

            let name_start = cur + 46;
            let next = name_start
                .checked_add(name_len)
                .and_then(|v| v.checked_add(extra_len))
                .and_then(|v| v.checked_add(comment_len))
                .ok_or("Overflow in ZIP entry header")?;
            if next > cd_end {
                break;
            }

            let raw_name = String::from_utf8_lossy(&data[name_start..name_start + name_len]);
            let norm_name = normalize_zip_path(&raw_name);
            if !norm_name.is_empty() && !raw_name.ends_with('/') {
                entries.push(ZipEntryMeta {
                    name: norm_name,
                    method,
                    compressed_size: comp_sz,
                    uncompressed_size: uncomp_sz,
                    local_header_offset: local_off,
                });
            }
            cur = next;
        }

        Ok(Self { data, entries })
    }

    pub fn entries(&self) -> &[ZipEntryMeta] {
        &self.entries
    }

    pub fn find_index(&self, target_path: &str) -> Option<usize> {
        let norm = normalize_zip_path(target_path);
        if let Some(idx) = self.entries.iter().position(|e| e.name == norm) {
            return Some(idx);
        }
        let lower = norm.to_lowercase();
        self.entries
            .iter()
            .position(|e| e.name.to_lowercase() == lower)
    }

    pub fn read_by_path(&self, target_path: &str) -> Option<Vec<u8>> {
        let idx = self.find_index(target_path)?;
        self.read_by_index(idx).ok()
    }

    pub fn read_by_index(&self, index: usize) -> Result<Vec<u8>, &'static str> {
        let meta = self.entries.get(index).ok_or("ZIP entry index out of bounds")?;
        if meta.uncompressed_size > MAX_ENTRY_UNCOMPRESSED {
            return Err("ZIP entry exceeds safe memory limit");
        }
        let loc = meta.local_header_offset;
        if loc + 30 > self.data.len() || read_u32_le(self.data, loc) != Some(LOC_SIG) {
            return Err("Invalid ZIP Local File Header");
        }
        let name_len = read_u16_le(self.data, loc + 26).unwrap_or(0) as usize;
        let extra_len = read_u16_le(self.data, loc + 28).unwrap_or(0) as usize;
        let payload_start = loc
            .checked_add(30)
            .and_then(|v| v.checked_add(name_len))
            .and_then(|v| v.checked_add(extra_len))
            .ok_or("ZIP local header overflow")?;
        let payload_end = payload_start
            .checked_add(meta.compressed_size)
            .filter(|&end| end <= self.data.len())
            .ok_or("ZIP compressed payload out of bounds")?;
        let slice = &self.data[payload_start..payload_end];

        match meta.method {
            0 => Ok(slice.to_vec()),
            8 => {
                let cap = meta.uncompressed_size.min(MAX_ENTRY_UNCOMPRESSED);
                let mut out = Vec::with_capacity(cap.min(256 * 1024));
                let mut dec = DeflateDecoder::new(slice).take(MAX_ENTRY_UNCOMPRESSED as u64);
                dec.read_to_end(&mut out)
                    .map_err(|_| "Deflate decompression error")?;
                Ok(out)
            }
            _ => Err("Unsupported ZIP compression method"),
        }
    }
}

#[inline]
fn read_u16_le(data: &[u8], off: usize) -> Option<u16> {
    let b = data.get(off..off + 2)?;
    Some(u16::from_le_bytes([b[0], b[1]]))
}

#[inline]
fn read_u32_le(data: &[u8], off: usize) -> Option<u32> {
    let b = data.get(off..off + 4)?;
    Some(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
}

/// Normalizes archive paths (percent-decoding `%20` etc., converting `\`, collapsing `.` and `..`).
pub fn normalize_zip_path(raw: &str) -> String {
    let unescaped = percent_decode(raw.trim());
    let mut parts: Vec<&str> = Vec::new();
    for seg in unescaped.split(['/', '\\']) {
        if seg.is_empty() || seg == "." {
            continue;
        }
        if seg == ".." {
            let _ = parts.pop();
        } else {
            parts.push(seg);
        }
    }
    parts.join("/")
}

/// Resolves a relative URI (`../Images/cover.jpg#top`) against a base archive file path (`OEBPS/Text/ch1.xhtml`),
/// returning `(normalized_archive_path, optional_fragment_id)`.
pub fn resolve_relative_href(base_file: &str, href: &str) -> (String, Option<String>) {
    let trimmed = href.trim();
    let (path_raw, frag) = match trimmed.split_once('#') {
        Some((p, f)) => (
            p.trim(),
            if f.trim().is_empty() {
                None
            } else {
                Some(percent_decode(f.trim()))
            },
        ),
        None => (trimmed, None),
    };
    let path_part = path_raw.split('?').next().unwrap_or(path_raw).trim();
    if path_part.is_empty() {
        return (normalize_zip_path(base_file), frag);
    }
    if let Some(stripped) = path_part.strip_prefix('/') {
        return (normalize_zip_path(stripped), frag);
    }
    let base_dir = match base_file.rfind('/') {
        Some(slash) => &base_file[..slash],
        None => "",
    };
    let joined = if base_dir.is_empty() {
        path_part.to_string()
    } else {
        format!("{base_dir}/{path_part}")
    };
    (normalize_zip_path(&joined), frag)
}

pub fn percent_decode(s: &str) -> String {
    if !s.contains('%') {
        return s.to_string();
    }
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let (Some(hi), Some(lo)) = (hex_nibble(bytes[i + 1]), hex_nibble(bytes[i + 2])) {
                out.push((hi << 4) | lo);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[inline]
fn hex_nibble(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

/// Helper used solely by unit tests (and tools) to build minimal valid ZIP archives in memory.
#[cfg(test)]
pub fn build_test_zip(files: &[(&str, &[u8], bool)]) -> Vec<u8> {
    use std::io::Write;
    use flate2::write::DeflateEncoder;
    use flate2::Compression;

    let mut out = Vec::new();
    let mut cd = Vec::new();

    for &(name, content, deflate) in files {
        let local_off = out.len() as u32;
        let (method, payload) = if deflate {
            let mut enc = DeflateEncoder::new(Vec::new(), Compression::fast());
            enc.write_all(content).unwrap();
            (8u16, enc.finish().unwrap())
        } else {
            (0u16, content.to_vec())
        };
        let crc = crc32_simple(content);
        let comp_sz = payload.len() as u32;
        let uncomp_sz = content.len() as u32;
        let name_b = name.as_bytes();

        // Local file header
        out.extend_from_slice(&LOC_SIG.to_le_bytes());
        out.extend_from_slice(&20u16.to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes());
        out.extend_from_slice(&method.to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes());
        out.extend_from_slice(&crc.to_le_bytes());
        out.extend_from_slice(&comp_sz.to_le_bytes());
        out.extend_from_slice(&uncomp_sz.to_le_bytes());
        out.extend_from_slice(&(name_b.len() as u16).to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes());
        out.extend_from_slice(name_b);
        out.extend_from_slice(&payload);

        // Central directory header
        cd.extend_from_slice(&CEN_SIG.to_le_bytes());
        cd.extend_from_slice(&20u16.to_le_bytes());
        cd.extend_from_slice(&20u16.to_le_bytes());
        cd.extend_from_slice(&0u16.to_le_bytes());
        cd.extend_from_slice(&method.to_le_bytes());
        cd.extend_from_slice(&0u16.to_le_bytes());
        cd.extend_from_slice(&0u16.to_le_bytes());
        cd.extend_from_slice(&crc.to_le_bytes());
        cd.extend_from_slice(&comp_sz.to_le_bytes());
        cd.extend_from_slice(&uncomp_sz.to_le_bytes());
        cd.extend_from_slice(&(name_b.len() as u16).to_le_bytes());
        cd.extend_from_slice(&0u16.to_le_bytes());
        cd.extend_from_slice(&0u16.to_le_bytes());
        cd.extend_from_slice(&0u16.to_le_bytes());
        cd.extend_from_slice(&0u16.to_le_bytes());
        cd.extend_from_slice(&0u32.to_le_bytes());
        cd.extend_from_slice(&local_off.to_le_bytes());
        cd.extend_from_slice(name_b);
    }

    let cd_off = out.len() as u32;
    let cd_sz = cd.len() as u32;
    let cnt = files.len() as u16;
    out.extend_from_slice(&cd);

    out.extend_from_slice(&EOCD_SIG.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes());
    out.extend_from_slice(&cnt.to_le_bytes());
    out.extend_from_slice(&cnt.to_le_bytes());
    out.extend_from_slice(&cd_sz.to_le_bytes());
    out.extend_from_slice(&cd_off.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes());
    out
}

#[cfg(test)]
fn crc32_simple(bytes: &[u8]) -> u32 {
    let mut crc = 0xFFFF_FFFFu32;
    for &b in bytes {
        crc ^= b as u32;
        for _ in 0..8 {
            let mask = (crc & 1).wrapping_neg();
            crc = (crc >> 1) ^ (0xEDB8_8320 & mask);
        }
    }
    !crc
}
