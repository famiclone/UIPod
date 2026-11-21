//! Generic PSF (Portable Sound Format) container, `[TAG]` metadata, and `_lib` resolver.
//!
//! Supports GSF (`0x22`, Game Boy Advance: `.gsf` / `.minigsf` / `.gsflib`) as well as
//! future PSF-family formats (`PSF1`, `PSF2`, `SSF`, `DSF`, `USF`, `SNSF`, `2SF`, `QSF`).

use std::collections::HashMap;
use std::io::Read;
use std::path::{Path, PathBuf};

use flate2::bufread::ZlibDecoder;

pub const PSF_VERSION_PSF1: u8 = 0x01;
pub const PSF_VERSION_PSF2: u8 = 0x02;
pub const PSF_VERSION_SSF: u8 = 0x11;
pub const PSF_VERSION_DSF: u8 = 0x12;
pub const PSF_VERSION_USF: u8 = 0x21;
pub const PSF_VERSION_GSF: u8 = 0x22;
pub const PSF_VERSION_SNSF: u8 = 0x23;
pub const PSF_VERSION_2SF: u8 = 0x24;
pub const PSF_VERSION_QSF: u8 = 0x41;

/// Maximum uncompressed program size (64 MiB safety cap; GBA max ROM is 32 MiB).
const MAX_UNCOMPRESSED_PROGRAM: usize = 64 * 1024 * 1024;

pub const DEFAULT_LENGTH_MS: u32 = 150_000;
pub const DEFAULT_FADE_MS: u32 = 5_000;

#[derive(Debug, Clone)]
pub struct PsfHeader {
    pub version: u8,
    pub reserved_size: u32,
    pub compressed_size: u32,
    pub compressed_crc32: u32,
}

impl PsfHeader {
    pub fn parse(bytes: &[u8]) -> Result<Self, &'static str> {
        if bytes.len() < 16 {
            return Err("PSF file too short");
        }
        if &bytes[0..3] != b"PSF" {
            return Err("Invalid PSF magic");
        }
        let version = bytes[3];
        let reserved_size = u32::from_le_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]);
        let compressed_size = u32::from_le_bytes([bytes[8], bytes[9], bytes[10], bytes[11]]);
        let compressed_crc32 = u32::from_le_bytes([bytes[12], bytes[13], bytes[14], bytes[15]]);
        Ok(Self {
            version,
            reserved_size,
            compressed_size,
            compressed_crc32,
        })
    }

    #[inline]
    pub fn tag_offset(&self) -> Option<u64> {
        16u64
            .checked_add(self.reserved_size as u64)?
            .checked_add(self.compressed_size as u64)
    }
}

#[derive(Debug, Clone, Default)]
pub struct PsfTags {
    pub raw: HashMap<String, String>,
    pub title: Option<String>,
    pub artist: Option<String>,
    pub game: Option<String>,
    pub year: Option<i32>,
    pub genre: Option<String>,
    pub comment: Option<String>,
    pub copyright: Option<String>,
    pub dumper: Option<String>,
    pub track_num: Option<i32>,
    pub length_ms: Option<u32>,
    pub fade_ms: Option<u32>,
    pub volume: Option<f32>,
    pub lib: Option<String>,
}

impl PsfTags {
    pub fn parse_tag_section(tag_bytes: &[u8]) -> Self {
        let mut tags = Self::default();
        if tag_bytes.len() < 5 || &tag_bytes[0..5] != b"[TAG]" {
            return tags;
        }
        let body = &tag_bytes[5..];
        let text = decode_psf_text(body);

        for raw_line in text.split('\n') {
            let line = raw_line.trim_end_matches('\r');
            let Some(eq_idx) = line.find('=') else {
                continue;
            };
            let key = line[..eq_idx].trim().to_ascii_lowercase();
            let val = line[eq_idx + 1..].trim();
            if key.is_empty() || val.is_empty() {
                continue;
            }
            tags.raw
                .entry(key)
                .and_modify(|existing| {
                    existing.push('\n');
                    existing.push_str(val);
                })
                .or_insert_with(|| val.to_string());
        }

        tags.title = tags.get_nonempty("title");
        tags.artist = tags.get_nonempty("artist");
        tags.game = tags.get_nonempty("game");
        tags.genre = tags.get_nonempty("genre");
        tags.comment = tags.get_nonempty("comment");
        tags.copyright = tags.get_nonempty("copyright");
        tags.dumper = tags
            .get_nonempty("gsfby")
            .or_else(|| tags.get_nonempty("psfby"))
            .or_else(|| tags.get_nonempty("2sfby"))
            .or_else(|| tags.get_nonempty("usfby"))
            .or_else(|| tags.get_nonempty("ssfby"));
        tags.lib = tags.get_nonempty("_lib");

        if let Some(y_str) = tags.raw.get("year") {
            tags.year = extract_first_i32(y_str);
        }
        if let Some(t_str) = tags.raw.get("track") {
            tags.track_num = extract_first_i32(t_str);
        }
        if let Some(len_str) = tags.raw.get("length") {
            tags.length_ms = parse_psf_time_ms(len_str);
        }
        if let Some(fade_str) = tags.raw.get("fade") {
            tags.fade_ms = parse_psf_time_ms(fade_str);
        }
        if let Some(vol_str) = tags.raw.get("volume") {
            if let Ok(v) = vol_str.trim().parse::<f32>() {
                if v.is_finite() && v > 0.0 {
                    tags.volume = Some(v.clamp(0.05, 8.0));
                }
            }
        }

        tags
    }

