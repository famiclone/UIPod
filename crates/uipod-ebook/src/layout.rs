//! Declarative `ReaderLayoutSpec` (`READER_SPEC`), word/CJK/Cyrillic line wrapper,
//! zero-cutoff `Paginated` ↔ `Infinite Scroll` layout engine, `Hide UI` (`3.0s` reveal)
//! state machine, interactive Footnote card layout, and LRU 4-shade image cache.

use crate::font_metrics::FontMetricsTable;
use crate::image_dither::{
    decode_illustration_gray_capped, fit_illustration_box, rasterize_gray_4shade_rgba,
    DecodedGrayImage,
};
use crate::{BlockKind, DocBlock, DocFormat, EbookDoc, InlineRun, InlineStyle};

/// Declarative layout & style constants for the E-Book Reader (`AGENTS.md` `LayoutSpec` contract).
pub struct ReaderLayoutSpec {
    pub screen_margin_x: f32,
    pub header_content_top_y: f32,
    pub chromeless_top_pad: f32,
    pub chromeless_bottom_pad: f32,
    pub bottom_strip_h: f32,
    pub progress_bar_h: f32,
    pub progress_bar_radius: f32,
    pub progress_bar_stroke: f32,
    pub progress_bar_inset: f32,
    pub progress_label_font: f32,
    pub progress_label_gap: f32,
    pub quote_bar_w: f32,
    pub quote_indent_x: f32,
    pub code_pad_x: f32,
    pub footnote_pill_pad_x: f32,
    pub footnote_pill_radius: f32,
    pub image_v_pad: f32,
    pub image_border_stroke: f32,
    pub image_corner_radius: f32,
    pub max_page_img_h_ratio: f32,
    pub fn_card_margin_x: f32,
    pub fn_card_max_w: f32,
    pub fn_card_max_h_ratio: f32,
    pub fn_card_radius: f32,
    pub fn_card_stroke: f32,
    pub fn_card_pad_x: f32,
    pub fn_card_pad_y: f32,
    pub fn_title_font: f32,
    pub fn_body_font: f32,
    pub fn_hint_font: f32,
    pub auto_hide_reveal_ms: u64,
}

pub const READER_SPEC: ReaderLayoutSpec = ReaderLayoutSpec {
    screen_margin_x: 32.0,
    header_content_top_y: 136.0,
    chromeless_top_pad: 26.0,
    chromeless_bottom_pad: 22.0,
    bottom_strip_h: 48.0,
    progress_bar_h: 16.0,
    progress_bar_radius: 8.0,
    progress_bar_stroke: 4.0,
    progress_bar_inset: 4.0,
    progress_label_font: 22.0,
    progress_label_gap: 16.0,
    quote_bar_w: 4.0,
    quote_indent_x: 22.0,
    code_pad_x: 8.0,
    footnote_pill_pad_x: 6.0,
    footnote_pill_radius: 6.0,
    image_v_pad: 12.0,
    image_border_stroke: 2.0,
    image_corner_radius: 6.0,
    max_page_img_h_ratio: 0.74,
    fn_card_margin_x: 48.0,
    fn_card_max_w: 880.0,
    fn_card_max_h_ratio: 0.64,
    fn_card_radius: 16.0,
    fn_card_stroke: 4.0,
    fn_card_pad_x: 24.0,
    fn_card_pad_y: 18.0,
    fn_title_font: 26.0,
    fn_body_font: 26.0,
    fn_hint_font: 20.0,
    auto_hide_reveal_ms: 3000,
};

pub const READER_FONT_SIZES: [f32; 4] = [26.0, 32.0, 38.0, 46.0];
pub const READER_LINE_SPACINGS: [f32; 3] = [1.24, 1.38, 1.56];

/// Bit flags packed into the `weight: u8` style byte passed to GPU text measure/draw callbacks:
/// - `bits 0..=1`: weight (`0=Regular, 1=Medium, 2=SemiBold, 3=Bold`)
/// - `bit 2 (0x04)`: Italic face (`NotoSerif-Italic` / `NotoSerif-BoldItalic`)
/// - `bit 3 (0x08)`: Editorial Serif family (`Noto Serif` + `Noto Serif CJK`) instead of `Geologica`
pub const FONT_STYLE_ITALIC_BIT: u8 = 0x04;
pub const FONT_STYLE_SERIF_BIT: u8 = 0x08;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReaderFont {
    Sans = 0,
    Serif = 1,
}

impl ReaderFont {
    pub const fn toggle(self) -> Self {
        match self {
            Self::Sans => Self::Serif,
            Self::Serif => Self::Sans,
        }
    }

    pub const fn from_u8(v: u8) -> Self {
        if v == 1 {
            Self::Serif
        } else {
            Self::Sans
        }
    }

