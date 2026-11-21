//! `.nsf` (NESM v1 & v2) and `.nsfe` (RIFF-style chunked) container parsers.
//!
//! Decodes metadata strings tolerantly across UTF-8, Japanese Shift-JIS (CP932),
//! and Windows-1252 / ISO-8859-1 so Famicom and NES rips never fail to load.

use crate::{ChipFlags, NsfError};

pub const DEFAULT_NTSC_SPEED_US: u16 = 16_666;
pub const DEFAULT_PAL_SPEED_US: u16 = 20_000;
pub const NTSC_CPU_CLOCK_HZ: u32 = 1_789_773;
pub const PAL_CPU_CLOCK_HZ: u32 = 1_662_607;

pub(crate) const UNMAPPED_BANK: u16 = u16::MAX;

/// Parsed low-level `.nsf` or `.nsfe` image with pre-sliced 4 KB ROM banks.
#[derive(Clone, Debug)]
pub(crate) struct ParsedNsf {
    pub total_subtracks: u16,
    pub starting_subtrack: u16, // 0-based
    pub init_addr: u16,
    pub play_addr: u16,
    pub game_title: String,
    pub artist: String,
    pub copyright: String,
    pub ripper: Option<String>,
    pub ntsc_speed_us: u16,
    pub pal_speed_us: u16,
    pub is_pal: bool,
    pub chip_flags: ChipFlags,
    pub payload_crc32: u32,
    pub rom_banks: Vec<[u8; 4096]>,
    pub initial_prg_ram: Option<Box<[u8; 8192]>>,
    pub linear_bank_map: [u16; 8],
    pub plst: Option<Vec<u16>>,
    pub tlbl: Vec<Option<String>>,
    pub time_ms: Vec<Option<u32>>,
    pub fade_ms: Vec<Option<u32>>,
}

impl ParsedNsf {
    pub fn parse(bytes: &[u8]) -> Result<Self, NsfError> {
        if bytes.len() < 8 {
            return Err(NsfError::TooShort);
        }
        if &bytes[0..4] == b"NSFE" {
            Self::parse_nsfe(bytes, true)
        } else if bytes.len() >= 5 && &bytes[0..5] == b"NESM\x1A" {
            Self::parse_nesm(bytes, true)
        } else {
            Err(NsfError::InvalidMagic)
        }
    }

    /// Parse header and NSFE/NSF2 metadata chunks ONLY, without slicing 4 KB ROM banks,
    /// allocating PRG RAM, or requiring PRG payload bytes after the 128-byte NESM header.
    pub fn parse_header_fast(bytes: &[u8]) -> Result<Self, NsfError> {
        if bytes.len() < 8 {
            return Err(NsfError::TooShort);
        }
        if &bytes[0..4] == b"NSFE" {
            Self::parse_nsfe(bytes, false)
        } else if bytes.len() >= 5 && &bytes[0..5] == b"NESM\x1A" {
            Self::parse_nesm(bytes, false)
        } else {
            Err(NsfError::InvalidMagic)
        }
    }

    #[inline]
    pub fn cpu_clock_hz(&self) -> u32 {
        if self.is_pal {
            PAL_CPU_CLOCK_HZ
        } else {
            NTSC_CPU_CLOCK_HZ
        }
    }

    #[inline]
    pub fn frame_speed_us(&self) -> u16 {
        let raw = if self.is_pal {
            self.pal_speed_us
        } else {
            self.ntsc_speed_us
        };
        if raw == 0 {
            if self.is_pal {
                DEFAULT_PAL_SPEED_US
            } else {
                DEFAULT_NTSC_SPEED_US
            }
        } else {
            raw
        }
    }

    #[inline]
    pub fn frame_cycles(&self) -> u32 {
        let clk = self.cpu_clock_hz() as u64;
        let speed = self.frame_speed_us() as u64;
        ((clk * speed) / 1_000_000).clamp(1_000, 200_000) as u32
    }

