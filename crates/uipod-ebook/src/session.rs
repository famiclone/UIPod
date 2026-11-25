//! Lazy Spine-Chapter E-Book Session (`BookSession`), O(1) `FontMetricsTable`-driven active-chapter
//! layout, character-weighted + background-refined global `READ / TOTAL` pagination, lazy image
//! decoding, and background OS worker (`BookBackgroundWorker`) with atomic `.bidx` persistence.

use std::borrow::Cow;
use std::collections::HashMap;
use std::fs::{self, File};
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::Instant;

use crate::bidx::{
    chapter_method, deserialize_bidx, image_method, load_bidx_from_disk, save_bidx_atomic,
    serialize_bidx, BookIndex, CachedChapterPayload, IndexImageEntry, IndexTocEntry,
    SavedReadingAnchor, SpineChapterMeta,
};
use crate::encoding::{decode_base64, decode_bytes_auto, decode_entities, detect_xml_encoding, TextEncoding};
use crate::epub::{
    format_footnote_badge, harvest_footnote_candidates, is_block_tag, is_epub_drm_encrypted_bytes,
    parse_epub2_ncx_toc, parse_epub3_nav_toc, push_merged_run, resolve_style, RawTocItem,
};
use crate::font_metrics::FontMetricsTable;
use crate::image_dither::probe_image_dimensions;
#[cfg(not(target_arch = "wasm32"))]
use crate::image_dither::{decode_illustration_gray_capped, DecodedGrayImage};
use crate::layout::{
    count_blocks_pages_fast, CachedDitherEntry, DitheredImageCache, ProgressDisplay, ReaderPrefs,
    ReaderViewState, ReadingMode, READER_FONT_SIZES,
};
use crate::mobi::{
    decompress_palmdoc_lz77, extract_mobi_footnote_at, looks_like_mobi_html,
    strip_mobi_trailing_data,
};
use crate::xml_html::{collapse_inline_ws, get_attr, MarkupToken, MarkupTokenizer};
use crate::zip_archive::{
    normalize_zip_path, read_zip_entry_direct, read_zip_entry_prefix_direct,
    resolve_relative_href, FileZipArchive, ZipArchive, ZipEntryMeta,
};
#[cfg(not(target_arch = "wasm32"))]
use crate::BlockKind;
use crate::{
    clean_stem_title, plain_md, DocBlock, DocBuilder, DocFormat, DocImage, EbookDoc,
    EbookError, FootnoteDef, InlineRun, InlineStyle,
};

const PLAIN_CHAPTER_TARGET_BYTES: usize = 24 * 1024;
const MOBI_CHAPTER_TARGET_BYTES: usize = 24 * 1024;

#[derive(Debug, Clone, Copy)]
pub enum BookmarkHint {
    None,
    LegacyGlobal(usize),
    ChapterLocal {
        chapter_idx: usize,
        char_offset: usize,
    },
}

impl BookmarkHint {
    pub fn parse(raw: &str) -> Self {
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            return Self::None;
        }
        let mut it = trimmed.split_whitespace();
        let Some(first) = it.next() else {
            return Self::None;
        };
        if first == "v2" {
            let ch = it.next().and_then(|s| s.parse::<usize>().ok()).unwrap_or(0);
            let off = it.next().and_then(|s| s.parse::<usize>().ok()).unwrap_or(0);
            Self::ChapterLocal {
                chapter_idx: ch,
                char_offset: off,
            }
        } else if let Ok(legacy) = first.parse::<usize>() {
            Self::LegacyGlobal(legacy)
        } else {
            Self::None
        }
    }

    pub fn format_v2(chapter_idx: usize, char_offset: usize) -> String {
        format!("v2 {chapter_idx} {char_offset}\n")
    }
}

#[derive(Debug, Clone, Copy, Default)]
pub struct OpenTimings {
    pub used_warm_bidx: bool,
    pub index_ms: f64,
    pub chapter_parse_ms: f64,
    pub total_open_ms: f64,
}

#[derive(Clone)]
pub struct ParsedChapter {
    pub chapter_idx: usize,
    pub doc: EbookDoc,
    pub resolved_fragments: Vec<(String, u32)>,
    pub fallback_headings: Vec<(String, u8, u32)>,
}

enum SessionBacking {
    File {
        canonical_path: String,
        path: PathBuf,
        file_size: u64,
        mtime_sec: u64,
        fb2_zip_buf: Option<Vec<u8>>,
        mobi_html_buf: Option<String>,
        inline_data_images: HashMap<usize, Vec<u8>>,
    },
    Memory {
        canonical_key: String,
        file_name: String,
        bytes: Vec<u8>,
        fb2_zip_buf: Option<Vec<u8>>,
        mobi_html_buf: Option<String>,
        inline_data_images: HashMap<usize, Vec<u8>>,
    },
    SingleDoc(EbookDoc),
}

type LayoutSig = (u32, u32, usize, usize, bool, u8);

/// Unified multi-chapter lazy e-book session (`O(1)` cold/warm open & `< 5 ms` font resize).
pub struct BookSession {
    pub index: BookIndex,
    backing: SessionBacking,
    pub active_chapter: usize,
    pub active_doc: EbookDoc,
    pub view: ReaderViewState,
    pub metrics: FontMetricsTable,
    pub open_timings: OpenTimings,

    /// Small bounded chapter cache (`active_chapter` + prefetched `active_chapter ± 1`, max 4 chapters).
    chapter_cache: Vec<ParsedChapter>,
    /// Cached external XHTML footnote snippets (`(file_path, elem_id) -> paragraphs`).
    ext_note_cache: HashMap<(String, String), Vec<String>>,
    ext_scanned_files: Vec<String>,

    /// Per-chapter exact page counts for `active_layout_sig` (`None` -> char-weighted estimate).
    layout_sig: Option<LayoutSig>,
    exact_chapter_pages: Vec<Option<u32>>,
    pub index_dirty: bool,
}

impl BookSession {
    /// Opens an e-book or text file from disk:
    /// - **Warm Path (`< 20 ms`):** `stat()` -> `reader_cache/<hash64>.bidx` -> cached active chapter (or direct-seek slice).
    /// - **Cold Path (`< 150 ms`):** lazy spine/section index + parses ONLY `active_chapter`.
    pub fn open_file(
        path: &Path,
        cache_root: Option<&Path>,
        bmark: BookmarkHint,
    ) -> Result<Self, EbookError> {
        let t_total = Instant::now();
        let meta = fs::metadata(path)
            .map_err(|e| EbookError::InvalidFormat(format!("Cannot stat file: {e}")))?;
        let file_size = meta.len();
        let mtime_sec = meta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let canonical_path = fs::canonicalize(path)
            .unwrap_or_else(|_| path.to_path_buf())
            .to_string_lossy()
            .into_owned();

        // 1. Try Warm `.bidx` Path first!
        if let Some(croot) = cache_root {
            let t_idx = Instant::now();
            if let Ok(loaded_idx) = load_bidx_from_disk(croot, &canonical_path, file_size, mtime_sec)
            {
                let index_ms = t_idx.elapsed().as_secs_f64() * 1000.0;
                let (target_ch, target_off) = resolve_initial_spot(&loaded_idx, bmark);
                let mut sess = Self::init_empty_from_index(
                    loaded_idx,
                    SessionBacking::File {
                        canonical_path,
                        path: path.to_path_buf(),
                        file_size,
                        mtime_sec,
                        fb2_zip_buf: None,
                        mobi_html_buf: None,
                        inline_data_images: HashMap::new(),
                    },
                );
                let t_ch = Instant::now();
                sess.activate_chapter(target_ch, target_off)?;
                let ch_ms = t_ch.elapsed().as_secs_f64() * 1000.0;
                sess.open_timings = OpenTimings {
                    used_warm_bidx: true,
                    index_ms,
                    chapter_parse_ms: ch_ms,
                    total_open_ms: t_total.elapsed().as_secs_f64() * 1000.0,
                };
                return Ok(sess);
            }
        }

        // 2. Cold Open Path (`O(1)` in untouched chapters/images)
        let t_idx = Instant::now();
        let file_name = path
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("Document");
        let (built_index, fb2_zip_buf, mobi_html_buf) =
            build_cold_index_from_file(path, file_name, file_size)?;
        let index_ms = t_idx.elapsed().as_secs_f64() * 1000.0;

        let (target_ch, target_off) = resolve_initial_spot(&built_index, bmark);
        let mut sess = Self::init_empty_from_index(
            built_index,
            SessionBacking::File {
                canonical_path,
                path: path.to_path_buf(),
                file_size,
                mtime_sec,
                fb2_zip_buf,
                mobi_html_buf,
                inline_data_images: HashMap::new(),
            },
        );
        let t_ch = Instant::now();
        sess.activate_chapter(target_ch, target_off)?;
        let ch_ms = t_ch.elapsed().as_secs_f64() * 1000.0;
        sess.index_dirty = true;
        sess.open_timings = OpenTimings {
            used_warm_bidx: false,
            index_ms,
            chapter_parse_ms: ch_ms,
            total_open_ms: t_total.elapsed().as_secs_f64() * 1000.0,
        };
        Ok(sess)
    }

    /// Opens an in-memory buffer (`WASM` simulator, bundled guide, unit tests) with optional
    /// serialized `.bidx` cache bytes for `< 2 ms` warm re-opens in WebAssembly.
    pub fn open_bytes(
        file_name: &str,
        bytes: Vec<u8>,
        cached_bidx: Option<&[u8]>,
        bmark: BookmarkHint,
    ) -> Result<Self, EbookError> {
        let t_total = Instant::now();
        let file_size = bytes.len() as u64;
        let canonical_key = format!("mem://{file_name}");

        if let Some(raw_bidx) = cached_bidx {
            let t_idx = Instant::now();
            if let Ok(loaded_idx) = deserialize_bidx(raw_bidx, &canonical_key, file_size, 0) {
                let index_ms = t_idx.elapsed().as_secs_f64() * 1000.0;
                let (target_ch, target_off) = resolve_initial_spot(&loaded_idx, bmark);
                let mut sess = Self::init_empty_from_index(
                    loaded_idx,
                    SessionBacking::Memory {
                        canonical_key,
                        file_name: file_name.to_string(),
                        bytes,
                        fb2_zip_buf: None,
                        mobi_html_buf: None,
                        inline_data_images: HashMap::new(),
                    },
                );
                let t_ch = Instant::now();
                sess.activate_chapter(target_ch, target_off)?;
                let ch_ms = t_ch.elapsed().as_secs_f64() * 1000.0;
                sess.open_timings = OpenTimings {
                    used_warm_bidx: true,
                    index_ms,
                    chapter_parse_ms: ch_ms,
                    total_open_ms: t_total.elapsed().as_secs_f64() * 1000.0,
                };
                return Ok(sess);
            }
        }

        let t_idx = Instant::now();
        let (built_index, fb2_zip_buf, mobi_html_buf) =
            build_cold_index_from_bytes(file_name, &bytes)?;
        let index_ms = t_idx.elapsed().as_secs_f64() * 1000.0;

        let (target_ch, target_off) = resolve_initial_spot(&built_index, bmark);
        let mut sess = Self::init_empty_from_index(
            built_index,
            SessionBacking::Memory {
                canonical_key,
                file_name: file_name.to_string(),
                bytes,
                fb2_zip_buf,
                mobi_html_buf,
                inline_data_images: HashMap::new(),
            },
        );
        let t_ch = Instant::now();
        sess.activate_chapter(target_ch, target_off)?;
        let ch_ms = t_ch.elapsed().as_secs_f64() * 1000.0;
        sess.index_dirty = true;
        sess.open_timings = OpenTimings {
            used_warm_bidx: false,
            index_ms,
            chapter_parse_ms: ch_ms,
            total_open_ms: t_total.elapsed().as_secs_f64() * 1000.0,
        };
        Ok(sess)
    }

    /// Wraps an already-constructed single-chapter [`EbookDoc`] (e.g. `error_document`).
    pub fn from_single_doc(doc: EbookDoc, initial_offset: usize) -> Self {
        let total_chars = doc.total_chars.max(1) as u32;
        let toc: Vec<IndexTocEntry> = doc
            .toc
            .iter()
            .map(|t| IndexTocEntry {
                title: t.title.clone(),
                level: t.level,
                target_chapter_idx: 0,
                target_char_offset: t.offset as u32,
                fragment: String::new(),
            })
            .collect();
        let images: Vec<IndexImageEntry> = doc
            .images
            .iter()
            .enumerate()
            .map(|(i, im)| IndexImageEntry {
                id_or_path: format!("inline:{i}"),
                id_hash: im.id_hash,
                byte_offset: 0,
                compressed_size: im.raw_bytes.len() as u32,
                uncompressed_size: im.raw_bytes.len() as u32,
                method: image_method::INLINE_DATA_URI,
                width: im.width,
                height: im.height,
            })
            .collect();
        let index = BookIndex {
            title: doc.title.clone(),
            author: doc.author.clone(),
            format: doc.format,
            detected_encoding: 0,
            mobi_extra_flags: 0,
            chapters: vec![SpineChapterMeta {
                path_or_id: "doc".into(),
                byte_offset: 0,
                compressed_size: 0,
                uncompressed_size: total_chars,
                method: chapter_method::PLAIN_RAW_SLICE,
                aux_u16: 0,
                aux_u32: 0,
                char_count: total_chars,
                cum_char_offset: 0,
                verified: true,
            }],
            toc,
            images,
            footnotes: doc.footnotes.clone(),
            footnote_keys: Vec::new(),
            saved_anchor: SavedReadingAnchor::default(),
            cached_chapter: None,
        };
        let view = ReaderViewState::new(initial_offset);
        Self {
            index,
            backing: SessionBacking::SingleDoc(doc.clone()),
            active_chapter: 0,
            active_doc: doc,
            view,
            metrics: FontMetricsTable::default(),
            open_timings: OpenTimings::default(),
            chapter_cache: Vec::new(),
            ext_note_cache: HashMap::new(),
            ext_scanned_files: Vec::new(),
            layout_sig: None,
            exact_chapter_pages: vec![None],
            index_dirty: false,
        }
    }

    fn init_empty_from_index(index: BookIndex, backing: SessionBacking) -> Self {
        let ch_len = index.chapters.len().max(1);
        let placeholder = EbookDoc {
            title: index.title.clone(),
            author: index.author.clone(),
            format: index.format,
            blocks: Vec::new(),
            images: Vec::new(),
            footnotes: Vec::new(),
            toc: Vec::new(),
            total_chars: 1,
        };
        Self {
            index,
            backing,
            active_chapter: 0,
            active_doc: placeholder,
            view: ReaderViewState::new(0),
            metrics: FontMetricsTable::default(),
            open_timings: OpenTimings::default(),
            chapter_cache: Vec::with_capacity(4),
            ext_note_cache: HashMap::new(),
            ext_scanned_files: Vec::new(),
            layout_sig: None,
            exact_chapter_pages: vec![None; ch_len],
            index_dirty: false,
        }
    }

    #[inline]
    pub fn title(&self) -> &str {
        &self.index.title
    }

    pub fn set_title(&mut self, title: String) {
        self.index.title = title.clone();
        self.active_doc.title = title;
    }

    #[inline]
    pub fn chapter_count(&self) -> usize {
        self.index.chapters.len().max(1)
    }

    #[inline]
    pub fn toc(&self) -> &[IndexTocEntry] {
        &self.index.toc
    }

    #[inline]
    pub fn footnote(&self, fn_idx: usize) -> Option<&FootnoteDef> {
        self.index
            .footnotes
            .get(fn_idx)
            .or_else(|| self.active_doc.footnotes.get(fn_idx))
    }

    /// Global cumulative offset across chapters (for compatibility with single-`usize` bookmark maps).
    pub fn global_anchor_offset(&self) -> usize {
        let base = self
            .index
            .chapters
            .get(self.active_chapter)
            .map(|c| c.cum_char_offset as usize)
            .unwrap_or(0);
        base.saturating_add(self.view.anchor_offset)
    }

    pub fn bookmark_string(&mut self, mode: ReadingMode) -> String {
        self.view.refresh_anchor_from_mode(mode);
        BookmarkHint::format_v2(self.active_chapter, self.view.anchor_offset)
    }

    /// Updates `saved_anchor` + `cached_chapter` in `self.index` and returns serialized `.bidx` bytes.
    pub fn export_bidx_bytes(&mut self, prefs: &ReaderPrefs) -> Option<Vec<u8>> {
        self.snapshot_anchor_into_index(prefs);
        match &self.backing {
            SessionBacking::File {
                canonical_path,
                file_size,
                mtime_sec,
                ..
            } => Some(serialize_bidx(
                &self.index,
                canonical_path,
                *file_size,
                *mtime_sec,
            )),
            SessionBacking::Memory {
                canonical_key,
                bytes,
                ..
            } => Some(serialize_bidx(
                &self.index,
                canonical_key,
                bytes.len() as u64,
                0,
            )),
            SessionBacking::SingleDoc(_) => None,
        }
    }

    /// Atomically persists `reader_cache/<hash64>.bidx` (`<hash64>.tmp` -> `rename`).
    pub fn save_bidx(&mut self, cache_root: &Path, prefs: &ReaderPrefs) -> bool {
        self.snapshot_anchor_into_index(prefs);
        if let SessionBacking::File {
            ref canonical_path,
            file_size,
            mtime_sec,
            ..
        } = self.backing
        {
            if save_bidx_atomic(cache_root, canonical_path, file_size, mtime_sec, &self.index)
                .is_ok()
            {
                self.index_dirty = false;
                return true;
            }
        }
        false
    }

    pub fn snapshot_anchor_into_index(&mut self, prefs: &ReaderPrefs) {
        self.view.refresh_anchor_from_mode(prefs.mode);
        self.index.saved_anchor = SavedReadingAnchor {
            has_anchor: true,
            chapter_idx: self.active_chapter as u32,
            char_offset: self.view.anchor_offset as u32,
            font_family: prefs.font,
            size_idx: prefs.size_idx as u8,
            spacing_idx: prefs.spacing_idx as u8,
            reading_mode: prefs.mode,
        };
        if self.active_doc.blocks.len() <= 6_000 {
            self.index.cached_chapter = Some(CachedChapterPayload {
                chapter_idx: self.active_chapter as u32,
                total_chars: self.active_doc.total_chars as u32,
                blocks: self.active_doc.blocks.clone(),
            });
        }
    }

    pub fn file_backing_info(&self) -> Option<(PathBuf, String, u64, u64)> {
        match &self.backing {
            SessionBacking::File {
                path,
                canonical_path,
                file_size,
                mtime_sec,
                ..
            } => Some((path.clone(), canonical_path.clone(), *file_size, *mtime_sec)),
            _ => None,
        }
    }

    /// Switches to `chapter_idx` (using `.bidx`'s `cached_chapter`, prefetch cache, or direct-seek slice)
    /// and positions `self.view` at `local_offset`.
    pub fn activate_chapter(
        &mut self,
        chapter_idx: usize,
        local_offset: usize,
    ) -> Result<(), EbookError> {
        let clamped_ch = chapter_idx.min(self.chapter_count().saturating_sub(1));

        // 1. Check `.bidx` warm `cached_chapter` first!
        if let Some(ref cc) = self.index.cached_chapter {
            if cc.chapter_idx as usize == clamped_ch && !cc.blocks.is_empty() {
                let doc = self.build_chapter_ebook_doc(cc.blocks.clone(), cc.total_chars as usize);
                self.install_active_doc(clamped_ch, doc, local_offset);
                return Ok(());
            }
        }

        // 2. Check in-session prefetched chapter cache (`active ± 1`)
        if let Some(pos) = self
            .chapter_cache
            .iter()
            .position(|c| c.chapter_idx == clamped_ch)
        {
            let parsed = self.chapter_cache[pos].clone();
            self.apply_parsed_chapter_metadata(&parsed);
            let doc = self.rebuild_doc_image_dims(parsed.doc);
            self.install_active_doc(clamped_ch, doc, local_offset);
            return Ok(());
        }

        // 3. Parse single chapter directly from backing store
        let parsed = self.parse_single_chapter(clamped_ch)?;
        self.apply_parsed_chapter_metadata(&parsed);
        self.cache_parsed_chapter(parsed.clone());
        let doc = self.rebuild_doc_image_dims(parsed.doc);
        self.install_active_doc(clamped_ch, doc, local_offset);
        Ok(())
    }

