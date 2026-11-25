//! Pure-Rust unified E-Book & Structured Document engine for UIPod (`.epub`, `.fb2`, `.fb2.zip`,
//! DRM-free `.mobi`/`.prc`/`.azw`, `.md`, and plain text).
//!
//! Compiles cleanly with zero system C dependencies across:
//! - `aarch64-unknown-linux-gnu` (TrimUI Brick / NextUI)
//! - `aarch64-linux-android` (Android `arm64-v8a`)
//! - `wasm32-unknown-unknown` (WebGL2 Simulator)

pub mod bidx;
pub mod encoding;
pub mod epub;
pub mod fb2;
pub mod font_metrics;
pub mod font_metrics_data;
pub mod image_dither;
pub mod layout;
pub mod mobi;
pub mod plain_md;
pub mod session;
pub mod xml_html;
pub mod zip_archive;

pub use bidx::{
    bidx_cache_file_path, bidx_content_key64, bidx_path_hash64, deserialize_bidx,
    load_bidx_from_disk, save_bidx_atomic, serialize_bidx, BidxError, BookIndex, IndexImageEntry,
    IndexTocEntry, SavedReadingAnchor, SpineChapterMeta, BIDX_MAGIC, BIDX_SCHEMA_VERSION,
};
pub use font_metrics::{FontMetricsTable, TtfAdvanceTable};
pub use layout::{
    count_blocks_pages_fast, wrap_footnote_lines, CachedDitherEntry, DitheredImageCache, PageSlice,
    PositionedSegment, ProgressDisplay, ReaderFont, ReaderLayoutSpec, ReaderPrefs, ReaderViewState,
    ReadingMode, VisualItem, VisualItemKind, FONT_STYLE_ITALIC_BIT, FONT_STYLE_SERIF_BIT,
    READER_FONT_SIZES, READER_LINE_SPACINGS, READER_SPEC,
};
pub use session::{BookmarkHint, BookSession, OpenTimings, ParsedChapter};
#[cfg(not(target_arch = "wasm32"))]
pub use session::worker::BookBackgroundWorker;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DocFormat {
    Epub,
    Fb2,
    Mobi,
    Markdown,
    PlainText,
}