    fn parse_nesm(bytes: &[u8], build_banks: bool) -> Result<Self, NsfError> {
        if bytes.len() < 0x80 {
            return Err(NsfError::TooShort);
        }
        let version = bytes[0x05];
        let total_raw = bytes[0x06];
        if total_raw == 0 {
            return Err(NsfError::InvalidHeader("NSF total_songs is 0"));
        }
        let total_subtracks = total_raw as u16;
        let starting_1based = bytes[0x07].max(1) as u16;
        let starting_subtrack = (starting_1based - 1).min(total_subtracks - 1);

        let load_addr = u16::from_le_bytes([bytes[0x08], bytes[0x09]]);
        let init_addr = u16::from_le_bytes([bytes[0x0A], bytes[0x0B]]);
        let play_addr = u16::from_le_bytes([bytes[0x0C], bytes[0x0D]]);
        if load_addr < 0x6000 {
            return Err(NsfError::InvalidHeader("NSF load_addr below $6000"));
        }

        let game_title = decode_nsf_text(&bytes[0x0E..0x2E]);
        let artist = decode_nsf_text(&bytes[0x2E..0x4E]);
        let copyright = decode_nsf_text(&bytes[0x4E..0x6E]);

        let ntsc_raw = u16::from_le_bytes([bytes[0x6E], bytes[0x6F]]);
        let ntsc_speed_us = if ntsc_raw == 0 {
            DEFAULT_NTSC_SPEED_US
        } else {
            ntsc_raw
        };

        let mut bank_init = [0u8; 8];
        bank_init.copy_from_slice(&bytes[0x70..0x78]);
        let has_bankswitching = bank_init.iter().any(|&b| b != 0);

        let pal_raw = u16::from_le_bytes([bytes[0x78], bytes[0x79]]);
        let pal_speed_us = if pal_raw == 0 {
            DEFAULT_PAL_SPEED_US
        } else {
            pal_raw
        };

        let tv_bits = bytes[0x7A];
        // Bit 0 = PAL, Bit 1 = Dual PAL/NTSC (NTSC preferred when dual)
        let is_pal = (tv_bits & 0x03) == 0x01;
        let chip_flags = ChipFlags(bytes[0x7B] & 0x3F);

        // NSF2 optional 24-bit PRG data length in 0x7D..0x80
        let nsf2_prg_len = u32::from_le_bytes([bytes[0x7D], bytes[0x7E], bytes[0x7F], 0]) as usize;
        let prg_end = if version >= 2 && nsf2_prg_len > 0 && 0x80 + nsf2_prg_len <= bytes.len() {
            0x80 + nsf2_prg_len
        } else {
            bytes.len()
        };
        let prg_data = &bytes[0x80..prg_end];
        if build_banks && prg_data.is_empty() {
            return Err(NsfError::InvalidHeader("NSF has empty PRG payload"));
        }

        let payload_crc32 = if prg_data.is_empty() {
            0
        } else {
            crc32fast::hash(prg_data)
        };
        let (rom_banks, initial_prg_ram, linear_bank_map) = if build_banks {
            build_rom_banks(prg_data, load_addr, has_bankswitching, &bank_init)
        } else {
            (Vec::new(), None, [UNMAPPED_BANK; 8])
        };

        let mut parsed = Self {
            total_subtracks,
            starting_subtrack,
            init_addr,
            play_addr,
            game_title,
            artist,
            copyright,
            ripper: None,
            ntsc_speed_us,
            pal_speed_us,
            is_pal,
            chip_flags,
            payload_crc32,
            rom_banks,
            initial_prg_ram,
            linear_bank_map,
            plst: None,
            tlbl: Vec::new(),
            time_ms: Vec::new(),
            fade_ms: Vec::new(),
        };

        // NSF2 may append NSFE metadata chunks immediately after PRG data
        if prg_end + 8 <= bytes.len() {
            let _ = parsed.parse_metadata_chunks(&bytes[prg_end..]);
        }

        Ok(parsed)
    }