    #[inline]
    pub const fn style_flag(self) -> u8 {
        match self {
            Self::Sans => 0,
            Self::Serif => FONT_STYLE_SERIF_BIT,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReadingMode {
    Paginated = 0,
    Scroll = 1,
}

impl ReadingMode {
    pub const fn toggle(self) -> Self {
        match self {
            Self::Paginated => Self::Scroll,
            Self::Scroll => Self::Paginated,
        }
    }

    pub const fn from_u8(v: u8) -> Self {
        if v == 1 {
            Self::Scroll
        } else {
            Self::Paginated
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProgressDisplay {
    Percent = 0,
    Pages = 1,
}

impl ProgressDisplay {
    pub const fn toggle(self) -> Self {
        match self {
            Self::Percent => Self::Pages,
            Self::Pages => Self::Percent,
        }
    }

    pub const fn from_u8(v: u8) -> Self {
        if v == 1 {
            Self::Pages
        } else {
            Self::Percent
        }
    }
}

/// Persistent global reader preferences (`settings.txt` / `localStorage`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReaderPrefs {
    pub mode: ReadingMode,
    pub size_idx: usize,
    pub spacing_idx: usize,
    pub hide_ui: bool,
    pub progress_display: ProgressDisplay,
    pub font: ReaderFont,
}

impl Default for ReaderPrefs {
    fn default() -> Self {
        Self {
            mode: ReadingMode::Paginated,
            size_idx: 1,
            spacing_idx: 1,
            hide_ui: false,
            progress_display: ProgressDisplay::Percent,
            font: ReaderFont::Sans,
        }
    }
}

impl ReaderPrefs {
    pub fn serialize(&self) -> String {
        format!(
            "v3 {} {} {} {} {} {}\n",
            self.mode as u8,
            self.size_idx,
            self.spacing_idx,
            u8::from(self.hide_ui),
            self.progress_display as u8,
            self.font as u8
        )
    }

    pub fn deserialize(raw: &str) -> Self {
        let mut it = raw.split_whitespace();
        let Some(first) = it.next() else {
            return Self::default();
        };
        if first == "v3" || first == "v2" {
            let mode = it
                .next()
                .and_then(|s| s.parse::<u8>().ok())
                .map(ReadingMode::from_u8)
                .unwrap_or(ReadingMode::Paginated);
            let sz = it
                .next()
                .and_then(|s| s.parse::<usize>().ok())
                .unwrap_or(1)
                .min(READER_FONT_SIZES.len() - 1);
            let sp = it
                .next()
                .and_then(|s| s.parse::<usize>().ok())
                .unwrap_or(1)
                .min(READER_LINE_SPACINGS.len() - 1);
            let hide = it
                .next()
                .and_then(|s| s.parse::<u8>().ok())
                .map(|v| v != 0)
                .unwrap_or(false);
            let prog = it
                .next()
                .and_then(|s| s.parse::<u8>().ok())
                .map(ProgressDisplay::from_u8)
                .unwrap_or(ProgressDisplay::Percent);
            let font = if first == "v3" {
                it.next()
                    .and_then(|s| s.parse::<u8>().ok())
                    .map(ReaderFont::from_u8)
                    .unwrap_or(ReaderFont::Sans)
            } else {
                ReaderFont::Sans
            };
            Self {
                mode,
                size_idx: sz,
                spacing_idx: sp,
                hide_ui: hide,
                progress_display: prog,
                font,
            }
        } else {
            // Backward compatibility with v0.2.x `<size_idx> <spacing_idx>`
            let sz = first
                .parse::<usize>()
                .unwrap_or(1)
                .min(READER_FONT_SIZES.len() - 1);
            let sp = it
                .next()
                .and_then(|s| s.parse::<usize>().ok())
                .unwrap_or(1)
                .min(READER_LINE_SPACINGS.len() - 1);
            Self {
                size_idx: sz,
                spacing_idx: sp,
                ..Self::default()
            }
        }
    }
}

#[derive(Debug, Clone)]
pub struct PositionedSegment {
    pub x_rel: f32,
    pub width: f32,
    pub text: String,
    pub font_size: f32,
    pub weight: u8,
    pub style: InlineStyle,
    pub footnote_idx: Option<usize>,
}

#[derive(Debug, Clone)]
pub enum VisualItemKind {
    TextLine {
        segments: Vec<PositionedSegment>,
        heading_level: u8,
        is_quote: bool,
        is_code: bool,
        centered: bool,
        line_width: f32,
    },
    Illustration {
        image_idx: usize,
        disp_w: u32,
        disp_h: u32,
        caption: String,
    },
    Spacer,
    PageBreak,
}

#[derive(Debug, Clone)]
pub struct VisualItem {
    pub start_offset: usize,
    pub end_offset: usize,
    pub y_scroll: f32,
    pub height: f32,
    pub kind: VisualItemKind,
}

#[derive(Debug, Clone, Copy)]
pub struct PageSlice {
    pub start_item: usize,
    pub end_item: usize,
    pub start_offset: usize,
    pub end_offset: usize,
}

/// Complete laid-out geometry for a book at a given viewport & font configuration,
/// plus live reading state (`anchor_offset`, `current_page`, `scroll_px`, `Hide UI`, Footnotes).
pub struct ReaderViewState {
    pub items: Vec<VisualItem>,
    pub pages: Vec<PageSlice>,
    pub total_scroll_h: f32,
    pub viewport_w: f32,
    pub viewport_top_y: f32,
    pub viewport_h: f32,
    pub col_x: f32,
    pub col_w: f32,

    pub anchor_offset: usize,
    pub current_page: usize,
    pub scroll_px: f32,

    /// Auto-hide chrome state when `prefs.hide_ui == true`
    pub ui_hidden: bool,
    pub ui_reveal_until_ms: u64,

    /// Footnote selection & open popup state
    pub focused_fn_slot: usize,
    pub open_footnote_idx: Option<usize>,
    pub footnote_scroll_line: usize,

    layout_key: Option<(u32, u32, usize, usize, bool, u8)>,
}

impl ReaderViewState {
    pub fn new(initial_offset: usize) -> Self {
        Self {
            items: Vec::new(),
            pages: Vec::new(),
            total_scroll_h: 0.0,
            viewport_w: 1024.0,
            viewport_top_y: READER_SPEC.header_content_top_y,
            viewport_h: 580.0,
            col_x: READER_SPEC.screen_margin_x,
            col_w: 960.0,
            anchor_offset: initial_offset,
            current_page: 0,
            scroll_px: 0.0,
            ui_hidden: false,
            ui_reveal_until_ms: 0,
            focused_fn_slot: 0,
            open_footnote_idx: None,
            footnote_scroll_line: 0,
            layout_key: None,
        }
    }

    #[inline]
    pub fn is_chrome_visible(&self, prefs: &ReaderPrefs, _now_ms: u64) -> bool {
        !prefs.hide_ui
    }

    pub fn tick_auto_hide(&mut self, prefs: &ReaderPrefs, _now_ms: u64) {
        self.ui_hidden = prefs.hide_ui;
        self.ui_reveal_until_ms = 0;
    }

    pub fn note_forward_motion(&mut self, prefs: &ReaderPrefs) {
        self.ui_hidden = prefs.hide_ui;
        self.ui_reveal_until_ms = 0;
        self.focused_fn_slot = 0;
    }

    pub fn note_backward_motion(&mut self, prefs: &ReaderPrefs, _now_ms: u64) {
        self.ui_hidden = prefs.hide_ui;
        self.ui_reveal_until_ms = 0;
        self.focused_fn_slot = 0;
    }

    pub fn invalidate_layout(&mut self) {
        self.layout_key = None;
    }

    /// Re-lays out the document if screen dimensions, font size, line spacing, or `hide_ui` changed,
    /// preserving the exact `anchor_offset` across both Paginated and Scroll modes.
    pub fn ensure_layout<F>(
        &mut self,
        doc: &EbookDoc,
        prefs: &ReaderPrefs,
        screen_w: f32,
        screen_h: f32,
        mut measure_text: F,
    ) where
        F: FnMut(&str, f32, u8) -> f32,
    {
        let key = (
            screen_w.round() as u32,
            screen_h.round() as u32,
            prefs.size_idx,
            prefs.spacing_idx,
            prefs.hide_ui,
            prefs.font as u8,
        );
        if self.layout_key == Some(key) && !self.items.is_empty() {
            return;
        }

        let saved_anchor = self.anchor_offset;
        self.viewport_w = screen_w;
        self.viewport_top_y = if prefs.hide_ui {
            READER_SPEC.chromeless_top_pad
        } else {
            READER_SPEC.header_content_top_y
        };
        self.viewport_h = (screen_h - self.viewport_top_y - READER_SPEC.bottom_strip_h).max(160.0);
        self.col_x = READER_SPEC.screen_margin_x;
        self.col_w = (screen_w - READER_SPEC.screen_margin_x * 2.0).max(220.0);

        let base_fsz = READER_FONT_SIZES[prefs.size_idx];
        let spacing = READER_LINE_SPACINGS[prefs.spacing_idx];
        let body_line_h = (base_fsz * spacing).round();
        let max_img_h = (self.viewport_h * READER_SPEC.max_page_img_h_ratio).max(96.0);

        let mut items: Vec<VisualItem> = Vec::with_capacity(doc.blocks.len() * 2);
        let mut y_cursor = 0.0f32;

        for block in &doc.blocks {
            match &block.kind {
                BlockKind::Spacer => {
                    let h = (body_line_h * 0.55).round();
                    items.push(VisualItem {
                        start_offset: block.start_offset,
                        end_offset: block.end_offset,
                        y_scroll: y_cursor,
                        height: h,
                        kind: VisualItemKind::Spacer,
                    });
                    y_cursor += h;
                }
                BlockKind::PageBreak => {
                    let h = (body_line_h * 0.65).round();
                    items.push(VisualItem {
                        start_offset: block.start_offset,
                        end_offset: block.end_offset,
                        y_scroll: y_cursor,
                        height: h,
                        kind: VisualItemKind::PageBreak,
                    });
                    y_cursor += h;
                }
                BlockKind::Image { image_idx, caption } => {
                    if let Some(img) = doc.images.get(*image_idx) {
                        let cap_extra = if caption.is_empty() { 0.0 } else { 28.0 };
                        let avail_img_h = (max_img_h - READER_SPEC.image_v_pad * 2.0 - cap_extra).max(64.0);
                        let (dw, dh) =
                            fit_illustration_box(img.width, img.height, self.col_w - 8.0, avail_img_h);
                        let total_h = (dh as f32) + READER_SPEC.image_v_pad * 2.0 + cap_extra;
                        items.push(VisualItem {
                            start_offset: block.start_offset,
                            end_offset: block.end_offset,
                            y_scroll: y_cursor,
                            height: total_h,
                            kind: VisualItemKind::Illustration {
                                image_idx: *image_idx,
                                disp_w: dw,
                                disp_h: dh,
                                caption: caption.clone(),
                            },
                        });
                        y_cursor += total_h;
                    }
                }
                BlockKind::Paragraph {
                    runs,
                    heading_level,
                    is_quote,
                    is_code,
                    centered,
                } => {
                    let h_lvl = *heading_level;
                    let fsz = match h_lvl {
                        1 => base_fsz + 6.0,
                        2 => base_fsz + 3.0,
                        3 => base_fsz + 1.0,
                        _ if *is_code => (base_fsz - 3.0).max(20.0),
                        _ => base_fsz,
                    };
                    let line_h = (fsz * spacing).round();
                    let indent = if *is_quote {
                        READER_SPEC.quote_indent_x
                    } else {
                        0.0
                    };
                    let max_line_w = (self.col_w - indent).max(120.0);

                    if h_lvl > 0 && !items.is_empty() {
                        if !matches!(
                            items.last().map(|it| &it.kind),
                            Some(VisualItemKind::Spacer | VisualItemKind::PageBreak)
                        ) {
                            let pre_gap = (body_line_h * 0.35).round();
                            items.push(VisualItem {
                                start_offset: block.start_offset,
                                end_offset: block.start_offset,
                                y_scroll: y_cursor,
                                height: pre_gap,
                                kind: VisualItemKind::Spacer,
                            });
                            y_cursor += pre_gap;
                        }
                    }

                    wrap_paragraph_runs(
                        runs,
                        block.start_offset,
                        block.end_offset,
                        fsz,
                        line_h,
                        h_lvl,
                        *is_quote,
                        *is_code,
                        *centered,
                        prefs.font,
                        indent,
                        max_line_w,
                        &mut y_cursor,
                        &mut items,
                        &mut measure_text,
                    );

                    // Subtle paragraph breathing gap for structured e-books
                    if doc.format.is_ebook_prose() && !*is_code {
                        let gap = (body_line_h * 0.28).round();
                        items.push(VisualItem {
                            start_offset: block.end_offset,
                            end_offset: block.end_offset,
                            y_scroll: y_cursor,
                            height: gap,
                            kind: VisualItemKind::Spacer,
                        });
                        y_cursor += gap;
                    }
                }
            }
        }

        if items.is_empty() {
            items.push(VisualItem {
                start_offset: 0,
                end_offset: 1,
                y_scroll: 0.0,
                height: body_line_h,
                kind: VisualItemKind::Spacer,
            });
            y_cursor = body_line_h;
        }

        self.pages = paginate_items(&items, self.viewport_h);
        self.items = items;
        self.total_scroll_h = y_cursor;
        self.layout_key = Some(key);
        self.sync_to_anchor(saved_anchor);
    }

    /// Synchronizes both `current_page` (Paginated) and `scroll_px` (Infinite Scroll) to `offset`!
    pub fn sync_to_anchor(&mut self, offset: usize) {
        if self.items.is_empty() {
            self.anchor_offset = offset;
            return;
        }
        let clamped = self
            .items
            .last()
            .map(|last| offset.min(last.end_offset))
            .unwrap_or(offset);
        self.anchor_offset = clamped;

        // 1. Sync `current_page`
        if !self.pages.is_empty() {
            let mut p_idx = match self
                .pages
                .binary_search_by_key(&clamped, |p| p.start_offset)
            {
                Ok(exact) => {
                    // Walk backward if multiple pages share start_offset, or forward to last matching
                    let mut i = exact;
                    while i > 0 && self.pages[i - 1].start_offset == clamped {
                        i -= 1;
                    }
                    i
                }
                Err(ins) => ins.saturating_sub(1),
            };
            p_idx = p_idx.min(self.pages.len() - 1);
            self.current_page = p_idx;
        } else {
            self.current_page = 0;
        }

        // 2. Sync `scroll_px`
        if !self.items.is_empty() {
            let item_idx = match self
                .items
                .binary_search_by_key(&clamped, |it| it.start_offset)
            {
                Ok(mut exact) => {
                    while exact > 0 && self.items[exact - 1].start_offset == clamped {
                        exact -= 1;
                    }
                    // Skip a pure PageBreak/Spacer so text starts crisply at the top of the viewport
                    while exact + 1 < self.items.len()
                        && matches!(
                            self.items[exact].kind,
                            VisualItemKind::PageBreak | VisualItemKind::Spacer
                        )
                    {
                        exact += 1;
                    }
                    exact
                }
                Err(ins) => ins.saturating_sub(1),
            };
            let max_s = self.max_scroll_px();
            self.scroll_px = self.items[item_idx].y_scroll.clamp(0.0, max_s);
        } else {
            self.scroll_px = 0.0;
        }
        self.focused_fn_slot = 0;
    }

    #[inline]
    pub fn max_scroll_px(&self) -> f32 {
        (self.total_scroll_h - self.viewport_h).max(0.0)
    }

    /// Updates `anchor_offset` from the currently visible page or scroll position.
    pub fn refresh_anchor_from_mode(&mut self, mode: ReadingMode) {
        match mode {
            ReadingMode::Paginated => {
                if let Some(page) = self.pages.get(self.current_page) {
                    if self.anchor_offset < page.start_offset || self.anchor_offset > page.end_offset
                    {
                        self.anchor_offset = page.start_offset;
                    }
                }
            }
            ReadingMode::Scroll => {
                let top_y = self.scroll_px + 4.0;
                let idx = match self.items.binary_search_by(|it| {
                    (it.y_scroll + it.height)
                        .partial_cmp(&top_y)
                        .unwrap_or(core::cmp::Ordering::Equal)
                }) {
                    Ok(i) | Err(i) => i.min(self.items.len().saturating_sub(1)),
                };
                if let Some(it) = self.items.get(idx) {
                    self.anchor_offset = it.start_offset;
                }
            }
        }
    }

    pub fn step_page(&mut self, delta: i32, prefs: &ReaderPrefs, now_ms: u64) -> bool {
        if self.pages.is_empty() {
            return false;
        }
        let max_p = self.pages.len() - 1;
        let next = if delta < 0 {
            self.current_page.saturating_sub((-delta) as usize)
        } else {
            (self.current_page + (delta as usize)).min(max_p)
        };
        if next != self.current_page {
            if next > self.current_page {
                self.note_forward_motion(prefs);
            } else {
                self.note_backward_motion(prefs, now_ms);
            }
            self.current_page = next;
            self.anchor_offset = self.pages[next].start_offset;
            // Keep scroll_px aligned too
            if let Some(first_it) = self.items.get(self.pages[next].start_item) {
                self.scroll_px = first_it.y_scroll.clamp(0.0, self.max_scroll_px());
            }
            true
        } else if delta < 0 {
            // Even on page 1, pressing Left/Up reveals UI chrome if hidden!
            self.note_backward_motion(prefs, now_ms);
            false
        } else {
            false
        }
    }

    pub fn scroll_by_px(&mut self, delta_px: f32, prefs: &ReaderPrefs, now_ms: u64) -> bool {
        let max_s = self.max_scroll_px();
        let next = (self.scroll_px + delta_px).clamp(0.0, max_s);
        if (next - self.scroll_px).abs() > 0.25 {
            if delta_px > 0.0 {
                self.note_forward_motion(prefs);
            } else if delta_px < 0.0 {
                self.note_backward_motion(prefs, now_ms);
            }
            self.scroll_px = next;
            self.refresh_anchor_from_mode(ReadingMode::Scroll);
            // Keep `current_page` synced in background
            if !self.pages.is_empty() {
                let p = match self
                    .pages
                    .binary_search_by_key(&self.anchor_offset, |pg| pg.start_offset)
                {
                    Ok(i) => i,
                    Err(i) => i.saturating_sub(1),
                };
                self.current_page = p.min(self.pages.len() - 1);
            }
            true
        } else if delta_px < 0.0 {
            self.note_backward_motion(prefs, now_ms);
            false
        } else {
            false
        }
    }

    /// Returns `(progress_0_to_1, left_readout_string)` for the full-width bottom progress bar!
    pub fn progress_metrics(&self, prefs: &ReaderPrefs) -> (f32, String) {
        let total_pages = self.pages.len().max(1);
        match prefs.mode {
            ReadingMode::Paginated => {
                let read_pages = (self.current_page + 1).min(total_pages);
                match prefs.progress_display {
                    ProgressDisplay::Percent => {
                        let frac = if total_pages <= 1 {
                            1.0
                        } else {
                            (self.current_page as f32) / ((total_pages - 1) as f32)
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
                let max_s = self.max_scroll_px();
                let scroll_f = if max_s <= 1.0 {
                    1.0
                } else {
                    (self.scroll_px / max_s).clamp(0.0, 1.0)
                };
                match prefs.progress_display {
                    ProgressDisplay::Percent => {
                        (scroll_f, format!("{}%", (scroll_f * 100.0).round() as u32))
                    }
                    ProgressDisplay::Pages => {
                        let read_pages = (((scroll_f * ((total_pages - 1) as f32)).round()
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

    /// Returns ordered unique `footnote_idx` markers currently visible on screen.
    pub fn visible_footnotes(&self, mode: ReadingMode) -> Vec<usize> {
        let mut out = Vec::new();
        match mode {
            ReadingMode::Paginated => {
                if let Some(pg) = self.pages.get(self.current_page) {
                    for it in &self.items[pg.start_item..pg.end_item] {
                        collect_item_footnotes(it, &mut out);
                    }
                }
            }
            ReadingMode::Scroll => {
                let top = self.scroll_px;
                let bot = self.scroll_px + self.viewport_h;
                for it in &self.items {
                    if it.y_scroll + it.height <= top + 2.0 {
                        continue;
                    }
                    if it.y_scroll >= bot - 2.0 {
                        break;
                    }
                    collect_item_footnotes(it, &mut out);
                }
            }
        }
        out
    }

    /// Returns the index of the active chapter in `doc.toc` (`<= anchor_offset`).
    pub fn active_toc_index(&self, doc: &EbookDoc) -> usize {
        if doc.toc.is_empty() {
            return 0;
        }
        let mut best = 0usize;
        for (i, entry) in doc.toc.iter().enumerate() {
            if entry.offset <= self.anchor_offset.saturating_add(2) {
                best = i;
            } else {
                break;
            }
        }
        best
    }
}

fn collect_item_footnotes(item: &VisualItem, out: &mut Vec<usize>) {
    if let VisualItemKind::TextLine { segments, .. } = &item.kind {
        for seg in segments {
            if let Some(fn_idx) = seg.footnote_idx {
                if !out.contains(&fn_idx) {
                    out.push(fn_idx);
                }
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn wrap_paragraph_runs<F>(
    runs: &[InlineRun],
    block_start: usize,
    block_end: usize,
    fsz: f32,
    line_h: f32,
    heading_level: u8,
    is_quote: bool,
    is_code: bool,
    centered: bool,
    font: ReaderFont,
    indent: f32,
    max_line_w: f32,
    y_cursor: &mut f32,
    items: &mut Vec<VisualItem>,
    measure_text: &mut F,
) where
    F: FnMut(&str, f32, u8) -> f32,
{
    let mut cur_segs: Vec<PositionedSegment> = Vec::new();
    let mut cur_x = 0.0f32;
    let mut cur_line_start = block_start;
    let mut cur_offset = block_start;
    let mut emitted_any = false;

    let emit_line = |segs: &mut Vec<PositionedSegment>,
                     line_w: f32,
                     l_start: usize,
                     l_end: usize,
                     y_cursor: &mut f32,
                     items: &mut Vec<VisualItem>,
                     emitted_any: &mut bool| {
        if segs.is_empty() {
            return;
        }
        let mut positioned = std::mem::take(segs);
        let shift_x = if centered {
            ((max_line_w - line_w).max(0.0)) * 0.5
        } else {
            indent
        };
        if shift_x > 0.0 {
            for s in &mut positioned {
                s.x_rel += shift_x;
            }
        }
        items.push(VisualItem {
            start_offset: l_start.min(block_end),
            end_offset: l_end.min(block_end).max(l_start.min(block_end)),
            y_scroll: *y_cursor,
            height: line_h,
            kind: VisualItemKind::TextLine {
                segments: positioned,
                heading_level,
                is_quote,
                is_code,
                centered,
                line_width: line_w,
            },
        });
        *y_cursor += line_h;
        *emitted_any = true;
    };

    let mut push_atom = |atom_text: &str,
                         atom_style: InlineStyle,
                         atom_fn: Option<usize>,
                         char_len: usize,
                         atom_w: f32,
                         r_fsz: f32,
                         weight: u8,
                         pad_x: f32| {
        let is_pure_space = atom_fn.is_none() && atom_text.bytes().all(|b| b == b' ');
        if cur_segs.is_empty() && is_pure_space && !is_code {
            cur_offset += char_len;
            cur_line_start = cur_offset;
            return;
        }

        if !cur_segs.is_empty() && cur_x + atom_w > max_line_w + 0.5 {
            emit_line(
                &mut cur_segs,
                cur_x,
                cur_line_start,
                cur_offset,
                y_cursor,
                items,
                &mut emitted_any,
            );
            cur_x = 0.0;
            cur_line_start = cur_offset;
            if is_pure_space && !is_code {
                cur_offset += char_len;
                cur_line_start = cur_offset;
                return;
            }
        }

        // Merge adjacent compatible plain segments in O(1) without re-shaping the line prefix!
        if atom_fn.is_none() && pad_x == 0.0 {
            if let Some(prev) = cur_segs.last_mut() {
                if prev.footnote_idx.is_none()
                    && prev.style == atom_style
                    && (prev.font_size - r_fsz).abs() < 0.1
                    && prev.weight == weight
                {
                    prev.text.push_str(atom_text);
                    prev.width += atom_w;
                    cur_x = prev.x_rel + prev.width;
                    cur_offset += char_len;
                    return;
                }
            }
        }

        cur_segs.push(PositionedSegment {
            x_rel: cur_x,
            width: atom_w,
            text: atom_text.to_string(),
            font_size: r_fsz,
            weight,
            style: atom_style,
            footnote_idx: atom_fn,
        });
        cur_x += atom_w;
        cur_offset += char_len;
    };

    for run in runs {
        let (r_fsz, weight, pad_x) = style_metrics(run.style, heading_level, fsz, font);
        if run.footnote_idx.is_some() {
            let w = measure_text(&run.text, r_fsz, weight) + pad_x * 2.0 + 4.0;
            let clen = run.text.chars().count().max(1);
            push_atom(&run.text, run.style, run.footnote_idx, clen, w, r_fsz, weight, pad_x);
            continue;
        }

        for_each_wrap_token(&run.text, is_code, |token| {
            let tok_w = measure_text(token, r_fsz, weight);
            let tok_chars = token.chars().count();
            if tok_w <= max_line_w || tok_chars <= 1 {
                push_atom(token, run.style, None, tok_chars, tok_w, r_fsz, weight, pad_x);
            } else {
                // Break extra-long unspaced token cleanly in O(chars) by accumulating single-char widths
                let mut chunk_start = 0usize;
                let mut chunk_chars = 0usize;
                let mut chunk_w = 0.0f32;
                let mut ch_buf = [0u8; 4];
                for (bi, ch) in token.char_indices() {
                    let s = ch.encode_utf8(&mut ch_buf);
                    let cw = measure_text(s, r_fsz, weight);
                    if chunk_chars > 0 && chunk_w + cw > max_line_w {
                        push_atom(
                            &token[chunk_start..bi],
                            run.style,
                            None,
                            chunk_chars,
                            chunk_w,
                            r_fsz,
                            weight,
                            pad_x,
                        );
                        chunk_start = bi;
                        chunk_chars = 1;
                        chunk_w = cw;
                    } else {
                        chunk_chars += 1;
                        chunk_w += cw;
                    }
                }
                if chunk_chars > 0 {
                    push_atom(
                        &token[chunk_start..],
                        run.style,
                        None,
                        chunk_chars,
                        chunk_w,
                        r_fsz,
                        weight,
                        pad_x,
                    );
                }
            }
        });
    }

    if !cur_segs.is_empty() {
        emit_line(
            &mut cur_segs,
            cur_x,
            cur_line_start,
            block_end.max(cur_offset),
            y_cursor,
            items,
            &mut emitted_any,
        );
    } else if !emitted_any {
        items.push(VisualItem {
            start_offset: block_start,
            end_offset: block_end,
            y_scroll: *y_cursor,
            height: line_h,
            kind: VisualItemKind::Spacer,
        });
        *y_cursor += line_h;
    }
}

/// Zero-allocation fast page counter for background worker chapter page-count refinement
/// (`< 0.15 ms` per chapter using a cloned `FontMetricsTable` without allocating `VisualItem`s).
pub fn count_blocks_pages_fast(
    blocks: &[DocBlock],
    image_dims: &[(u32, u32)],
    format: DocFormat,
    prefs: &ReaderPrefs,
    screen_w: f32,
    screen_h: f32,
    metrics: &mut FontMetricsTable,
) -> usize {
    let viewport_top_y = if prefs.hide_ui {
        READER_SPEC.chromeless_top_pad
    } else {
        READER_SPEC.header_content_top_y
    };
    let viewport_h = (screen_h - viewport_top_y - READER_SPEC.bottom_strip_h).max(160.0);
    let col_w = (screen_w - READER_SPEC.screen_margin_x * 2.0).max(220.0);
    let base_fsz = READER_FONT_SIZES[prefs.size_idx];
    let spacing = READER_LINE_SPACINGS[prefs.spacing_idx];
    let body_line_h = (base_fsz * spacing).round();
    let max_img_h = (viewport_h * READER_SPEC.max_page_img_h_ratio).max(96.0);
    metrics.reconfigure(prefs.font, base_fsz);

    struct PageAcc {
        viewport_h: f32,
        page_count: usize,
        on_page: bool,
        used_h: f32,
        last_was_space_or_break: bool,
    }
    impl PageAcc {
        #[inline]
        fn push_h(&mut self, h: f32, is_spacer: bool, is_break: bool) {
            if is_break {
                if self.on_page {
                    self.page_count += 1;
                    self.on_page = false;
                    self.used_h = 0.0;
                }
                self.last_was_space_or_break = true;
                return;
            }
            if is_spacer {
                self.last_was_space_or_break = true;
                if !self.on_page {
                    return;
                }
                if self.used_h + h > self.viewport_h + 0.5 {
                    self.page_count += 1;
                    self.on_page = false;
                    self.used_h = 0.0;
                } else {
                    self.used_h += h;
                }
                return;
            }
            self.last_was_space_or_break = false;
            if !self.on_page {
                self.on_page = true;
                self.used_h = h;
            } else if self.used_h + h > self.viewport_h + 0.5 {
                self.page_count += 1;
                self.used_h = h;
            } else {
                self.used_h += h;
            }
        }
    }

    let mut acc = PageAcc {
        viewport_h,
        page_count: 0,
        on_page: false,
        used_h: 0.0,
        last_was_space_or_break: true,
    };

    for block in blocks {
        match &block.kind {
            BlockKind::Spacer => acc.push_h((body_line_h * 0.55).round(), true, false),
            BlockKind::PageBreak => acc.push_h((body_line_h * 0.65).round(), false, true),
            BlockKind::Image { image_idx, caption } => {
                if let Some(&(iw, ih)) = image_dims.get(*image_idx) {
                    if iw > 0 && ih > 0 {
                        let cap_extra = if caption.is_empty() { 0.0 } else { 28.0 };
                        let avail_img_h =
                            (max_img_h - READER_SPEC.image_v_pad * 2.0 - cap_extra).max(64.0);
                        let (_, dh) = fit_illustration_box(iw, ih, col_w - 8.0, avail_img_h);
                        let total_h = (dh as f32) + READER_SPEC.image_v_pad * 2.0 + cap_extra;
                        acc.push_h(total_h, false, false);
                    }
                }
            }
            BlockKind::Paragraph {
                runs,
                heading_level,
                is_quote,
                is_code,
                ..
            } => {
                let h_lvl = *heading_level;
                let fsz = match h_lvl {
                    1 => base_fsz + 6.0,
                    2 => base_fsz + 3.0,
                    3 => base_fsz + 1.0,
                    _ if *is_code => (base_fsz - 3.0).max(20.0),
                    _ => base_fsz,
                };
                let line_h = (fsz * spacing).round();
                let indent = if *is_quote {
                    READER_SPEC.quote_indent_x
                } else {
                    0.0
                };
                let max_line_w = (col_w - indent).max(120.0);

                if h_lvl > 0 && !acc.last_was_space_or_break {
                    acc.push_h((body_line_h * 0.35).round(), true, false);
                }

                let mut cur_x = 0.0f32;
                let mut line_nonempty = false;
                let mut para_lines = 0usize;

                let mut step_tok = |w: f32, is_space: bool| {
                    if !line_nonempty && is_space && !*is_code {
                        return;
                    }
                    if line_nonempty && cur_x + w > max_line_w + 0.5 {
                        acc.push_h(line_h, false, false);
                        para_lines += 1;
                        cur_x = 0.0;
                        line_nonempty = false;
                        if is_space && !*is_code {
                            return;
                        }
                    }
                    cur_x += w;
                    line_nonempty = true;
                };

                for run in runs {
                    let (r_fsz, weight, pad_x) = style_metrics(run.style, h_lvl, fsz, prefs.font);
                    if run.footnote_idx.is_some() {
                        let w = metrics.measure_text(&run.text, r_fsz, weight) + pad_x * 2.0 + 4.0;
                        step_tok(w, false);
                        continue;
                    }
                    for_each_wrap_token(&run.text, *is_code, |tok| {
                        let is_sp = tok.bytes().all(|b| b == b' ');
                        let tw = metrics.measure_text(tok, r_fsz, weight);
                        if tw <= max_line_w || tok.chars().count() <= 1 {
                            step_tok(tw, is_sp);
                        } else {
                            let mut cw_acc = 0.0f32;
                            let mut cnt = 0usize;
                            for ch in tok.chars() {
                                let cw = metrics.char_width(ch, r_fsz, weight);
                                if cnt > 0 && cw_acc + cw > max_line_w {
                                    step_tok(cw_acc, false);
                                    cw_acc = cw;
                                    cnt = 1;
                                } else {
                                    cw_acc += cw;
                                    cnt += 1;
                                }
                            }
                            if cnt > 0 {
                                step_tok(cw_acc, false);
                            }
                        }
                    });
                }

                if line_nonempty {
                    acc.push_h(line_h, false, false);
                } else if para_lines == 0 {
                    acc.push_h(line_h, true, false);
                }

                if format.is_ebook_prose() && !*is_code {
                    acc.push_h((body_line_h * 0.28).round(), true, false);
                }
            }
        }
    }

    if acc.on_page {
        acc.page_count += 1;
    }
    acc.page_count.max(1)
}

fn style_metrics(
    style: InlineStyle,
    heading_level: u8,
    base_fsz: f32,
    font: ReaderFont,
) -> (f32, u8, f32) {
    let serif_flag = font.style_flag();
    if heading_level == 1 {
        return (base_fsz, 3 | serif_flag, 0.0);
    }
    if heading_level >= 2 {
        return (base_fsz, 2 | serif_flag, 0.0);
    }
    match style {
        InlineStyle::Regular => (base_fsz, serif_flag, 0.0),
        InlineStyle::Bold => (base_fsz, 3 | serif_flag, 0.0),
        InlineStyle::BoldItalic => (base_fsz, 3 | FONT_STYLE_ITALIC_BIT | serif_flag, 0.0),
        InlineStyle::Italic => (base_fsz, 1 | FONT_STYLE_ITALIC_BIT | serif_flag, 0.0),
        InlineStyle::Code => ((base_fsz - 3.0).max(19.0), 1, 0.0),
        InlineStyle::FootnoteRef => (
            (base_fsz * 0.78).clamp(18.0, 28.0),
            3 | serif_flag,
            READER_SPEC.footnote_pill_pad_x,
        ),
    }
}

/// Zero-allocation `&str` slice scanner yielding wrap tokens (words + trailing space, plus individual
/// CJK ideographs with attached trailing CJK punctuation).
fn for_each_wrap_token<'a, F: FnMut(&'a str)>(
    s: &'a str,
    preserve_leading_spaces: bool,
    mut emit: F,
) {
    if s.is_empty() {
        return;
    }
    let bytes = s.as_bytes();
    let mut cur_start = 0usize;
    let mut iter = s.char_indices().peekable();

    if preserve_leading_spaces {
        let mut end_sp = 0usize;
        while let Some(&(bi, ' ')) = iter.peek() {
            end_sp = bi + 1;
            let _ = iter.next();
        }
        if end_sp > 0 {
            emit(&s[0..end_sp]);
            cur_start = end_sp;
        }
    }

    while let Some((bi, ch)) = iter.next() {
        if is_cjk_breakable(ch) {
            if bi > cur_start {
                emit(&s[cur_start..bi]);
            }
            let mut cjk_end = bi + ch.len_utf8();
            while let Some(&(nbi, next)) = iter.peek() {
                if matches!(
                    next,
                    '。' | '、' | '，' | '．' | '！' | '？' | '」' | '』' | '）' | '》'
                ) {
                    cjk_end = nbi + next.len_utf8();
                    let _ = iter.next();
                } else {
                    break;
                }
            }
            emit(&s[bi..cjk_end]);
            cur_start = cjk_end;
        } else if ch == ' ' {
            let mut sp_end = bi + 1;
            while sp_end < bytes.len() && bytes[sp_end] == b' ' {
                sp_end += 1;
                let _ = iter.next();
            }
            emit(&s[cur_start..sp_end]);
            cur_start = sp_end;
        }
    }
    if cur_start < s.len() {
        emit(&s[cur_start..]);
    }
}

#[inline]
fn is_cjk_breakable(ch: char) -> bool {
    let u = ch as u32;
    (0x3040..=0x30FF).contains(&u)
        || (0x3400..=0x4DBF).contains(&u)
        || (0x4E00..=0x9FFF).contains(&u)
        || (0xAC00..=0xD7AF).contains(&u)
}

/// Packs `items` into discrete non-overlapping pages with **zero cut-off lines or clipped images**.
fn paginate_items(items: &[VisualItem], viewport_h: f32) -> Vec<PageSlice> {
    let mut pages = Vec::new();
    let n = items.len();
    let mut i = 0usize;

    while i < n {
        // Skip leading PageBreak / Spacer at the very top of a fresh page
        while i < n
            && matches!(
                items[i].kind,
                VisualItemKind::PageBreak | VisualItemKind::Spacer
            )
        {
            i += 1;
        }
        if i >= n {
            break;
        }

        let start_item = i;
        let start_offset = items[start_item].start_offset;
        let mut used_h = 0.0f32;
        let mut end_item = start_item;

        while end_item < n {
            let it = &items[end_item];
            if matches!(it.kind, VisualItemKind::PageBreak) {
                end_item += 1;
                break;
            }
            let h = it.height;
            if end_item > start_item && used_h + h > viewport_h + 0.5 {
                break;
            }
            used_h += h;
            end_item += 1;
        }

        if end_item == start_item {
            end_item = start_item + 1;
        }
        let end_offset = items[end_item - 1].end_offset;
        pages.push(PageSlice {
            start_item,
            end_item,
            start_offset,
            end_offset,
        });
        i = end_item;
    }

    if pages.is_empty() {
        pages.push(PageSlice {
            start_item: 0,
            end_item: items.len().min(1),
            start_offset: 0,
            end_offset: 1,
        });
    }
    pages
}

/// Wraps a Footnote's paragraphs for the floating reader popup card.
pub fn wrap_footnote_lines<F>(
    paragraphs: &[String],
    font_size: f32,
    max_w: f32,
    mut measure_text: F,
) -> Vec<String>
where
    F: FnMut(&str, f32, u8) -> f32,
{
    let mut out = Vec::new();
    for (pi, para) in paragraphs.iter().enumerate() {
        if pi > 0 && !out.is_empty() {
            out.push(String::new());
        }
        let mut cur = String::new();
        let mut cur_w = 0.0f32;
        for_each_wrap_token(para, false, |tok| {
            let tw = measure_text(tok, font_size, 0);
            if cur.is_empty() || cur_w + tw <= max_w {
                cur.push_str(tok);
                cur_w += tw;
            } else {
                out.push(cur.trim_end().to_string());
                let trimmed = tok.trim_start();
                cur_w = measure_text(trimmed, font_size, 0);
                cur.clear();
                cur.push_str(trimmed);
            }
        });
        if !cur.trim().is_empty() {
            out.push(cur.trim_end().to_string());
        }
    }
    if out.is_empty() {
        out.push(String::from("—"));
    }
    out
}

/// Default decoded grayscale cache budget (`4 MiB`).
pub const DEFAULT_GRAY_CACHE_BYTES: usize = 4 * 1024 * 1024;
/// Default dithered 4-shade `RGBA8` cache budget (`4 MiB`).
pub const DEFAULT_RGBA_CACHE_BYTES: usize = 4 * 1024 * 1024;

/// Two-tier byte-bounded LRU illustration cache (`4 MiB` decoded grayscale + `4 MiB` 4-shade `RGBA8`,
/// `<= 8 MiB` combined ceiling):
/// - Decoded `DecodedGrayImage` buffers persist across font-size, mode, `Hide UI`, and theme changes
///   so JPEG/PNG/GIF/BMP bytes are **never** re-decoded after initial page entry or background prefetch.
pub struct DitheredImageCache {
    gray_entries: Vec<(u64, DecodedGrayImage)>,
    gray_bytes: usize,
    max_gray_bytes: usize,
    entries: Vec<CachedDitherEntry>,
    rgba_bytes: usize,
    max_rgba_bytes: usize,
    max_entries: usize,
}

pub struct CachedDitherEntry {
    pub tex_key: u64,
    pub id_hash: u64,
    pub image_idx: usize,
    pub width: u32,
    pub height: u32,
    pub theme_sig: u64,
    pub rgba: Vec<u8>,
}

impl DitheredImageCache {
    pub fn new(max_entries: usize) -> Self {
        Self::with_budgets(
            max_entries,
            DEFAULT_GRAY_CACHE_BYTES,
            DEFAULT_RGBA_CACHE_BYTES,
        )
    }

    pub fn with_budgets(
        max_entries: usize,
        max_gray_bytes: usize,
        max_rgba_bytes: usize,
    ) -> Self {
        Self {
            gray_entries: Vec::with_capacity(16),
            gray_bytes: 0,
            max_gray_bytes: max_gray_bytes.max(512 * 1024),
            entries: Vec::with_capacity(max_entries.max(4)),
            rgba_bytes: 0,
            max_rgba_bytes: max_rgba_bytes.max(512 * 1024),
            max_entries: max_entries.max(4),
        }
    }

    /// Inserts a pre-decoded grayscale illustration (e.g. from the background prefetch worker)
    /// into the bounded `4 MiB` grayscale LRU cache.
    pub fn insert_decoded_gray(&mut self, id_hash: u64, gray: DecodedGrayImage) {
        if let Some(pos) = self.gray_entries.iter().position(|(h, _)| *h == id_hash) {
            let (_, old) = self.gray_entries.remove(pos);
            self.gray_bytes = self.gray_bytes.saturating_sub(old.byte_len());
        }
        let sz = gray.byte_len();
        while !self.gray_entries.is_empty() && self.gray_bytes + sz > self.max_gray_bytes {
            let (_, ev) = self.gray_entries.remove(0);
            self.gray_bytes = self.gray_bytes.saturating_sub(ev.byte_len());
        }
        self.gray_bytes += sz;
        self.gray_entries.push((id_hash, gray));
    }

    pub fn has_decoded_gray(&self, id_hash: u64) -> bool {
        self.gray_entries.iter().any(|(h, _)| *h == id_hash)
    }

    pub fn gray_cache_bytes(&self) -> usize {
        self.gray_bytes
    }

    pub fn rgba_cache_bytes(&self) -> usize {
        self.rgba_bytes
    }

    pub fn get_or_rasterize(
        &mut self,
        doc: &EbookDoc,
        image_idx: usize,
        disp_w: u32,
        disp_h: u32,
        bg_rgb: u32,
        fg_rgb: u32,
        fg1_rgb: u32,
        fg2_rgb: u32,
    ) -> Option<&CachedDitherEntry> {
        let img = doc.images.get(image_idx)?;
        let id_hash = img.id_hash;
        let raw_slice = img.raw_bytes.as_slice();
        self.get_or_rasterize_lazy(
            id_hash,
            image_idx,
            disp_w,
            disp_h,
            bg_rgb,
            fg_rgb,
            fg1_rgb,
            fg2_rgb,
            || Some(std::borrow::Cow::Borrowed(raw_slice)),
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub fn get_or_rasterize_lazy<'b, F>(
        &mut self,
        id_hash: u64,
        image_idx: usize,
        disp_w: u32,
        disp_h: u32,
        bg_rgb: u32,
        fg_rgb: u32,
        fg1_rgb: u32,
        fg2_rgb: u32,
        load_raw: F,
    ) -> Option<&CachedDitherEntry>
    where
        F: FnOnce() -> Option<std::borrow::Cow<'b, [u8]>>,
    {
        let theme_sig = ((bg_rgb as u64) << 40)
            ^ ((fg_rgb as u64) << 24)
            ^ ((fg1_rgb as u64) << 12)
            ^ (fg2_rgb as u64);

        if let Some(pos) = self.entries.iter().position(|e| {
            e.id_hash == id_hash
                && e.width == disp_w
                && e.height == disp_h
                && e.theme_sig == theme_sig
        }) {
            let item = self.entries.remove(pos);
            self.entries.push(item);
            return self.entries.last();
        }

        // Check Tier-1 decoded grayscale cache (`zero` JPEG/PNG re-decode on resize/mode/theme switch!)
        let gray_pos = if let Some(p) = self.gray_entries.iter().position(|(h, _)| *h == id_hash) {
            let pair = self.gray_entries.remove(p);
            self.gray_entries.push(pair);
            self.gray_entries.len() - 1
        } else {
            let raw = load_raw()?;
            let decoded = decode_illustration_gray_capped(&raw, bg_rgb)?;
            self.insert_decoded_gray(id_hash, decoded);
            self.gray_entries.len().checked_sub(1)?
        };

        let rgba = rasterize_gray_4shade_rgba(
            &self.gray_entries[gray_pos].1,
            disp_w,
            disp_h,
            bg_rgb,
            fg_rgb,
            fg1_rgb,
            fg2_rgb,
        );
        let tex_key = id_hash.wrapping_mul(0x9E37_79B9_7F4A_7C15)
            ^ theme_sig
            ^ ((disp_w as u64) << 16)
            ^ (disp_h as u64);

        let entry_sz = rgba.len() + 48;
        while !self.entries.is_empty()
            && (self.entries.len() >= self.max_entries
                || self.rgba_bytes + entry_sz > self.max_rgba_bytes)
        {
            let old = self.entries.remove(0);
            self.rgba_bytes = self.rgba_bytes.saturating_sub(old.rgba.len() + 48);
        }
        self.rgba_bytes += entry_sz;
        self.entries.push(CachedDitherEntry {
            tex_key,
            id_hash,
            image_idx,
            width: disp_w,
            height: disp_h,
            theme_sig,
            rgba,
        });
        self.entries.last()
    }
}