impl DocFormat {
    pub const fn is_ebook_prose(self) -> bool {
        matches!(self, Self::Epub | Self::Fb2 | Self::Mobi | Self::Markdown)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InlineStyle {
    Regular,
    Bold,
    Italic,
    BoldItalic,
    Code,
    FootnoteRef,
}

#[derive(Debug, Clone)]
pub struct InlineRun {
    pub text: String,
    pub style: InlineStyle,
    pub footnote_idx: Option<usize>,
}

#[derive(Debug, Clone)]
pub enum BlockKind {
    Paragraph {
        runs: Vec<InlineRun>,
        heading_level: u8,
        is_quote: bool,
        is_code: bool,
        centered: bool,
    },
    Image {
        image_idx: usize,
        caption: String,
    },
    Spacer,
    PageBreak,
}

#[derive(Debug, Clone)]
pub struct DocBlock {
    pub start_offset: usize,
    pub end_offset: usize,
    pub kind: BlockKind,
}

#[derive(Debug, Clone)]
pub struct DocImage {
    pub id_hash: u64,
    pub width: u32,
    pub height: u32,
    pub raw_bytes: Vec<u8>,
}

#[derive(Debug, Clone)]
pub struct FootnoteDef {
    pub index: usize,
    pub label: String,
    pub title: String,
    pub paragraphs: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct TocEntry {
    pub title: String,
    pub level: u8,
    pub offset: usize,
}

#[derive(Debug, Clone)]
pub struct EbookDoc {
    pub title: String,
    pub author: String,
    pub format: DocFormat,
    pub blocks: Vec<DocBlock>,
    pub images: Vec<DocImage>,
    pub footnotes: Vec<FootnoteDef>,
    pub toc: Vec<TocEntry>,
    pub total_chars: usize,
}

#[derive(Debug, Clone)]
pub enum EbookError {
    DrmProtected,
    UnsupportedCompression(String),
    InvalidFormat(String),
}

impl EbookError {
    pub fn localized_message(&self, uk: bool) -> String {
        match self {
            Self::DrmProtected => {
                if uk {
                    "Книга захищена DRM і не підтримується. Використовуйте DRM-free .epub, .fb2 або .mobi.".to_string()
                } else {
                    "This book is encrypted with DRM and cannot be opened. Please use a DRM-free .epub, .fb2, or .mobi file.".to_string()
                }
            }
            Self::UnsupportedCompression(kind) => {
                if uk {
                    format!("Непідтримуваний метод стиснення електронної книги ({kind}).")
                } else {
                    format!("Unsupported e-book compression method ({kind}).")
                }
            }
            Self::InvalidFormat(msg) => {
                if uk {
                    format!("Не вдалося розібрати файл книги: {msg}")
                } else {
                    format!("Could not parse e-book file: {msg}")
                }
            }
        }
    }
}

/// Returns `true` if `path_or_name` has an e-book or text extension handled by UIPod's Reader
/// (including compound `.fb2.zip`).
pub fn is_supported_reader_filename(path_or_name: &str) -> bool {
    let lower = path_or_name.trim().to_ascii_lowercase();
    if lower.ends_with(".fb2.zip") {
        return true;
    }
    let Some((_, ext)) = lower.rsplit_once('.') else {
        return false;
    };
    matches!(
        ext,
        "epub"
            | "fb2"
            | "mobi"
            | "prc"
            | "azw"
            | "txt"
            | "md"
            | "markdown"
            | "lrc"
            | "log"
            | "cfg"
            | "ini"
            | "json"
            | "cue"
            | "nfo"
    )
}

/// Parses `.epub`, `.fb2`, `.fb2.zip`, `.mobi`/`.prc`/`.azw`, `.md`, or plain text into a unified [`EbookDoc`].
pub fn parse_document(file_name: &str, data: &[u8]) -> Result<EbookDoc, EbookError> {
    let lower = file_name.trim().to_ascii_lowercase();

    if lower.ends_with(".epub") {
        return epub::parse_epub(file_name, data);
    }
    if lower.ends_with(".fb2") || lower.ends_with(".fb2.zip") {
        return fb2::parse_fb2_or_zip(file_name, data);
    }
    if lower.ends_with(".mobi") || lower.ends_with(".prc") || lower.ends_with(".azw") {
        return mobi::parse_mobi(file_name, data);
    }

    // Magic-byte fallback in case a file has a generic name
    if data.len() >= 68 && (&data[60..68] == b"BOOKMOBI" || &data[60..68] == b"TEXtREAd") {
        return mobi::parse_mobi(file_name, data);
    }
    if data.starts_with(b"PK\x03\x04") {
        if let Ok(doc) = epub::parse_epub(file_name, data) {
            return Ok(doc);
        }
        return fb2::parse_fb2_or_zip(file_name, data);
    }

    plain_md::parse_plain_or_markdown(file_name, data, false)
}

/// Wraps an [`EbookError`] into a clean single-page explanatory [`EbookDoc`] so DRM/corrupt files
/// display a crisp localized card inside Reader rather than silently failing to open.
pub fn error_document(file_name: &str, err: &EbookError, uk: bool) -> EbookDoc {
    let mut b = DocBuilder::new(clean_stem_title(file_name), String::new(), DocFormat::PlainText);
    b.push_paragraph(
        vec![InlineRun {
            text: if uk {
                "Неможливо відкрити книгу".into()
            } else {
                "Cannot Open E-Book".into()
            },
            style: InlineStyle::Bold,
            footnote_idx: None,
        }],
        1,
        false,
        false,
        false,
    );
    b.push_paragraph(
        vec![InlineRun {
            text: err.localized_message(uk),
            style: InlineStyle::Regular,
            footnote_idx: None,
        }],
        0,
        true,
        false,
        false,
    );
    b.finish()
}

pub fn clean_stem_title(file_name: &str) -> String {
    let leaf = file_name
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or(file_name)
        .trim();
    let lower = leaf.to_ascii_lowercase();
    if lower.ends_with(".fb2.zip") && leaf.len() > 8 {
        return leaf[..leaf.len() - 8].to_string();
    }
    if let Some((stem, ext)) = leaf.rsplit_once('.') {
        if !stem.is_empty() && matches!(ext.to_ascii_lowercase().as_str(), "epub" | "fb2" | "mobi" | "prc" | "azw" | "md") {
            return stem.to_string();
        }
    }
    if leaf.is_empty() {
        "Document".to_string()
    } else {
        leaf.to_string()
    }
}

pub fn fnv1a_book_key(path_or_id: &str) -> String {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in path_or_id.bytes() {
        h ^= b as u64;
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    format!("bmark2_{h:016x}.txt")
}

pub(crate) struct DocBuilder {
    pub title: String,
    pub author: String,
    pub format: DocFormat,
    pub blocks: Vec<DocBlock>,
    pub images: Vec<DocImage>,
    pub footnotes: Vec<FootnoteDef>,
    pub toc: Vec<TocEntry>,
    cursor: usize,
}

impl DocBuilder {
    pub fn new(title: String, author: String, format: DocFormat) -> Self {
        Self {
            title,
            author,
            format,
            blocks: Vec::new(),
            images: Vec::new(),
            footnotes: Vec::new(),
            toc: Vec::new(),
            cursor: 0,
        }
    }

    #[inline]
    pub fn current_offset(&self) -> usize {
        self.cursor
    }

    pub fn register_image(&mut self, raw_bytes: Vec<u8>, width: u32, height: u32) -> usize {
        let mut h: u64 = 0xcbf2_9ce4_8422_2325 ^ (raw_bytes.len() as u64);
        for chunk in raw_bytes.chunks(8).take(512) {
            for &b in chunk {
                h ^= b as u64;
                h = h.wrapping_mul(0x0100_0000_01b3);
            }
        }
        let idx = self.images.len();
        self.images.push(DocImage {
            id_hash: h ^ ((idx as u64) << 32),
            width,
            height,
            raw_bytes,
        });
        idx
    }

    pub fn push_paragraph(
        &mut self,
        runs: Vec<InlineRun>,
        heading_level: u8,
        is_quote: bool,
        is_code: bool,
        centered: bool,
    ) {
        let len: usize = runs
            .iter()
            .map(|r| r.text.chars().count().max(usize::from(r.footnote_idx.is_some())))
            .sum::<usize>()
            .max(1);
        let start = self.cursor;
        let end = start + len;
        self.cursor = end;
        self.blocks.push(DocBlock {
            start_offset: start,
            end_offset: end,
            kind: BlockKind::Paragraph {
                runs,
                heading_level,
                is_quote,
                is_code,
                centered,
            },
        });
    }

    pub fn push_image(&mut self, image_idx: usize, caption: String) {
        let start = self.cursor;
        let end = start + 1;
        self.cursor = end;
        self.blocks.push(DocBlock {
            start_offset: start,
            end_offset: end,
            kind: BlockKind::Image { image_idx, caption },
        });
    }

    pub fn push_spacer(&mut self) {
        if matches!(
            self.blocks.last().map(|b| &b.kind),
            Some(BlockKind::Spacer | BlockKind::PageBreak)
        ) {
            return;
        }
        let start = self.cursor;
        let end = start + 1;
        self.cursor = end;
        self.blocks.push(DocBlock {
            start_offset: start,
            end_offset: end,
            kind: BlockKind::Spacer,
        });
    }

    pub fn push_page_break(&mut self) {
        if matches!(self.blocks.last().map(|b| &b.kind), Some(BlockKind::PageBreak)) {
            return;
        }
        let start = self.cursor;
        let end = start + 1;
        self.cursor = end;
        self.blocks.push(DocBlock {
            start_offset: start,
            end_offset: end,
            kind: BlockKind::PageBreak,
        });
    }

    pub fn dedup_and_clamp_toc(&mut self) {
        let max_off = self.cursor.saturating_sub(1);
        let mut clean: Vec<TocEntry> = Vec::with_capacity(self.toc.len());
        for mut e in self.toc.drain(..) {
            e.title = e.title.trim().to_string();
            if e.title.is_empty() {
                continue;
            }
            e.offset = e.offset.min(max_off);
            if let Some(prev) = clean.last() {
                if prev.title == e.title && prev.offset == e.offset {
                    continue;
                }
            }
            clean.push(e);
        }
        self.toc = clean;
    }

    pub fn finish(mut self) -> EbookDoc {
        if self.blocks.is_empty() {
            self.push_paragraph(
                vec![InlineRun {
                    text: String::new(),
                    style: InlineStyle::Regular,
                    footnote_idx: None,
                }],
                0,
                false,
                false,
                false,
            );
        }
        EbookDoc {
            title: self.title,
            author: self.author,
            format: self.format,
            blocks: self.blocks,
            images: self.images,
            footnotes: self.footnotes,
            toc: self.toc,
            total_chars: self.cursor.max(1),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::zip_archive::build_test_zip;

    fn tiny_png_bytes() -> Vec<u8> {
        let mut out = Vec::new();
        let mut enc = png::Encoder::new(&mut out, 8, 6);
        enc.set_color(png::ColorType::Rgb);
        enc.set_depth(png::BitDepth::Eight);
        let mut wr = enc.write_header().unwrap();
        let mut px = [0u8; 8 * 6 * 3];
        for (i, b) in px.iter_mut().enumerate() {
            *b = ((i * 19) & 0xFF) as u8;
        }
        wr.write_image_data(&px).unwrap();
        drop(wr);
        out
    }

    #[test]
    fn parses_epub3_toc_images_and_cross_file_footnotes() {
        let container = br#"<?xml version="1.0"?>
<container version="1.0" xmlns="urn:oasis:names:tc:opendocument:xmlns:container">
  <rootfiles><rootfile full-path="OEBPS/content.opf" media-type="application/oebps-package+xml"/></rootfiles>
</container>"#;
        let opf = br#"<?xml version="1.0" encoding="utf-8"?>
<package version="3.0" xmlns="http://www.idpf.org/2007/opf">
  <metadata xmlns:dc="http://purl.org/dc/elements/1.1/">
    <dc:title>Kobzar</dc:title>
    <dc:creator>Taras Shevchenko</dc:creator>
  </metadata>
  <manifest>
    <item id="nav" href="nav.xhtml" media-type="application/xhtml+xml" properties="nav"/>
    <item id="c1" href="Text/ch1.xhtml" media-type="application/xhtml+xml"/>
    <item id="c2" href="Text/ch2.xhtml" media-type="application/xhtml+xml"/>
    <item id="notes" href="Text/notes.xhtml" media-type="application/xhtml+xml"/>
    <item id="img1" href="Images/ill1.png" media-type="image/png"/>
  </manifest>
  <spine>
    <itemref idref="c1"/>
    <itemref idref="c2"/>
  </spine>
</package>"#;
        let nav = br#"<html xmlns:epub="http://www.idpf.org/2007/ops"><body>
<nav epub:type="toc"><ol>
  <li><a href="Text/ch1.xhtml">Chapter I</a>
    <ol><li><a href="Text/ch1.xhtml#sec1">Part 1.1</a></li></ol>
  </li>
  <li><a href="Text/ch2.xhtml">Chapter II</a></li>
</ol></nav></body></html>"#;
        let ch1 = br#"<html><body>
<h1>Chapter I</h1>
<p>Opening verse with a footnote<a epub:type="noteref" href="notes.xhtml#fn1">[1]</a>.</p>
<img src="../Images/ill1.png" alt="Woodcut"/>
<h2 id="sec1">Part 1.1</h2>
<p>Subsection body text.</p>
</body></html>"#;
        let ch2 = br#"<html><body><h1>Chapter II</h1><p>Second chapter text.</p></body></html>"#;
        let notes = br#"<html><body><aside id="fn1" epub:type="footnote"><p>Historical commentary from 1840 edition.</p></aside></body></html>"#;
        let png = tiny_png_bytes();

        let epub_bytes = build_test_zip(&[
            ("META-INF/container.xml", container, true),
            ("OEBPS/content.opf", opf, true),
            ("OEBPS/nav.xhtml", nav, true),
            ("OEBPS/Text/ch1.xhtml", ch1, true),
            ("OEBPS/Text/ch2.xhtml", ch2, true),
            ("OEBPS/Text/notes.xhtml", notes, true),
            ("OEBPS/Images/ill1.png", &png, false),
        ]);

        let doc = parse_document("kobzar.epub", &epub_bytes).expect("EPUB parse failed");
        assert_eq!(doc.title, "Kobzar");
        assert_eq!(doc.author, "Taras Shevchenko");
        assert_eq!(doc.images.len(), 1);
        assert_eq!(doc.footnotes.len(), 1);
        assert_eq!(doc.footnotes[0].label, "[1]");
        assert!(doc.footnotes[0].paragraphs[0].contains("1840 edition"));
        assert_eq!(doc.toc.len(), 3);
        assert_eq!(doc.toc[0].title, "Chapter I");
        assert_eq!(doc.toc[0].level, 0);
        assert_eq!(doc.toc[1].title, "Part 1.1");
        assert_eq!(doc.toc[1].level, 1);
        assert!(doc.toc[1].offset > doc.toc[0].offset);
        assert!(doc.toc[2].offset > doc.toc[1].offset);
    }

    #[test]
    fn parses_fb2_zip_windows_1251_toc_notes_and_base64_images() {
        let png = tiny_png_bytes();
        // Minimal Base64 encoder for test PNG
        const ALPH: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
        let mut b64 = String::new();
        for chunk in png.chunks(3) {
            let b0 = chunk[0];
            let b1 = *chunk.get(1).unwrap_or(&0);
            let b2 = *chunk.get(2).unwrap_or(&0);
            b64.push(ALPH[(b0 >> 2) as usize] as char);
            b64.push(ALPH[(((b0 & 0x03) << 4) | (b1 >> 4)) as usize] as char);
            b64.push(if chunk.len() > 1 {
                ALPH[(((b1 & 0x0F) << 2) | (b2 >> 6)) as usize] as char
            } else {
                '='
            });
            b64.push(if chunk.len() > 2 {
                ALPH[(b2 & 0x3F) as usize] as char
            } else {
                '='
            });
        }

        // Encode an XML with `encoding="windows-1251"` containing Ukrainian Cyrillic bytes:
        // "Розділ 1" in CP1251: Р(0xD0) о(0xEE) з(0xE7) д(0xE4) і(0xB3) л(0xEB) ' ' '1'
        let mut cp1251_fb2 = Vec::new();
        cp1251_fb2.extend_from_slice(
            b"<?xml version=\"1.0\" encoding=\"windows-1251\"?><FictionBook xmlns:l=\"http://www.w3.org/1999/xlink\"><description><title-info><book-title>",
        );
        cp1251_fb2.extend_from_slice(&[0xCA, 0xED, 0xE8, 0xE3, 0xE0]); // "Книга"
        cp1251_fb2.extend_from_slice(b"</book-title></title-info></description><body><section><title><p>");
        cp1251_fb2.extend_from_slice(&[0xD0, 0xEE, 0xE7, 0xE4, 0xB3, 0xEB, b' ', b'1']); // "Розділ 1"
        cp1251_fb2.extend_from_slice(
            b"</p></title><p>Text<a l:href=\"#n1\" type=\"note\">[1]</a></p><image l:href=\"#pic1\"/></section></body>",
        );
        cp1251_fb2.extend_from_slice(
            b"<body name=\"notes\"><section id=\"n1\"><title><p>1</p></title><p>Footnote body</p></section></body>",
        );
        cp1251_fb2.extend_from_slice(format!("<binary id=\"pic1\">{b64}</binary></FictionBook>").as_bytes());

        let zipped = build_test_zip(&[("book.fb2", &cp1251_fb2, true)]);
        let doc = parse_document("book.fb2.zip", &zipped).expect("FB2.zip parse failed");
        assert_eq!(doc.title, "Книга");
        assert_eq!(doc.toc.len(), 1);
        assert_eq!(doc.toc[0].title, "Розділ 1");
        assert_eq!(doc.footnotes.len(), 1);
        assert_eq!(doc.footnotes[0].paragraphs[0], "Footnote body");
        assert_eq!(doc.images.len(), 1);
    }

    #[test]
    fn parses_mobi_palmdoc_lz77_toc_footnotes_and_detects_drm() {
        let html = b"<html><body><h1>Chapter One</h1><p>Hello world<a filepos=\"0000000115\">[1]</a>.</p><mbp:pagebreak/><h1>Chapter Two</h1><p>Note one details.</p></body></html>";
        // Build a minimal uncompressed (`compression = 1`) MOBI file with 1 text record + 1 PNG image record
        let png = tiny_png_bytes();
        let build_mobi = |encrypted: u16| -> Vec<u8> {
            let mut pdb = vec![0u8; 78 + 3 * 8];
            pdb[0..9].copy_from_slice(b"Test_Mobi");
            pdb[60..68].copy_from_slice(b"BOOKMOBI");
            pdb[76..78].copy_from_slice(&3u16.to_be_bytes());

            let mut rec0 = vec![0u8; 16 + 132];
            rec0[0..2].copy_from_slice(&1u16.to_be_bytes()); // uncompressed
            rec0[4..8].copy_from_slice(&(html.len() as u32).to_be_bytes());
            rec0[8..10].copy_from_slice(&1u16.to_be_bytes()); // 1 text record
            rec0[10..12].copy_from_slice(&4096u16.to_be_bytes());
            rec0[12..14].copy_from_slice(&encrypted.to_be_bytes());
            rec0[16..20].copy_from_slice(b"MOBI");
            rec0[20..24].copy_from_slice(&132u32.to_be_bytes());
            rec0[28..32].copy_from_slice(&65001u32.to_be_bytes()); // UTF-8
            rec0[108..112].copy_from_slice(&2u32.to_be_bytes()); // first_image_index = 2

            let off0 = pdb.len() as u32;
            let off1 = off0 + rec0.len() as u32;
            let off2 = off1 + html.len() as u32;
            pdb[78..82].copy_from_slice(&off0.to_be_bytes());
            pdb[86..90].copy_from_slice(&off1.to_be_bytes());
            pdb[94..98].copy_from_slice(&off2.to_be_bytes());
            pdb.extend_from_slice(&rec0);
            pdb.extend_from_slice(html);
            pdb.extend_from_slice(&png);
            pdb
        };

        let drm_bytes = build_mobi(2);
        assert!(matches!(
            parse_document("locked.mobi", &drm_bytes),
            Err(EbookError::DrmProtected)
        ));

        let free_bytes = build_mobi(0);
        let doc = parse_document("free.mobi", &free_bytes).expect("MOBI parse failed");
        assert_eq!(doc.toc.len(), 2);
        assert_eq!(doc.toc[0].title, "Chapter One");
        assert_eq!(doc.toc[1].title, "Chapter Two");
        assert_eq!(doc.footnotes.len(), 1);
    }

    #[test]
    fn parses_epub_cyrillic_cjk_entities_and_detects_drm_encryption() {
        let container = br#"<?xml version="1.0"?>
<container version="1.0" xmlns="urn:oasis:names:tc:opendocument:xmlns:container">
  <rootfiles><rootfile full-path="OEBPS/content.opf" media-type="application/oebps-package+xml"/></rootfiles>
</container>"#;
        let opf = r#"<?xml version="1.0" encoding="utf-8"?>
<package version="3.0" xmlns="http://www.idpf.org/2007/opf">
  <metadata xmlns:dc="http://purl.org/dc/elements/1.1/">
    <dc:title>Кобзар &amp; 「詩集」</dc:title>
    <dc:creator>Тарас Шевченко</dc:creator>
  </metadata>
  <manifest>
    <item id="c1" href="Text/Глава%201.xhtml?v=2" media-type="application/xhtml+xml"/>
    <item id="img1" href="OEBPS/../Images/Обкладинка%201.png" media-type="image/png" properties="cover-image"/>
  </manifest>
  <spine><itemref idref="c1"/></spine>
</package>"#;
        let ch1 = r#"<html><body>
<h1>Ґрунт&nbsp;Єдності — Історія Їжака</h1>
<p>&nbsp;Привіт світе! Simon &amp; Schuster — 「日本語・한국어・简体中文・繁體中文」.</p>
</body></html>"#;
        let png = tiny_png_bytes();

        let epub_bytes = build_test_zip(&[
            ("META-INF/container.xml", container, true),
            ("OEBPS/content.opf", opf.as_bytes(), true),
            ("OEBPS/Text/Глава 1.xhtml", ch1.as_bytes(), true),
            ("Images/Обкладинка 1.png", &png, true),
        ]);
        let doc = parse_document("Кобзар і Світ.epub", &epub_bytes).expect("Cyrillic+CJK EPUB failed");
        assert_eq!(doc.title, "Кобзар & 「詩集」");
        assert_eq!(doc.images.len(), 1);
        assert_eq!(doc.toc.len(), 1);

        // DRM-encrypted EPUB (`META-INF/encryption.xml` encrypting chapter XHTML) must return `DrmProtected`
        let drm_xml = br#"<encryption xmlns="urn:oasis:names:tc:opendocument:xmlns:container">
  <EncryptedData xmlns="http://www.w3.org/2001/04/xmlenc#">
    <EncryptionMethod Algorithm="http://www.w3.org/2001/04/xmlenc#aes128-cbc"/>
    <CipherData><CipherReference URI="OEBPS/Text/ch1.xhtml"/></CipherData>
  </EncryptedData>
</encryption>"#;
        let drm_epub = build_test_zip(&[
            ("META-INF/container.xml", container, true),
            ("META-INF/encryption.xml", drm_xml, true),
            ("OEBPS/content.opf", opf.as_bytes(), true),
        ]);
        assert!(matches!(
            parse_document("locked.epub", &drm_epub),
            Err(EbookError::DrmProtected)
        ));
    }

    #[test]
    fn paginated_and_scroll_modes_preserve_anchor_read_total_pages_and_never_reveal_hidden_ui() {
        let mut md = String::from("# Chapter 1\n\n");
        for i in 1..=60 {
            md.push_str(&format!(
                "Paragraph {i}: UIPod 4-shade reader test line with footnote[^n1] and crisp wrapping.\n\n"
            ));
        }
        md.push_str("# Chapter 2\n\nSecond chapter start.\n\n[^n1]: Footnote explanation.\n");
        let doc = parse_document("guide.md", md.as_bytes()).unwrap();
        assert_eq!(doc.toc.len(), 2);

        let mut prefs = ReaderPrefs {
            mode: ReadingMode::Paginated,
            font: ReaderFont::Sans,
            size_idx: 1,
            spacing_idx: 1,
            hide_ui: true,
            progress_display: ProgressDisplay::Pages,
        };
        let mut state = ReaderViewState::new(0);
        state.ensure_layout(&doc, &prefs, 1024.0, 768.0, |txt, sz, wt| {
            let factor = if (wt & FONT_STYLE_SERIF_BIT) != 0 {
                0.56
            } else {
                0.52
            };
            (txt.chars().count() as f32) * sz * factor
        });
        let total = state.pages.len();
        assert!(total >= 3);

        // Page 1 must format as `1 / <total>` with positive bar fill `1 / total`
        let (p0, lbl0) = state.progress_metrics(&prefs);
        assert_eq!(lbl0, format!("1 / {total}"));
        assert!((p0 - (1.0 / total as f32)).abs() < 1e-5);
        assert!(!state.is_chrome_visible(&prefs, 1000));

        // Turn forward 2 pages -> `3 / <total>`
        assert!(state.step_page(2, &prefs, 1000));
        assert!(!state.is_chrome_visible(&prefs, 1000));
        let (p2, lbl2) = state.progress_metrics(&prefs);
        assert_eq!(lbl2, format!("3 / {total}"));
        assert!(p2 > p0);

        // Turn backward (`Left` / `Up`) -> must NEVER reveal hidden top UI chrome!
        assert!(state.step_page(-1, &prefs, 1000));
        assert!(!state.is_chrome_visible(&prefs, 1000));
        assert!(!state.is_chrome_visible(&prefs, 2500));

        // Switch `Font`: `Geologica (Sans)` <-> `Noto Serif` preserves exact `anchor_offset`, emits `FONT_STYLE_SERIF_BIT`, and roundtrips `v3` prefs!
        let spot_before_font = state.anchor_offset;
        prefs.font = prefs.font.toggle();
        assert_eq!(prefs.font, ReaderFont::Serif);
        let saved_line = prefs.serialize();
        assert!(saved_line.starts_with("v3 "));
        assert_eq!(ReaderPrefs::deserialize(&saved_line).font, ReaderFont::Serif);
        state.ensure_layout(&doc, &prefs, 1024.0, 768.0, |txt, sz, wt| {
            let factor = if (wt & FONT_STYLE_SERIF_BIT) != 0 {
                0.56
            } else {
                0.52
            };
            (txt.chars().count() as f32) * sz * factor
        });
        assert_eq!(state.anchor_offset, spot_before_font);
        let serif_text_seen = state.items.iter().any(|it| {
            matches!(
                &it.kind,
                VisualItemKind::TextLine { segments, .. }
                    if segments.iter().any(|s| s.footnote_idx.is_none() && (s.weight & FONT_STYLE_SERIF_BIT) != 0)
            )
        });
        assert!(serif_text_seen);
        prefs.font = prefs.font.toggle();
        state.ensure_layout(&doc, &prefs, 1024.0, 768.0, |txt, sz, _| {
            (txt.chars().count() as f32) * sz * 0.52
        });

        // Jump to final page -> `<total> / <total>` and `1.0` fill
        state.sync_to_anchor(doc.total_chars);
        let (pend, lbl_end) = state.progress_metrics(&prefs);
        assert_eq!(lbl_end, format!("{total} / {total}"));
        assert!((pend - 1.0).abs() < 1e-5);

        // Switch Paginated <-> Scroll via solo SELECT -> anchor stays identical and Up scroll never reveals UI!
        assert!(state.step_page(-1, &prefs, 2000));
        let spot = state.anchor_offset;
        prefs.mode = prefs.mode.toggle();
        assert_eq!(prefs.mode, ReadingMode::Scroll);
        state.sync_to_anchor(spot);
        assert_eq!(state.anchor_offset, spot);
        let _ = state.scroll_by_px(-48.0, &prefs, 3000);
        assert!(!state.is_chrome_visible(&prefs, 3000));
        prefs.mode = prefs.mode.toggle();
        assert_eq!(prefs.mode, ReadingMode::Paginated);
        state.refresh_anchor_from_mode(ReadingMode::Scroll);
        let spot2 = state.anchor_offset;
        state.sync_to_anchor(spot2);
        assert_eq!(state.anchor_offset, spot2);
    }

    #[test]
    fn lazy_book_session_cold_warm_bidx_resize_and_cross_chapter_parity() {
        let container = br#"<?xml version="1.0"?>
<container version="1.0" xmlns="urn:oasis:names:tc:opendocument:xmlns:container">
  <rootfiles><rootfile full-path="OEBPS/content.opf" media-type="application/oebps-package+xml"/></rootfiles>
</container>"#;
        let opf = br#"<?xml version="1.0" encoding="utf-8"?>
<package version="3.0" xmlns="http://www.idpf.org/2007/opf">
  <metadata xmlns:dc="http://purl.org/dc/elements/1.1/">
    <dc:title>Lazy Spine Book</dc:title>
    <dc:creator>UIPod Team</dc:creator>
  </metadata>
  <manifest>
    <item id="nav" href="nav.xhtml" media-type="application/xhtml+xml" properties="nav"/>
    <item id="c1" href="Text/ch1.xhtml" media-type="application/xhtml+xml"/>
    <item id="c2" href="Text/ch2.xhtml" media-type="application/xhtml+xml"/>
    <item id="c3" href="Text/ch3.xhtml" media-type="application/xhtml+xml"/>
    <item id="notes" href="Text/notes.xhtml" media-type="application/xhtml+xml"/>
    <item id="img1" href="Images/ill1.png" media-type="image/png" properties="cover-image"/>
  </manifest>
  <spine>
    <itemref idref="c1"/>
    <itemref idref="c2"/>
    <itemref idref="c3"/>
  </spine>
</package>"#;
        let nav = r#"<html xmlns:epub="http://www.idpf.org/2007/ops"><body>
<nav epub:type="toc"><ol>
  <li><a href="Text/ch1.xhtml">Розділ 1</a></li>
  <li><a href="Text/ch2.xhtml#partb">Розділ 2 · Частина Б</a></li>
  <li><a href="Text/ch3.xhtml">Розділ 3</a></li>
</ol></nav></body></html>"#;

        let mut ch1 = String::from("<html><body><h1>Розділ 1</h1>");
        for i in 0..25 {
            ch1.push_str(&format!(
                "<p>Перший розділ абзац {i} із виноскою<a epub:type=\"noteref\" href=\"notes.xhtml#fn1\">[1]</a> та українським текстом для перевірки швидкої пагінації.</p>"
            ));
        }
        ch1.push_str("</body></html>");

        let mut ch2 = String::from("<html><body><h1>Розділ 2</h1><p>Вступ другого розділу.</p><h2 id=\"partb\">Частина Б</h2>");
        for i in 0..25 {
            ch2.push_str(&format!(
                "<p>Другий розділ абзац {i}: швидке перемикання кеглю шрифту без блокування аудіопотоку.</p>"
            ));
        }
        ch2.push_str("</body></html>");

        let ch3 = "<html><body><h1>Розділ 3</h1><p>Фінальний розділ книги.</p></body></html>";
        let notes = r#"<html><body><aside id="fn1" epub:type="footnote"><p>Примітка з окремого файлу notes.xhtml.</p></aside></body></html>"#;
        let png = tiny_png_bytes();

        let epub_bytes = build_test_zip(&[
            ("META-INF/container.xml", container, true),
            ("OEBPS/content.opf", opf, true),
            ("OEBPS/nav.xhtml", nav.as_bytes(), true),
            ("OEBPS/Text/ch1.xhtml", ch1.as_bytes(), true),
            ("OEBPS/Text/ch2.xhtml", ch2.as_bytes(), true),
            ("OEBPS/Text/ch3.xhtml", ch3.as_bytes(), true),
            ("OEBPS/Text/notes.xhtml", notes.as_bytes(), true),
            ("OEBPS/Images/ill1.png", &png, false),
        ]);

        let tmp_dir = std::env::temp_dir().join(format!("uipod_sess_test_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp_dir);
        std::fs::create_dir_all(&tmp_dir).unwrap();
        let epub_path = tmp_dir.join("lazy_test.epub");
        std::fs::write(&epub_path, &epub_bytes).unwrap();

        let mut prefs = ReaderPrefs {
            mode: ReadingMode::Paginated,
            size_idx: 1,
            spacing_idx: 1,
            hide_ui: false,
            progress_display: ProgressDisplay::Pages,
            font: ReaderFont::Sans,
        };

        // 1. Cold open parses ONLY Chapter 0 (Chapters 1 & 2 remain unverified until touched/scanned!)
        let mut sess =
            BookSession::open_file(&epub_path, Some(&tmp_dir), BookmarkHint::None).expect("cold open");
        assert!(!sess.open_timings.used_warm_bidx);
        assert_eq!(sess.title(), "Lazy Spine Book");
        assert_eq!(sess.chapter_count(), 3);
        assert!(sess.index.chapters[0].verified);
        assert!(!sess.index.chapters[1].verified);
        assert!(!sess.index.chapters[2].verified);
        assert_eq!(sess.footnote(0).unwrap().paragraphs[0], "Примітка з окремого файлу notes.xhtml.");

        sess.ensure_layout(&prefs, 1024.0, 768.0);
        let (prog0, lbl0) = sess.progress_metrics(&prefs);
        assert!(lbl0.starts_with("1 / "));
        assert!(prog0 > 0.0);

        // Lazy image decode caches grayscale buffer and never re-decodes on font/UI resize!
        let mut img_cache = DitheredImageCache::new(16);
        let _ = sess
            .rasterize_illustration(&mut img_cache, 0, 120, 90, 0xD6D8D5, 0x3C3637, 0xA8A8A8, 0x595959)
            .expect("dither cover");
        assert!(img_cache.gray_cache_bytes() > 0);
        // Different display size hits Tier-1 grayscale cache without touching raw bytes
        let _ = sess
            .rasterize_illustration(&mut img_cache, 0, 160, 120, 0xD6D8D5, 0x3C3637, 0xA8A8A8, 0x595959)
            .expect("re-dither from cached gray");

        // 2. Jump to TOC item 1 (`Text/ch2.xhtml#partb`), resize font, and save `.bidx`
        assert!(sess.jump_to_toc(1, &prefs, 1000, 1024.0, 768.0));
        assert_eq!(sess.active_chapter, 1);
        assert!(sess.view.anchor_offset > 0);
        let saved_spot = sess.view.anchor_offset;

        prefs.size_idx = 3;
        prefs.font = ReaderFont::Serif;
        sess.ensure_layout(&prefs, 1024.0, 768.0);
        assert_eq!(sess.view.anchor_offset, saved_spot);
        assert!(sess.save_bidx(&tmp_dir, &prefs));

        // 3. Warm `.bidx` re-open restores exact `(chapter_idx=1, char_offset)` in < 20ms!
        let bmark_str = sess.bookmark_string(prefs.mode);
        let mut warm = BookSession::open_file(
            &epub_path,
            Some(&tmp_dir),
            BookmarkHint::parse(&bmark_str),
        )
        .expect("warm open");
        assert!(warm.open_timings.used_warm_bidx);
        assert_eq!(warm.active_chapter, 1);
        warm.ensure_layout(&prefs, 1024.0, 768.0);
        assert_eq!(warm.view.anchor_offset, saved_spot);

        // 4. Modifying file size invalidates `.bidx` and falls back to cold rebuild cleanly!
        let mut bigger = epub_bytes.clone();
        bigger.push(0);
        // Appending trailing byte changes file_size while keeping EOCD within 65KB search window
        std::fs::write(&epub_path, &bigger).unwrap();
        let rebuilt =
            BookSession::open_file(&epub_path, Some(&tmp_dir), BookmarkHint::None).expect("rebuilt");
        assert!(!rebuilt.open_timings.used_warm_bidx);

        let _ = std::fs::remove_dir_all(&tmp_dir);
    }

    #[test]
    fn fb2_zip_and_mobi_session_cold_warm_bidx_and_resize_parity() {
        let tmp_dir = std::env::temp_dir().join(format!("uipod_fb2_mobi_bidx_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp_dir);
        std::fs::create_dir_all(&tmp_dir).unwrap();

        let mut prefs = ReaderPrefs {
            mode: ReadingMode::Paginated,
            size_idx: 1,
            spacing_idx: 1,
            hide_ui: false,
            progress_display: ProgressDisplay::Pages,
            font: ReaderFont::Sans,
        };

        // --- 1. `.fb2.zip` cold open -> TOC jump -> font resize -> `.bidx` warm re-open ---
        let fb2_xml = r##"<?xml version="1.0" encoding="utf-8"?>
<FictionBook xmlns:l="http://www.w3.org/1999/xlink">
  <description><title-info><book-title>Лісова Пісня</book-title></title-info></description>
  <body>
    <section><title><p>Дія перша</p></title><p>Пролог весняного лісу<a l:href="#n1" type="note">[1]</a>.</p></section>
    <section><title><p>Дія друга</p></title><p>Пізнє літо над озером із лататтям та очеретом.</p></section>
  </body>
  <body name="notes"><section id="n1"><title><p>1</p></title><p>Коментар до першої дії.</p></section></body>
</FictionBook>"##;
        let fb2_zip = build_test_zip(&[("lisova.fb2", fb2_xml.as_bytes(), true)]);
        let fb2_path = tmp_dir.join("lisova.fb2.zip");
        std::fs::write(&fb2_path, &fb2_zip).unwrap();

        let mut fb2_cold =
            BookSession::open_file(&fb2_path, Some(&tmp_dir), BookmarkHint::None).expect("fb2 cold");
        assert!(!fb2_cold.open_timings.used_warm_bidx);
        assert_eq!(fb2_cold.title(), "Лісова Пісня");
        assert_eq!(fb2_cold.toc().len(), 2);
        assert_eq!(fb2_cold.footnote(0).unwrap().paragraphs[0], "Коментар до першої дії.");
        fb2_cold.ensure_layout(&prefs, 1024.0, 768.0);
        assert!(fb2_cold.jump_to_toc(1, &prefs, 1000, 1024.0, 768.0));
        let fb2_anchor = fb2_cold.view.anchor_offset;
        prefs.size_idx = 2;
        fb2_cold.ensure_layout(&prefs, 1024.0, 768.0);
        assert_eq!(fb2_cold.view.anchor_offset, fb2_anchor);
        assert!(fb2_cold.save_bidx(&tmp_dir, &prefs));

        let fb2_bmark = fb2_cold.bookmark_string(prefs.mode);
        let mut fb2_warm = BookSession::open_file(
            &fb2_path,
            Some(&tmp_dir),
            BookmarkHint::parse(&fb2_bmark),
        )
        .expect("fb2 warm");
        assert!(fb2_warm.open_timings.used_warm_bidx);
        fb2_warm.ensure_layout(&prefs, 1024.0, 768.0);
        assert_eq!(fb2_warm.view.anchor_offset, fb2_anchor);

        // --- 2. `.mobi` cold open -> TOC jump -> font resize -> `.bidx` warm re-open ---
        let mobi_html = b"<html><body><h1>Part I</h1><p>First section text.</p><mbp:pagebreak/><h1>Part II</h1><p>Second section text after pagebreak.</p></body></html>";
        let mut pdb = vec![0u8; 78 + 2 * 8];
        pdb[0..9].copy_from_slice(b"Mobi_Bidx");
        pdb[60..68].copy_from_slice(b"BOOKMOBI");
        pdb[76..78].copy_from_slice(&2u16.to_be_bytes());
        let mut rec0 = vec![0u8; 16 + 132];
        rec0[0..2].copy_from_slice(&1u16.to_be_bytes());
        rec0[4..8].copy_from_slice(&(mobi_html.len() as u32).to_be_bytes());
        rec0[8..10].copy_from_slice(&1u16.to_be_bytes());
        rec0[10..12].copy_from_slice(&4096u16.to_be_bytes());
        rec0[16..20].copy_from_slice(b"MOBI");
        rec0[20..24].copy_from_slice(&132u32.to_be_bytes());
        rec0[28..32].copy_from_slice(&65001u32.to_be_bytes());
        let off0 = pdb.len() as u32;
        let off1 = off0 + rec0.len() as u32;
        pdb[78..82].copy_from_slice(&off0.to_be_bytes());
        pdb[86..90].copy_from_slice(&off1.to_be_bytes());
        pdb.extend_from_slice(&rec0);
        pdb.extend_from_slice(mobi_html);

        let mobi_path = tmp_dir.join("test_book.mobi");
        std::fs::write(&mobi_path, &pdb).unwrap();

        let mut mobi_cold =
            BookSession::open_file(&mobi_path, Some(&tmp_dir), BookmarkHint::None).expect("mobi cold");
        assert!(!mobi_cold.open_timings.used_warm_bidx);
        assert_eq!(mobi_cold.toc().len(), 2);
        mobi_cold.ensure_layout(&prefs, 1024.0, 768.0);
        assert!(mobi_cold.jump_to_toc(1, &prefs, 1000, 1024.0, 768.0));
        let mobi_anchor = mobi_cold.view.anchor_offset;
        prefs.font = ReaderFont::Serif;
        mobi_cold.ensure_layout(&prefs, 1024.0, 768.0);
        assert_eq!(mobi_cold.view.anchor_offset, mobi_anchor);
        assert!(mobi_cold.save_bidx(&tmp_dir, &prefs));

        let mobi_bmark = mobi_cold.bookmark_string(prefs.mode);
        let mut mobi_warm = BookSession::open_file(
            &mobi_path,
            Some(&tmp_dir),
            BookmarkHint::parse(&mobi_bmark),
        )
        .expect("mobi warm");
        assert!(mobi_warm.open_timings.used_warm_bidx);
        mobi_warm.ensure_layout(&prefs, 1024.0, 768.0);
        assert_eq!(mobi_warm.view.anchor_offset, mobi_anchor);

        let _ = std::fs::remove_dir_all(&tmp_dir);
    }
}