    fn parse_nsfe(bytes: &[u8], build_banks: bool) -> Result<Self, NsfError> {
        let mut pos = 4usize;
        let mut info_seen = false;
        let mut load_addr = 0x8000u16;
        let mut init_addr = 0x8000u16;
        let mut play_addr = 0x8003u16;
        let mut is_pal = false;
        let mut chip_flags = ChipFlags(0);
        let mut total_subtracks = 1u16;
        let mut starting_subtrack = 0u16;
        let mut ntsc_speed_us = DEFAULT_NTSC_SPEED_US;
        let mut pal_speed_us = DEFAULT_PAL_SPEED_US;
        let mut has_bankswitching = false;
        let mut bank_init = [0u8; 8];
        let mut prg_slice: Option<&[u8]> = None;

        let mut game_title = String::new();
        let mut artist = String::new();
        let mut copyright = String::new();
        let mut ripper = None;
        let mut plst: Option<Vec<u16>> = None;
        let mut tlbl: Vec<Option<String>> = Vec::new();
        let mut time_ms: Vec<Option<u32>> = Vec::new();
        let mut fade_ms: Vec<Option<u32>> = Vec::new();

        while pos + 8 <= bytes.len() {
            let chunk_len =
                u32::from_le_bytes([bytes[pos], bytes[pos + 1], bytes[pos + 2], bytes[pos + 3]])
                    as usize;
            let chunk_id = &bytes[pos + 4..pos + 8];
            pos += 8;

            if chunk_id == b"NEND" {
                break;
            }
            if pos.checked_add(chunk_len).is_none_or(|end| end > bytes.len()) {
                return Err(NsfError::InvalidHeader("NSFE chunk exceeds file size"));
            }
            let data = &bytes[pos..pos + chunk_len];
            pos += chunk_len;

            match chunk_id {
                b"INFO" => {
                    if data.len() < 8 {
                        return Err(NsfError::InvalidHeader("NSFE INFO chunk < 8 bytes"));
                    }
                    load_addr = u16::from_le_bytes([data[0], data[1]]);
                    init_addr = u16::from_le_bytes([data[2], data[3]]);
                    play_addr = u16::from_le_bytes([data[4], data[5]]);
                    if load_addr < 0x6000 {
                        return Err(NsfError::InvalidHeader("NSFE load_addr below $6000"));
                    }
                    is_pal = (data[6] & 0x03) == 0x01;
                    chip_flags = ChipFlags(data[7] & 0x3F);
                    if data.len() >= 9 && data[8] > 0 {
                        total_subtracks = data[8] as u16;
                    }
                    if data.len() >= 10 {
                        starting_subtrack = (data[9] as u16).min(total_subtracks.saturating_sub(1));
                    }
                    info_seen = true;
                }
                b"DATA" => {
                    prg_slice = Some(data);
                }
                b"BANK" => {
                    has_bankswitching = true;
                    for (i, slot) in bank_init.iter_mut().enumerate() {
                        *slot = data.get(i).copied().unwrap_or(0);
                    }
                }
                b"rate" => {
                    if data.len() >= 2 {
                        let r = u16::from_le_bytes([data[0], data[1]]);
                        if r > 0 {
                            ntsc_speed_us = r;
                        }
                    }
                    if data.len() >= 4 {
                        let r = u16::from_le_bytes([data[2], data[3]]);
                        if r > 0 {
                            pal_speed_us = r;
                        }
                    }
                }
                b"plst" => {
                    plst = Some(data.iter().map(|&b| b as u16).collect());
                }
                b"tlbl" => {
                    tlbl = split_nul_strings(data)
                        .into_iter()
                        .map(|s| if s.is_empty() { None } else { Some(s) })
                        .collect();
                }
                b"time" => {
                    time_ms = parse_i32_ms_list(data);
                }
                b"fade" => {
                    fade_ms = parse_i32_ms_list(data);
                }
                b"auth" => {
                    let strings = split_nul_strings(data);
                    if let Some(s) = strings.first() {
                        game_title = s.clone();
                    }
                    if let Some(s) = strings.get(1) {
                        artist = s.clone();
                    }
                    if let Some(s) = strings.get(2) {
                        copyright = s.clone();
                    }
                    if let Some(s) = strings.get(3) {
                        if !s.is_empty() {
                            ripper = Some(s.clone());
                        }
                    }
                }
                _ => {
                    // Ignore unknown optional chunks; fail only on mandatory uppercase chunks per NSFE spec?
                    // Tolerating unknown chunks maximizes real-world compatibility.
                }
            }
        }

        if !info_seen {
            return Err(NsfError::MissingChunk("INFO"));
        }
        let (payload_crc32, rom_banks, initial_prg_ram, linear_bank_map) = if build_banks {
            let prg_data = prg_slice.ok_or(NsfError::MissingChunk("DATA"))?;
            if prg_data.is_empty() {
                return Err(NsfError::InvalidHeader("NSFE DATA chunk is empty"));
            }
            let crc = crc32fast::hash(prg_data);
            let (banks, ram, map) =
                build_rom_banks(prg_data, load_addr, has_bankswitching, &bank_init);
            (crc, banks, ram, map)
        } else {
            let crc = prg_slice
                .filter(|d| !d.is_empty())
                .map_or(0, crc32fast::hash);
            (crc, Vec::new(), None, [UNMAPPED_BANK; 8])
        };

        Ok(Self {
            total_subtracks,
            starting_subtrack,
            init_addr,
            play_addr,
            game_title,
            artist,
            copyright,
            ripper,
            ntsc_speed_us,
            pal_speed_us,
            is_pal,
            chip_flags,
            payload_crc32,
            rom_banks,
            initial_prg_ram,
            linear_bank_map,
            plst,
            tlbl,
            time_ms,
            fade_ms,
        })
    }

