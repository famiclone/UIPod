//! Persistent Binary Book Index Cache (`reader_cache/<hash64>.bidx`) for `< 20 ms` warm re-opens
//! across `.epub`, `.fb2`, `.fb2.zip`, `.mobi`/`.prc`/`.azw`, `.md`, and `.txt`.
//!
//! Header layout (`40` bytes, little-endian):
//! - `[0..4]`:   Magic `"UIBK"`
//! - `[4..6]`:   `SCHEMA_VERSION: u16`
//! - `[6..8]`:   `flags: u16`
//! - `[8..16]`:  `content_key: u64` (`fnv1a64` of `(canonical_path, file_size, mtime_sec)`)
//! - `[16..24]`: `file_size: u64`
//! - `[24..32]`: `mtime_sec: u64`
//! - `[32..36]`: `payload_len: u32`
//! - `[36..40]`: `payload_crc32: u32` (IEEE CRC32 over `[40 .. 40 + payload_len]`)

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use crate::layout::{ReaderFont, ReadingMode};
use crate::{BlockKind, DocBlock, DocFormat, FootnoteDef, InlineRun, InlineStyle};

pub const BIDX_MAGIC: [u8; 4] = *b"UIBK";
pub const BIDX_SCHEMA_VERSION: u16 = 1;
pub const BIDX_HEADER_BYTES: usize = 40;
/// Cap pre-parsed cached chapter payload inside `.bidx` at `512 KiB` so `.bidx` reads stay `< 1 ms`.
const MAX_CACHED_CHAPTER_BLOCKS: usize = 6_000;
const MAX_BIDX_PAYLOAD_BYTES: usize = 8 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BidxError {
    Io(String),
    InvalidMagic,
    SchemaMismatch { found: u16, expected: u16 },
    StaleFile,
    ChecksumMismatch,
    TruncatedOrCorrupt,
}

/// Compression / slice storage method recorded in [`SpineChapterMeta::method`].
pub mod chapter_method {
    pub const ZIP_STORED: u16 = 0;
    pub const ZIP_DEFLATE: u16 = 8;
    pub const FB2_RAW_SLICE: u16 = 100;
    pub const FB2_ZIP_INNER_SLICE: u16 = 101;
    pub const MOBI_UNCOMP_RECORDS: u16 = 200;
    pub const MOBI_LZ77_RECORDS: u16 = 201;
    pub const PLAIN_RAW_SLICE: u16 = 300;
}