    fn get_nonempty(&self, key: &str) -> Option<String> {
        self.raw
            .get(key)
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
    }

    /// Fill missing album/artist/year/genre/copyright metadata from a parent `_lib` file's tags.
    pub fn merge_fallback_from(&mut self, parent: &PsfTags) {
        if self.game.is_none() {
            self.game = parent.game.clone();
        }
        if self.artist.is_none() {
            self.artist = parent.artist.clone();
        }
        if self.year.is_none() {
            self.year = parent.year;
        }
        if self.genre.is_none() {
            self.genre = parent.genre.clone();
        }
        if self.copyright.is_none() {
            self.copyright = parent.copyright.clone();
        }
        if self.dumper.is_none() {
            self.dumper = parent.dumper.clone();
        }
    }

    /// Effective play duration (before fade) and fade duration in milliseconds.
    pub fn effective_timing_ms(&self) -> (u32, u32) {
        match (self.length_ms, self.fade_ms) {
            (Some(len), Some(fade)) => (len.max(100), fade),
            (Some(len), None) => (len.max(100), DEFAULT_FADE_MS),
            (None, Some(fade)) => (DEFAULT_LENGTH_MS, fade),
            (None, None) => (DEFAULT_LENGTH_MS, DEFAULT_FADE_MS),
        }
    }

    pub fn total_duration_ms(&self) -> u32 {
        let (play, fade) = self.effective_timing_ms();
        play.saturating_add(fade)
    }

    /// Secondary overlay libraries `_lib2`, `_lib3`, ...
    pub fn extra_libs(&self) -> Vec<String> {
        let mut out = Vec::new();
        let mut idx = 2usize;
        loop {
            let key = format!("_lib{idx}");
            if let Some(val) = self.get_nonempty(&key) {
                out.push(val);
                idx += 1;
            } else {
                break;
            }
        }
        out
    }
}

#[derive(Debug, Clone)]
pub struct PsfFile {
    pub header: PsfHeader,
    pub reserved: Vec<u8>,
    pub program: Vec<u8>,
    pub tags: PsfTags,
}

impl PsfFile {
    pub fn parse(bytes: &[u8], expected_version: Option<u8>) -> Result<Self, &'static str> {
        let header = PsfHeader::parse(bytes)?;
        if let Some(ver) = expected_version {
            if header.version != ver {
                return Err("Unexpected PSF version");
            }
        }

        let res_start = 16usize;
        let res_end = res_start
            .checked_add(header.reserved_size as usize)
            .ok_or("PSF reserved size overflow")?;
        let comp_end = res_end
            .checked_add(header.compressed_size as usize)
            .ok_or("PSF compressed size overflow")?;

        if comp_end > bytes.len() {
            return Err("PSF truncated before end of program data");
        }

        let reserved = bytes[res_start..res_end].to_vec();
        let comp_slice = &bytes[res_end..comp_end];

        let mut program = Vec::new();
        if !comp_slice.is_empty() {
            let actual_crc = crc32fast::hash(comp_slice);
            if actual_crc != header.compressed_crc32 {
                return Err("PSF program CRC32 mismatch");
            }
            let decoder = ZlibDecoder::new(comp_slice);
            decoder
                .take((MAX_UNCOMPRESSED_PROGRAM + 1) as u64)
                .read_to_end(&mut program)
                .map_err(|_| "PSF zlib decompression failed")?;
            if program.len() > MAX_UNCOMPRESSED_PROGRAM {
                return Err("PSF uncompressed program exceeds safety limit");
            }
        }

        let tags = if comp_end < bytes.len() {
            PsfTags::parse_tag_section(&bytes[comp_end..])
        } else {
            PsfTags::default()
        };

        Ok(Self {
            header,
            reserved,
            program,
            tags,
        })
    }

    pub fn parse_tags_from_slice(
        bytes: &[u8],
        expected_version: Option<u8>,
    ) -> Result<PsfTags, &'static str> {
        let header = PsfHeader::parse(bytes)?;
        if let Some(ver) = expected_version {
            if header.version != ver {
                return Err("Unexpected PSF version");
            }
        }
        let tag_off = header.tag_offset().ok_or("PSF offset overflow")? as usize;
        if tag_off > bytes.len() {
            return Err("PSF truncated");
        }
        if tag_off == bytes.len() {
            return Ok(PsfTags::default());
        }
        Ok(PsfTags::parse_tag_section(&bytes[tag_off..]))
    }
}

fn decode_psf_text(bytes: &[u8]) -> String {
    let trimmed = match bytes.iter().position(|&b| b == 0) {
        Some(nul) => &bytes[..nul],
        None => bytes,
    };
    if let Ok(utf8) = std::str::from_utf8(trimmed) {
        return utf8.to_string();
    }
    // Fallback for ISO-8859-1 / Windows-1252 tagged PSF rips
    trimmed.iter().map(|&b| b as char).collect()
}