    fn parse_metadata_chunks(&mut self, tail: &[u8]) -> Result<(), NsfError> {
        let mut pos = 0usize;
        while pos + 8 <= tail.len() {
            let chunk_len =
                u32::from_le_bytes([tail[pos], tail[pos + 1], tail[pos + 2], tail[pos + 3]])
                    as usize;
            let chunk_id = &tail[pos + 4..pos + 8];
            pos += 8;
            if chunk_id == b"NEND" {
                break;
            }
            let Some(end) = pos.checked_add(chunk_len) else {
                break;
            };
            if end > tail.len() {
                break;
            }
            let data = &tail[pos..end];
            pos = end;

            match chunk_id {
                b"plst" => {
                    self.plst = Some(data.iter().map(|&b| b as u16).collect());
                }
                b"tlbl" => {
                    self.tlbl = split_nul_strings(data)
                        .into_iter()
                        .map(|s| if s.is_empty() { None } else { Some(s) })
                        .collect();
                }
                b"time" => {
                    self.time_ms = parse_i32_ms_list(data);
                }
                b"fade" => {
                    self.fade_ms = parse_i32_ms_list(data);
                }
                b"auth" => {
                    let strings = split_nul_strings(data);
                    if let Some(s) = strings.first().filter(|s| !s.is_empty()) {
                        self.game_title = s.clone();
                    }
                    if let Some(s) = strings.get(1).filter(|s| !s.is_empty()) {
                        self.artist = s.clone();
                    }
                    if let Some(s) = strings.get(2).filter(|s| !s.is_empty()) {
                        self.copyright = s.clone();
                    }
                    if let Some(s) = strings.get(3).filter(|s| !s.is_empty()) {
                        self.ripper = Some(s.clone());
                    }
                }
                _ => {}
            }
        }
        Ok(())
    }
}

type RomBankLayout = (Vec<[u8; 4096]>, Option<Box<[u8; 8192]>>, [u16; 8]);

fn build_rom_banks(
    prg_data: &[u8],
    load_addr: u16,
    has_bankswitching: bool,
    bank_init: &[u8; 8],
) -> RomBankLayout {
    if has_bankswitching {
        let pad = (load_addr & 0x0FFF) as usize;
        let total_len = pad + prg_data.len();
        let num_banks = total_len.div_ceil(4096).max(1);
        let mut banks = vec![[0u8; 4096]; num_banks];
        for (i, &b) in prg_data.iter().enumerate() {
            let addr = pad + i;
            banks[addr >> 12][addr & 0x0FFF] = b;
        }
        let mut map = [UNMAPPED_BANK; 8];
        for (slot, &b_idx) in bank_init.iter().enumerate() {
            if (b_idx as usize) < banks.len() {
                map[slot] = b_idx as u16;
            }
        }
        (banks, None, map)
    } else {
        // Non-bankswitched flat address image starting at `load_addr` ($6000..$FFFF)
        let mut prg_ram: Option<Box<[u8; 8192]>> = None;
        if load_addr < 0x8000 {
            let mut ram_box = Box::new([0u8; 8192]);
            let ram_start = (load_addr - 0x6000) as usize;
            let copy_len = prg_data.len().min(8192usize.saturating_sub(ram_start));
            ram_box[ram_start..ram_start + copy_len].copy_from_slice(&prg_data[..copy_len]);
            prg_ram = Some(ram_box);
        }

        let rom_slice_offset = 0x8000usize.saturating_sub(load_addr as usize);
        let rom_load_addr = load_addr.max(0x8000);
        let rom_data = if rom_slice_offset < prg_data.len() {
            &prg_data[rom_slice_offset..]
        } else {
            &[][..]
        };

        let pad = (rom_load_addr & 0x0FFF) as usize;
        let total_len = pad + rom_data.len();
        let num_banks = total_len.div_ceil(4096).max(1);
        let mut banks = vec![[0u8; 4096]; num_banks];
        for (i, &b) in rom_data.iter().enumerate() {
            let addr = pad + i;
            banks[addr >> 12][addr & 0x0FFF] = b;
        }

        let first_slot = ((rom_load_addr - 0x8000) >> 12) as usize;
        let mut map = [UNMAPPED_BANK; 8];
        for b in 0..banks.len() {
            let slot = first_slot + b;
            if slot < 8 && !rom_data.is_empty() {
                map[slot] = b as u16;
            }
        }
        (banks, prg_ram, map)
    }
}