    fn install_active_doc(&mut self, chapter_idx: usize, doc: EbookDoc, local_offset: usize) {
        self.active_chapter = chapter_idx;
        self.active_doc = doc;
        self.view.anchor_offset = local_offset;
        self.view.open_footnote_idx = None;
        self.view.footnote_scroll_line = 0;
        self.view.invalidate_layout();
    }

    pub fn cache_parsed_chapter(&mut self, parsed: ParsedChapter) {
        if let Some(pos) = self
            .chapter_cache
            .iter()
            .position(|c| c.chapter_idx == parsed.chapter_idx)
        {
            self.chapter_cache.remove(pos);
        }
        if self.chapter_cache.len() >= 4 {
            self.chapter_cache.remove(0);
        }
        self.chapter_cache.push(parsed);
    }

    pub fn apply_parsed_chapter_metadata(&mut self, parsed: &ParsedChapter) {
        let ch_idx = parsed.chapter_idx;
        if let Some(ch_meta) = self.index.chapters.get_mut(ch_idx) {
            let exact_chars = parsed.doc.total_chars.max(1) as u32;
            if !ch_meta.verified || ch_meta.char_count != exact_chars {
                ch_meta.char_count = exact_chars;
                ch_meta.verified = true;
                self.index.recompute_cumulative_offsets();
                self.index_dirty = true;
            }
        }

        // Resolve any `#fragment` TOC entries targeting this chapter!
        for t in &mut self.index.toc {
            if t.target_chapter_idx as usize == ch_idx && !t.fragment.is_empty() {
                if let Some(&(_, off)) = parsed
                    .resolved_fragments
                    .iter()
                    .find(|(f, _)| f == &t.fragment)
                {
                    t.target_char_offset = off;
                    t.fragment.clear();
                    self.index_dirty = true;
                }
            }
        }

        // Populate fallback heading TOC if book had no explicit `nav`/`ncx` TOC
        if self.index.toc.is_empty() && !parsed.fallback_headings.is_empty() {
            for &(ref htitle, lvl, off) in &parsed.fallback_headings {
                self.index.toc.push(IndexTocEntry {
                    title: htitle.clone(),
                    level: lvl,
                    target_chapter_idx: ch_idx as u32,
                    target_char_offset: off,
                    fragment: String::new(),
                });
            }
            self.index_dirty = true;
        }
    }

    fn build_chapter_ebook_doc(&self, blocks: Vec<DocBlock>, total_chars: usize) -> EbookDoc {
        if let SessionBacking::SingleDoc(ref d) = self.backing {
            return d.clone();
        }
        let images: Vec<DocImage> = self
            .index
            .images
            .iter()
            .map(|im| DocImage {
                id_hash: im.id_hash,
                width: im.width.max(1),
                height: im.height.max(1),
                raw_bytes: Vec::new(),
            })
            .collect();
        EbookDoc {
            title: self.index.title.clone(),
            author: self.index.author.clone(),
            format: self.index.format,
            blocks,
            images,
            footnotes: self.index.footnotes.clone(),
            toc: Vec::new(),
            total_chars: total_chars.max(1),
        }
    }

    fn rebuild_doc_image_dims(&self, mut doc: EbookDoc) -> EbookDoc {
        if matches!(self.backing, SessionBacking::SingleDoc(_)) {
            return doc;
        }
        doc.images = self
            .index
            .images
            .iter()
            .map(|im| DocImage {
                id_hash: im.id_hash,
                width: im.width.max(1),
                height: im.height.max(1),
                raw_bytes: Vec::new(),
            })
            .collect();
        doc.footnotes = self.index.footnotes.clone();
        doc
    }

    /// Lays out **only the active chapter** (`< 3 ms`) using the lock-free `O(1)` [`FontMetricsTable`].
    pub fn ensure_layout(&mut self, prefs: &ReaderPrefs, screen_w: f32, screen_h: f32) -> bool {
        let sig: LayoutSig = (
            screen_w.round() as u32,
            screen_h.round() as u32,
            prefs.size_idx,
            prefs.spacing_idx,
            prefs.hide_ui,
            prefs.font as u8,
        );
        let sig_changed = self.layout_sig != Some(sig);
        if sig_changed {
            self.layout_sig = Some(sig);
            self.exact_chapter_pages.fill(None);
            if self.exact_chapter_pages.len() != self.chapter_count() {
                self.exact_chapter_pages = vec![None; self.chapter_count()];
            }
        }

        let base_fsz = READER_FONT_SIZES[prefs.size_idx];
        self.metrics.reconfigure(prefs.font, base_fsz);

        let metrics_ref = &mut self.metrics;
        self.view
            .ensure_layout(&self.active_doc, prefs, screen_w, screen_h, |txt, sz, wt| {
                metrics_ref.measure_text(txt, sz, wt)
            });

        if let Some(slot) = self.exact_chapter_pages.get_mut(self.active_chapter) {
            *slot = Some(self.view.pages.len().max(1) as u32);
        }

        // Also refine any already-cached neighbor chapters (`active ± 1`) in `< 0.3 ms`!
        if sig_changed && !self.chapter_cache.is_empty() {
            let img_dims: Vec<(u32, u32)> = self
                .index
                .images
                .iter()
                .map(|im| (im.width.max(1), im.height.max(1)))
                .collect();
            for cached in &self.chapter_cache {
                if cached.chapter_idx != self.active_chapter {
                    if let Some(slot) = self.exact_chapter_pages.get_mut(cached.chapter_idx) {
                        let p = count_blocks_pages_fast(
                            &cached.doc.blocks,
                            &img_dims,
                            self.index.format,
                            prefs,
                            screen_w,
                            screen_h,
                            &mut self.metrics,
                        );
                        *slot = Some(p as u32);
                    }
                }
            }
        }
        sig_changed
    }

    pub fn set_exact_chapter_pages(&mut self, sig: LayoutSig, chapter_idx: usize, pages: u32) {
        if self.layout_sig == Some(sig) {
            if let Some(slot) = self.exact_chapter_pages.get_mut(chapter_idx) {
                *slot = Some(pages.max(1));
            }
        }
    }

    #[inline]
    pub fn current_layout_sig(&self) -> Option<LayoutSig> {
        self.layout_sig
    }

    /// Computes global `(progress_0_to_1, readout_label)` across all spine chapters in `O(chapters)` (`< 1 µs`),
    /// blending exact per-chapter page counts with character-weighted page estimation!
    pub fn progress_metrics(&self, prefs: &ReaderPrefs) -> (f32, String) {
        if self.chapter_count() <= 1 {
            return self.view.progress_metrics(prefs);
        }

        let mut known_chars = 0u64;
        let mut known_pages = 0u64;
        for (i, ch) in self.index.chapters.iter().enumerate() {
            if let Some(Some(p)) = self.exact_chapter_pages.get(i) {
                // Avoid skewing global density from a tiny 20-char cover/half-title page
                if ch.char_count >= 350 || self.chapter_count() <= 2 {
                    known_chars += ch.char_count as u64;
                    known_pages += (*p).max(1) as u64;
                }
            }
        }
        let base_fsz = READER_FONT_SIZES[prefs.size_idx];
        let line_h = (base_fsz * crate::READER_LINE_SPACINGS[prefs.spacing_idx]).max(20.0);
        let geom_chars_per_page = ((self.view.col_w / (base_fsz * 0.53)).max(12.0)
            * (self.view.viewport_h / line_h).max(4.0)
            * 0.78)
            .max(180.0);

        let avg_cpp = if known_pages > 0 && known_chars >= 350 {
            ((known_chars as f32) / (known_pages as f32)).clamp(120.0, 6000.0)
        } else {
            geom_chars_per_page
        };

        let mut pages_before = 0usize;
        let mut total_pages = 0usize;
        for (i, ch) in self.index.chapters.iter().enumerate() {
            let p = match self.exact_chapter_pages.get(i).copied().flatten() {
                Some(exact) => exact.max(1) as usize,
                None => ((ch.char_count as f32 / avg_cpp).round() as usize).max(1),
            };
            if i < self.active_chapter {
                pages_before += p;
            }
            total_pages += p;
        }
        total_pages = total_pages.max(1);

        match prefs.mode {
            ReadingMode::Paginated => {
                let cur_in_ch = self
                    .view
                    .current_page
                    .min(self.view.pages.len().saturating_sub(1));
                let global_zero_idx = (pages_before + cur_in_ch).min(total_pages - 1);
                let read_pages = global_zero_idx + 1;
                match prefs.progress_display {
                    ProgressDisplay::Percent => {
                        let frac = if total_pages <= 1 {
                            1.0
                        } else {
                            (global_zero_idx as f32) / ((total_pages - 1) as f32)
                        }
                        .clamp(0.0, 1.0);
                        (frac, format!("{}%", (frac * 100.0).round() as u32))
                    }
                    ProgressDisplay::Pages => {
                        let frac = (read_pages as f32 / total_pages as f32).clamp(0.0, 1.0);
                        (frac, format!("{read_pages} / {total_pages}"))
                    }
                }
            }
            ReadingMode::Scroll => {
                let max_s = self.view.max_scroll_px();
                let local_f = if max_s <= 1.0 {
                    1.0
                } else {
                    (self.view.scroll_px / max_s).clamp(0.0, 1.0)
                };
                let cur_ch_pages = self
                    .exact_chapter_pages
                    .get(self.active_chapter)
                    .copied()
                    .flatten()
                    .unwrap_or(1)
                    .max(1) as usize;
                let global_f = if total_pages <= 1 {
                    1.0
                } else {
                    let pos_p = (pages_before as f32) + local_f * ((cur_ch_pages - 1) as f32);
                    (pos_p / ((total_pages - 1) as f32)).clamp(0.0, 1.0)
                };
                match prefs.progress_display {
                    ProgressDisplay::Percent => {
                        (global_f, format!("{}%", (global_f * 100.0).round() as u32))
                    }
                    ProgressDisplay::Pages => {
                        let read_pages = (((global_f * ((total_pages - 1) as f32)).round()
                            as usize)
                            + 1)
                            .min(total_pages);
                        let frac = (read_pages as f32 / total_pages as f32).clamp(0.0, 1.0);
                        (frac, format!("{read_pages} / {total_pages}"))
                    }
                }
            }
        }
    }

    /// Steps pages forward/backward, seamlessly crossing spine chapter boundaries!
    pub fn step_page(
        &mut self,
        delta: i32,
        prefs: &ReaderPrefs,
        now_ms: u64,
        screen_w: f32,
        screen_h: f32,
    ) -> bool {
        if self.view.step_page(delta, prefs, now_ms) {
            return true;
        }
        if delta > 0 && self.active_chapter + 1 < self.chapter_count() {
            if self.activate_chapter(self.active_chapter + 1, 0).is_ok() {
                self.ensure_layout(prefs, screen_w, screen_h);
                self.view.note_forward_motion(prefs);
                return true;
            }
        } else if delta < 0 && self.active_chapter > 0 {
            if self
                .activate_chapter(self.active_chapter - 1, usize::MAX / 2)
                .is_ok()
            {
                self.ensure_layout(prefs, screen_w, screen_h);
                if !self.view.pages.is_empty() {
                    let last = self.view.pages.len() - 1;
                    self.view.current_page = last;
                    self.view.anchor_offset = self.view.pages[last].start_offset;
                    if let Some(it) = self.view.items.get(self.view.pages[last].start_item) {
                        self.view.scroll_px = it.y_scroll.clamp(0.0, self.view.max_scroll_px());
                    }
                }
                self.view.note_backward_motion(prefs, now_ms);
                return true;
            }
        }
        false
    }

    /// Scrolls smoothly in `Scroll` mode, transitioning across chapters at top/bottom edges.
    pub fn scroll_by_px(
        &mut self,
        delta_px: f32,
        prefs: &ReaderPrefs,
        now_ms: u64,
        screen_w: f32,
        screen_h: f32,
        allow_chapter_edge: bool,
    ) -> bool {
        if self.view.scroll_by_px(delta_px, prefs, now_ms) {
            return true;
        }
        if !allow_chapter_edge {
            return false;
        }
        if delta_px > 0.0 && self.active_chapter + 1 < self.chapter_count() {
            if self.activate_chapter(self.active_chapter + 1, 0).is_ok() {
                self.ensure_layout(prefs, screen_w, screen_h);
                self.view.scroll_px = 0.0;
                self.view.anchor_offset = 0;
                self.view.note_forward_motion(prefs);
                return true;
            }
        } else if delta_px < 0.0 && self.active_chapter > 0 {
            if self
                .activate_chapter(self.active_chapter - 1, usize::MAX / 2)
                .is_ok()
            {
                self.ensure_layout(prefs, screen_w, screen_h);
                self.view.scroll_px = self.view.max_scroll_px();
                self.view.refresh_anchor_from_mode(ReadingMode::Scroll);
                self.view.note_backward_motion(prefs, now_ms);
                return true;
            }
        }
        false
    }

    pub fn active_toc_index(&self) -> usize {
        if self.index.toc.is_empty() {
            return 0;
        }
        let cur_ch = self.active_chapter as u32;
        let cur_off = (self.view.anchor_offset as u32).saturating_add(2);
        let mut best = 0usize;
        for (i, entry) in self.index.toc.iter().enumerate() {
            if entry.target_chapter_idx < cur_ch
                || (entry.target_chapter_idx == cur_ch && entry.target_char_offset <= cur_off)
            {
                best = i;
            } else if entry.target_chapter_idx > cur_ch {
                break;
            }
        }
        best
    }

    pub fn toc_entry_percent(&self, toc_idx: usize) -> u32 {
        let Some(t) = self.index.toc.get(toc_idx) else {
            return 0;
        };
        let total = self.index.total_book_chars();
        let ch_base = self
            .index
            .chapters
            .get(t.target_chapter_idx as usize)
            .map(|c| c.cum_char_offset)
            .unwrap_or(0);
        (((ch_base + t.target_char_offset as u64) * 100) / total).min(100) as u32
    }

    pub fn jump_to_toc(
        &mut self,
        toc_idx: usize,
        prefs: &ReaderPrefs,
        now_ms: u64,
        screen_w: f32,
        screen_h: f32,
    ) -> bool {
        let Some(entry) = self.index.toc.get(toc_idx).cloned() else {
            return false;
        };
        let target_ch = entry.target_chapter_idx as usize;
        if target_ch != self.active_chapter {
            if self
                .activate_chapter(target_ch, entry.target_char_offset as usize)
                .is_err()
            {
                return false;
            }
        }
        // Re-read potentially newly-resolved `#fragment` offset!
        let resolved_off = self
            .index
            .toc
            .get(toc_idx)
            .map(|e| e.target_char_offset as usize)
            .unwrap_or(entry.target_char_offset as usize);
        self.ensure_layout(prefs, screen_w, screen_h);
        self.view.open_footnote_idx = None;
        self.view.sync_to_anchor(resolved_off);
        if prefs.hide_ui {
            self.view.note_backward_motion(prefs, now_ms);
        }
        true
    }

    pub fn jump_to_beginning(&mut self, prefs: &ReaderPrefs, screen_w: f32, screen_h: f32) {
        if self.active_chapter != 0 {
            let _ = self.activate_chapter(0, 0);
        }
        self.ensure_layout(prefs, screen_w, screen_h);
        self.view.open_footnote_idx = None;
        self.view.sync_to_anchor(0);
    }

    pub fn jump_to_end(&mut self, prefs: &ReaderPrefs, screen_w: f32, screen_h: f32) {
        let last_ch = self.chapter_count().saturating_sub(1);
        if self.active_chapter != last_ch {
            let _ = self.activate_chapter(last_ch, usize::MAX / 2);
        }
        self.ensure_layout(prefs, screen_w, screen_h);
        self.view.open_footnote_idx = None;
        self.view.sync_to_anchor(self.active_doc.total_chars);
    }