/// Image payload addressing mode recorded in [`IndexImageEntry::method`].
pub mod image_method {
    pub const ZIP_STORED: u16 = 0;
    pub const ZIP_DEFLATE: u16 = 8;
    pub const FB2_BASE64_SLICE: u16 = 16;
    pub const FB2_ZIP_BASE64_SLICE: u16 = 17;
    pub const RAW_FILE_SLICE: u16 = 24;
    pub const INLINE_DATA_URI: u16 = 32;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpineChapterMeta {
    pub path_or_id: String,
    pub byte_offset: u64,
    pub compressed_size: u32,
    pub uncompressed_size: u32,
    pub method: u16,
    /// Format-specific auxiliary `u16` (e.g. `bit 0 = prepend_cover_image`, or MOBI `end_rec` delta).
    pub aux_u16: u16,
    /// Format-specific auxiliary `u32` (e.g. `.fb2.zip` inner entry index or MOBI `first_rec_skip`).
    pub aux_u32: u32,
    pub char_count: u32,
    pub cum_char_offset: u64,
    pub verified: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IndexTocEntry {
    pub title: String,
    pub level: u8,
    pub target_chapter_idx: u32,
    pub target_char_offset: u32,
    pub fragment: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IndexImageEntry {
    pub id_or_path: String,
    pub id_hash: u64,
    pub byte_offset: u64,
    pub compressed_size: u32,
    pub uncompressed_size: u32,
    pub method: u16,
    pub width: u32,
    pub height: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SavedReadingAnchor {
    pub has_anchor: bool,
    pub chapter_idx: u32,
    pub char_offset: u32,
    pub font_family: ReaderFont,
    pub size_idx: u8,
    pub spacing_idx: u8,
    pub reading_mode: ReadingMode,
}

impl Default for SavedReadingAnchor {
    fn default() -> Self {
        Self {
            has_anchor: false,
            chapter_idx: 0,
            char_offset: 0,
            font_family: ReaderFont::Sans,
            size_idx: 1,
            spacing_idx: 1,
            reading_mode: ReadingMode::Paginated,
        }
    }
}

#[derive(Debug, Clone)]
pub struct CachedChapterPayload {
    pub chapter_idx: u32,
    pub total_chars: u32,
    pub blocks: Vec<DocBlock>,
}

#[derive(Debug, Clone)]
pub struct BookIndex {
    pub title: String,
    pub author: String,
    pub format: DocFormat,
    /// Detected `TextEncoding` discriminant (`0 = Utf8, 1 = Utf16Le, 2 = Utf16Be, 3 = Win1251, 4 = Koi8U, 5 = Koi8R, 6 = Win1252`).
    pub detected_encoding: u8,
    pub mobi_extra_flags: u16,
    pub chapters: Vec<SpineChapterMeta>,
    pub toc: Vec<IndexTocEntry>,
    pub images: Vec<IndexImageEntry>,
    pub footnotes: Vec<FootnoteDef>,
    pub footnote_keys: Vec<String>,
    pub saved_anchor: SavedReadingAnchor,
    pub cached_chapter: Option<CachedChapterPayload>,
}

impl BookIndex {
    pub fn recompute_cumulative_offsets(&mut self) {
        let mut acc = 0u64;
        for ch in &mut self.chapters {
            ch.char_count = ch.char_count.max(1);
            ch.cum_char_offset = acc;
            acc = acc.saturating_add(ch.char_count as u64);
        }
    }

    pub fn total_book_chars(&self) -> u64 {
        self.chapters
            .last()
            .map(|c| c.cum_char_offset.saturating_add(c.char_count.max(1) as u64))
            .unwrap_or(1)
            .max(1)
    }

    pub fn all_chapters_verified(&self) -> bool {
        self.chapters.iter().all(|c| c.verified)
    }

    /// Maps a legacy flat book-wide character offset into `(chapter_idx, local_char_offset)`.
    pub fn map_global_offset(&self, global_off: u64) -> (usize, usize) {
        if self.chapters.is_empty() {
            return (0, 0);
        }
        let idx = match self
            .chapters
            .binary_search_by_key(&global_off, |c| c.cum_char_offset)
        {
            Ok(i) => i,
            Err(ins) => ins.saturating_sub(1),
        }
        .min(self.chapters.len() - 1);
        let ch = &self.chapters[idx];
        let local = global_off
            .saturating_sub(ch.cum_char_offset)
            .min(ch.char_count as u64) as usize;
        (idx, local)
    }
}

/// Computes deterministic 64-bit `fnv1a` path hash used for `reader_cache/<path_hash>.bidx`
/// (so updating a book at the same path overwrites its old `.bidx` cleanly without orphan files).
pub fn bidx_path_hash64(canonical_path: &str) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in canonical_path.trim().bytes() {
        h ^= b as u64;
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    h
}

/// Computes 64-bit `fnv1a` content fingerprint over `(canonical_path, file_size, mtime_sec)`.
pub fn bidx_content_key64(canonical_path: &str, file_size: u64, mtime_sec: u64) -> u64 {
    let mut h = bidx_path_hash64(canonical_path);
    for &b in &file_size.to_le_bytes() {
        h ^= b as u64;
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    for &b in &mtime_sec.to_le_bytes() {
        h ^= b as u64;
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    h
}

pub fn bidx_cache_file_path(cache_root: &Path, canonical_path: &str) -> PathBuf {
    let h = bidx_path_hash64(canonical_path);
    cache_root.join("reader_cache").join(format!("{h:016x}.bidx"))
}

pub fn crc32_ieee(bytes: &[u8]) -> u32 {
    const TABLE: [u32; 256] = {
        let mut t = [0u32; 256];
        let mut i = 0;
        while i < 256 {
            let mut c = i as u32;
            let mut k = 0;
            while k < 8 {
                if (c & 1) != 0 {
                    c = 0xEDB8_8320 ^ (c >> 1);
                } else {
                    c >>= 1;
                }
                k += 1;
            }
            t[i] = c;
            i += 1;
        }
        t
    };
    let mut crc = 0xFFFF_FFFFu32;
    for &b in bytes {
        let idx = ((crc ^ (b as u32)) & 0xFF) as usize;
        crc = (crc >> 8) ^ TABLE[idx];
    }
    !crc
}

/// Serializes `BookIndex` into the compact binary `.bidx` wire format (`UIBK` header + CRC32).
pub fn serialize_bidx(
    index: &BookIndex,
    canonical_path: &str,
    file_size: u64,
    mtime_sec: u64,
) -> Vec<u8> {
    let mut payload = Vec::with_capacity(4096);

    write_str(&mut payload, &index.title);
    write_str(&mut payload, &index.author);
    payload.push(encode_format(index.format));
    payload.push(index.detected_encoding);
    write_u16(&mut payload, index.mobi_extra_flags);

    // 1. Spine / Chapter Table
    write_u32(&mut payload, index.chapters.len() as u32);
    for ch in &index.chapters {
        write_str(&mut payload, &ch.path_or_id);
        write_u64(&mut payload, ch.byte_offset);
        write_u32(&mut payload, ch.compressed_size);
        write_u32(&mut payload, ch.uncompressed_size);
        write_u16(&mut payload, ch.method);
        write_u16(&mut payload, ch.aux_u16);
        write_u32(&mut payload, ch.aux_u32);
        write_u32(&mut payload, ch.char_count);
        write_u64(&mut payload, ch.cum_char_offset);
        payload.push(u8::from(ch.verified));
    }

    // 2. Pre-resolved TOC
    write_u32(&mut payload, index.toc.len() as u32);
    for t in &index.toc {
        write_str(&mut payload, &t.title);
        payload.push(t.level);
        write_u32(&mut payload, t.target_chapter_idx);
        write_u32(&mut payload, t.target_char_offset);
        write_str(&mut payload, &t.fragment);
    }

    // 3. Pre-resolved Image Manifest
    write_u32(&mut payload, index.images.len() as u32);
    for img in &index.images {
        write_str(&mut payload, &img.id_or_path);
        write_u64(&mut payload, img.id_hash);
        write_u64(&mut payload, img.byte_offset);
        write_u32(&mut payload, img.compressed_size);
        write_u32(&mut payload, img.uncompressed_size);
        write_u16(&mut payload, img.method);
        write_u32(&mut payload, img.width);
        write_u32(&mut payload, img.height);
    }

    // 4. Pre-resolved Footnote Manifest
    let fn_len = index.footnotes.len();
    write_u32(&mut payload, fn_len as u32);
    for (i, f) in index.footnotes.iter().enumerate() {
        let key = index.footnote_keys.get(i).map(|s| s.as_str()).unwrap_or("");
        write_str(&mut payload, key);
        write_str(&mut payload, &f.label);
        write_str(&mut payload, &f.title);
        write_u32(&mut payload, f.paragraphs.len() as u32);
        for p in &f.paragraphs {
            write_str(&mut payload, p);
        }
    }

    // 5. Saved Reading Anchor
    let a = &index.saved_anchor;
    payload.push(u8::from(a.has_anchor));
    write_u32(&mut payload, a.chapter_idx);
    write_u32(&mut payload, a.char_offset);
    payload.push(a.font_family as u8);
    payload.push(a.size_idx);
    payload.push(a.spacing_idx);
    payload.push(a.reading_mode as u8);

    // 6. Optional Pre-Parsed Active Chapter Token/Block Stream
    let mut flags: u16 = if index.all_chapters_verified() { 1 } else { 0 };
    if let Some(ref cc) = index.cached_chapter {
        if cc.blocks.len() <= MAX_CACHED_CHAPTER_BLOCKS {
            flags |= 2;
            payload.push(1);
            write_u32(&mut payload, cc.chapter_idx);
            write_u32(&mut payload, cc.total_chars);
            write_u32(&mut payload, cc.blocks.len() as u32);
            for blk in &cc.blocks {
                write_u32(&mut payload, blk.start_offset as u32);
                write_u32(&mut payload, blk.end_offset as u32);
                match &blk.kind {
                    BlockKind::Spacer => payload.push(0),
                    BlockKind::PageBreak => payload.push(1),
                    BlockKind::Image { image_idx, caption } => {
                        payload.push(2);
                        write_u32(&mut payload, *image_idx as u32);
                        write_str(&mut payload, caption);
                    }
                    BlockKind::Paragraph {
                        runs,
                        heading_level,
                        is_quote,
                        is_code,
                        centered,
                    } => {
                        payload.push(3);
                        payload.push(*heading_level);
                        let pflags = u8::from(*is_quote)
                            | (u8::from(*is_code) << 1)
                            | (u8::from(*centered) << 2);
                        payload.push(pflags);
                        write_u32(&mut payload, runs.len() as u32);
                        for r in runs {
                            payload.push(encode_inline_style(r.style));
                            match r.footnote_idx {
                                Some(fi) => write_u32(&mut payload, (fi as u32).saturating_add(1)),
                                None => write_u32(&mut payload, 0),
                            }
                            write_str(&mut payload, &r.text);
                        }
                    }
                }
            }
        } else {
            payload.push(0);
        }
    } else {
        payload.push(0);
    }

    let content_key = bidx_content_key64(canonical_path, file_size, mtime_sec);
    let crc = crc32_ieee(&payload);
    let mut out = Vec::with_capacity(BIDX_HEADER_BYTES + payload.len());
    out.extend_from_slice(&BIDX_MAGIC);
    write_u16(&mut out, BIDX_SCHEMA_VERSION);
    write_u16(&mut out, flags);
    write_u64(&mut out, content_key);
    write_u64(&mut out, file_size);
    write_u64(&mut out, mtime_sec);
    write_u32(&mut out, payload.len() as u32);
    write_u32(&mut out, crc);
    out.extend_from_slice(&payload);
    out
}

/// Validates header (`magic`, `SCHEMA_VERSION`, `file_size`, `mtime_sec`, `content_key`, `CRC32`)
/// and deserializes `BookIndex` in a single sequential pass (`< 1 ms`).
pub fn deserialize_bidx(
    bytes: &[u8],
    canonical_path: &str,
    expected_size: u64,
    expected_mtime: u64,
) -> Result<BookIndex, BidxError> {
    if bytes.len() < BIDX_HEADER_BYTES {
        return Err(BidxError::TruncatedOrCorrupt);
    }
    if bytes[0..4] != BIDX_MAGIC {
        return Err(BidxError::InvalidMagic);
    }
    let mut hdr_cur = 4usize;
    let schema = read_u16(bytes, &mut hdr_cur)?;
    if schema != BIDX_SCHEMA_VERSION {
        return Err(BidxError::SchemaMismatch {
            found: schema,
            expected: BIDX_SCHEMA_VERSION,
        });
    }
    let _flags = read_u16(bytes, &mut hdr_cur)?;
    let stored_key = read_u64(bytes, &mut hdr_cur)?;
    let stored_size = read_u64(bytes, &mut hdr_cur)?;
    let stored_mtime = read_u64(bytes, &mut hdr_cur)?;
    let payload_len = read_u32(bytes, &mut hdr_cur)? as usize;
    let stored_crc = read_u32(bytes, &mut hdr_cur)?;

    let expected_key = bidx_content_key64(canonical_path, expected_size, expected_mtime);
    if stored_size != expected_size || stored_mtime != expected_mtime || stored_key != expected_key
    {
        return Err(BidxError::StaleFile);
    }
    if payload_len > MAX_BIDX_PAYLOAD_BYTES || bytes.len() < BIDX_HEADER_BYTES + payload_len {
        return Err(BidxError::TruncatedOrCorrupt);
    }
    let payload = &bytes[BIDX_HEADER_BYTES..BIDX_HEADER_BYTES + payload_len];
    if crc32_ieee(payload) != stored_crc {
        return Err(BidxError::ChecksumMismatch);
    }

    let mut cur = 0usize;
    let title = read_str(payload, &mut cur)?;
    let author = read_str(payload, &mut cur)?;
    let format = decode_format(read_u8(payload, &mut cur)?)?;
    let detected_encoding = read_u8(payload, &mut cur)?;
    let mobi_extra_flags = read_u16(payload, &mut cur)?;

    let ch_count = read_u32(payload, &mut cur)? as usize;
    if ch_count > 65_536 {
        return Err(BidxError::TruncatedOrCorrupt);
    }
    let mut chapters = Vec::with_capacity(ch_count.min(1024));
    for _ in 0..ch_count {
        chapters.push(SpineChapterMeta {
            path_or_id: read_str(payload, &mut cur)?,
            byte_offset: read_u64(payload, &mut cur)?,
            compressed_size: read_u32(payload, &mut cur)?,
            uncompressed_size: read_u32(payload, &mut cur)?,
            method: read_u16(payload, &mut cur)?,
            aux_u16: read_u16(payload, &mut cur)?,
            aux_u32: read_u32(payload, &mut cur)?,
            char_count: read_u32(payload, &mut cur)?,
            cum_char_offset: read_u64(payload, &mut cur)?,
            verified: read_u8(payload, &mut cur)? != 0,
        });
    }

    let toc_count = read_u32(payload, &mut cur)? as usize;
    if toc_count > 65_536 {
        return Err(BidxError::TruncatedOrCorrupt);
    }
    let mut toc = Vec::with_capacity(toc_count.min(1024));
    for _ in 0..toc_count {
        toc.push(IndexTocEntry {
            title: read_str(payload, &mut cur)?,
            level: read_u8(payload, &mut cur)?,
            target_chapter_idx: read_u32(payload, &mut cur)?,
            target_char_offset: read_u32(payload, &mut cur)?,
            fragment: read_str(payload, &mut cur)?,
        });
    }

    let img_count = read_u32(payload, &mut cur)? as usize;
    if img_count > 65_536 {
        return Err(BidxError::TruncatedOrCorrupt);
    }
    let mut images = Vec::with_capacity(img_count.min(512));
    for _ in 0..img_count {
        images.push(IndexImageEntry {
            id_or_path: read_str(payload, &mut cur)?,
            id_hash: read_u64(payload, &mut cur)?,
            byte_offset: read_u64(payload, &mut cur)?,
            compressed_size: read_u32(payload, &mut cur)?,
            uncompressed_size: read_u32(payload, &mut cur)?,
            method: read_u16(payload, &mut cur)?,
            width: read_u32(payload, &mut cur)?,
            height: read_u32(payload, &mut cur)?,
        });
    }

    let fn_count = read_u32(payload, &mut cur)? as usize;
    if fn_count > 65_536 {
        return Err(BidxError::TruncatedOrCorrupt);
    }
    let mut footnotes = Vec::with_capacity(fn_count.min(512));
    let mut footnote_keys = Vec::with_capacity(fn_count.min(512));
    for index in 0..fn_count {
        let key = read_str(payload, &mut cur)?;
        let label = read_str(payload, &mut cur)?;
        let fn_title = read_str(payload, &mut cur)?;
        let p_cnt = read_u32(payload, &mut cur)? as usize;
        if p_cnt > 4096 {
            return Err(BidxError::TruncatedOrCorrupt);
        }
        let mut paragraphs = Vec::with_capacity(p_cnt.min(32));
        for _ in 0..p_cnt {
            paragraphs.push(read_str(payload, &mut cur)?);
        }
        footnote_keys.push(key);
        footnotes.push(FootnoteDef {
            index,
            label,
            title: fn_title,
            paragraphs,
        });
    }

    let saved_anchor = SavedReadingAnchor {
        has_anchor: read_u8(payload, &mut cur)? != 0,
        chapter_idx: read_u32(payload, &mut cur)?,
        char_offset: read_u32(payload, &mut cur)?,
        font_family: ReaderFont::from_u8(read_u8(payload, &mut cur)?),
        size_idx: read_u8(payload, &mut cur)?,
        spacing_idx: read_u8(payload, &mut cur)?,
        reading_mode: ReadingMode::from_u8(read_u8(payload, &mut cur)?),
    };

    let has_cc = read_u8(payload, &mut cur)? != 0;
    let cached_chapter = if has_cc {
        let chapter_idx = read_u32(payload, &mut cur)?;
        let total_chars = read_u32(payload, &mut cur)?;
        let blk_cnt = read_u32(payload, &mut cur)? as usize;
        if blk_cnt > MAX_CACHED_CHAPTER_BLOCKS * 2 {
            return Err(BidxError::TruncatedOrCorrupt);
        }
        let mut blocks = Vec::with_capacity(blk_cnt.min(1024));
        for _ in 0..blk_cnt {
            let start_offset = read_u32(payload, &mut cur)? as usize;
            let end_offset = read_u32(payload, &mut cur)? as usize;
            let tag = read_u8(payload, &mut cur)?;
            let kind = match tag {
                0 => BlockKind::Spacer,
                1 => BlockKind::PageBreak,
                2 => {
                    let image_idx = read_u32(payload, &mut cur)? as usize;
                    let caption = read_str(payload, &mut cur)?;
                    BlockKind::Image { image_idx, caption }
                }
                3 => {
                    let heading_level = read_u8(payload, &mut cur)?;
                    let pf = read_u8(payload, &mut cur)?;
                    let run_cnt = read_u32(payload, &mut cur)? as usize;
                    if run_cnt > 16_384 {
                        return Err(BidxError::TruncatedOrCorrupt);
                    }
                    let mut runs = Vec::with_capacity(run_cnt.min(64));
                    for _ in 0..run_cnt {
                        let style = decode_inline_style(read_u8(payload, &mut cur)?)?;
                        let fn_raw = read_u32(payload, &mut cur)?;
                        let footnote_idx = if fn_raw == 0 {
                            None
                        } else {
                            Some((fn_raw - 1) as usize)
                        };
                        let text = read_str(payload, &mut cur)?;
                        runs.push(InlineRun {
                            text,
                            style,
                            footnote_idx,
                        });
                    }
                    BlockKind::Paragraph {
                        runs,
                        heading_level,
                        is_quote: (pf & 1) != 0,
                        is_code: (pf & 2) != 0,
                        centered: (pf & 4) != 0,
                    }
                }
                _ => return Err(BidxError::TruncatedOrCorrupt),
            };
            blocks.push(DocBlock {
                start_offset,
                end_offset,
                kind,
            });
        }
        Some(CachedChapterPayload {
            chapter_idx,
            total_chars,
            blocks,
        })
    } else {
        None
    };

    Ok(BookIndex {
        title,
        author,
        format,
        detected_encoding,
        mobi_extra_flags,
        chapters,
        toc,
        images,
        footnotes,
        footnote_keys,
        saved_anchor,
        cached_chapter,
    })
}

/// Loads and validates `reader_cache/<hash64>.bidx` from `cache_root` in a single sequential read.
pub fn load_bidx_from_disk(
    cache_root: &Path,
    canonical_path: &str,
    expected_size: u64,
    expected_mtime: u64,
) -> Result<BookIndex, BidxError> {
    let p = bidx_cache_file_path(cache_root, canonical_path);
    let raw = fs::read(&p).map_err(|e| BidxError::Io(e.to_string()))?;
    deserialize_bidx(&raw, canonical_path, expected_size, expected_mtime)
}

/// Atomically writes `reader_cache/<hash64>.tmp` and `rename`s it to `reader_cache/<hash64>.bidx`
/// so sudden power loss on TrimUI Brick never leaves a half-written cache file.
pub fn save_bidx_atomic(
    cache_root: &Path,
    canonical_path: &str,
    file_size: u64,
    mtime_sec: u64,
    index: &BookIndex,
) -> Result<PathBuf, BidxError> {
    let dir = cache_root.join("reader_cache");
    fs::create_dir_all(&dir).map_err(|e| BidxError::Io(e.to_string()))?;
    let h = bidx_path_hash64(canonical_path);
    let tmp_path = dir.join(format!("{h:016x}.tmp"));
    let final_path = dir.join(format!("{h:016x}.bidx"));
    let encoded = serialize_bidx(index, canonical_path, file_size, mtime_sec);

    {
        let mut f = fs::File::create(&tmp_path).map_err(|e| BidxError::Io(e.to_string()))?;
        f.write_all(&encoded)
            .map_err(|e| BidxError::Io(e.to_string()))?;
        let _ = f.flush();
    }
    fs::rename(&tmp_path, &final_path).map_err(|e| {
        let _ = fs::remove_file(&tmp_path);
        BidxError::Io(e.to_string())
    })?;
    Ok(final_path)
}

#[inline]
fn encode_format(f: DocFormat) -> u8 {
    match f {
        DocFormat::Epub => 0,
        DocFormat::Fb2 => 1,
        DocFormat::Mobi => 2,
        DocFormat::Markdown => 3,
        DocFormat::PlainText => 4,
    }
}

#[inline]
fn decode_format(v: u8) -> Result<DocFormat, BidxError> {
    match v {
        0 => Ok(DocFormat::Epub),
        1 => Ok(DocFormat::Fb2),
        2 => Ok(DocFormat::Mobi),
        3 => Ok(DocFormat::Markdown),
        4 => Ok(DocFormat::PlainText),
        _ => Err(BidxError::TruncatedOrCorrupt),
    }
}

#[inline]
fn encode_inline_style(s: InlineStyle) -> u8 {
    match s {
        InlineStyle::Regular => 0,
        InlineStyle::Bold => 1,
        InlineStyle::Italic => 2,
        InlineStyle::BoldItalic => 3,
        InlineStyle::Code => 4,
        InlineStyle::FootnoteRef => 5,
    }
}

#[inline]
fn decode_inline_style(v: u8) -> Result<InlineStyle, BidxError> {
    match v {
        0 => Ok(InlineStyle::Regular),
        1 => Ok(InlineStyle::Bold),
        2 => Ok(InlineStyle::Italic),
        3 => Ok(InlineStyle::BoldItalic),
        4 => Ok(InlineStyle::Code),
        5 => Ok(InlineStyle::FootnoteRef),
        _ => Err(BidxError::TruncatedOrCorrupt),
    }
}

#[inline]
fn write_u16(out: &mut Vec<u8>, v: u16) {
    out.extend_from_slice(&v.to_le_bytes());
}

#[inline]
fn write_u32(out: &mut Vec<u8>, v: u32) {
    out.extend_from_slice(&v.to_le_bytes());
}

#[inline]
fn write_u64(out: &mut Vec<u8>, v: u64) {
    out.extend_from_slice(&v.to_le_bytes());
}

#[inline]
fn write_str(out: &mut Vec<u8>, s: &str) {
    let b = s.as_bytes();
    write_u32(out, b.len() as u32);
    out.extend_from_slice(b);
}

#[inline]
fn read_u8(src: &[u8], cur: &mut usize) -> Result<u8, BidxError> {
    let b = *src.get(*cur).ok_or(BidxError::TruncatedOrCorrupt)?;
    *cur += 1;
    Ok(b)
}

#[inline]
fn read_u16(src: &[u8], cur: &mut usize) -> Result<u16, BidxError> {
    let sl = src
        .get(*cur..*cur + 2)
        .ok_or(BidxError::TruncatedOrCorrupt)?;
    *cur += 2;
    Ok(u16::from_le_bytes([sl[0], sl[1]]))
}

#[inline]
fn read_u32(src: &[u8], cur: &mut usize) -> Result<u32, BidxError> {
    let sl = src
        .get(*cur..*cur + 4)
        .ok_or(BidxError::TruncatedOrCorrupt)?;
    *cur += 4;
    Ok(u32::from_le_bytes([sl[0], sl[1], sl[2], sl[3]]))
}

#[inline]
fn read_u64(src: &[u8], cur: &mut usize) -> Result<u64, BidxError> {
    let sl = src
        .get(*cur..*cur + 8)
        .ok_or(BidxError::TruncatedOrCorrupt)?;
    *cur += 8;
    Ok(u64::from_le_bytes([
        sl[0], sl[1], sl[2], sl[3], sl[4], sl[5], sl[6], sl[7],
    ]))
}

#[inline]
fn read_str(src: &[u8], cur: &mut usize) -> Result<String, BidxError> {
    let len = read_u32(src, cur)? as usize;
    if len > MAX_BIDX_PAYLOAD_BYTES {
        return Err(BidxError::TruncatedOrCorrupt);
    }
    let sl = src
        .get(*cur..*cur + len)
        .ok_or(BidxError::TruncatedOrCorrupt)?;
    *cur += len;
    std::str::from_utf8(sl)
        .map(|s| s.to_string())
        .map_err(|_| BidxError::TruncatedOrCorrupt)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_book_index() -> BookIndex {
        let mut idx = BookIndex {
            title: "Кобзар".into(),
            author: "Тарас Шевченко".into(),
            format: DocFormat::Epub,
            detected_encoding: 0,
            mobi_extra_flags: 0,
            chapters: vec![
                SpineChapterMeta {
                    path_or_id: "OEBPS/Text/ch1.xhtml".into(),
                    byte_offset: 512,
                    compressed_size: 2048,
                    uncompressed_size: 6144,
                    method: chapter_method::ZIP_DEFLATE,
                    aux_u16: 1,
                    aux_u32: 0,
                    char_count: 3820,
                    cum_char_offset: 0,
                    verified: true,
                },
                SpineChapterMeta {
                    path_or_id: "OEBPS/Text/ch2.xhtml".into(),
                    byte_offset: 2640,
                    compressed_size: 4096,
                    uncompressed_size: 11200,
                    method: chapter_method::ZIP_DEFLATE,
                    aux_u16: 0,
                    aux_u32: 0,
                    char_count: 7100,
                    cum_char_offset: 3820,
                    verified: false,
                },
            ],
            toc: vec![
                IndexTocEntry {
                    title: "Причинна".into(),
                    level: 0,
                    target_chapter_idx: 0,
                    target_char_offset: 0,
                    fragment: String::new(),
                },
                IndexTocEntry {
                    title: "Катерина".into(),
                    level: 0,
                    target_chapter_idx: 1,
                    target_char_offset: 42,
                    fragment: "sec2".into(),
                },
            ],
            images: vec![IndexImageEntry {
                id_or_path: "OEBPS/Images/cover.jpg".into(),
                id_hash: 0xDEAD_BEEF_CAFE_1234,
                byte_offset: 9800,
                compressed_size: 48_000,
                uncompressed_size: 48_000,
                method: image_method::ZIP_STORED,
                width: 600,
                height: 800,
            }],
            footnotes: vec![FootnoteDef {
                index: 0,
                label: "[1]".into(),
                title: "[1]".into(),
                paragraphs: vec!["Перше видання 1840 року в Санкт-Петербурзі.".into()],
            }],
            footnote_keys: vec!["OEBPS/Text/notes.xhtml#fn1".into()],
            saved_anchor: SavedReadingAnchor {
                has_anchor: true,
                chapter_idx: 1,
                char_offset: 240,
                font_family: ReaderFont::Serif,
                size_idx: 2,
                spacing_idx: 1,
                reading_mode: ReadingMode::Paginated,
            },
            cached_chapter: Some(CachedChapterPayload {
                chapter_idx: 1,
                total_chars: 120,
                blocks: vec![
                    DocBlock {
                        start_offset: 0,
                        end_offset: 8,
                        kind: BlockKind::Paragraph {
                            runs: vec![InlineRun {
                                text: "Катерина".into(),
                                style: InlineStyle::Bold,
                                footnote_idx: None,
                            }],
                            heading_level: 1,
                            is_quote: false,
                            is_code: false,
                            centered: false,
                        },
                    },
                    DocBlock {
                        start_offset: 8,
                        end_offset: 9,
                        kind: BlockKind::Image {
                            image_idx: 0,
                            caption: "Ілюстрація".into(),
                        },
                    },
                    DocBlock {
                        start_offset: 9,
                        end_offset: 32,
                        kind: BlockKind::Paragraph {
                            runs: vec![
                                InlineRun {
                                    text: "Кохайтеся, чорнобриві".into(),
                                    style: InlineStyle::Italic,
                                    footnote_idx: None,
                                },
                                InlineRun {
                                    text: "[1]".into(),
                                    style: InlineStyle::FootnoteRef,
                                    footnote_idx: Some(0),
                                },
                            ],
                            heading_level: 0,
                            is_quote: true,
                            is_code: false,
                            centered: false,
                        },
                    },
                ],
            }),
        };
        idx.recompute_cumulative_offsets();
        idx
    }

    #[test]
    fn bidx_roundtrip_schema_mismatch_size_mtime_and_crc_invalidation() {
        let orig = sample_book_index();
        let path = "/mnt/SDCARD/Books/kobzar.epub";
        let size = 524_288u64;
        let mtime = 1_764_000_123u64;

        let tmp_root = std::env::temp_dir().join(format!("uipod_bidx_test_{}", std::process::id()));
        let _ = fs::remove_dir_all(&tmp_root);
        let written = save_bidx_atomic(&tmp_root, path, size, mtime, &orig).expect("atomic save");
        assert!(written.exists());
        assert!(!tmp_root
            .join("reader_cache")
            .join(format!("{:016x}.tmp", bidx_path_hash64(path)))
            .exists());

        // 1. Exact warm roundtrip
        let loaded = load_bidx_from_disk(&tmp_root, path, size, mtime).expect("warm load");
        assert_eq!(loaded.title, "Кобзар");
        assert_eq!(loaded.author, "Тарас Шевченко");
        assert_eq!(loaded.chapters, orig.chapters);
        assert_eq!(loaded.toc, orig.toc);
        assert_eq!(loaded.images, orig.images);
        assert_eq!(loaded.footnotes.len(), 1);
        assert_eq!(loaded.footnotes[0].paragraphs, orig.footnotes[0].paragraphs);
        assert_eq!(loaded.saved_anchor, orig.saved_anchor);
        let cc = loaded.cached_chapter.expect("cached active chapter");
        assert_eq!(cc.chapter_idx, 1);
        assert_eq!(cc.blocks.len(), 3);

        // 2. File size or mtime change invalidates automatically
        assert_eq!(
            load_bidx_from_disk(&tmp_root, path, size + 1, mtime).unwrap_err(),
            BidxError::StaleFile
        );
        assert_eq!(
            load_bidx_from_disk(&tmp_root, path, size, mtime + 1).unwrap_err(),
            BidxError::StaleFile
        );

        // 3. Schema version mismatch invalidates cleanly without panicking
        let mut raw = fs::read(&written).unwrap();
        let bumped_ver = BIDX_SCHEMA_VERSION + 1;
        raw[4..6].copy_from_slice(&bumped_ver.to_le_bytes());
        assert_eq!(
            deserialize_bidx(&raw, path, size, mtime).unwrap_err(),
            BidxError::SchemaMismatch {
                found: bumped_ver,
                expected: BIDX_SCHEMA_VERSION,
            }
        );

        // 4. Payload corruption caught by IEEE CRC32
        let mut corrupted = fs::read(&written).unwrap();
        let last = corrupted.len() - 1;
        corrupted[last] ^= 0x5A;
        assert_eq!(
            deserialize_bidx(&corrupted, path, size, mtime).unwrap_err(),
            BidxError::ChecksumMismatch
        );

        // 5. Bad magic rejected
        let mut bad_magic = fs::read(&written).unwrap();
        bad_magic[0] = b'X';
        assert_eq!(
            deserialize_bidx(&bad_magic, path, size, mtime).unwrap_err(),
            BidxError::InvalidMagic
        );

        let _ = fs::remove_dir_all(&tmp_root);
    }
}