fn split_nul_strings(data: &[u8]) -> Vec<String> {
    let mut out = Vec::new();
    for raw in data.split(|&b| b == 0) {
        out.push(decode_nsf_text_unterminated(raw));
    }
    // Remove trailing artifact after final '\0'
    if out.len() > 1 && data.last() == Some(&0) {
        out.pop();
    }
    out
}

fn parse_i32_ms_list(data: &[u8]) -> Vec<Option<u32>> {
    let mut out = Vec::with_capacity(data.len() / 4);
    let mut i = 0usize;
    while i + 4 <= data.len() {
        let val = i32::from_le_bytes([data[i], data[i + 1], data[i + 2], data[i + 3]]);
        if val >= 0 {
            out.push(Some(val as u32));
        } else {
            out.push(None);
        }
        i += 4;
    }
    out
}

/// Decode a NUL-padded NSF string field with UTF-8, Shift-JIS, and Windows-1252 tolerance.
pub(crate) fn decode_nsf_text(bytes: &[u8]) -> String {
    let slice = match bytes.iter().position(|&b| b == 0) {
        Some(nul) => &bytes[..nul],
        None => bytes,
    };
    decode_nsf_text_unterminated(slice)
}

fn decode_nsf_text_unterminated(slice: &[u8]) -> String {
    if slice.is_empty() {
        return String::new();
    }
    if let Ok(utf8) = std::str::from_utf8(slice) {
        return utf8.trim().to_string();
    }
    if looks_like_shift_jis(slice) {
        return decode_shift_jis_lossy(slice).trim().to_string();
    }
    decode_windows_1252(slice).trim().to_string()
}

fn looks_like_shift_jis(bytes: &[u8]) -> bool {
    let mut i = 0usize;
    let mut sjis_pairs = 0usize;
    let mut half_kana = 0usize;

    while i < bytes.len() {
        let b = bytes[i];
        if b < 0x80 {
            i += 1;
            continue;
        }
        if (0xA1..=0xDF).contains(&b) {
            // Isolated 0xA9 ('©') before space/digits is Western copyright, not half-width kana
            if b == 0xA9 && bytes.get(i + 1).is_none_or(|&n| n < 0x80) && half_kana == 0 {
                return false;
            }
            half_kana += 1;
            i += 1;
            continue;
        }
        if (0x81..=0x9F).contains(&b) || (0xE0..=0xEF).contains(&b) {
            let Some(&trail) = bytes.get(i + 1) else {
                return false;
            };
            if !((0x40..=0x7E).contains(&trail) || (0x80..=0xFC).contains(&trail)) {
                return false;
            }
            // Guard accented Latin-1 lowercase words like "Pokémon" (0xE9 'm')
            if (0xE0..=0xEF).contains(&b)
                && trail.is_ascii_lowercase()
                && i > 0
                && bytes[i - 1].is_ascii_alphabetic()
                && sjis_pairs == 0
            {
                return false;
            }
            sjis_pairs += 1;
            i += 2;
            continue;
        }
        return false;
    }

    sjis_pairs > 0 || half_kana >= 2
}