    /// Lazily loads, decodes (into `4 MiB` grayscale cache), and 4-shade Bayer-dithers `image_idx`
    /// only when its page is actually drawn!
    #[allow(clippy::too_many_arguments)]
    pub fn rasterize_illustration<'a>(
        &mut self,
        img_cache: &'a mut DitheredImageCache,
        image_idx: usize,
        disp_w: u32,
        disp_h: u32,
        bg_rgb: u32,
        fg_rgb: u32,
        fg1_rgb: u32,
        fg2_rgb: u32,
    ) -> Option<&'a CachedDitherEntry> {
        if let SessionBacking::SingleDoc(ref doc) = self.backing {
            return img_cache.get_or_rasterize(
                doc, image_idx, disp_w, disp_h, bg_rgb, fg_rgb, fg1_rgb, fg2_rgb,
            );
        }
        let id_hash = self.index.images.get(image_idx)?.id_hash;
        let raw_opt = if img_cache.has_decoded_gray(id_hash) {
            None
        } else {
            self.load_raw_image_bytes(image_idx)
        };
        img_cache.get_or_rasterize_lazy(
            id_hash,
            image_idx,
            disp_w,
            disp_h,
            bg_rgb,
            fg_rgb,
            fg1_rgb,
            fg2_rgb,
            || raw_opt.map(Cow::Owned),
        )
    }

    pub fn load_raw_image_bytes(&mut self, image_idx: usize) -> Option<Vec<u8>> {
        let img = self.index.images.get(image_idx)?.clone();
        match &mut self.backing {
            SessionBacking::SingleDoc(doc) => doc.images.get(image_idx).map(|i| i.raw_bytes.clone()),
            SessionBacking::File {
                path,
                file_size,
                fb2_zip_buf,
                inline_data_images,
                ..
            } => {
                if img.method == image_method::INLINE_DATA_URI {
                    return inline_data_images.get(&image_idx).cloned();
                }
                if img.method == image_method::FB2_ZIP_BASE64_SLICE {
                    if fb2_zip_buf.is_none() {
                        *fb2_zip_buf = inflate_fb2_zip_file(path).ok();
                    }
                    let buf = fb2_zip_buf.as_ref()?;
                    let s = img.byte_offset as usize;
                    let e = s.checked_add(img.compressed_size as usize)?.min(buf.len());
                    return decode_base64(&String::from_utf8_lossy(&buf[s..e]));
                }
                let mut f = File::open(path).ok()?;
                match img.method {
                    image_method::ZIP_STORED | image_method::ZIP_DEFLATE => read_zip_entry_direct(
                        &mut f,
                        *file_size,
                        img.byte_offset,
                        img.compressed_size as usize,
                        img.uncompressed_size as usize,
                        img.method,
                    )
                    .ok(),
                    image_method::FB2_BASE64_SLICE => {
                        f.seek(SeekFrom::Start(img.byte_offset)).ok()?;
                        let mut b64 = vec![0u8; (img.compressed_size as usize).min(24 * 1024 * 1024)];
                        f.read_exact(&mut b64).ok()?;
                        decode_base64(&String::from_utf8_lossy(&b64))
                    }
                    image_method::RAW_FILE_SLICE => {
                        f.seek(SeekFrom::Start(img.byte_offset)).ok()?;
                        let mut raw = vec![0u8; (img.compressed_size as usize).min(24 * 1024 * 1024)];
                        f.read_exact(&mut raw).ok()?;
                        Some(raw)
                    }
                    _ => None,
                }
            }
            SessionBacking::Memory {
                bytes,
                fb2_zip_buf,
                inline_data_images,
                ..
            } => {
                if img.method == image_method::INLINE_DATA_URI {
                    return inline_data_images.get(&image_idx).cloned();
                }
                if img.method == image_method::FB2_ZIP_BASE64_SLICE {
                    if fb2_zip_buf.is_none() {
                        *fb2_zip_buf = inflate_fb2_zip_bytes(bytes).ok();
                    }
                    let buf = fb2_zip_buf.as_ref()?;
                    let s = img.byte_offset as usize;
                    let e = s.checked_add(img.compressed_size as usize)?.min(buf.len());
                    return decode_base64(&String::from_utf8_lossy(&buf[s..e]));
                }
                match img.method {
                    image_method::ZIP_STORED | image_method::ZIP_DEFLATE => {
                        let mut cur = std::io::Cursor::new(bytes.as_slice());
                        read_zip_entry_direct(
                            &mut cur,
                            bytes.len() as u64,
                            img.byte_offset,
                            img.compressed_size as usize,
                            img.uncompressed_size as usize,
                            img.method,
                        )
                        .ok()
                    }
                    image_method::FB2_BASE64_SLICE => {
                        let s = img.byte_offset as usize;
                        let e = s.checked_add(img.compressed_size as usize)?.min(bytes.len());
                        decode_base64(&String::from_utf8_lossy(&bytes[s..e]))
                    }
                    image_method::RAW_FILE_SLICE => {
                        let s = img.byte_offset as usize;
                        let e = s.checked_add(img.compressed_size as usize)?.min(bytes.len());
                        Some(bytes[s..e].to_vec())
                    }
                    _ => None,
                }
            }
        }
    }

    pub fn parse_single_chapter(&mut self, chapter_idx: usize) -> Result<ParsedChapter, EbookError> {
        if let SessionBacking::SingleDoc(ref d) = self.backing {
            return Ok(ParsedChapter {
                chapter_idx: 0,
                doc: d.clone(),
                resolved_fragments: Vec::new(),
                fallback_headings: Vec::new(),
            });
        }
        let ch = self
            .index
            .chapters
            .get(chapter_idx)
            .cloned()
            .ok_or_else(|| EbookError::InvalidFormat("Chapter index out of range".into()))?;

        match ch.method {
            chapter_method::ZIP_STORED | chapter_method::ZIP_DEFLATE => {
                self.parse_epub_chapter_at(chapter_idx, &ch)
            }
            chapter_method::FB2_RAW_SLICE | chapter_method::FB2_ZIP_INNER_SLICE => {
                self.parse_fb2_chapter_at(chapter_idx, &ch)
            }
            chapter_method::MOBI_UNCOMP_RECORDS | chapter_method::MOBI_LZ77_RECORDS => {
                self.parse_mobi_chapter_at(chapter_idx, &ch)
            }
            chapter_method::PLAIN_RAW_SLICE => self.parse_plain_chapter_at(chapter_idx, &ch),
            _ => Err(EbookError::InvalidFormat("Unknown chapter encoding method".into())),
        }
    }

    fn parse_epub_chapter_at(
        &mut self,
        chapter_idx: usize,
        ch: &SpineChapterMeta,
    ) -> Result<ParsedChapter, EbookError> {
        let raw_xhtml = self.read_epub_entry_bytes(
            ch.byte_offset,
            ch.compressed_size as usize,
            ch.uncompressed_size as usize,
            ch.method,
        )?;
        let xhtml = decode_bytes_auto(&raw_xhtml, None);

        // Harvest same-chapter footnote definitions
        let mut local_notes: HashMap<(String, String), Vec<String>> = HashMap::new();
        harvest_footnote_candidates(&ch.path_or_id, &xhtml, &mut local_notes);

        let mut builder = DocBuilder::new(
            self.index.title.clone(),
            self.index.author.clone(),
            DocFormat::Epub,
        );

        // Optional cover image on Chapter 0 (`aux_u16 = cover_img_idx + 1`)
        if chapter_idx == 0 && ch.aux_u16 > 0 {
            let cov_idx = (ch.aux_u16 - 1) as usize;
            if let Some(cov_path) = self.index.images.get(cov_idx).map(|i| i.id_or_path.clone()) {
                let leaf = cov_path.rsplit('/').next().unwrap_or(&cov_path);
                if leaf.is_empty() || !xhtml.contains(leaf) {
                    self.ensure_image_probed(cov_idx);
                    builder.push_image(cov_idx, String::new());
                }
            }
        }

        let mut resolved_fragments: Vec<(String, u32)> = Vec::new();
        let mut fallback_headings: Vec<(String, u8, u32)> = Vec::new();
        let mut in_head = false;
        let mut skip_aside_depth = 0usize;
        let mut bold_depth = 0usize;
        let mut italic_depth = 0usize;
        let mut code_depth = 0usize;
        let mut quote_depth = 0usize;
        let mut pre_depth = 0usize;
        let mut heading_level = 0u8;
        let mut list_li_pending = false;
        let mut runs: Vec<InlineRun> = Vec::new();
        let mut active_noteref: Option<(String, String)> = None;
        let mut noteref_text = String::new();

        let flush_para = |runs: &mut Vec<InlineRun>,
                          heading_level: u8,
                          quote_depth: usize,
                          pre_depth: usize,
                          fallback_headings: &mut Vec<(String, u8, u32)>,
                          builder: &mut DocBuilder| {
            if runs.is_empty() {
                return;
            }
            let joined: String = runs.iter().map(|r| r.text.as_str()).collect();
            if joined.trim().is_empty() {
                runs.clear();
                return;
            }
            let off = builder.current_offset() as u32;
            if (1..=3).contains(&heading_level) {
                fallback_headings.push((joined.trim().to_string(), heading_level - 1, off));
            }
            let drained = std::mem::take(runs);
            builder.push_paragraph(
                drained,
                heading_level,
                quote_depth > 0,
                pre_depth > 0,
                heading_level == 1,
            );
        };

        for tok in MarkupTokenizer::new(&xhtml) {
            match tok {
                MarkupToken::StartTag {
                    name_lower,
                    attrs_raw,
                    self_closing,
                } => {
                    if name_lower == "head" && !self_closing {
                        in_head = true;
                        continue;
                    }
                    if in_head {
                        continue;
                    }
                    if skip_aside_depth > 0 {
                        if !self_closing && name_lower == "aside" {
                            skip_aside_depth += 1;
                        }
                        continue;
                    }
                    if name_lower == "aside" && !self_closing {
                        let et = get_attr(attrs_raw, "epub:type")
                            .or_else(|| get_attr(attrs_raw, "role"))
                            .unwrap_or_default()
                            .to_ascii_lowercase();
                        if et.contains("footnote") || et.contains("endnote") || et.contains("note")
                        {
                            skip_aside_depth = 1;
                            continue;
                        }
                    }

                    if let Some(id) =
                        get_attr(attrs_raw, "id").or_else(|| get_attr(attrs_raw, "name"))
                    {
                        let clean_id = id.trim().to_string();
                        if !clean_id.is_empty() {
                            if is_block_tag(&name_lower) {
                                flush_para(
                                    &mut runs,
                                    heading_level,
                                    quote_depth,
                                    pre_depth,
                                    &mut fallback_headings,
                                    &mut builder,
                                );
                            }
                            if !resolved_fragments.iter().any(|(f, _)| f == &clean_id) {
                                resolved_fragments.push((clean_id, builder.current_offset() as u32));
                            }
                        }
                    }

                    match name_lower.as_str() {
                        "h1" | "h2" | "h3" | "h4" | "h5" | "h6" => {
                            flush_para(
                                &mut runs,
                                heading_level,
                                quote_depth,
                                pre_depth,
                                &mut fallback_headings,
                                &mut builder,
                            );
                            if !self_closing {
                                heading_level = match name_lower.as_str() {
                                    "h1" => 1,
                                    "h2" => 2,
                                    _ => 3,
                                };
                            }
                        }
                        "p" | "div" | "section" | "article" | "header" | "footer" | "tr"
                        | "dt" | "dd" | "figcaption" => {
                            flush_para(
                                &mut runs,
                                heading_level,
                                quote_depth,
                                pre_depth,
                                &mut fallback_headings,
                                &mut builder,
                            );
                        }
                        "li" => {
                            flush_para(
                                &mut runs,
                                heading_level,
                                quote_depth,
                                pre_depth,
                                &mut fallback_headings,
                                &mut builder,
                            );
                            list_li_pending = true;
                        }
                        "blockquote" if !self_closing => {
                            flush_para(
                                &mut runs,
                                heading_level,
                                quote_depth,
                                pre_depth,
                                &mut fallback_headings,
                                &mut builder,
                            );
                            quote_depth += 1;
                        }
                        "pre" if !self_closing => {
                            flush_para(
                                &mut runs,
                                heading_level,
                                quote_depth,
                                pre_depth,
                                &mut fallback_headings,
                                &mut builder,
                            );
                            pre_depth += 1;
                        }
                        "hr" => {
                            flush_para(
                                &mut runs,
                                heading_level,
                                quote_depth,
                                pre_depth,
                                &mut fallback_headings,
                                &mut builder,
                            );
                            builder.push_spacer();
                        }
                        "br" => {
                            if pre_depth > 0 || !runs.is_empty() {
                                flush_para(
                                    &mut runs,
                                    heading_level,
                                    quote_depth,
                                    pre_depth,
                                    &mut fallback_headings,
                                    &mut builder,
                                );
                            }
                        }
                        "b" | "strong" if !self_closing => bold_depth += 1,
                        "i" | "em" | "cite" | "dfn" if !self_closing => italic_depth += 1,
                        "code" | "tt" | "kbd" | "samp" if !self_closing => code_depth += 1,
                        "img" | "image" => {
                            if let Some(src) =
                                get_attr(attrs_raw, "src").or_else(|| get_attr(attrs_raw, "href"))
                            {
                                let alt = get_attr(attrs_raw, "alt").unwrap_or_default();
                                flush_para(
                                    &mut runs,
                                    heading_level,
                                    quote_depth,
                                    pre_depth,
                                    &mut fallback_headings,
                                    &mut builder,
                                );
                                if let Some(img_idx) =
                                    self.resolve_epub_image_idx(&ch.path_or_id, &src)
                                {
                                    self.ensure_image_probed(img_idx);
                                    builder.push_image(img_idx, collapse_inline_ws(&alt, true));
                                }
                            }
                        }
                        "a" if !self_closing => {
                            if let Some(href) = get_attr(attrs_raw, "href") {
                                let et = get_attr(attrs_raw, "epub:type")
                                    .or_else(|| get_attr(attrs_raw, "role"))
                                    .or_else(|| get_attr(attrs_raw, "class"))
                                    .unwrap_or_default()
                                    .to_ascii_lowercase();
                                let (t_path, frag_opt) =
                                    resolve_relative_href(&ch.path_or_id, &href);
                                if let Some(frag) = frag_opt {
                                    let looks_noteref = et.contains("note")
                                        || et.contains("fn")
                                        || frag.to_ascii_lowercase().starts_with("fn")
                                        || frag.to_ascii_lowercase().starts_with("note")
                                        || t_path != ch.path_or_id
                                        || local_notes
                                            .contains_key(&(t_path.clone(), frag.clone()));
                                    if looks_noteref || et.is_empty() {
                                        active_noteref = Some((t_path, frag));
                                        noteref_text.clear();
                                    }
                                }
                            }
                        }
                        _ => {}
                    }
                }
                MarkupToken::Text(t) => {
                    if in_head || skip_aside_depth > 0 {
                        continue;
                    }
                    if active_noteref.is_some() {
                        noteref_text.push_str(t);
                        continue;
                    }
                    if pre_depth > 0 {
                        let dec = decode_entities(t);
                        for (li, line) in dec.split('\n').enumerate() {
                            if li > 0 {
                                flush_para(
                                    &mut runs,
                                    heading_level,
                                    quote_depth,
                                    pre_depth,
                                    &mut fallback_headings,
                                    &mut builder,
                                );
                            }
                            let clean = line.trim_end_matches('\r').replace('\t', "    ");
                            if !clean.is_empty() {
                                runs.push(InlineRun {
                                    text: clean,
                                    style: InlineStyle::Code,
                                    footnote_idx: None,
                                });
                            }
                        }
                        continue;
                    }
                    let trim_lead = runs.is_empty() && !list_li_pending;
                    let clean = collapse_inline_ws(t, trim_lead);
                    if clean.is_empty() || (runs.is_empty() && clean.trim().is_empty()) {
                        continue;
                    }
                    if list_li_pending {
                        list_li_pending = false;
                        runs.push(InlineRun {
                            text: "• ".into(),
                            style: InlineStyle::Bold,
                            footnote_idx: None,
                        });
                    }
                    push_merged_run(
                        &mut runs,
                        clean,
                        resolve_style(bold_depth, italic_depth, code_depth),
                    );
                }
                MarkupToken::EndTag { name_lower } => {
                    if name_lower == "head" {
                        in_head = false;
                        continue;
                    }
                    if in_head {
                        continue;
                    }
                    if skip_aside_depth > 0 {
                        if name_lower == "aside" {
                            skip_aside_depth -= 1;
                        }
                        continue;
                    }
                    match name_lower.as_str() {
                        "a" => {
                            if let Some((t_path, frag)) = active_noteref.take() {
                                let raw_lbl =
                                    collapse_inline_ws(&noteref_text, true).trim().to_string();
                                let is_compact = raw_lbl.chars().count() <= 8;
                                if is_compact {
                                    if let Some(fn_idx) = self.resolve_epub_footnote(
                                        &ch.path_or_id,
                                        &t_path,
                                        &frag,
                                        &raw_lbl,
                                        &local_notes,
                                    ) {
                                        let badge = self.index.footnotes[fn_idx].label.clone();
                                        runs.push(InlineRun {
                                            text: badge,
                                            style: InlineStyle::FootnoteRef,
                                            footnote_idx: Some(fn_idx),
                                        });
                                    } else if !raw_lbl.is_empty() {
                                        push_merged_run(
                                            &mut runs,
                                            raw_lbl,
                                            resolve_style(bold_depth, italic_depth, code_depth),
                                        );
                                    }
                                } else if !raw_lbl.is_empty() {
                                    push_merged_run(
                                        &mut runs,
                                        raw_lbl,
                                        resolve_style(bold_depth, italic_depth, code_depth),
                                    );
                                }
                            }
                        }
                        "h1" | "h2" | "h3" | "h4" | "h5" | "h6" => {
                            flush_para(
                                &mut runs,
                                heading_level,
                                quote_depth,
                                pre_depth,
                                &mut fallback_headings,
                                &mut builder,
                            );
                            heading_level = 0;
                        }
                        "p" | "div" | "section" | "article" | "li" | "tr" | "dt" | "dd"
                        | "figcaption" => {
                            list_li_pending = false;
                            flush_para(
                                &mut runs,
                                heading_level,
                                quote_depth,
                                pre_depth,
                                &mut fallback_headings,
                                &mut builder,
                            );
                        }
                        "blockquote" => {
                            flush_para(
                                &mut runs,
                                heading_level,
                                quote_depth,
                                pre_depth,
                                &mut fallback_headings,
                                &mut builder,
                            );
                            quote_depth = quote_depth.saturating_sub(1);
                        }
                        "pre" => {
                            flush_para(
                                &mut runs,
                                heading_level,
                                quote_depth,
                                pre_depth,
                                &mut fallback_headings,
                                &mut builder,
                            );
                            pre_depth = pre_depth.saturating_sub(1);
                        }
                        "b" | "strong" => bold_depth = bold_depth.saturating_sub(1),
                        "i" | "em" | "cite" | "dfn" => italic_depth = italic_depth.saturating_sub(1),
                        "code" | "tt" | "kbd" | "samp" => code_depth = code_depth.saturating_sub(1),
                        _ => {}
                    }
                }
            }
        }
        flush_para(
            &mut runs,
            heading_level,
            quote_depth,
            pre_depth,
            &mut fallback_headings,
            &mut builder,
        );

        let mut doc = builder.finish();
        doc = self.rebuild_doc_image_dims(doc);
        Ok(ParsedChapter {
            chapter_idx,
            doc,
            resolved_fragments,
            fallback_headings,
        })
    }

    fn resolve_epub_footnote(
        &mut self,
        cur_chapter_path: &str,
        target_path: &str,
        frag: &str,
        raw_lbl: &str,
        local_notes: &HashMap<(String, String), Vec<String>>,
    ) -> Option<usize> {
        let key = format!("{target_path}#{frag}");
        if let Some(existing) = self.index.footnote_keys.iter().position(|k| k == &key) {
            return Some(existing);
        }
        let paras = if let Some(p) = local_notes.get(&(target_path.to_string(), frag.to_string())) {
            p.clone()
        } else if target_path != cur_chapter_path {
            if !self.ext_scanned_files.iter().any(|p| p == target_path) {
                self.ext_scanned_files.push(target_path.to_string());
                if let Some(raw) = self.read_zip_by_name(target_path) {
                    let txt = decode_bytes_auto(&raw, None);
                    harvest_footnote_candidates(target_path, &txt, &mut self.ext_note_cache);
                }
            }
            self.ext_note_cache
                .get(&(target_path.to_string(), frag.to_string()))?
                .clone()
        } else {
            return None;
        };

        let idx = self.index.footnotes.len();
        let badge = format_footnote_badge(raw_lbl, idx + 1);
        self.index.footnote_keys.push(key);
        self.index.footnotes.push(FootnoteDef {
            index: idx,
            label: badge.clone(),
            title: badge,
            paragraphs: paras,
        });
        self.index_dirty = true;
        Some(idx)
    }

    fn resolve_epub_image_idx(&mut self, base_file: &str, src: &str) -> Option<usize> {
        if src.starts_with("data:image/") {
            let bytes = decode_base64(src)?;
            let (w, h) = probe_image_dimensions(&bytes)?;
            let idx = self.index.images.len();
            let id_hash = hash_bytes_fast(&bytes) ^ ((idx as u64) << 32);
            self.index.images.push(IndexImageEntry {
                id_or_path: format!("inline:{idx}"),
                id_hash,
                byte_offset: 0,
                compressed_size: bytes.len() as u32,
                uncompressed_size: bytes.len() as u32,
                method: image_method::INLINE_DATA_URI,
                width: w,
                height: h,
            });
            match &mut self.backing {
                SessionBacking::File {
                    inline_data_images, ..
                }
                | SessionBacking::Memory {
                    inline_data_images, ..
                } => {
                    inline_data_images.insert(idx, bytes);
                }
                SessionBacking::SingleDoc(_) => {}
            }
            return Some(idx);
        }
        let (norm_path, _) = resolve_relative_href(base_file, src);
        if let Some(pos) = self
            .index
            .images
            .iter()
            .position(|im| im.id_or_path == norm_path)
        {
            return Some(pos);
        }
        let lower = norm_path.to_lowercase();
        self.index
            .images
            .iter()
            .position(|im| im.id_or_path.to_lowercase() == lower)
    }

    fn ensure_image_probed(&mut self, img_idx: usize) {
        let Some(im) = self.index.images.get(img_idx).cloned() else {
            return;
        };
        if im.width > 0 && im.height > 0 {
            return;
        }
        let dims = match im.method {
            image_method::ZIP_STORED | image_method::ZIP_DEFLATE => {
                let pref = self.read_epub_entry_prefix(
                    im.byte_offset,
                    im.compressed_size as usize,
                    im.method,
                    32 * 1024,
                );
                pref.ok()
                    .and_then(|b| probe_image_dimensions(&b))
                    .or_else(|| {
                        self.load_raw_image_bytes(img_idx)
                            .and_then(|b| probe_image_dimensions(&b))
                    })
            }
            image_method::FB2_BASE64_SLICE | image_method::FB2_ZIP_BASE64_SLICE => {
                self.probe_fb2_base64_dims(&im)
                    .or_else(|| {
                        self.load_raw_image_bytes(img_idx)
                            .and_then(|b| probe_image_dimensions(&b))
                    })
            }
            image_method::RAW_FILE_SLICE => self
                .read_raw_slice_prefix(im.byte_offset, (im.compressed_size as usize).min(32 * 1024))
                .and_then(|b| probe_image_dimensions(&b)),
            _ => None,
        };
        if let Some((w, h)) = dims {
            if let Some(slot) = self.index.images.get_mut(img_idx) {
                slot.width = w;
                slot.height = h;
                self.index_dirty = true;
            }
        }
    }

    fn probe_fb2_base64_dims(&mut self, im: &IndexImageEntry) -> Option<(u32, u32)> {
        let take_b64 = (im.compressed_size as usize).min(48 * 1024);
        let slice_bytes = match &mut self.backing {
            SessionBacking::File {
                path, fb2_zip_buf, ..
            } => {
                if im.method == image_method::FB2_ZIP_BASE64_SLICE {
                    if fb2_zip_buf.is_none() {
                        *fb2_zip_buf = inflate_fb2_zip_file(path).ok();
                    }
                    let buf = fb2_zip_buf.as_ref()?;
                    let s = im.byte_offset as usize;
                    let e = s.checked_add(take_b64)?.min(buf.len());
                    buf[s..e].to_vec()
                } else {
                    let mut f = File::open(path).ok()?;
                    f.seek(SeekFrom::Start(im.byte_offset)).ok()?;
                    let mut out = vec![0u8; take_b64];
                    f.read_exact(&mut out).ok()?;
                    out
                }
            }
            SessionBacking::Memory {
                bytes, fb2_zip_buf, ..
            } => {
                if im.method == image_method::FB2_ZIP_BASE64_SLICE {
                    if fb2_zip_buf.is_none() {
                        *fb2_zip_buf = inflate_fb2_zip_bytes(bytes).ok();
                    }
                    let buf = fb2_zip_buf.as_ref()?;
                    let s = im.byte_offset as usize;
                    let e = s.checked_add(take_b64)?.min(buf.len());
                    buf[s..e].to_vec()
                } else {
                    let s = im.byte_offset as usize;
                    let e = s.checked_add(take_b64)?.min(bytes.len());
                    bytes[s..e].to_vec()
                }
            }
            SessionBacking::SingleDoc(_) => return None,
        };
        let dec = decode_base64(&String::from_utf8_lossy(&slice_bytes))?;
        probe_image_dimensions(&dec)
    }

    fn read_raw_slice_prefix(&self, offset: u64, len: usize) -> Option<Vec<u8>> {
        match &self.backing {
            SessionBacking::File { path, .. } => {
                let mut f = File::open(path).ok()?;
                f.seek(SeekFrom::Start(offset)).ok()?;
                let mut buf = vec![0u8; len];
                let n = f.read(&mut buf).ok()?;
                buf.truncate(n);
                Some(buf)
            }
            SessionBacking::Memory { bytes, .. } => {
                let s = offset as usize;
                if s >= bytes.len() {
                    return None;
                }
                let e = (s + len).min(bytes.len());
                Some(bytes[s..e].to_vec())
            }
            SessionBacking::SingleDoc(_) => None,
        }
    }

    fn read_epub_entry_bytes(
        &self,
        local_off: u64,
        comp_sz: usize,
        uncomp_sz: usize,
        method: u16,
    ) -> Result<Vec<u8>, EbookError> {
        match &self.backing {
            SessionBacking::File {
                path, file_size, ..
            } => {
                let mut f = File::open(path)
                    .map_err(|e| EbookError::InvalidFormat(format!("Open error: {e}")))?;
                read_zip_entry_direct(&mut f, *file_size, local_off, comp_sz, uncomp_sz, method)
                    .map_err(|e| EbookError::InvalidFormat(e.to_string()))
            }
            SessionBacking::Memory { bytes, .. } => {
                let mut cur = std::io::Cursor::new(bytes.as_slice());
                read_zip_entry_direct(
                    &mut cur,
                    bytes.len() as u64,
                    local_off,
                    comp_sz,
                    uncomp_sz,
                    method,
                )
                .map_err(|e| EbookError::InvalidFormat(e.to_string()))
            }
            SessionBacking::SingleDoc(_) => Err(EbookError::InvalidFormat("Not ZIP backed".into())),
        }
    }

    fn read_epub_entry_prefix(
        &self,
        local_off: u64,
        comp_sz: usize,
        method: u16,
        max_bytes: usize,
    ) -> Result<Vec<u8>, EbookError> {
        match &self.backing {
            SessionBacking::File {
                path, file_size, ..
            } => {
                let mut f = File::open(path)
                    .map_err(|e| EbookError::InvalidFormat(format!("Open error: {e}")))?;
                read_zip_entry_prefix_direct(&mut f, *file_size, local_off, comp_sz, method, max_bytes)
                    .map_err(|e| EbookError::InvalidFormat(e.to_string()))
            }
            SessionBacking::Memory { bytes, .. } => {
                let mut cur = std::io::Cursor::new(bytes.as_slice());
                read_zip_entry_prefix_direct(
                    &mut cur,
                    bytes.len() as u64,
                    local_off,
                    comp_sz,
                    method,
                    max_bytes,
                )
                .map_err(|e| EbookError::InvalidFormat(e.to_string()))
            }
            SessionBacking::SingleDoc(_) => Err(EbookError::InvalidFormat("Not ZIP backed".into())),
        }
    }

    fn read_zip_by_name(&self, archive_path: &str) -> Option<Vec<u8>> {
        match &self.backing {
            SessionBacking::File { path, .. } => {
                let f = File::open(path).ok()?;
                let mut z = FileZipArchive::open(f).ok()?;
                z.read_by_path(archive_path)
            }
            SessionBacking::Memory { bytes, .. } => {
                let z = ZipArchive::parse(bytes).ok()?;
                z.read_by_path(archive_path)
            }
            SessionBacking::SingleDoc(_) => None,
        }
    }

    fn parse_fb2_chapter_at(
        &mut self,
        chapter_idx: usize,
        ch: &SpineChapterMeta,
    ) -> Result<ParsedChapter, EbookError> {
        let raw_slice = match &mut self.backing {
            SessionBacking::File {
                path, fb2_zip_buf, ..
            } => {
                if ch.method == chapter_method::FB2_ZIP_INNER_SLICE {
                    if fb2_zip_buf.is_none() {
                        *fb2_zip_buf = Some(inflate_fb2_zip_file(path)?);
                    }
                    let buf = fb2_zip_buf.as_ref().unwrap();
                    let s = ch.byte_offset as usize;
                    let e = s
                        .checked_add(ch.uncompressed_size as usize)
                        .unwrap_or(buf.len())
                        .min(buf.len());
                    buf[s..e].to_vec()
                } else {
                    let mut f = File::open(path)
                        .map_err(|e| EbookError::InvalidFormat(format!("FB2 read error: {e}")))?;
                    f.seek(SeekFrom::Start(ch.byte_offset))
                        .map_err(|e| EbookError::InvalidFormat(e.to_string()))?;
                    let mut out = vec![0u8; ch.uncompressed_size as usize];
                    f.read_exact(&mut out)
                        .map_err(|e| EbookError::InvalidFormat(e.to_string()))?;
                    out
                }
            }
            SessionBacking::Memory {
                bytes, fb2_zip_buf, ..
            } => {
                if ch.method == chapter_method::FB2_ZIP_INNER_SLICE {
                    if fb2_zip_buf.is_none() {
                        *fb2_zip_buf = Some(inflate_fb2_zip_bytes(bytes)?);
                    }
                    let buf = fb2_zip_buf.as_ref().unwrap();
                    let s = ch.byte_offset as usize;
                    let e = s
                        .checked_add(ch.uncompressed_size as usize)
                        .unwrap_or(buf.len())
                        .min(buf.len());
                    buf[s..e].to_vec()
                } else {
                    let s = ch.byte_offset as usize;
                    let e = s
                        .checked_add(ch.uncompressed_size as usize)
                        .unwrap_or(bytes.len())
                        .min(bytes.len());
                    bytes[s..e].to_vec()
                }
            }
            SessionBacking::SingleDoc(_) => return Err(EbookError::InvalidFormat("Invalid FB2".into())),
        };

        let enc = TextEncoding::from_u8(self.index.detected_encoding);
        let xml = decode_bytes_auto(&raw_slice, Some(enc));
        let mut builder = DocBuilder::new(
            self.index.title.clone(),
            self.index.author.clone(),
            DocFormat::Fb2,
        );

        let mut cover_img_idx: Option<usize> = None;
        if chapter_idx == 0 && ch.aux_u16 > 0 {
            let cidx = (ch.aux_u16 - 1) as usize;
            cover_img_idx = Some(cidx);
            self.ensure_image_probed(cidx);
            builder.push_image(cidx, String::new());
        }

        let mut section_depth = 0usize;
        let mut in_title = false;
        let mut is_subtitle = false;
        let mut epigraph_or_cite_depth = 0usize;
        let mut poem_depth = 0usize;
        let mut bold_depth = 0usize;
        let mut italic_depth = 0usize;
        let mut code_depth = 0usize;
        let mut runs: Vec<InlineRun> = Vec::new();
        let mut active_note_id: Option<String> = None;
        let mut note_link_txt = String::new();

        let flush_runs = |runs: &mut Vec<InlineRun>,
                          in_title: bool,
                          is_subtitle: bool,
                          section_depth: usize,
                          quote: bool,
                          code: bool,
                          builder: &mut DocBuilder| {
            if runs.is_empty() {
                return;
            }
            let plain: String = runs.iter().map(|r| r.text.as_str()).collect();
            if plain.trim().is_empty() {
                runs.clear();
                return;
            }
            let h_level = if in_title {
                if section_depth <= 1 {
                    1
                } else if section_depth == 2 {
                    2
                } else {
                    3
                }
            } else if is_subtitle {
                2
            } else {
                0
            };
            let drained = std::mem::take(runs);
            builder.push_paragraph(drained, h_level, quote, code, in_title && section_depth <= 1);
        };

        for tok in MarkupTokenizer::new(&xml) {
            match tok {
                MarkupToken::StartTag {
                    name_lower,
                    attrs_raw,
                    self_closing,
                } => match name_lower.as_str() {
                    "section" if !self_closing => {
                        flush_runs(
                            &mut runs,
                            in_title,
                            is_subtitle,
                            section_depth,
                            epigraph_or_cite_depth > 0 || poem_depth > 0,
                            code_depth > 0,
                            &mut builder,
                        );
                        section_depth += 1;
                    }
                    "title" if !self_closing => {
                        flush_runs(
                            &mut runs,
                            in_title,
                            is_subtitle,
                            section_depth,
                            epigraph_or_cite_depth > 0 || poem_depth > 0,
                            code_depth > 0,
                            &mut builder,
                        );
                        in_title = true;
                    }
                    "subtitle" if !self_closing => {
                        flush_runs(
                            &mut runs,
                            in_title,
                            is_subtitle,
                            section_depth,
                            epigraph_or_cite_depth > 0 || poem_depth > 0,
                            code_depth > 0,
                            &mut builder,
                        );
                        is_subtitle = true;
                    }
                    "epigraph" | "cite" | "annotation" if !self_closing => {
                        flush_runs(
                            &mut runs,
                            in_title,
                            is_subtitle,
                            section_depth,
                            epigraph_or_cite_depth > 0 || poem_depth > 0,
                            code_depth > 0,
                            &mut builder,
                        );
                        epigraph_or_cite_depth += 1;
                    }
                    "poem" if !self_closing => {
                        flush_runs(
                            &mut runs,
                            in_title,
                            is_subtitle,
                            section_depth,
                            epigraph_or_cite_depth > 0 || poem_depth > 0,
                            code_depth > 0,
                            &mut builder,
                        );
                        poem_depth += 1;
                    }
                    "stanza" | "p" | "v" | "text-author" => {
                        flush_runs(
                            &mut runs,
                            in_title,
                            is_subtitle,
                            section_depth,
                            epigraph_or_cite_depth > 0 || poem_depth > 0,
                            code_depth > 0,
                            &mut builder,
                        );
                        if name_lower == "text-author" && !self_closing {
                            italic_depth += 1;
                        }
                    }
                    "empty-line" => {
                        flush_runs(
                            &mut runs,
                            in_title,
                            is_subtitle,
                            section_depth,
                            epigraph_or_cite_depth > 0 || poem_depth > 0,
                            code_depth > 0,
                            &mut builder,
                        );
                        builder.push_spacer();
                    }
                    "strong" | "b" if !self_closing => bold_depth += 1,
                    "emphasis" | "i" if !self_closing => italic_depth += 1,
                    "code" if !self_closing => code_depth += 1,
                    "image" => {
                        if let Some(href) = get_attr(attrs_raw, "href") {
                            let id = href.trim().trim_start_matches('#');
                            let lower = id.to_ascii_lowercase();
                            if let Some(img_idx) = self
                                .index
                                .images
                                .iter()
                                .position(|im| im.id_or_path == id || im.id_or_path.to_ascii_lowercase() == lower)
                            {
                                if cover_img_idx != Some(img_idx) || builder.current_offset() > 4 {
                                    flush_runs(
                                        &mut runs,
                                        in_title,
                                        is_subtitle,
                                        section_depth,
                                        epigraph_or_cite_depth > 0 || poem_depth > 0,
                                        code_depth > 0,
                                        &mut builder,
                                    );
                                    self.ensure_image_probed(img_idx);
                                    let alt = get_attr(attrs_raw, "alt").unwrap_or_default();
                                    builder.push_image(img_idx, collapse_inline_ws(&alt, true));
                                }
                            }
                        }
                    }
                    "a" if !self_closing => {
                        let atype = get_attr(attrs_raw, "type")
                            .unwrap_or_default()
                            .to_ascii_lowercase();
                        if let Some(href) = get_attr(attrs_raw, "href") {
                            let target = href.trim().trim_start_matches('#').to_string();
                            if !target.is_empty()
                                && (atype == "note"
                                    || self.index.footnote_keys.iter().any(|k| k == &target))
                            {
                                active_note_id = Some(target);
                                note_link_txt.clear();
                            }
                        }
                    }
                    _ => {}
                },
                MarkupToken::Text(t) => {
                    if active_note_id.is_some() {
                        note_link_txt.push_str(t);
                        continue;
                    }
                    let clean = collapse_inline_ws(t, runs.is_empty());
                    if clean.is_empty() || (runs.is_empty() && clean.trim().is_empty()) {
                        continue;
                    }
                    let style = resolve_style(
                        bold_depth,
                        italic_depth + usize::from(epigraph_or_cite_depth > 0),
                        code_depth,
                    );
                    push_merged_run(&mut runs, clean, style);
                }
                MarkupToken::EndTag { name_lower } => match name_lower.as_str() {
                    "a" => {
                        if let Some(note_id) = active_note_id.take() {
                            let raw_lbl = collapse_inline_ws(&note_link_txt, true).trim().to_string();
                            if let Some(fn_idx) =
                                self.index.footnote_keys.iter().position(|k| k == &note_id)
                            {
                                let badge = if raw_lbl.is_empty() {
                                    self.index.footnotes[fn_idx].label.clone()
                                } else {
                                    format_footnote_badge(&raw_lbl, fn_idx + 1)
                                };
                                runs.push(InlineRun {
                                    text: badge,
                                    style: InlineStyle::FootnoteRef,
                                    footnote_idx: Some(fn_idx),
                                });
                            } else if !raw_lbl.is_empty() {
                                push_merged_run(
                                    &mut runs,
                                    raw_lbl,
                                    resolve_style(bold_depth, italic_depth, code_depth),
                                );
                            }
                        }
                    }
                    "title" if in_title => {
                        flush_runs(
                            &mut runs,
                            in_title,
                            is_subtitle,
                            section_depth,
                            epigraph_or_cite_depth > 0 || poem_depth > 0,
                            code_depth > 0,
                            &mut builder,
                        );
                        in_title = false;
                    }
                    "subtitle" => {
                        flush_runs(
                            &mut runs,
                            in_title,
                            is_subtitle,
                            section_depth,
                            epigraph_or_cite_depth > 0 || poem_depth > 0,
                            code_depth > 0,
                            &mut builder,
                        );
                        is_subtitle = false;
                    }
                    "p" | "v" => flush_runs(
                        &mut runs,
                        in_title,
                        is_subtitle,
                        section_depth,
                        epigraph_or_cite_depth > 0 || poem_depth > 0,
                        code_depth > 0,
                        &mut builder,
                    ),
                    "text-author" => {
                        flush_runs(
                            &mut runs,
                            in_title,
                            is_subtitle,
                            section_depth,
                            epigraph_or_cite_depth > 0 || poem_depth > 0,
                            code_depth > 0,
                            &mut builder,
                        );
                        italic_depth = italic_depth.saturating_sub(1);
                    }
                    "stanza" => {
                        flush_runs(
                            &mut runs,
                            in_title,
                            is_subtitle,
                            section_depth,
                            epigraph_or_cite_depth > 0 || poem_depth > 0,
                            code_depth > 0,
                            &mut builder,
                        );
                        builder.push_spacer();
                    }
                    "epigraph" | "cite" | "annotation" => {
                        flush_runs(
                            &mut runs,
                            in_title,
                            is_subtitle,
                            section_depth,
                            epigraph_or_cite_depth > 0 || poem_depth > 0,
                            code_depth > 0,
                            &mut builder,
                        );
                        epigraph_or_cite_depth = epigraph_or_cite_depth.saturating_sub(1);
                    }
                    "poem" => {
                        flush_runs(
                            &mut runs,
                            in_title,
                            is_subtitle,
                            section_depth,
                            epigraph_or_cite_depth > 0 || poem_depth > 0,
                            code_depth > 0,
                            &mut builder,
                        );
                        poem_depth = poem_depth.saturating_sub(1);
                    }
                    "section" => {
                        flush_runs(
                            &mut runs,
                            in_title,
                            is_subtitle,
                            section_depth,
                            epigraph_or_cite_depth > 0 || poem_depth > 0,
                            code_depth > 0,
                            &mut builder,
                        );
                        section_depth = section_depth.saturating_sub(1);
                    }
                    "strong" | "b" => bold_depth = bold_depth.saturating_sub(1),
                    "emphasis" | "i" => italic_depth = italic_depth.saturating_sub(1),
                    "code" => code_depth = code_depth.saturating_sub(1),
                    _ => {}
                },
            }
        }
        flush_runs(
            &mut runs,
            in_title,
            is_subtitle,
            section_depth,
            epigraph_or_cite_depth > 0 || poem_depth > 0,
            code_depth > 0,
            &mut builder,
        );

        let doc = self.rebuild_doc_image_dims(builder.finish());
        Ok(ParsedChapter {
            chapter_idx,
            doc,
            resolved_fragments: Vec::new(),
            fallback_headings: Vec::new(),
        })
    }

    fn parse_mobi_chapter_at(
        &mut self,
        chapter_idx: usize,
        ch: &SpineChapterMeta,
    ) -> Result<ParsedChapter, EbookError> {
        let html_slice = self.load_mobi_chapter_html(ch)?;
        let mut builder = DocBuilder::new(
            self.index.title.clone(),
            self.index.author.clone(),
            DocFormat::Mobi,
        );

        let mut cover_idx: Option<usize> = None;
        if chapter_idx == 0 && (ch.aux_u16 & 0x8000) != 0 {
            let cidx = (ch.aux_u16 & 0x7FFF) as usize;
            if cidx < self.index.images.len() {
                cover_idx = Some(cidx);
                self.ensure_image_probed(cidx);
                builder.push_image(cidx, String::new());
            }
        }

        let mut heading_level = 0u8;
        let mut bold_depth = 0usize;
        let mut italic_depth = 0usize;
        let mut code_depth = 0usize;
        let mut quote_depth = 0usize;
        let mut runs: Vec<InlineRun> = Vec::new();
        let mut active_fp: Option<String> = None;
        let mut fp_link_txt = String::new();
        let mut fallback_headings: Vec<(String, u8, u32)> = Vec::new();

        let flush_para = |runs: &mut Vec<InlineRun>,
                          heading_level: u8,
                          quote_depth: usize,
                          fallback_headings: &mut Vec<(String, u8, u32)>,
                          builder: &mut DocBuilder| {
            if runs.is_empty() {
                return;
            }
            let plain: String = runs.iter().map(|r| r.text.as_str()).collect();
            if plain.trim().is_empty() {
                runs.clear();
                return;
            }
            let off = builder.current_offset() as u32;
            if (1..=3).contains(&heading_level) {
                fallback_headings.push((plain.trim().to_string(), heading_level - 1, off));
            }
            let drained = std::mem::take(runs);
            builder.push_paragraph(
                drained,
                heading_level,
                quote_depth > 0,
                false,
                heading_level == 1,
            );
        };

        for tok in MarkupTokenizer::new(&html_slice) {
            match tok {
                MarkupToken::StartTag {
                    name_lower,
                    attrs_raw,
                    self_closing,
                } => match name_lower.as_str() {
                    "h1" | "h2" | "h3" | "h4" | "h5" | "h6" => {
                        flush_para(
                            &mut runs,
                            heading_level,
                            quote_depth,
                            &mut fallback_headings,
                            &mut builder,
                        );
                        if !self_closing {
                            heading_level = match name_lower.as_str() {
                                "h1" => 1,
                                "h2" => 2,
                                _ => 3,
                            };
                        }
                    }
                    "p" | "div" | "li" | "tr" | "dt" | "dd" | "br" => {
                        flush_para(
                            &mut runs,
                            heading_level,
                            quote_depth,
                            &mut fallback_headings,
                            &mut builder,
                        );
                    }
                    "pagebreak" => {
                        flush_para(
                            &mut runs,
                            heading_level,
                            quote_depth,
                            &mut fallback_headings,
                            &mut builder,
                        );
                        builder.push_page_break();
                    }
                    "hr" => {
                        flush_para(
                            &mut runs,
                            heading_level,
                            quote_depth,
                            &mut fallback_headings,
                            &mut builder,
                        );
                        builder.push_spacer();
                    }
                    "blockquote" if !self_closing => {
                        flush_para(
                            &mut runs,
                            heading_level,
                            quote_depth,
                            &mut fallback_headings,
                            &mut builder,
                        );
                        quote_depth += 1;
                    }
                    "b" | "strong" if !self_closing => bold_depth += 1,
                    "i" | "em" if !self_closing => italic_depth += 1,
                    "code" | "tt" if !self_closing => code_depth += 1,
                    "img" => {
                        if let Some(rec_attr) = get_attr(attrs_raw, "recindex") {
                            if let Ok(one_based) = rec_attr.trim().parse::<usize>() {
                                if one_based >= 1 {
                                    let img_idx = one_based - 1;
                                    if img_idx < self.index.images.len()
                                        && (cover_idx != Some(img_idx) || builder.current_offset() > 4)
                                    {
                                        flush_para(
                                            &mut runs,
                                            heading_level,
                                            quote_depth,
                                            &mut fallback_headings,
                                            &mut builder,
                                        );
                                        self.ensure_image_probed(img_idx);
                                        let alt = get_attr(attrs_raw, "alt").unwrap_or_default();
                                        builder.push_image(img_idx, collapse_inline_ws(&alt, true));
                                    }
                                }
                            }
                        }
                    }
                    "a" if !self_closing => {
                        if let Some(fp_str) = get_attr(attrs_raw, "filepos") {
                            if let Ok(fp_val) = fp_str.trim().parse::<usize>() {
                                active_fp = Some(format!("fp:{fp_val}"));
                                fp_link_txt.clear();
                            }
                        }
                    }
                    _ => {}
                },
                MarkupToken::Text(t) => {
                    if active_fp.is_some() {
                        fp_link_txt.push_str(t);
                        continue;
                    }
                    let clean = collapse_inline_ws(t, runs.is_empty());
                    if clean.is_empty() || (runs.is_empty() && clean.trim().is_empty()) {
                        continue;
                    }
                    push_merged_run(
                        &mut runs,
                        clean,
                        resolve_style(bold_depth, italic_depth, code_depth),
                    );
                }
                MarkupToken::EndTag { name_lower } => match name_lower.as_str() {
                    "a" => {
                        if let Some(fp_key) = active_fp.take() {
                            let raw_lbl = collapse_inline_ws(&fp_link_txt, true).trim().to_string();
                            if let Some(fn_idx) =
                                self.index.footnote_keys.iter().position(|k| k == &fp_key)
                            {
                                let badge = self.index.footnotes[fn_idx].label.clone();
                                runs.push(InlineRun {
                                    text: badge,
                                    style: InlineStyle::FootnoteRef,
                                    footnote_idx: Some(fn_idx),
                                });
                            } else if !raw_lbl.is_empty() {
                                push_merged_run(
                                    &mut runs,
                                    raw_lbl,
                                    resolve_style(bold_depth, italic_depth, code_depth),
                                );
                            }
                        }
                    }
                    "h1" | "h2" | "h3" | "h4" | "h5" | "h6" => {
                        flush_para(
                            &mut runs,
                            heading_level,
                            quote_depth,
                            &mut fallback_headings,
                            &mut builder,
                        );
                        heading_level = 0;
                    }
                    "p" | "div" | "li" | "tr" | "dt" | "dd" => {
                        flush_para(
                            &mut runs,
                            heading_level,
                            quote_depth,
                            &mut fallback_headings,
                            &mut builder,
                        );
                    }
                    "blockquote" => {
                        flush_para(
                            &mut runs,
                            heading_level,
                            quote_depth,
                            &mut fallback_headings,
                            &mut builder,
                        );
                        quote_depth = quote_depth.saturating_sub(1);
                    }
                    "b" | "strong" => bold_depth = bold_depth.saturating_sub(1),
                    "i" | "em" => italic_depth = italic_depth.saturating_sub(1),
                    "code" | "tt" => code_depth = code_depth.saturating_sub(1),
                    _ => {}
                },
            }
        }
        flush_para(
            &mut runs,
            heading_level,
            quote_depth,
            &mut fallback_headings,
            &mut builder,
        );

        let doc = self.rebuild_doc_image_dims(builder.finish());
        Ok(ParsedChapter {
            chapter_idx,
            doc,
            resolved_fragments: Vec::new(),
            fallback_headings,
        })
    }

    fn load_mobi_chapter_html(&mut self, ch: &SpineChapterMeta) -> Result<String, EbookError> {
        // Fast path if decoded MOBI HTML is already resident from cold open
        let start = ch.byte_offset as usize;
        let end_len = ch.uncompressed_size as usize;
        match &mut self.backing {
            SessionBacking::File {
                path, mobi_html_buf, ..
            } => {
                if mobi_html_buf.is_none() {
                    let enc = TextEncoding::from_u8(self.index.detected_encoding);
                    *mobi_html_buf = Some(decompress_mobi_html_from_file(path, enc)?);
                }
                let html = mobi_html_buf.as_ref().unwrap();
                let s = clamp_char_boundary(html, start.min(html.len()));
                let e = clamp_char_boundary(html, start.saturating_add(end_len).min(html.len()));
                Ok(html[s..e.max(s)].to_string())
            }
            SessionBacking::Memory {
                bytes,
                mobi_html_buf,
                ..
            } => {
                if mobi_html_buf.is_none() {
                    let enc = TextEncoding::from_u8(self.index.detected_encoding);
                    *mobi_html_buf = Some(decompress_mobi_html_from_slice(bytes, enc)?);
                }
                let html = mobi_html_buf.as_ref().unwrap();
                let s = clamp_char_boundary(html, start.min(html.len()));
                let e = clamp_char_boundary(html, start.saturating_add(end_len).min(html.len()));
                Ok(html[s..e.max(s)].to_string())
            }
            SessionBacking::SingleDoc(_) => Err(EbookError::InvalidFormat("Invalid MOBI".into())),
        }
    }

    fn parse_plain_chapter_at(
        &mut self,
        chapter_idx: usize,
        ch: &SpineChapterMeta,
    ) -> Result<ParsedChapter, EbookError> {
        let (file_name, slice) = match &self.backing {
            SessionBacking::File { path, .. } => {
                let mut f = File::open(path)
                    .map_err(|e| EbookError::InvalidFormat(format!("Read error: {e}")))?;
                f.seek(SeekFrom::Start(ch.byte_offset))
                    .map_err(|e| EbookError::InvalidFormat(e.to_string()))?;
                let mut buf = vec![0u8; ch.uncompressed_size as usize];
                f.read_exact(&mut buf)
                    .map_err(|e| EbookError::InvalidFormat(e.to_string()))?;
                let fname = path
                    .file_name()
                    .and_then(|s| s.to_str())
                    .unwrap_or("doc.txt")
                    .to_string();
                (fname, buf)
            }
            SessionBacking::Memory {
                file_name, bytes, ..
            } => {
                let s = ch.byte_offset as usize;
                let e = s
                    .checked_add(ch.uncompressed_size as usize)
                    .unwrap_or(bytes.len())
                    .min(bytes.len());
                (file_name.clone(), bytes[s..e].to_vec())
            }
            SessionBacking::SingleDoc(_) => {
                return Err(EbookError::InvalidFormat("Invalid plain doc".into()))
            }
        };

        let force_md = self.index.format == DocFormat::Markdown;
        let mut doc = plain_md::parse_plain_or_markdown(&file_name, &slice, force_md)?;
        doc.title = self.index.title.clone();
        doc.author = self.index.author.clone();
        if !self.index.footnotes.is_empty() {
            doc.footnotes = self.index.footnotes.clone();
        }
        Ok(ParsedChapter {
            chapter_idx,
            doc,
            resolved_fragments: Vec::new(),
            fallback_headings: Vec::new(),
        })
    }
}