fn extract_first_i32(s: &str) -> Option<i32> {
    let trimmed = s.trim();
    let digits: String = trimmed
        .chars()
        .skip_while(|c| !c.is_ascii_digit())
        .take_while(|c| c.is_ascii_digit())
        .collect();
    if digits.is_empty() {
        None
    } else {
        digits.parse::<i32>().ok()
    }
}

/// Parses PSF `[TAG]` time values (`SS`, `SS.xxx`, `MM:SS.xxx`, `HH:MM:SS.xxx`, or `,` decimals).
pub fn parse_psf_time_ms(s: &str) -> Option<u32> {
    let s = s.trim();
    if s.is_empty() {
        return None;
    }

    let parts: Vec<&str> = s.split(':').collect();
    if parts.len() > 3 {
        return None;
    }

    let mut total_ms: u64 = 0;
    for (idx, part) in parts.iter().enumerate() {
        let is_last = idx + 1 == parts.len();
        if !is_last {
            let unit = part.trim().parse::<u64>().ok()?;
            total_ms = total_ms.checked_mul(60)?.checked_add(unit * 1000)?;
        } else {
            let normalized = part.trim().replace(',', ".");
            let (sec_str, frac_str) = match normalized.split_once('.') {
                Some((a, b)) => (a, Some(b)),
                None => (normalized.as_str(), None),
            };
            let secs = if sec_str.is_empty() {
                0
            } else {
                sec_str.parse::<u64>().ok()?
            };
            let mut ms_frac = 0u64;
            if let Some(f) = frac_str {
                let digits: Vec<u8> = f
                    .bytes()
                    .take_while(|b| b.is_ascii_digit())
                    .take(3)
                    .collect();
                if !digits.is_empty() {
                    let mut val = 0u64;
                    for &d in &digits {
                        val = val * 10 + (d - b'0') as u64;
                    }
                    ms_frac = match digits.len() {
                        1 => val * 100,
                        2 => val * 10,
                        _ => val,
                    };
                }
            }
            total_ms = total_ms
                .checked_mul(60)?
                .checked_add(secs.checked_mul(1000)?.checked_add(ms_frac)?)?;
        }
    }

    u32::try_from(total_ms).ok()
}

/// Extract optional leading track number (e.g. `"01 BGM #01.minigsf"` -> `Some(1)`)
/// and clean filename stem (`"01 BGM #01"` / `"BGM #01"`).
pub fn infer_track_and_title_from_path(path: &str) -> (Option<i32>, Option<String>) {
    let file_name = path.rsplit(['/', '\\']).next().unwrap_or(path).trim();
    if file_name.is_empty() {
        return (None, None);
    }
    let stem = match file_name.rfind('.') {
        Some(dot) if dot > 0 => &file_name[..dot],
        _ => file_name,
    }
    .trim();
    if stem.is_empty() {
        return (None, None);
    }

    let digit_len = stem.bytes().take_while(|b| b.is_ascii_digit()).count();
    if (1..=3).contains(&digit_len) {
        let rest = &stem[digit_len..];
        if let Some(stripped) = rest
            .strip_prefix(" - ")
            .or_else(|| rest.strip_prefix(". "))
            .or_else(|| rest.strip_prefix('_'))
            .or_else(|| rest.strip_prefix(' '))
        {
            let num = stem[..digit_len].parse::<i32>().ok().filter(|&n| n > 0);
            let clean = stripped.trim();
            let title = if clean.is_empty() {
                stem.to_string()
            } else {
                clean.to_string()
            };
            return (num, Some(title));
        }
    }

    (None, Some(stem.to_string()))
}

/// Resolve a relative `_lib` filename inside `base_dir`, with case-insensitive directory scan fallback.
pub fn resolve_sibling_path(base_dir: &Path, lib_rel: &str) -> Option<PathBuf> {
    let cleaned = lib_rel.trim().replace('\\', "/");
    if cleaned.is_empty() {
        return None;
    }
    // Reject path traversal outside parent chain while allowing clean relative names
    let rel_path = Path::new(&cleaned);
    if rel_path.is_absolute() {
        return None;
    }
    for comp in rel_path.components() {
        if matches!(
            comp,
            std::path::Component::ParentDir | std::path::Component::RootDir
        ) {
            return None;
        }
    }

    let direct = base_dir.join(rel_path);
    if direct.is_file() {
        return Some(direct);
    }

    // Case-insensitive lookup for single-segment `_lib` filenames (e.g. `agb-baee-usa.gsflib` vs `AGB-BAEE-USA.gsflib`)
    let parent = direct.parent()?;
    let target_name = direct.file_name()?.to_str()?;
    let entries = std::fs::read_dir(parent).ok()?;
    for entry in entries.flatten() {
        if let Some(name) = entry.file_name().to_str() {
            if name.eq_ignore_ascii_case(target_name) {
                let p = entry.path();
                if p.is_file() {
                    return Some(p);
                }
            }
        }
    }
    None
}