fn decode_shift_jis_lossy(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len());
    let mut i = 0usize;
    while i < bytes.len() {
        let b = bytes[i];
        if b < 0x80 {
            out.push(b as char);
            i += 1;
        } else if (0xA1..=0xDF).contains(&b) {
            if let Some(ch) = char::from_u32(0xFF61 + (b - 0xA1) as u32) {
                out.push(ch);
            }
            i += 1;
        } else if ((0x81..=0x9F).contains(&b) || (0xE0..=0xEF).contains(&b))
            && i + 1 < bytes.len()
        {
            let t = bytes[i + 1];
            out.push(decode_sjis_double(b, t));
            i += 2;
        } else {
            out.push(decode_cp1252_byte(b));
            i += 1;
        }
    }
    out
}

fn decode_sjis_double(lead: u8, trail: u8) -> char {
    match (lead, trail) {
        // Common JIS X 0208 punctuation (`0x81xx`)
        (0x81, 0x40) => '\u{3000}', // Ideographic space
        (0x81, 0x41) => '、',
        (0x81, 0x42) => '。',
        (0x81, 0x45) => '・',
        (0x81, 0x46) => '：',
        (0x81, 0x5B) => 'ー', // Katakana-Hiragana prolonged sound mark
        (0x81, 0x69) => '（',
        (0x81, 0x6A) => '）',
        (0x81, 0x75) => '「',
        (0x81, 0x76) => '」',
        // Full-width digits `0x824F..=0x8258`
        (0x82, 0x4F..=0x58) => char::from_u32(0xFF10 + (trail - 0x4F) as u32).unwrap_or('?'),
        // Full-width uppercase Latin `0x8260..=0x8279`
        (0x82, 0x60..=0x79) => char::from_u32(0xFF21 + (trail - 0x60) as u32).unwrap_or('?'),
        // Full-width lowercase Latin `0x8281..=0x829A`
        (0x82, 0x81..=0x9A) => char::from_u32(0xFF41 + (trail - 0x81) as u32).unwrap_or('?'),
        // Hiragana `0x829F..=0x82F1` -> `U+3041..=U+3093`
        (0x82, 0x9F..=0xF1) => char::from_u32(0x3041 + (trail - 0x9F) as u32).unwrap_or('?'),
        // Katakana `0x8340..=0x8396` (skipping 0x7F gap) -> `U+30A1..=U+30F6`
        (0x83, 0x40..=0x7E) => char::from_u32(0x30A1 + (trail - 0x40) as u32).unwrap_or('?'),
        (0x83, 0x80..=0x96) => char::from_u32(0x30A1 + (trail - 0x41) as u32).unwrap_or('?'),
        // Fallback replacement marker for unmapped JIS Kanji (never panics or fails)
        _ => '\u{30FB}',
    }
}

fn decode_windows_1252(bytes: &[u8]) -> String {
    bytes.iter().map(|&b| decode_cp1252_byte(b)).collect()
}

fn decode_cp1252_byte(b: u8) -> char {
    match b {
        0x80 => '€',
        0x85 => '…',
        0x91 => '‘',
        0x92 => '’',
        0x93 => '“',
        0x94 => '”',
        0x96 => '–',
        0x97 => '—',
        0x99 => '™',
        _ => b as char,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_utf8_latin1_and_shift_jis_titles() {
        // UTF-8
        assert_eq!(decode_nsf_text("悪魔城伝説\0\0".as_bytes()), "悪魔城伝説");
        // Windows-1252 copyright & accents
        assert_eq!(
            decode_nsf_text(b"\xA9 1990 Chris H\xFClsbeck\0"),
            "© 1990 Chris Hülsbeck"
        );
        assert_eq!(decode_nsf_text(b"Pok\xE9mon\0"), "Pokémon");
        // Shift-JIS full-width Katakana "ロックマン2"
        let sjis_rockman = b"\x83\x8D\x83\x62\x83\x4E\x83\x7D\x83\x932\0";
        assert_eq!(decode_nsf_text(sjis_rockman), "ロックマン2");
        // Shift-JIS half-width Katakana "ﾛｯｸﾏﾝ"
        let half_rockman = b"\xDB\xAF\xB8\xCF\xDD\0";
        assert_eq!(decode_nsf_text(half_rockman), "ﾛｯｸﾏﾝ");
    }
}