fn clamp_char_boundary(s: &str, mut pos: usize) -> usize {
    pos = pos.min(s.len());
    while pos > 0 && !s.is_char_boundary(pos) {
        pos -= 1;
    }
    pos
}

fn resolve_initial_spot(index: &BookIndex, bmark: BookmarkHint) -> (usize, usize) {
    let max_ch = index.chapters.len().saturating_sub(1);
    match bmark {
        BookmarkHint::ChapterLocal {
            chapter_idx,
            char_offset,
        } => (chapter_idx.min(max_ch), char_offset),
        BookmarkHint::LegacyGlobal(g) if g > 0 => index.map_global_offset(g as u64),
        _ => {
            if index.saved_anchor.has_anchor {
                (
                    (index.saved_anchor.chapter_idx as usize).min(max_ch),
                    index.saved_anchor.char_offset as usize,
                )
            } else {
                (0, 0)
            }
        }
    }
}

fn hash_bytes_fast(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325 ^ (bytes.len() as u64);
    for &b in bytes.iter().take(4096) {
        h ^= b as u64;
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    h
}

fn hash_str_id(s: &str, idx: usize) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325 ^ ((idx as u64) << 32);
    for b in s.bytes() {
        h ^= b as u64;
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    h
}

/* ============================================================================
 * Cold Index Builders (`O(1)` in untouched chapters & images)
 * ========================================================================== */

fn build_cold_index_from_file(
    path: &Path,
    file_name: &str,
    file_size: u64,
) -> Result<(BookIndex, Option<Vec<u8>>, Option<String>), EbookError> {
    let lower = file_name.trim().to_ascii_lowercase();
    if lower.ends_with(".epub") {
        let f = File::open(path).map_err(|e| EbookError::InvalidFormat(e.to_string()))?;
        let mut zip = FileZipArchive::open(f).map_err(|e| EbookError::InvalidFormat(e.to_string()))?;
        let idx = build_epub_index_from_entries(file_name, zip.entries().to_vec(), |p| {
            zip.read_by_path(p)
        })?;
        return Ok((idx, None, None));
    }
    if lower.ends_with(".fb2.zip") {
        let inner = inflate_fb2_zip_file(path)?;
        let idx = build_fb2_index_from_bytes(file_name, &inner, true)?;
        return Ok((idx, Some(inner), None));
    }
    if lower.ends_with(".fb2") {
        let raw = fs::read(path).map_err(|e| EbookError::InvalidFormat(e.to_string()))?;
        if raw.starts_with(b"PK\x03\x04") {
            let inner = inflate_fb2_zip_bytes(&raw)?;
            let idx = build_fb2_index_from_bytes(file_name, &inner, true)?;
            return Ok((idx, Some(inner), None));
        }
        let idx = build_fb2_index_from_bytes(file_name, &raw, false)?;
        return Ok((idx, None, None));
    }
    if lower.ends_with(".mobi") || lower.ends_with(".prc") || lower.ends_with(".azw") {
        let mut f = File::open(path).map_err(|e| EbookError::InvalidFormat(e.to_string()))?;
        let (idx, html) = build_mobi_index_from_reader(file_name, &mut f, file_size)?;
        return Ok((idx, None, html));
    }

    // Sniff magic header (first 68 bytes) for generic filenames
    let mut head = [0u8; 68];
    if let Ok(mut f) = File::open(path) {
        let n = f.read(&mut head).unwrap_or(0);
        if n >= 68 && (&head[60..68] == b"BOOKMOBI" || &head[60..68] == b"TEXtREAd") {
            let _ = f.seek(SeekFrom::Start(0));
            let (idx, html) = build_mobi_index_from_reader(file_name, &mut f, file_size)?;
            return Ok((idx, None, html));
        }
        if n >= 4 && &head[0..4] == b"PK\x03\x04" {
            let _ = f.seek(SeekFrom::Start(0));
            if let Ok(mut zip) = FileZipArchive::open(f) {
                if let Ok(idx) = build_epub_index_from_entries(
                    file_name,
                    zip.entries().to_vec(),
                    |p| zip.read_by_path(p),
                ) {
                    return Ok((idx, None, None));
                }
            }
            let inner = inflate_fb2_zip_file(path)?;
            let idx = build_fb2_index_from_bytes(file_name, &inner, true)?;
            return Ok((idx, Some(inner), None));
        }
    }

    let raw = fs::read(path).map_err(|e| EbookError::InvalidFormat(e.to_string()))?;
    let idx = build_plain_or_md_index(file_name, &raw)?;
    Ok((idx, None, None))
}

fn build_cold_index_from_bytes(
    file_name: &str,
    bytes: &[u8],
) -> Result<(BookIndex, Option<Vec<u8>>, Option<String>), EbookError> {
    let lower = file_name.trim().to_ascii_lowercase();
    if lower.ends_with(".epub") {
        let zip = ZipArchive::parse(bytes).map_err(|e| EbookError::InvalidFormat(e.to_string()))?;
        let idx = build_epub_index_from_entries(file_name, zip.entries().to_vec(), |p| {
            zip.read_by_path(p)
        })?;
        return Ok((idx, None, None));
    }
    if lower.ends_with(".fb2.zip") {
        let inner = inflate_fb2_zip_bytes(bytes)?;
        let idx = build_fb2_index_from_bytes(file_name, &inner, true)?;
        return Ok((idx, Some(inner), None));
    }
    if lower.ends_with(".fb2") {
        if bytes.starts_with(b"PK\x03\x04") {
            let inner = inflate_fb2_zip_bytes(bytes)?;
            let idx = build_fb2_index_from_bytes(file_name, &inner, true)?;
            return Ok((idx, Some(inner), None));
        }
        let idx = build_fb2_index_from_bytes(file_name, bytes, false)?;
        return Ok((idx, None, None));
    }
    if lower.ends_with(".mobi") || lower.ends_with(".prc") || lower.ends_with(".azw") {
        let mut cur = std::io::Cursor::new(bytes);
        let (idx, html) = build_mobi_index_from_reader(file_name, &mut cur, bytes.len() as u64)?;
        return Ok((idx, None, html));
    }
    if bytes.len() >= 68 && (&bytes[60..68] == b"BOOKMOBI" || &bytes[60..68] == b"TEXtREAd") {
        let mut cur = std::io::Cursor::new(bytes);
        let (idx, html) = build_mobi_index_from_reader(file_name, &mut cur, bytes.len() as u64)?;
        return Ok((idx, None, html));
    }
    if bytes.starts_with(b"PK\x03\x04") {
        let zip = ZipArchive::parse(bytes).map_err(|e| EbookError::InvalidFormat(e.to_string()))?;
        if let Ok(idx) =
            build_epub_index_from_entries(file_name, zip.entries().to_vec(), |p| zip.read_by_path(p))
        {
            return Ok((idx, None, None));
        }
        let inner = inflate_fb2_zip_bytes(bytes)?;
        let idx = build_fb2_index_from_bytes(file_name, &inner, true)?;
        return Ok((idx, Some(inner), None));
    }
    let idx = build_plain_or_md_index(file_name, bytes)?;
    Ok((idx, None, None))
}

fn build_epub_index_from_entries<F>(
    file_name: &str,
    entries: Vec<ZipEntryMeta>,
    mut read_by_path: F,
) -> Result<BookIndex, EbookError>
where
    F: FnMut(&str) -> Option<Vec<u8>>,
{
    if let Some(enc_bytes) = read_by_path("META-INF/encryption.xml") {
        if is_epub_drm_encrypted_bytes(&enc_bytes) {
            return Err(EbookError::DrmProtected);
        }
    }

    let find_entry = |target: &str| -> Option<&ZipEntryMeta> {
        let norm = normalize_zip_path(target);
        if let Some(e) = entries.iter().find(|e| e.name == norm) {
            return Some(e);
        }
        let low = norm.to_lowercase();
        entries.iter().find(|e| e.name.to_lowercase() == low)
    };

    let opf_path = {
        let mut found = None;
        if let Some(cbytes) = read_by_path("META-INF/container.xml") {
            let xml = decode_bytes_auto(&cbytes, None);
            for tok in MarkupTokenizer::new(&xml) {
                if let MarkupToken::StartTag {
                    name_lower,
                    attrs_raw,
                    ..
                } = tok
                {
                    if name_lower == "rootfile" {
                        if let Some(fp) = get_attr(attrs_raw, "full-path") {
                            let norm = normalize_zip_path(&fp);
                            if !norm.is_empty() {
                                found = Some(norm);
                                break;
                            }
                        }
                    }
                }
            }
        }
        found.or_else(|| {
            entries
                .iter()
                .find(|e| e.name.to_ascii_lowercase().ends_with(".opf"))
                .map(|e| e.name.clone())
        })
        .ok_or_else(|| EbookError::InvalidFormat("Missing EPUB OPF package".into()))?
    };

    let opf_bytes = read_by_path(&opf_path)
        .ok_or_else(|| EbookError::InvalidFormat("Cannot read EPUB OPF file".into()))?;
    let opf_xml = decode_bytes_auto(&opf_bytes, None);

    struct OpfItem {
        href: String,
        media_type: String,
        properties: String,
    }
    let mut book_title = String::new();
    let mut book_author = String::new();
    let mut cover_meta_id: Option<String> = None;
    let mut manifest: HashMap<String, OpfItem> = HashMap::new();
    let mut spine_ids: Vec<String> = Vec::new();
    let mut ncx_id: Option<String> = None;
    let mut cur_meta_tag: Option<&'static str> = None;
    let mut meta_buf = String::new();

    for tok in MarkupTokenizer::new(&opf_xml) {
        match tok {
            MarkupToken::StartTag {
                name_lower,
                attrs_raw,
                self_closing,
            } => match name_lower.as_str() {
                "title" if book_title.is_empty() && !self_closing => {
                    cur_meta_tag = Some("title");
                    meta_buf.clear();
                }
                "creator" if book_author.is_empty() && !self_closing => {
                    cur_meta_tag = Some("creator");
                    meta_buf.clear();
                }
                "meta" => {
                    if let (Some(n), Some(c)) =
                        (get_attr(attrs_raw, "name"), get_attr(attrs_raw, "content"))
                    {
                        if n.eq_ignore_ascii_case("cover") && !c.trim().is_empty() {
                            cover_meta_id = Some(c.trim().to_string());
                        }
                    }
                }
                "item" => {
                    if let (Some(id), Some(href)) =
                        (get_attr(attrs_raw, "id"), get_attr(attrs_raw, "href"))
                    {
                        let (mut resolved, _) = resolve_relative_href(&opf_path, &href);
                        if find_entry(&resolved).is_none() {
                            let (root_rel, _) = resolve_relative_href("", &href);
                            if find_entry(&root_rel).is_some() {
                                resolved = root_rel;
                            }
                        }
                        manifest.insert(
                            id,
                            OpfItem {
                                href: resolved,
                                media_type: get_attr(attrs_raw, "media-type")
                                    .unwrap_or_default()
                                    .to_ascii_lowercase(),
                                properties: get_attr(attrs_raw, "properties")
                                    .unwrap_or_default()
                                    .to_ascii_lowercase(),
                            },
                        );
                    }
                }
                "spine" => {
                    if let Some(t) = get_attr(attrs_raw, "toc") {
                        if !t.trim().is_empty() {
                            ncx_id = Some(t.trim().to_string());
                        }
                    }
                }
                "itemref" => {
                    if let Some(idref) = get_attr(attrs_raw, "idref") {
                        let linear = get_attr(attrs_raw, "linear").unwrap_or_default();
                        if !linear.eq_ignore_ascii_case("no") || spine_ids.is_empty() {
                            spine_ids.push(idref);
                        }
                    }
                }
                _ => {}
            },
            MarkupToken::Text(t) => {
                if cur_meta_tag.is_some() {
                    meta_buf.push_str(t);
                }
            }
            MarkupToken::EndTag { name_lower } => {
                if let Some(active) = cur_meta_tag {
                    if active == name_lower {
                        let clean = collapse_inline_ws(&meta_buf, true).trim().to_string();
                        if !clean.is_empty() {
                            if active == "title" && book_title.is_empty() {
                                book_title = clean;
                            } else if active == "creator" && book_author.is_empty() {
                                book_author = clean;
                            }
                        }
                        cur_meta_tag = None;
                    }
                }
            }
        }
    }

    if spine_ids.is_empty() {
        for (id, item) in &manifest {
            if item.media_type.contains("html") || item.media_type.contains("xml") {
                spine_ids.push(id.clone());
            }
        }
    }

    // Build lazy Image Manifest from ZIP Central Directory headers (zero image decompression!)
    let mut images: Vec<IndexImageEntry> = Vec::new();
    for ze in &entries {
        let low = ze.name.to_ascii_lowercase();
        let is_img = low.ends_with(".jpg")
            || low.ends_with(".jpeg")
            || low.ends_with(".png")
            || low.ends_with(".gif")
            || low.ends_with(".bmp")
            || manifest
                .values()
                .any(|m| m.href == ze.name && m.media_type.starts_with("image/"));
        if is_img && (ze.method == 0 || ze.method == 8) {
            let idx = images.len();
            images.push(IndexImageEntry {
                id_or_path: ze.name.clone(),
                id_hash: hash_str_id(&ze.name, idx) ^ ((ze.uncompressed_size as u64) << 16),
                byte_offset: ze.local_header_offset as u64,
                compressed_size: ze.compressed_size as u32,
                uncompressed_size: ze.uncompressed_size as u32,
                method: ze.method,
                width: 0,
                height: 0,
            });
        }
    }

    let cover_href = manifest
        .values()
        .find(|m| m.properties.split_whitespace().any(|p| p == "cover-image"))
        .map(|m| m.href.clone())
        .or_else(|| {
            cover_meta_id
                .as_ref()
                .and_then(|cid| manifest.get(cid))
                .map(|m| m.href.clone())
        });
    let cover_img_idx = cover_href.and_then(|ch| {
        images
            .iter()
            .position(|im| im.id_or_path == ch || im.id_or_path.eq_ignore_ascii_case(&ch))
    });

    // Build Spine Chapter Table from Central Directory sizes (zero chapter decompression!)
    let mut chapters: Vec<SpineChapterMeta> = Vec::new();
    let mut spine_path_to_idx: HashMap<String, u32> = HashMap::new();
    for id in &spine_ids {
        let Some(mitem) = manifest.get(id) else {
            continue;
        };
        let Some(ze) = find_entry(&mitem.href) else {
            continue;
        };
        if ze.method != 0 && ze.method != 8 {
            return Err(EbookError::UnsupportedCompression(format!(
                "ZIP method {}",
                ze.method
            )));
        }
        let ch_idx = chapters.len() as u32;
        spine_path_to_idx
            .entry(ze.name.clone())
            .or_insert(ch_idx);
        spine_path_to_idx
            .entry(ze.name.to_lowercase())
            .or_insert(ch_idx);
        let est_chars = ((ze.uncompressed_size as u64 * 5) / 9).clamp(64, 5_000_000) as u32;
        let aux_u16 = if ch_idx == 0 {
            cover_img_idx.map(|i| (i + 1) as u16).unwrap_or(0)
        } else {
            0
        };
        chapters.push(SpineChapterMeta {
            path_or_id: ze.name.clone(),
            byte_offset: ze.local_header_offset as u64,
            compressed_size: ze.compressed_size as u32,
            uncompressed_size: ze.uncompressed_size as u32,
            method: ze.method,
            aux_u16,
            aux_u32: 0,
            char_count: est_chars,
            cum_char_offset: 0,
            verified: false,
        });
    }
    if chapters.is_empty() {
        return Err(EbookError::InvalidFormat("EPUB spine has no readable chapters".into()));
    }

    // Parse ONLY `nav.xhtml` or `toc.ncx` for the Table of Contents!
    let mut raw_toc: Vec<RawTocItem> = Vec::new();
    if let Some(nav_item) = manifest
        .values()
        .find(|m| m.properties.split_whitespace().any(|p| p == "nav"))
    {
        if let Some(nav_b) = read_by_path(&nav_item.href) {
            let nav_html = decode_bytes_auto(&nav_b, None);
            raw_toc = parse_epub3_nav_toc(&nav_item.href, &nav_html);
        }
    }
    if raw_toc.is_empty() {
        let ncx_path = ncx_id
            .as_ref()
            .and_then(|id| manifest.get(id))
            .map(|m| m.href.clone())
            .or_else(|| {
                manifest
                    .values()
                    .find(|m| {
                        m.media_type.contains("dtbncx")
                            || m.href.to_ascii_lowercase().ends_with(".ncx")
                    })
                    .map(|m| m.href.clone())
            });
        if let Some(np) = ncx_path {
            if let Some(ncx_b) = read_by_path(&np) {
                let ncx_xml = decode_bytes_auto(&ncx_b, None);
                raw_toc = parse_epub2_ncx_toc(&np, &ncx_xml);
            }
        }
    }

    let mut toc: Vec<IndexTocEntry> = Vec::with_capacity(raw_toc.len());
    for item in raw_toc {
        let title = item.title.trim().to_string();
        if title.is_empty() {
            continue;
        }
        let target_ch = spine_path_to_idx
            .get(&item.target_path)
            .or_else(|| spine_path_to_idx.get(&item.target_path.to_lowercase()))
            .copied();
        if let Some(ch_idx) = target_ch {
            let frag = item.fragment.unwrap_or_default();
            if let Some(prev) = toc.last() {
                if prev.title == title
                    && prev.target_chapter_idx == ch_idx
                    && prev.fragment == frag
                {
                    continue;
                }
            }
            toc.push(IndexTocEntry {
                title,
                level: item.level.min(4),
                target_chapter_idx: ch_idx,
                target_char_offset: 0,
                fragment: frag,
            });
        }
    }

    let mut index = BookIndex {
        title: if book_title.is_empty() {
            clean_stem_title(file_name)
        } else {
            book_title
        },
        author: book_author,
        format: DocFormat::Epub,
        detected_encoding: 0,
        mobi_extra_flags: 0,
        chapters,
        toc,
        images,
        footnotes: Vec::new(),
        footnote_keys: Vec::new(),
        saved_anchor: SavedReadingAnchor::default(),
        cached_chapter: None,
    };
    index.recompute_cumulative_offsets();
    Ok(index)
}

fn inflate_fb2_zip_file(path: &Path) -> Result<Vec<u8>, EbookError> {
    let f = File::open(path).map_err(|e| EbookError::InvalidFormat(e.to_string()))?;
    let mut zip = FileZipArchive::open(f).map_err(|e| EbookError::InvalidFormat(e.to_string()))?;
    let idx = zip
        .entries()
        .iter()
        .position(|e| e.name.to_ascii_lowercase().ends_with(".fb2"))
        .or_else(|| {
            zip.entries()
                .iter()
                .position(|e| e.name.to_ascii_lowercase().ends_with(".xml"))
        })
        .or_else(|| if !zip.entries().is_empty() { Some(0) } else { None })
        .ok_or_else(|| EbookError::InvalidFormat("Empty .fb2.zip archive".into()))?;
    zip.read_by_index(idx)
        .map_err(|e| EbookError::InvalidFormat(e.to_string()))
}

fn inflate_fb2_zip_bytes(bytes: &[u8]) -> Result<Vec<u8>, EbookError> {
    let zip = ZipArchive::parse(bytes).map_err(|e| EbookError::InvalidFormat(e.to_string()))?;
    let idx = zip
        .entries()
        .iter()
        .position(|e| e.name.to_ascii_lowercase().ends_with(".fb2"))
        .or_else(|| {
            zip.entries()
                .iter()
                .position(|e| e.name.to_ascii_lowercase().ends_with(".xml"))
        })
        .or_else(|| if !zip.entries().is_empty() { Some(0) } else { None })
        .ok_or_else(|| EbookError::InvalidFormat("Empty .fb2.zip archive".into()))?;
    zip.read_by_index(idx)
        .map_err(|e| EbookError::InvalidFormat(e.to_string()))
}

/// Fast byte-level `.fb2` indexer: scans `<description>`, top-level `<section>` byte offsets,
/// `<body name="notes">`, and `<binary id="...">` Base64 offsets with **zero Base64 image decoding**!
fn build_fb2_index_from_bytes(
    file_name: &str,
    raw: &[u8],
    is_zip_inner: bool,
) -> Result<BookIndex, EbookError> {
    if raw.is_empty() {
        return Err(EbookError::InvalidFormat("Empty FB2 document".into()));
    }
    let enc = detect_xml_encoding(raw);
    if matches!(enc, TextEncoding::Utf16Le | TextEncoding::Utf16Be) {
        // Rare UTF-16 FB2 fallback: transcode once and wrap as single-doc index
        let full_doc = crate::fb2::parse_fb2_or_zip(file_name, raw)?;
        let s = BookSession::from_single_doc(full_doc, 0);
        return Ok(s.index);
    }

    let mut book_title = String::new();
    let mut author_parts: Vec<String> = Vec::new();
    let mut cover_id: Option<String> = None;

    // 1. Parse `<description>...</description>` (~2–12 KB)
    if let Some(desc_end) = find_ascii_ci(raw, 0, b"</description>") {
        let desc_xml = decode_bytes_auto(&raw[..desc_end + 14], Some(enc));
        let mut in_title_info = false;
        let mut in_cover = false;
        let mut in_author = false;
        let mut field: Option<&'static str> = None;
        let mut buf = String::new();
        for tok in MarkupTokenizer::new(&desc_xml) {
            match tok {
                MarkupToken::StartTag {
                    name_lower,
                    attrs_raw,
                    self_closing,
                } => match name_lower.as_str() {
                    "title-info" if !self_closing => in_title_info = true,
                    "coverpage" if in_title_info && !self_closing => in_cover = true,
                    "image" if in_cover && cover_id.is_none() => {
                        if let Some(h) = get_attr(attrs_raw, "href") {
                            let id = h.trim().trim_start_matches('#').to_string();
                            if !id.is_empty() {
                                cover_id = Some(id);
                            }
                        }
                    }
                    "book-title" if in_title_info && !self_closing => {
                        field = Some("title");
                        buf.clear();
                    }
                    "author" if in_title_info && !self_closing => in_author = true,
                    "first-name" | "middle-name" | "last-name" | "nickname"
                        if in_author && !self_closing =>
                    {
                        field = Some("author");
                        buf.clear();
                    }
                    _ => {}
                },
                MarkupToken::Text(t) => {
                    if field.is_some() {
                        buf.push_str(t);
                    }
                }
                MarkupToken::EndTag { name_lower } => match name_lower.as_str() {
                    "book-title" if field == Some("title") => {
                        let c = collapse_inline_ws(&buf, true).trim().to_string();
                        if !c.is_empty() && book_title.is_empty() {
                            book_title = c;
                        }
                        field = None;
                    }
                    "first-name" | "middle-name" | "last-name" | "nickname"
                        if field == Some("author") =>
                    {
                        let c = collapse_inline_ws(&buf, true).trim().to_string();
                        if !c.is_empty() && author_parts.len() < 4 {
                            author_parts.push(c);
                        }
                        field = None;
                    }
                    "author" => in_author = false,
                    "coverpage" => in_cover = false,
                    "title-info" => {
                        in_title_info = false;
                        in_author = false;
                        in_cover = false;
                    }
                    _ => {}
                },
            }
        }
    }

    // 2. Scan `<body>` ranges & `<binary id="...">` byte offsets linearly in raw bytes!
    let mut main_body_range: Option<(usize, usize)> = None;
    let mut notes_ranges: Vec<(usize, usize)> = Vec::new();
    let mut images: Vec<IndexImageEntry> = Vec::new();

    let mut pos = 0usize;
    while pos < raw.len() {
        let Some(lt_rel) = raw[pos..].iter().position(|&b| b == b'<') else {
            break;
        };
        let tag_start = pos + lt_rel;
        if tag_start + 5 >= raw.len() {
            break;
        }
        let next_b = raw[tag_start + 1].to_ascii_lowercase();
        if next_b == b'b' {
            if starts_with_tag_ci(&raw[tag_start..], b"<body") {
                if let Some(gt_rel) = raw[tag_start..].iter().position(|&b| b == b'>') {
                    let open_end = tag_start + gt_rel + 1;
                    let open_hdr = String::from_utf8_lossy(&raw[tag_start..open_end]);
                    let low_hdr = open_hdr.to_ascii_lowercase();
                    let is_notes = low_hdr.contains("\"notes\"")
                        || low_hdr.contains("'notes'")
                        || low_hdr.contains("\"comments\"")
                        || low_hdr.contains("'comments'")
                        || low_hdr.contains("\"footnotes\"");
                    if let Some(body_close) = find_ascii_ci(raw, open_end, b"</body>") {
                        if is_notes {
                            notes_ranges.push((open_end, body_close));
                        } else if main_body_range.is_none() {
                            main_body_range = Some((open_end, body_close));
                        }
                        pos = body_close + 7;
                        continue;
                    }
                }
            } else if starts_with_tag_ci(&raw[tag_start..], b"<binary") {
                if let Some(gt_rel) = raw[tag_start..].iter().position(|&b| b == b'>') {
                    let open_end = tag_start + gt_rel + 1;
                    let attrs = String::from_utf8_lossy(&raw[tag_start + 7..open_end - 1]);
                    if let Some(bin_close) = find_ascii_ci(raw, open_end, b"</binary>") {
                        if let Some(id) = get_attr(&attrs, "id") {
                            let clean_id = id.trim().trim_start_matches('#').to_string();
                            if !clean_id.is_empty() && bin_close > open_end {
                                let idx = images.len();
                                let method = if is_zip_inner {
                                    image_method::FB2_ZIP_BASE64_SLICE
                                } else {
                                    image_method::FB2_BASE64_SLICE
                                };
                                images.push(IndexImageEntry {
                                    id_hash: hash_str_id(&clean_id, idx)
                                        ^ (((bin_close - open_end) as u64) << 16),
                                    id_or_path: clean_id,
                                    byte_offset: open_end as u64,
                                    compressed_size: (bin_close - open_end) as u32,
                                    uncompressed_size: ((bin_close - open_end) * 3 / 4) as u32,
                                    method,
                                    width: 0,
                                    height: 0,
                                });
                            }
                        }
                        pos = bin_close + 9;
                        continue;
                    }
                }
            }
        }
        pos = tag_start + 1;
    }

    let (body_start, body_end) = main_body_range
        .ok_or_else(|| EbookError::InvalidFormat("Missing FB2 <body>".into()))?;

    // 3. Parse `<body name="notes">` (compact text only, zero binary images)
    let mut footnotes: Vec<FootnoteDef> = Vec::new();
    let mut footnote_keys: Vec<String> = Vec::new();
    for (ns, ne) in notes_ranges {
        let nxml = decode_bytes_auto(&raw[ns..ne], Some(enc));
        let mut sec_stack: Vec<(Option<String>, String, Vec<String>)> = Vec::new();
        let mut in_ntitle = false;
        let mut pbuf = String::new();
        for tok in MarkupTokenizer::new(&nxml) {
            match tok {
                MarkupToken::StartTag {
                    name_lower,
                    attrs_raw,
                    self_closing,
                } => match name_lower.as_str() {
                    "section" if !self_closing => {
                        let id = get_attr(attrs_raw, "id")
                            .map(|s| s.trim().trim_start_matches('#').to_string())
                            .filter(|s| !s.is_empty());
                        sec_stack.push((id, String::new(), Vec::new()));
                    }
                    "title" if !sec_stack.is_empty() && !self_closing => {
                        in_ntitle = true;
                        pbuf.clear();
                    }
                    "p" | "v" | "subtitle" if !sec_stack.is_empty() && !self_closing => {
                        pbuf.clear();
                    }
                    _ => {}
                },
                MarkupToken::Text(t) => {
                    if !sec_stack.is_empty() {
                        pbuf.push_str(t);
                    }
                }
                MarkupToken::EndTag { name_lower } => match name_lower.as_str() {
                    "title" => {
                        in_ntitle = false;
                        let c = collapse_inline_ws(&pbuf, true).trim().to_string();
                        if let Some((_, ref mut ts, _)) = sec_stack.last_mut() {
                            if !c.is_empty() {
                                *ts = c;
                            }
                        }
                        pbuf.clear();
                    }
                    "p" | "v" | "subtitle" if !in_ntitle => {
                        let c = collapse_inline_ws(&pbuf, true).trim().to_string();
                        if !c.is_empty() {
                            if let Some((_, _, ref mut ps)) = sec_stack.last_mut() {
                                ps.push(c);
                            }
                        }
                        pbuf.clear();
                    }
                    "section" => {
                        if let Some((Some(id), ntitle, paras)) = sec_stack.pop() {
                            if !paras.is_empty() || !ntitle.is_empty() {
                                let idx = footnotes.len();
                                let badge = format_footnote_badge(&ntitle, idx + 1);
                                let heading = if ntitle.is_empty() {
                                    badge.clone()
                                } else {
                                    format!("{badge}  {ntitle}")
                                };
                                footnote_keys.push(id);
                                footnotes.push(FootnoteDef {
                                    index: idx,
                                    label: badge,
                                    title: heading,
                                    paragraphs: if paras.is_empty() {
                                        vec![ntitle]
                                    } else {
                                        paras
                                    },
                                });
                            }
                        }
                    }
                    _ => {}
                },
            }
        }
    }

    // 4. Split main `<body>` into top-level `<section>...</section>` chapters + extract TOC titles!
    let ch_method = if is_zip_inner {
        chapter_method::FB2_ZIP_INNER_SLICE
    } else {
        chapter_method::FB2_RAW_SLICE
    };
    let cover_idx = cover_id.and_then(|cid| {
        let low = cid.to_ascii_lowercase();
        images
            .iter()
            .position(|im| im.id_or_path == cid || im.id_or_path.to_ascii_lowercase() == low)
    });

    let mut chapters: Vec<SpineChapterMeta> = Vec::new();
    let mut toc: Vec<IndexTocEntry> = Vec::new();
    let section_ranges = scan_top_level_fb2_sections(&raw[body_start..body_end]);

    if section_ranges.is_empty() {
        let len = (body_end - body_start).max(1);
        chapters.push(SpineChapterMeta {
            path_or_id: "body".into(),
            byte_offset: body_start as u64,
            compressed_size: len as u32,
            uncompressed_size: len as u32,
            method: ch_method,
            aux_u16: cover_idx.map(|i| (i + 1) as u16).unwrap_or(0),
            aux_u32: 0,
            char_count: ((len * 5) / 9).max(32) as u32,
            cum_char_offset: 0,
            verified: false,
        });
    } else {
        for (si, (rel_s, rel_e)) in section_ranges.into_iter().enumerate() {
            let abs_s = body_start + rel_s;
            let abs_e = body_start + rel_e;
            let len = abs_e.saturating_sub(abs_s).max(1);
            let ch_idx = chapters.len() as u32;
            // Peek first 1.5 KB of section to extract `<title>` for TOC!
            let peek_end = (abs_s + 1536).min(abs_e);
            let peek_xml = decode_bytes_auto(&raw[abs_s..peek_end], Some(enc));
            if let Some(sec_title) = extract_fb2_section_title(&peek_xml) {
                toc.push(IndexTocEntry {
                    title: sec_title,
                    level: 0,
                    target_chapter_idx: ch_idx,
                    target_char_offset: 0,
                    fragment: String::new(),
                });
            }
            chapters.push(SpineChapterMeta {
                path_or_id: format!("sec{si}"),
                byte_offset: abs_s as u64,
                compressed_size: len as u32,
                uncompressed_size: len as u32,
                method: ch_method,
                aux_u16: if si == 0 {
                    cover_idx.map(|i| (i + 1) as u16).unwrap_or(0)
                } else {
                    0
                },
                aux_u32: 0,
                char_count: ((len * 5) / 9).max(32) as u32,
                cum_char_offset: 0,
                verified: false,
            });
        }
    }

    let mut index = BookIndex {
        title: if book_title.is_empty() {
            clean_stem_title(file_name)
        } else {
            book_title
        },
        author: author_parts.join(" "),
        format: DocFormat::Fb2,
        detected_encoding: enc.to_u8(),
        mobi_extra_flags: 0,
        chapters,
        toc,
        images,
        footnotes,
        footnote_keys,
        saved_anchor: SavedReadingAnchor::default(),
        cached_chapter: None,
    };
    index.recompute_cumulative_offsets();
    Ok(index)
}

fn scan_top_level_fb2_sections(body_bytes: &[u8]) -> Vec<(usize, usize)> {
    let mut ranges = Vec::new();
    let mut depth = 0usize;
    let mut cur_sec_start = 0usize;
    let mut pos = 0usize;

    while pos < body_bytes.len() {
        let Some(lt_rel) = body_bytes[pos..].iter().position(|&b| b == b'<') else {
            break;
        };
        let tag_pos = pos + lt_rel;
        let rem = &body_bytes[tag_pos..];
        if starts_with_tag_ci(rem, b"<section") {
            let gt_rel = rem.iter().position(|&b| b == b'>').unwrap_or(rem.len() - 1);
            let self_closing = gt_rel > 0 && rem[gt_rel - 1] == b'/';
            if !self_closing {
                if depth == 0 {
                    cur_sec_start = tag_pos;
                }
                depth += 1;
            }
            pos = tag_pos + gt_rel + 1;
            continue;
        }
        if starts_with_tag_ci(rem, b"</section") {
            let gt_rel = rem.iter().position(|&b| b == b'>').unwrap_or(rem.len() - 1);
            let close_end = tag_pos + gt_rel + 1;
            if depth > 0 {
                depth -= 1;
                if depth == 0 {
                    ranges.push((cur_sec_start, close_end));
                }
            }
            pos = close_end;
            continue;
        }
        pos = tag_pos + 1;
    }
    ranges
}

fn extract_fb2_section_title(peek_xml: &str) -> Option<String> {
    let mut in_title = false;
    let mut sec_depth = 0usize;
    let mut lines: Vec<String> = Vec::new();
    let mut cur = String::new();
    for tok in MarkupTokenizer::new(peek_xml) {
        match tok {
            MarkupToken::StartTag {
                name_lower,
                self_closing,
                ..
            } => {
                if name_lower == "section" && !self_closing {
                    sec_depth += 1;
                    if sec_depth > 1 {
                        break;
                    }
                } else if name_lower == "title" && sec_depth == 1 && !self_closing {
                    in_title = true;
                } else if in_title && matches!(name_lower.as_str(), "p" | "v") {
                    let c = collapse_inline_ws(&cur, true).trim().to_string();
                    if !c.is_empty() {
                        lines.push(c);
                        cur.clear();
                    }
                }
            }
            MarkupToken::Text(t) => {
                if in_title {
                    cur.push_str(t);
                }
            }
            MarkupToken::EndTag { name_lower } => {
                if in_title && matches!(name_lower.as_str(), "p" | "v") {
                    let c = collapse_inline_ws(&cur, true).trim().to_string();
                    if !c.is_empty() {
                        lines.push(c);
                        cur.clear();
                    }
                } else if name_lower == "title" && in_title {
                    let c = collapse_inline_ws(&cur, true).trim().to_string();
                    if !c.is_empty() {
                        lines.push(c);
                    }
                    break;
                }
            }
        }
    }
    if lines.is_empty() {
        None
    } else {
        Some(lines.join(" — "))
    }
}

#[inline]
fn starts_with_tag_ci(rem: &[u8], tag_lower: &[u8]) -> bool {
    if rem.len() <= tag_lower.len() {
        return false;
    }
    for (i, &tb) in tag_lower.iter().enumerate() {
        if rem[i].to_ascii_lowercase() != tb {
            return false;
        }
    }
    matches!(rem[tag_lower.len()], b'>' | b'/' | b' ' | b'\t' | b'\r' | b'\n')
}

fn find_ascii_ci(haystack: &[u8], from: usize, needle_lower: &[u8]) -> Option<usize> {
    if needle_lower.is_empty() || from + needle_lower.len() > haystack.len() {
        return None;
    }
    let first = needle_lower[0];
    let last_start = haystack.len() - needle_lower.len();
    let mut i = from;
    while i <= last_start {
        if haystack[i].to_ascii_lowercase() == first {
            let mut matched = true;
            for j in 1..needle_lower.len() {
                if haystack[i + j].to_ascii_lowercase() != needle_lower[j] {
                    matched = false;
                    break;
                }
            }
            if matched {
                return Some(i);
            }
        }
        i += 1;
    }
    None
}

/* ---- Lazy `.mobi` / `.prc` / `.azw` Index Builder ----------------------- */

fn build_mobi_index_from_reader<R: Read + Seek>(
    file_name: &str,
    reader: &mut R,
    file_size: u64,
) -> Result<(BookIndex, Option<String>), EbookError> {
    if file_size < 86 {
        return Err(EbookError::InvalidFormat(
            "File too short for PalmDB/MOBI".into(),
        ));
    }
    let mut pdb_hdr = [0u8; 78];
    reader
        .read_exact(&mut pdb_hdr)
        .map_err(|e| EbookError::InvalidFormat(e.to_string()))?;

    let pdb_name = String::from_utf8_lossy(&pdb_hdr[0..32])
        .trim_end_matches('\0')
        .trim()
        .to_string();
    let magic_type = &pdb_hdr[60..68];
    if magic_type != b"BOOKMOBI" && magic_type != b"TEXtREAd" {
        return Err(EbookError::InvalidFormat(
            "Invalid PalmDB type (expected BOOKMOBI or TEXtREAd)".into(),
        ));
    }
    let num_records = u16::from_be_bytes([pdb_hdr[76], pdb_hdr[77]]) as usize;
    if num_records < 2 || (78 + num_records * 8) as u64 > file_size {
        return Err(EbookError::InvalidFormat("Invalid PalmDB record table".into()));
    }

    let mut rec_tbl = vec![0u8; num_records * 8];
    reader
        .read_exact(&mut rec_tbl)
        .map_err(|e| EbookError::InvalidFormat(e.to_string()))?;
    let mut rec_offsets = Vec::with_capacity(num_records + 1);
    for i in 0..num_records {
        let p = i * 8;
        let off = u32::from_be_bytes([rec_tbl[p], rec_tbl[p + 1], rec_tbl[p + 2], rec_tbl[p + 3]])
            as u64;
        rec_offsets.push(off.min(file_size));
    }
    rec_offsets.push(file_size);

    let rec0_len = rec_offsets[1].saturating_sub(rec_offsets[0]) as usize;
    if rec0_len < 16 || rec0_len > 2 * 1024 * 1024 {
        return Err(EbookError::InvalidFormat("Invalid MOBI Record 0".into()));
    }
    reader
        .seek(SeekFrom::Start(rec_offsets[0]))
        .map_err(|e| EbookError::InvalidFormat(e.to_string()))?;
    let mut rec0 = vec![0u8; rec0_len];
    reader
        .read_exact(&mut rec0)
        .map_err(|e| EbookError::InvalidFormat(e.to_string()))?;

    let compression = u16::from_be_bytes([rec0[0], rec0[1]]);
    let text_rec_count = u16::from_be_bytes([rec0[8], rec0[9]]) as usize;
    let encryption_type = u16::from_be_bytes([rec0[12], rec0[13]]);

    if encryption_type != 0 {
        return Err(EbookError::DrmProtected);
    }
    if compression == 17480 {
        return Err(EbookError::UnsupportedCompression("MOBI Huff/CDIC".into()));
    }
    if compression != 1 && compression != 2 {
        return Err(EbookError::InvalidFormat(format!(
            "Unknown PalmDOC compression ({compression})"
        )));
    }

    let mut text_enc = TextEncoding::Windows1252;
    let mut first_image_rec: Option<usize> = None;
    let mut extra_data_flags: u16 = 0;
    let mut full_title = String::new();
    let mut author = String::new();
    let mut exth_cover_offset: Option<usize> = None;

    if rec0.len() >= 36 && &rec0[16..20] == b"MOBI" {
        let mobi_hdr_len = u32::from_be_bytes([rec0[20], rec0[21], rec0[22], rec0[23]]) as usize;
        let codepage = u32::from_be_bytes([rec0[28], rec0[29], rec0[30], rec0[31]]);
        text_enc = match codepage {
            65001 => TextEncoding::Utf8,
            1251 => TextEncoding::Windows1251,
            _ => TextEncoding::Windows1252,
        };
        if rec0.len() >= 92 {
            let fn_off = u32::from_be_bytes([rec0[84], rec0[85], rec0[86], rec0[87]]) as usize;
            let fn_len = u32::from_be_bytes([rec0[88], rec0[89], rec0[90], rec0[91]]) as usize;
            if fn_len > 0 && fn_off + fn_len <= rec0.len() {
                full_title = decode_bytes_auto(&rec0[fn_off..fn_off + fn_len], Some(text_enc))
                    .trim()
                    .to_string();
            }
        }
        if rec0.len() >= 112 {
            let img_idx = u32::from_be_bytes([rec0[108], rec0[109], rec0[110], rec0[111]]);
            if img_idx != 0xFFFF_FFFF && (img_idx as usize) < num_records {
                first_image_rec = Some(img_idx as usize);
            }
        }
        if mobi_hdr_len >= 0xF4 && rec0.len() >= 16 + 0xF4 {
            extra_data_flags = u16::from_be_bytes([rec0[16 + 0xF2], rec0[16 + 0xF3]]);
        }
        let exth_flags = if rec0.len() >= 132 {
            u32::from_be_bytes([rec0[128], rec0[129], rec0[130], rec0[131]])
        } else {
            0
        };
        let exth_off = 16 + mobi_hdr_len;
        if (exth_flags & 0x40) != 0
            && exth_off + 12 <= rec0.len()
            && &rec0[exth_off..exth_off + 4] == b"EXTH"
        {
            let exth_cnt = u32::from_be_bytes([
                rec0[exth_off + 8],
                rec0[exth_off + 9],
                rec0[exth_off + 10],
                rec0[exth_off + 11],
            ]) as usize;
            let mut cur = exth_off + 12;
            for _ in 0..exth_cnt {
                if cur + 8 > rec0.len() {
                    break;
                }
                let rtype =
                    u32::from_be_bytes([rec0[cur], rec0[cur + 1], rec0[cur + 2], rec0[cur + 3]]);
                let rlen = u32::from_be_bytes([
                    rec0[cur + 4],
                    rec0[cur + 5],
                    rec0[cur + 6],
                    rec0[cur + 7],
                ]) as usize;
                if rlen < 8 || cur + rlen > rec0.len() {
                    break;
                }
                let pl = &rec0[cur + 8..cur + rlen];
                match rtype {
                    100 if author.is_empty() => {
                        author = decode_bytes_auto(pl, Some(text_enc)).trim().to_string();
                    }
                    503 => {
                        let upd = decode_bytes_auto(pl, Some(text_enc)).trim().to_string();
                        if !upd.is_empty() {
                            full_title = upd;
                        }
                    }
                    201 if pl.len() == 4 => {
                        let co = u32::from_be_bytes([pl[0], pl[1], pl[2], pl[3]]);
                        if co != 0xFFFF_FFFF {
                            exth_cover_offset = Some(co as usize);
                        }
                    }
                    _ => {}
                }
                cur += rlen;
            }
        }
    }

    // Build lazy Image Manifest from PDB record offsets (zero image record reads!)
    let mut images: Vec<IndexImageEntry> = Vec::new();
    if let Some(first_img) = first_image_rec {
        for r in first_img..num_records {
            let s = rec_offsets[r];
            let e = rec_offsets[r + 1];
            let len = e.saturating_sub(s) as u32;
            images.push(IndexImageEntry {
                id_or_path: format!("rec:{r}"),
                id_hash: hash_str_id("mobi_rec", r) ^ ((len as u64) << 16),
                byte_offset: s,
                compressed_size: len,
                uncompressed_size: len,
                method: image_method::RAW_FILE_SLICE,
                width: 0,
                height: 0,
            });
        }
    }

    // Read ONLY text records `1..=max_rec` (skipping all trailing MBs of embedded images!)
    let max_rec = text_rec_count.min(num_records.saturating_sub(1));
    let raw_text = decompress_mobi_records_range(
        reader,
        &rec_offsets,
        1,
        max_rec,
        compression,
        extra_data_flags,
    )?;

    let doc_title = if !full_title.is_empty() {
        full_title
    } else if !pdb_name.is_empty() {
        pdb_name
    } else {
        clean_stem_title(file_name)
    };

    if magic_type == b"TEXtREAd" && !looks_like_mobi_html(&raw_text) {
        let text = decode_bytes_auto(&raw_text, Some(text_enc));
        let mut idx = build_plain_or_md_index(&doc_title, text.as_bytes())?;
        idx.title = doc_title;
        idx.format = DocFormat::Mobi;
        return Ok((idx, None));
    }

    let decoded_html = decode_bytes_auto(&raw_text, Some(text_enc));
    let is_single_byte = text_enc != TextEncoding::Utf8;
    let char_to_str_byte: Vec<usize> = if is_single_byte {
        decoded_html.char_indices().map(|(bi, _)| bi).collect()
    } else {
        Vec::new()
    };
    let map_fp = |fp: usize| -> usize {
        if is_single_byte {
            if char_to_str_byte.is_empty() {
                0
            } else if fp >= char_to_str_byte.len() {
                decoded_html.len()
            } else {
                char_to_str_byte[fp]
            }
        } else {
            clamp_char_boundary(&decoded_html, fp)
        }
    };

    // Split `decoded_html` into chapter slices along `<mbp:pagebreak/>` or `<h1>` boundaries
    let split_points = scan_mobi_chapter_splits(&decoded_html);
    let ch_method = if compression == 1 {
        chapter_method::MOBI_UNCOMP_RECORDS
    } else {
        chapter_method::MOBI_LZ77_RECORDS
    };
    let cover_aux: u16 = match exth_cover_offset {
        Some(co) if co < images.len() => 0x8000 | (co as u16),
        _ => 0,
    };

    let mut chapters: Vec<SpineChapterMeta> = Vec::with_capacity(split_points.len());
    for (ci, &(s, e)) in split_points.iter().enumerate() {
        let len = e.saturating_sub(s).max(1);
        chapters.push(SpineChapterMeta {
            path_or_id: format!("mobi_ch{ci}"),
            byte_offset: s as u64,
            compressed_size: len as u32,
            uncompressed_size: len as u32,
            method: ch_method,
            aux_u16: if ci == 0 { cover_aux } else { 0 },
            aux_u32: 0,
            char_count: ((len * 5) / 8).max(32) as u32,
            cum_char_offset: 0,
            verified: false,
        });
    }

    // Scan `<a filepos="...">` links & `<h1>`/`<h2>` headings once for pre-resolved TOC + Footnotes!
    let mut footnotes: Vec<FootnoteDef> = Vec::new();
    let mut footnote_keys: Vec<String> = Vec::new();
    let mut raw_toc_links: Vec<(String, usize)> = Vec::new();
    let mut heading_toc: Vec<(String, u8, usize)> = Vec::new();

    {
        let mut cur_fp: Option<(usize, String)> = None;
        let mut lbl_buf = String::new();
        let mut cur_h: Option<(u8, usize)> = None;
        let mut h_buf = String::new();

        for tok in MarkupTokenizer::new(&decoded_html) {
            match tok {
                MarkupToken::StartTag {
                    name_lower,
                    attrs_raw,
                    self_closing,
                } => {
                    if name_lower == "a" && !self_closing {
                        if let Some(fp_str) = get_attr(attrs_raw, "filepos") {
                            if let Ok(fp_val) = fp_str.trim().parse::<usize>() {
                                cur_fp = Some((map_fp(fp_val), format!("fp:{fp_val}")));
                                lbl_buf.clear();
                            }
                        }
                    } else if matches!(name_lower.as_str(), "h1" | "h2") && !self_closing {
                        let approx = (attrs_raw.as_ptr() as usize)
                            .saturating_sub(decoded_html.as_ptr() as usize);
                        cur_h = Some((if name_lower == "h1" { 0 } else { 1 }, approx));
                        h_buf.clear();
                    }
                }
                MarkupToken::Text(t) => {
                    if cur_fp.is_some() {
                        lbl_buf.push_str(t);
                    }
                    if cur_h.is_some() {
                        h_buf.push_str(t);
                    }
                }
                MarkupToken::EndTag { name_lower } => {
                    if name_lower == "a" {
                        if let Some((target_str_idx, fp_key)) = cur_fp.take() {
                            let clean = collapse_inline_ws(&lbl_buf, true).trim().to_string();
                            let is_compact_fn = !clean.is_empty()
                                && clean.chars().count() <= 7
                                && clean.chars().any(|c| c.is_ascii_digit() || c == '*');
                            if is_compact_fn {
                                if !footnote_keys.iter().any(|k| k == &fp_key) {
                                    let snippet =
                                        extract_mobi_footnote_at(&decoded_html, target_str_idx);
                                    if !snippet.is_empty() {
                                        let idx = footnotes.len();
                                        let badge = format_footnote_badge(&clean, idx + 1);
                                        footnote_keys.push(fp_key);
                                        footnotes.push(FootnoteDef {
                                            index: idx,
                                            label: badge.clone(),
                                            title: badge,
                                            paragraphs: snippet,
                                        });
                                    }
                                }
                            } else if !clean.is_empty() {
                                raw_toc_links.push((clean, target_str_idx));
                            }
                        }
                    } else if matches!(name_lower.as_str(), "h1" | "h2") {
                        if let Some((lvl, pos_in_html)) = cur_h.take() {
                            let clean = collapse_inline_ws(&h_buf, true).trim().to_string();
                            if !clean.is_empty() {
                                heading_toc.push((clean, lvl, pos_in_html));
                            }
                        }
                    }
                }
            }
        }
    }

    let map_html_pos_to_chapter = |hpos: usize| -> (u32, u32) {
        let idx = match chapters.binary_search_by_key(&(hpos as u64), |c| c.byte_offset) {
            Ok(i) => i,
            Err(ins) => ins.saturating_sub(1),
        }
        .min(chapters.len().saturating_sub(1));
        let ch = &chapters[idx];
        let rel_bytes = hpos.saturating_sub(ch.byte_offset as usize);
        let approx_chars = ((rel_bytes * 5) / 8) as u32;
        (idx as u32, if rel_bytes < 32 { 0 } else { approx_chars })
    };

    let mut toc: Vec<IndexTocEntry> = Vec::new();
    if raw_toc_links.len() >= 2 {
        for (title, tpos) in raw_toc_links {
            let (ch_i, ch_off) = map_html_pos_to_chapter(tpos);
            toc.push(IndexTocEntry {
                title,
                level: 0,
                target_chapter_idx: ch_i,
                target_char_offset: ch_off,
                fragment: String::new(),
            });
        }
    } else {
        for (title, lvl, tpos) in heading_toc {
            let (ch_i, ch_off) = map_html_pos_to_chapter(tpos);
            toc.push(IndexTocEntry {
                title,
                level: lvl,
                target_chapter_idx: ch_i,
                target_char_offset: ch_off,
                fragment: String::new(),
            });
        }
    }

    let mut index = BookIndex {
        title: doc_title,
        author,
        format: DocFormat::Mobi,
        detected_encoding: text_enc.to_u8(),
        mobi_extra_flags: extra_data_flags,
        chapters,
        toc,
        images,
        footnotes,
        footnote_keys,
        saved_anchor: SavedReadingAnchor::default(),
        cached_chapter: None,
    };
    index.recompute_cumulative_offsets();
    Ok((index, Some(decoded_html)))
}

fn decompress_mobi_records_range<R: Read + Seek>(
    reader: &mut R,
    rec_offsets: &[u64],
    start_rec: usize,
    end_rec: usize,
    compression: u16,
    extra_flags: u16,
) -> Result<Vec<u8>, EbookError> {
    if start_rec > end_rec || end_rec + 1 >= rec_offsets.len() {
        return Ok(Vec::new());
    }
    let byte_start = rec_offsets[start_rec];
    let byte_end = rec_offsets[end_rec + 1];
    let span = byte_end.saturating_sub(byte_start) as usize;
    if span > 24 * 1024 * 1024 {
        return Err(EbookError::InvalidFormat("MOBI text stream exceeds 24 MiB".into()));
    }
    reader
        .seek(SeekFrom::Start(byte_start))
        .map_err(|e| EbookError::InvalidFormat(e.to_string()))?;
    let mut packed = vec![0u8; span];
    reader
        .read_exact(&mut packed)
        .map_err(|e| EbookError::InvalidFormat(e.to_string()))?;

    let mut raw_text = Vec::with_capacity((span * 2).min(16 * 1024 * 1024));
    for r in start_rec..=end_rec {
        let rs = (rec_offsets[r] - byte_start) as usize;
        let re = (rec_offsets[r + 1] - byte_start) as usize;
        if rs > re || re > packed.len() {
            continue;
        }
        let trimmed = strip_mobi_trailing_data(&packed[rs..re], extra_flags);
        if compression == 1 {
            raw_text.extend_from_slice(trimmed);
        } else {
            decompress_palmdoc_lz77(trimmed, &mut raw_text);
        }
        if raw_text.len() >= 16 * 1024 * 1024 {
            raw_text.truncate(16 * 1024 * 1024);
            break;
        }
    }
    Ok(raw_text)
}

fn decompress_mobi_html_from_file(path: &Path, enc: TextEncoding) -> Result<String, EbookError> {
    let mut f = File::open(path).map_err(|e| EbookError::InvalidFormat(e.to_string()))?;
    let file_size = f
        .metadata()
        .map(|m| m.len())
        .map_err(|e| EbookError::InvalidFormat(e.to_string()))?;
    let (_, html_opt) = build_mobi_index_from_reader("book.mobi", &mut f, file_size)?;
    let _ = enc;
    Ok(html_opt.unwrap_or_default())
}

fn decompress_mobi_html_from_slice(bytes: &[u8], enc: TextEncoding) -> Result<String, EbookError> {
    let mut cur = std::io::Cursor::new(bytes);
    let (_, html_opt) = build_mobi_index_from_reader("book.mobi", &mut cur, bytes.len() as u64)?;
    let _ = enc;
    Ok(html_opt.unwrap_or_default())
}

fn scan_mobi_chapter_splits(html: &str) -> Vec<(usize, usize)> {
    if html.len() <= MOBI_CHAPTER_TARGET_BYTES {
        return vec![(0, html.len())];
    }
    let bytes = html.as_bytes();
    let mut cuts = vec![0usize];
    let mut last_cut = 0usize;
    let mut i = 0usize;
    while i + 4 < bytes.len() {
        if bytes[i] == b'<' {
            let is_pb = starts_with_tag_ci(&bytes[i..], b"<mbp:pagebreak");
            let is_h1 = starts_with_tag_ci(&bytes[i..], b"<h1");
            let is_p = starts_with_tag_ci(&bytes[i..], b"<p");
            if is_pb && i.saturating_sub(last_cut) >= 1024 {
                let after = bytes[i..]
                    .iter()
                    .position(|&b| b == b'>')
                    .map(|g| i + g + 1)
                    .unwrap_or(i);
                let cut = clamp_char_boundary(html, after);
                if cut > last_cut {
                    cuts.push(cut);
                    last_cut = cut;
                    i = cut;
                    continue;
                }
            } else if (is_h1 && i.saturating_sub(last_cut) >= 4096)
                || (is_p && i.saturating_sub(last_cut) >= MOBI_CHAPTER_TARGET_BYTES)
            {
                let cut = clamp_char_boundary(html, i);
                if cut > last_cut {
                    cuts.push(cut);
                    last_cut = cut;
                }
            }
        }
        i += 1;
    }
    let mut out = Vec::with_capacity(cuts.len());
    for w in 0..cuts.len() {
        let s = cuts[w];
        let e = if w + 1 < cuts.len() {
            cuts[w + 1]
        } else {
            html.len()
        };
        if e > s {
            out.push((s, e));
        }
    }
    if out.is_empty() {
        out.push((0, html.len()));
    }
    out
}

fn build_plain_or_md_index(file_name: &str, raw: &[u8]) -> Result<BookIndex, EbookError> {
    let lower = file_name.trim().to_ascii_lowercase();
    let is_md = lower.ends_with(".md") || lower.ends_with(".markdown");
    let fmt = if is_md {
        DocFormat::Markdown
    } else {
        DocFormat::PlainText
    };

    // Keep single-chapter fast path for files <= 48 KB (preserves cross-reference footnotes in Markdown!)
    if raw.len() <= 48 * 1024 {
        let doc = plain_md::parse_plain_or_markdown(file_name, raw, is_md)?;
        let toc: Vec<IndexTocEntry> = doc
            .toc
            .iter()
            .map(|t| IndexTocEntry {
                title: t.title.clone(),
                level: t.level,
                target_chapter_idx: 0,
                target_char_offset: t.offset as u32,
                fragment: String::new(),
            })
            .collect();
        let mut idx = BookIndex {
            title: doc.title,
            author: doc.author,
            format: fmt,
            detected_encoding: 0,
            mobi_extra_flags: 0,
            chapters: vec![SpineChapterMeta {
                path_or_id: "ch0".into(),
                byte_offset: 0,
                compressed_size: raw.len() as u32,
                uncompressed_size: raw.len() as u32,
                method: chapter_method::PLAIN_RAW_SLICE,
                aux_u16: 0,
                aux_u32: 0,
                char_count: doc.total_chars.max(1) as u32,
                cum_char_offset: 0,
                verified: true,
            }],
            toc,
            images: Vec::new(),
            footnotes: doc.footnotes,
            footnote_keys: Vec::new(),
            saved_anchor: SavedReadingAnchor::default(),
            cached_chapter: Some(CachedChapterPayload {
                chapter_idx: 0,
                total_chars: doc.total_chars.max(1) as u32,
                blocks: doc.blocks,
            }),
        };
        idx.recompute_cumulative_offsets();
        return Ok(idx);
    }

    // Multi-chapter split along clean `\n\n` paragraph breaks for large `.txt` / `.md` logs/books
    let mut chapters = Vec::new();
    let mut start = 0usize;
    while start < raw.len() {
        let mut end = (start + PLAIN_CHAPTER_TARGET_BYTES).min(raw.len());
        if end < raw.len() {
            if let Some(nl) = raw[end..raw.len().min(end + 4096)]
                .windows(2)
                .position(|w| w == b"\n\n")
            {
                end += nl + 2;
            } else if let Some(nl) = raw[end..raw.len().min(end + 4096)]
                .iter()
                .position(|&b| b == b'\n')
            {
                end += nl + 1;
            }
        }
        let len = end.saturating_sub(start).max(1);
        chapters.push(SpineChapterMeta {
            path_or_id: format!("part{}", chapters.len()),
            byte_offset: start as u64,
            compressed_size: len as u32,
            uncompressed_size: len as u32,
            method: chapter_method::PLAIN_RAW_SLICE,
            aux_u16: 0,
            aux_u32: 0,
            char_count: ((len * 7) / 8).max(16) as u32,
            cum_char_offset: 0,
            verified: false,
        });
        start = end;
    }

    let mut idx = BookIndex {
        title: clean_stem_title(file_name),
        author: String::new(),
        format: fmt,
        detected_encoding: 0,
        mobi_extra_flags: 0,
        chapters,
        toc: Vec::new(),
        images: Vec::new(),
        footnotes: Vec::new(),
        footnote_keys: Vec::new(),
        saved_anchor: SavedReadingAnchor::default(),
        cached_chapter: None,
    };
    idx.recompute_cumulative_offsets();
    Ok(idx)
}

/* ============================================================================
 * Background OS Worker Thread (`#[cfg(not(target_arch = "wasm32"))]`)
 * ========================================================================== */

#[cfg(not(target_arch = "wasm32"))]
pub mod worker {
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::mpsc::{self, Receiver, Sender};
    use std::sync::Arc;
    use std::thread::{self, JoinHandle};

    use super::*;

    pub enum WorkerCmd {
        FocusChapter {
            active_chapter: usize,
            bg_rgb: u32,
        },
        RefinePages {
            sig: LayoutSig,
            prefs: ReaderPrefs,
            screen_w: f32,
            screen_h: f32,
        },
        Shutdown,
    }

    pub enum WorkerEvent {
        PrefetchedChapter(ParsedChapter),
        PrefetchedGrayImage {
            id_hash: u64,
            gray: DecodedGrayImage,
        },
        ChapterScanned {
            chapter_idx: usize,
            char_count: u32,
            resolved_fragments: Vec<(String, u32)>,
            fallback_headings: Vec<(String, u8, u32)>,
            footnotes: Vec<FootnoteDef>,
            footnote_keys: Vec<String>,
            probed_images: Vec<(usize, u32, u32)>,
        },
        ChapterPageCount {
            sig: LayoutSig,
            chapter_idx: usize,
            pages: u32,
        },
    }

    pub struct BookBackgroundWorker {
        cmd_tx: Sender<WorkerCmd>,
        evt_rx: Receiver<WorkerEvent>,
        cancel: Arc<AtomicBool>,
        join: Option<JoinHandle<()>>,
    }

    impl BookBackgroundWorker {
        pub fn spawn(
            session: &BookSession,
            cache_root: Option<PathBuf>,
            initial_prefs: ReaderPrefs,
            screen_w: f32,
            screen_h: f32,
            bg_rgb: u32,
        ) -> Option<Self> {
            let (file_path, _canon, _fsize, _mtime) = session.file_backing_info()?;
            let index_clone = session.index.clone();
            let active_ch = session.active_chapter;
            let init_sig = session.current_layout_sig();

            let (cmd_tx, cmd_rx) = mpsc::channel::<WorkerCmd>();
            let (evt_tx, evt_rx) = mpsc::channel::<WorkerEvent>();
            let cancel = Arc::new(AtomicBool::new(false));
            let cancel_worker = Arc::clone(&cancel);

            let handle = thread::spawn(move || {
                run_worker_loop(
                    file_path,
                    cache_root,
                    index_clone,
                    active_ch,
                    init_sig,
                    initial_prefs,
                    screen_w,
                    screen_h,
                    bg_rgb,
                    cmd_rx,
                    evt_tx,
                    cancel_worker,
                );
            });

            Some(Self {
                cmd_tx,
                evt_rx,
                cancel,
                join: Some(handle),
            })
        }

        pub fn notify_focus_chapter(&self, active_chapter: usize, bg_rgb: u32) {
            let _ = self.cmd_tx.send(WorkerCmd::FocusChapter {
                active_chapter,
                bg_rgb,
            });
        }

        pub fn notify_layout_changed(&self, prefs: ReaderPrefs, screen_w: f32, screen_h: f32) {
            let sig: LayoutSig = (
                screen_w.round() as u32,
                screen_h.round() as u32,
                prefs.size_idx,
                prefs.spacing_idx,
                prefs.hide_ui,
                prefs.font as u8,
            );
            let _ = self.cmd_tx.send(WorkerCmd::RefinePages {
                sig,
                prefs,
                screen_w,
                screen_h,
            });
        }

        /// Non-blocking drain of completed background worker updates (called once per frame, `< 0.05 ms`).
        pub fn poll_into(
            &self,
            session: &mut BookSession,
            img_cache: &mut DitheredImageCache,
        ) -> bool {
            let mut changed = false;
            while let Ok(ev) = self.evt_rx.try_recv() {
                changed = true;
                match ev {
                    WorkerEvent::PrefetchedChapter(parsed) => {
                        session.apply_parsed_chapter_metadata(&parsed);
                        session.cache_parsed_chapter(parsed);
                    }
                    WorkerEvent::PrefetchedGrayImage { id_hash, gray } => {
                        img_cache.insert_decoded_gray(id_hash, gray);
                    }
                    WorkerEvent::ChapterScanned {
                        chapter_idx,
                        char_count,
                        resolved_fragments,
                        fallback_headings,
                        footnotes,
                        footnote_keys,
                        probed_images,
                    } => {
                        if let Some(ch) = session.index.chapters.get_mut(chapter_idx) {
                            ch.char_count = char_count.max(1);
                            ch.verified = true;
                            session.index.recompute_cumulative_offsets();
                        }
                        for (i, w, h) in probed_images {
                            if let Some(im) = session.index.images.get_mut(i) {
                                im.width = w;
                                im.height = h;
                            }
                        }
                        if footnotes.len() > session.index.footnotes.len() {
                            session.index.footnotes = footnotes;
                            session.index.footnote_keys = footnote_keys;
                            session.active_doc.footnotes = session.index.footnotes.clone();
                        }
                        for t in &mut session.index.toc {
                            if t.target_chapter_idx as usize == chapter_idx
                                && !t.fragment.is_empty()
                            {
                                if let Some(&(_, off)) =
                                    resolved_fragments.iter().find(|(f, _)| f == &t.fragment)
                                {
                                    t.target_char_offset = off;
                                    t.fragment.clear();
                                }
                            }
                        }
                        if session.index.toc.is_empty() && !fallback_headings.is_empty() {
                            for (title, lvl, off) in fallback_headings {
                                session.index.toc.push(IndexTocEntry {
                                    title,
                                    level: lvl,
                                    target_chapter_idx: chapter_idx as u32,
                                    target_char_offset: off,
                                    fragment: String::new(),
                                });
                            }
                        }
                    }
                    WorkerEvent::ChapterPageCount {
                        sig,
                        chapter_idx,
                        pages,
                    } => {
                        session.set_exact_chapter_pages(sig, chapter_idx, pages);
                    }
                }
            }
            changed
        }
    }

    impl Drop for BookBackgroundWorker {
        fn drop(&mut self) {
            self.cancel.store(true, Ordering::Release);
            let _ = self.cmd_tx.send(WorkerCmd::Shutdown);
            if let Some(h) = self.join.take() {
                let _ = h.join();
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn run_worker_loop(
        file_path: PathBuf,
        cache_root: Option<PathBuf>,
        initial_index: BookIndex,
        mut focus_ch: usize,
        mut active_sig: Option<LayoutSig>,
        mut cur_prefs: ReaderPrefs,
        mut cur_sw: f32,
        mut cur_sh: f32,
        mut cur_bg: u32,
        cmd_rx: Receiver<WorkerCmd>,
        evt_tx: Sender<WorkerEvent>,
        cancel: Arc<AtomicBool>,
    ) {
        let Ok(meta) = fs::metadata(&file_path) else {
            return;
        };
        let file_size = meta.len();
        let mtime_sec = meta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let canonical_path = fs::canonicalize(&file_path)
            .unwrap_or_else(|_| file_path.clone())
            .to_string_lossy()
            .into_owned();

        let mut worker_sess = BookSession::init_empty_from_index(
            initial_index,
            SessionBacking::File {
                canonical_path,
                path: file_path,
                file_size,
                mtime_sec,
                fb2_zip_buf: None,
                mobi_html_buf: None,
                inline_data_images: HashMap::new(),
            },
        );
        let mut metrics = FontMetricsTable::new(
            cur_prefs.font,
            READER_FONT_SIZES[cur_prefs.size_idx],
        );

        let total_chapters = worker_sess.chapter_count();
        let mut parsed_all: Vec<Option<ParsedChapter>> = vec![None; total_chapters];

        let prefetch_neighbors = |ws: &mut BookSession,
                                  parsed_all: &mut [Option<ParsedChapter>],
                                  center: usize,
                                  bg: u32,
                                  evt_tx: &Sender<WorkerEvent>,
                                  cancel: &AtomicBool| {
            let mut targets = Vec::with_capacity(3);
            targets.push(center);
            if center + 1 < total_chapters {
                targets.push(center + 1);
            }
            if center > 0 {
                targets.push(center - 1);
            }
            for &ch_i in &targets {
                if cancel.load(Ordering::Acquire) {
                    return;
                }
                if parsed_all[ch_i].is_none() {
                    if let Ok(p) = ws.parse_single_chapter(ch_i) {
                        ws.apply_parsed_chapter_metadata(&p);
                        if ch_i != center {
                            let _ = evt_tx.send(WorkerEvent::PrefetchedChapter(p.clone()));
                        }
                        parsed_all[ch_i] = Some(p);
                    }
                }
                // Pre-decode any illustrations referenced in `center` and `center ± 1`!
                if let Some(ref p) = parsed_all[ch_i] {
                    for blk in &p.doc.blocks {
                        if let BlockKind::Image { image_idx, .. } = blk.kind {
                            if let Some(im) = ws.index.images.get(image_idx).cloned() {
                                if let Some(raw) = ws.load_raw_image_bytes(image_idx) {
                                    if let Some(gray) = decode_illustration_gray_capped(&raw, bg) {
                                        let _ = evt_tx.send(WorkerEvent::PrefetchedGrayImage {
                                            id_hash: im.id_hash,
                                            gray,
                                        });
                                    }
                                }
                            }
                        }
                    }
                }
            }
        };

        // Step 1: Immediately prefetch `active_chapter ± 1` & nearby illustrations
        prefetch_neighbors(
            &mut worker_sess,
            &mut parsed_all,
            focus_ch,
            cur_bg,
            &evt_tx,
            &cancel,
        );

        // Step 2: Scan all remaining chapters to verify char_counts, TOC fragments, footnotes, & page counts
        let mut wrote_initial_bidx = false;
        for ch_i in 0..total_chapters {
            if cancel.load(Ordering::Acquire) {
                return;
            }
            // Service any high-priority focus/resize command immediately!
            while let Ok(cmd) = cmd_rx.try_recv() {
                match cmd {
                    WorkerCmd::FocusChapter {
                        active_chapter,
                        bg_rgb,
                    } => {
                        focus_ch = active_chapter.min(total_chapters.saturating_sub(1));
                        cur_bg = bg_rgb;
                        prefetch_neighbors(
                            &mut worker_sess,
                            &mut parsed_all,
                            focus_ch,
                            cur_bg,
                            &evt_tx,
                            &cancel,
                        );
                    }
                    WorkerCmd::RefinePages {
                        sig,
                        prefs,
                        screen_w,
                        screen_h,
                    } => {
                        active_sig = Some(sig);
                        cur_prefs = prefs;
                        cur_sw = screen_w;
                        cur_sh = screen_h;
                    }
                    WorkerCmd::Shutdown => return,
                }
            }

            if parsed_all[ch_i].is_none() {
                if let Ok(p) = worker_sess.parse_single_chapter(ch_i) {
                    let prev_img_cnt = worker_sess.index.images.len();
                    worker_sess.apply_parsed_chapter_metadata(&p);
                    let mut probed = Vec::new();
                    for idx in 0..prev_img_cnt.min(worker_sess.index.images.len()) {
                        let im = &worker_sess.index.images[idx];
                        if im.width > 0 && im.height > 0 {
                            probed.push((idx, im.width, im.height));
                        }
                    }
                    let _ = evt_tx.send(WorkerEvent::ChapterScanned {
                        chapter_idx: ch_i,
                        char_count: p.doc.total_chars.max(1) as u32,
                        resolved_fragments: p.resolved_fragments.clone(),
                        fallback_headings: p.fallback_headings.clone(),
                        footnotes: worker_sess.index.footnotes.clone(),
                        footnote_keys: worker_sess.index.footnote_keys.clone(),
                        probed_images: probed,
                    });
                    parsed_all[ch_i] = Some(p);
                }
            }

            if let (Some(sig), Some(ref p)) = (active_sig, &parsed_all[ch_i]) {
                let img_dims: Vec<(u32, u32)> = worker_sess
                    .index
                    .images
                    .iter()
                    .map(|im| (im.width.max(1), im.height.max(1)))
                    .collect();
                let pages = count_blocks_pages_fast(
                    &p.doc.blocks,
                    &img_dims,
                    worker_sess.index.format,
                    &cur_prefs,
                    cur_sw,
                    cur_sh,
                    &mut metrics,
                );
                let _ = evt_tx.send(WorkerEvent::ChapterPageCount {
                    sig,
                    chapter_idx: ch_i,
                    pages: pages as u32,
                });
            }
        }

        // Step 3: Persist verified `.bidx` atomically on the worker thread!
        if !cancel.load(Ordering::Acquire) {
            if let Some(ref croot) = cache_root {
                if let Some(ref p_focus) = parsed_all.get(focus_ch).and_then(|o| o.as_ref()) {
                    if p_focus.doc.blocks.len() <= 6_000 {
                        worker_sess.index.cached_chapter = Some(CachedChapterPayload {
                            chapter_idx: focus_ch as u32,
                            total_chars: p_focus.doc.total_chars as u32,
                            blocks: p_focus.doc.blocks.clone(),
                        });
                    }
                }
                if let SessionBacking::File {
                    ref canonical_path,
                    file_size,
                    mtime_sec,
                    ..
                } = worker_sess.backing
                {
                    let _ = save_bidx_atomic(
                        croot,
                        canonical_path,
                        file_size,
                        mtime_sec,
                        &worker_sess.index,
                    );
                    wrote_initial_bidx = true;
                }
            }
        }
        let _ = wrote_initial_bidx;

        // Step 4: Park cleanly waiting for chapter-prefetch or font-resize page-refinement requests
        while let Ok(cmd) = cmd_rx.recv() {
            if cancel.load(Ordering::Acquire) {
                break;
            }
            match cmd {
                WorkerCmd::FocusChapter {
                    active_chapter,
                    bg_rgb,
                } => {
                    focus_ch = active_chapter.min(total_chapters.saturating_sub(1));
                    cur_bg = bg_rgb;
                    prefetch_neighbors(
                        &mut worker_sess,
                        &mut parsed_all,
                        focus_ch,
                        cur_bg,
                        &evt_tx,
                        &cancel,
                    );
                }
                WorkerCmd::RefinePages {
                    sig,
                    prefs,
                    screen_w,
                    screen_h,
                } => {
                    let _ = &active_sig;
                    cur_prefs = prefs;
                    cur_sw = screen_w;
                    cur_sh = screen_h;
                    let img_dims: Vec<(u32, u32)> = worker_sess
                        .index
                        .images
                        .iter()
                        .map(|im| (im.width.max(1), im.height.max(1)))
                        .collect();
                    for ch_i in 0..total_chapters {
                        if cancel.load(Ordering::Acquire) {
                            return;
                        }
                        if let Some(ref p) = parsed_all[ch_i] {
                            let pages = count_blocks_pages_fast(
                                &p.doc.blocks,
                                &img_dims,
                                worker_sess.index.format,
                                &cur_prefs,
                                cur_sw,
                                cur_sh,
                                &mut metrics,
                            );
                            let _ = evt_tx.send(WorkerEvent::ChapterPageCount {
                                sig,
                                chapter_idx: ch_i,
                                pages: pages as u32,
                            });
                        }
                    }
                }
                WorkerCmd::Shutdown => break,
            }
        }
    }
}
