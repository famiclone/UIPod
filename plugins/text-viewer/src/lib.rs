//! Full-featured 4-Shade E-Book & Document Reader plugin for UIPod (`v0.3.0`).
//!
//! Supported formats: `.epub`, `.fb2`, `.fb2.zip`, DRM-free `.mobi`/`.prc`/`.azw`,
//! `.md`/`.markdown`, `.txt`, `.lrc`, `.log`, `.cfg`, `.ini`, `.json`, `.cue`, `.nfo`.
//!
//! Features:
//! - Inline 4-shade Bayer-dithered book covers & chapter illustrations
//! - Interactive floating Footnote cards (`A` to open/cycle, `Up/Down` to scroll, `B` to close)
//! - Navigable hierarchical Table of Contents (`MENU -> Chapters`, auto-highlighting current chapter)
//! - Dual Reading Modes (`Paginated Left↔Right` vs `Infinite Scroll Up↕Down`) toggled via solo `SELECT`
//! - `Hide UI` distraction-free reading (`3.0s` chrome reveal on upward/backward navigation)
//! - Full-width horizontal bottom progress bar (`Percent` or `Pages read / unread`)
//! - In-reader background music controls (`START` Play/Pause, `L2`/`R2` Prev/Next track with HUD toast)

use std::fs;
use std::path::Path;

use uipod_ebook::{
    error_document, fnv1a_book_key, parse_document, wrap_footnote_lines, DitheredImageCache,
    EbookDoc, InlineStyle, ProgressDisplay, ReaderFont, ReaderPrefs, ReaderViewState, ReadingMode,
    VisualItemKind, READER_FONT_SIZES, READER_LINE_SPACINGS, READER_SPEC,
};
use uipod_plugin_sdk::{
    btn, uipod_declare_plugin, FontWeight, HostContext, MenuItem, MenuOutcome, PluginCategory,
    PluginKind, PluginTheme, TextAlign,
};

const MAX_BOOK_BYTES: u64 = 64 * 1024 * 1024; // 64 MiB safety cap

fn default_guide_markdown(uk: bool) -> &'static str {
    if uk {
        "# Читалка електронних книг UIPod\n\
         \n\
         Відкривайте файли **.epub**, **.fb2**, **.fb2.zip**, **.mobi** (без DRM), а також **.md**, **.txt**, **.lrc** та **.log** прямо з меню «Огляд» (Browse), натиснувши **A**[^fn1].\n\
         \n\
         ## Керування під час читання\n\
         - **Вліво / Вправо** — попередня / наступна сторінка (або стрибок на екран у режимі прокрутки)\n\
         - **Вгору / Вниз** — плавна прокрутка тексту або вибір виноски\n\
         - **SELECT** — миттєве перемикання **Сторінки (↔)** / **Прокрутка (↕)** зі збереженням абзацу\n\
         - **A** — відкрити виноску на поточній сторінці (**B** — закрити картку виноски)\n\
         - **MENU / Утримання A** — опції читалки (**Розділи / Зміст**, Режим, **Шрифт (Geologica / Noto Serif)**, Розмір, Ховати UI, Індикатор прогресу)\n\
         - **START** — Play / Pause музики у фоні\n\
         - **L2 / R2** — попередній / наступний музичний трек\n\
         - **B** — зберегти прогрес і вийти (утримання **B** — на Головну)\n\
         \n\
         [^fn1]: Демонстраційна виноска UIPod! У книгах .epub, .fb2 та .mobi виноски відкриваються у такій спливаючій картці без втрати місця читання."
    } else {
        "# UIPod E-Book & Document Reader\n\
         \n\
         Open **.epub**, **.fb2**, **.fb2.zip**, DRM-free **.mobi**, **.md**, **.txt**, **.lrc**, and **.log** files directly from **Main -> Browse** by pressing **A**[^fn1].\n\
         \n\
         ## Reader Controls\n\
         - **D-Pad Left / Right** — Previous / Next page (or screen jump in Scroll mode)\n\
         - **D-Pad Up / Down** — Smooth vertical scroll or footnote focus\n\
         - **SELECT** — Toggle **Paginated (↔)** vs **Infinite Scroll (↕)** at the exact same spot\n\
         - **A** — Open visible **Footnote** popup card (**B** closes the card)\n\
         - **MENU / Hold A** — **Reader Options** (**Chapters / TOC**, Reading Mode, **Font (Geologica / Noto Serif)**, Font Size, Hide UI, Progress Label)\n\
         - **START** — Toggle background music Play / Pause\n\
         - **L2 / R2** — Previous / Next background music track\n\
         - **B** — Save reading progress & exit (**Hold B** -> Main Menu)\n\
         \n\
         [^fn1]: Sample UIPod interactive footnote! Footnotes in .epub, .fb2, and .mobi books open right here in a scrollable card without losing your reading spot."
    }
}

fn load_prefs(ctx: &HostContext) -> ReaderPrefs {
    if let Some(raw) = ctx
        .load_plugin_data("text_viewer", "settings.txt")
        .and_then(|b| String::from_utf8(b).ok())
    {
        return ReaderPrefs::deserialize(&raw);
    }
    ReaderPrefs::default()
}

fn save_prefs(ctx: &HostContext, prefs: &ReaderPrefs) {
    let s = prefs.serialize();
    let _ = ctx.save_plugin_data("text_viewer", "settings.txt", s.as_bytes());
}

fn load_book_offset(ctx: &HostContext, bmark_key: &Option<String>) -> usize {
    bmark_key
        .as_ref()
        .and_then(|k| ctx.load_plugin_data("text_viewer", k))
        .and_then(|b| String::from_utf8(b).ok())
        .and_then(|s| s.trim().parse::<usize>().ok())
        .unwrap_or(0)
}

fn save_book_offset(ctx: &HostContext, bmark_key: &Option<String>, offset: usize) {
    if let Some(ref k) = bmark_key {
        let _ = ctx.save_plugin_data("text_viewer", k, offset.to_string().as_bytes());
    }
}

fn show_track_skip_toast(ctx: &HostContext, forward: bool) {
    if let Some(info) = ctx.audio_track_info() {
        let arrow = if forward { "»" } else { "«" };
        let label = if !info.artist.is_empty() && !info.title.is_empty() {
            format!("{arrow} {} — {}", info.artist, info.title)
        } else if !info.title.is_empty() {
            format!("{arrow} {}", info.title)
        } else {
            return;
        };
        ctx.show_splash(&label, 1500);
    }
}

enum OptionsOutcome {
    BackToReading,
    JumpedToChapter,
    ExitReader,
}

/// Opens the hierarchical `Chapters` (`Table of Contents`) screen.
fn open_chapters_menu(
    ctx: &HostContext,
    doc: &EbookDoc,
    view: &mut ReaderViewState,
    prefs: &ReaderPrefs,
    bmark_key: &Option<String>,
    now_ms: u64,
) -> OptionsOutcome {
    if doc.toc.is_empty() {
        ctx.show_splash(ctx.tr("No chapters", "Немає розділів"), 1200);
        return OptionsOutcome::BackToReading;
    }

    let active_idx = view.active_toc_index(doc);
    let total_chars = doc.total_chars.max(1);
    let mut labels: Vec<String> = Vec::with_capacity(doc.toc.len());
    let mut values: Vec<String> = Vec::with_capacity(doc.toc.len());

    for (i, ch) in doc.toc.iter().enumerate() {
        let indent = match ch.level {
            0 => "",
            1 => "· ",
            2 => "  · ",
            _ => "    · ",
        };
        let active_prefix = if i == active_idx { "▸ " } else { "" };
        labels.push(format!("{active_prefix}{indent}{}", ch.title));
        let pct = ((ch.offset * 100) / total_chars).min(100);
        values.push(format!("{pct}%"));
    }

    let items: Vec<MenuItem<'_>> = labels
        .iter()
        .zip(values.iter())
        .map(|(l, v)| MenuItem::option(l.as_str(), v.as_str()))
        .collect();

    match ctx.show_menu(ctx.tr("Chapters", "Розділи"), &items, active_idx) {
        MenuOutcome::Selected(idx) => {
            if let Some(target) = doc.toc.get(idx) {
                view.open_footnote_idx = None;
                view.sync_to_anchor(target.offset);
                if prefs.hide_ui {
                    view.note_backward_motion(prefs, now_ms);
                }
                save_book_offset(ctx, bmark_key, view.anchor_offset);
            }
            OptionsOutcome::JumpedToChapter
        }
        MenuOutcome::Cancelled => OptionsOutcome::BackToReading,
        MenuOutcome::ForceExit => OptionsOutcome::ExitReader,
    }
}

/// Opens `Reader Options` (`MENU`), with `Chapters` at the top row.
fn open_reader_options_menu(
    ctx: &HostContext,
    doc: &EbookDoc,
    view: &mut ReaderViewState,
    prefs: &mut ReaderPrefs,
    bmark_key: &Option<String>,
) -> bool {
    let mut sel = 0usize;
    loop {
        let chap_val = if doc.toc.is_empty() {
            ctx.tr("No chapters", "Немає розділів").to_string()
        } else {
            format!("{}", doc.toc.len())
        };
        let mode_lbl = match prefs.mode {
            ReadingMode::Paginated => ctx.tr("Pages (↔)", "Сторінки (↔)"),
            ReadingMode::Scroll => ctx.tr("Scroll (↕)", "Прокрутка (↕)"),
        };
        let font_lbl = match prefs.font {
            ReaderFont::Sans => ctx.tr("Geologica (Sans)", "Geologica (Без засічок)"),
            ReaderFont::Serif => ctx.tr("Noto Serif", "Noto Serif (Із засічками)"),
        };
        let sz_lbl = match prefs.size_idx {
            0 => ctx.tr("Small", "Малий"),
            1 => ctx.tr("Medium", "Середній"),
            2 => ctx.tr("Large", "Великий"),
            _ => ctx.tr("Extra Large", "Дуже великий"),
        };
        let sp_lbl = match prefs.spacing_idx {
            0 => ctx.tr("Compact", "Щільний"),
            1 => ctx.tr("Normal", "Звичайний"),
            _ => ctx.tr("Relaxed", "Широкий"),
        };
        let hide_lbl = if prefs.hide_ui {
            ctx.tr("On", "Увімк")
        } else {
            ctx.tr("Off", "Вимк")
        };
        let prog_lbl = match prefs.progress_display {
            ProgressDisplay::Percent => ctx.tr("Percent", "Відсотки"),
            ProgressDisplay::Pages => ctx.tr("Pages", "Сторінки"),
        };

        let chap_item = if doc.toc.is_empty() {
            MenuItem::option(ctx.tr("Chapters", "Розділи"), &chap_val)
        } else {
            MenuItem {
                label: ctx.tr("Chapters", "Розділи"),
                value: Some(&chap_val),
                has_chevron: true,
            }
        };

        let items = [
            chap_item,
            MenuItem::option(ctx.tr("Reading Mode", "Режим читання"), mode_lbl),
            MenuItem::option(ctx.tr("Font", "Шрифт"), font_lbl),
            MenuItem::option(ctx.tr("Font Size", "Розмір шрифту"), sz_lbl),
            MenuItem::option(ctx.tr("Line Spacing", "Міжрядковий інтервал"), sp_lbl),
            MenuItem::option(ctx.tr("Hide UI", "Ховати UI"), hide_lbl),
            MenuItem::option(ctx.tr("Progress Label", "Індикатор прогресу"), prog_lbl),
            MenuItem::action(ctx.tr("Go to Beginning", "На початок")),
            MenuItem::action(ctx.tr("Go to End", "У кінець")),
            MenuItem::action(ctx.tr("Exit", "Вихід")),
        ];

        match ctx.show_menu(
            ctx.tr("Reader Options", "Опції читалки"),
            &items,
            sel,
        ) {
            MenuOutcome::Cancelled => return false,
            MenuOutcome::ForceExit => return true,
            MenuOutcome::Selected(0) => {
                sel = 0;
                if doc.toc.is_empty() {
                    ctx.show_splash(ctx.tr("No chapters", "Немає розділів"), 1100);
                } else {
                    match open_chapters_menu(ctx, doc, view, prefs, bmark_key, ctx.ticks_ms()) {
                        OptionsOutcome::JumpedToChapter => return false,
                        OptionsOutcome::BackToReading => {}
                        OptionsOutcome::ExitReader => return true,
                    }
                }
            }
            MenuOutcome::Selected(1) => {
                sel = 1;
                view.refresh_anchor_from_mode(prefs.mode);
                let spot = view.anchor_offset;
                prefs.mode = prefs.mode.toggle();
                save_prefs(ctx, prefs);
                view.sync_to_anchor(spot);
                save_book_offset(ctx, bmark_key, view.anchor_offset);
            }
            MenuOutcome::Selected(2) => {
                sel = 2;
                view.refresh_anchor_from_mode(prefs.mode);
                prefs.font = prefs.font.toggle();
                save_prefs(ctx, prefs);
                view.invalidate_layout();
            }
            MenuOutcome::Selected(3) => {
                sel = 3;
                view.refresh_anchor_from_mode(prefs.mode);
                prefs.size_idx = (prefs.size_idx + 1) % READER_FONT_SIZES.len();
                save_prefs(ctx, prefs);
                view.invalidate_layout();
            }
            MenuOutcome::Selected(4) => {
                sel = 4;
                view.refresh_anchor_from_mode(prefs.mode);
                prefs.spacing_idx = (prefs.spacing_idx + 1) % READER_LINE_SPACINGS.len();
                save_prefs(ctx, prefs);
                view.invalidate_layout();
            }
            MenuOutcome::Selected(5) => {
                sel = 5;
                view.refresh_anchor_from_mode(prefs.mode);
                prefs.hide_ui = !prefs.hide_ui;
                view.ui_hidden = prefs.hide_ui;
                view.ui_reveal_until_ms = 0;
                save_prefs(ctx, prefs);
                view.invalidate_layout();
            }
            MenuOutcome::Selected(6) => {
                sel = 6;
                prefs.progress_display = prefs.progress_display.toggle();
                save_prefs(ctx, prefs);
            }
            MenuOutcome::Selected(7) => {
                view.open_footnote_idx = None;
                view.sync_to_anchor(0);
                save_book_offset(ctx, bmark_key, 0);
                return false;
            }
            MenuOutcome::Selected(8) => {
                view.open_footnote_idx = None;
                view.sync_to_anchor(doc.total_chars);
                save_book_offset(ctx, bmark_key, view.anchor_offset);
                return false;
            }
            MenuOutcome::Selected(_) => return true,
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn draw_visual_item(
    ctx: &HostContext,
    doc: &EbookDoc,
    img_cache: &mut DitheredImageCache,
    t: PluginTheme,
    col_x: f32,
    col_w: f32,
    top_y: f32,
    item_h: f32,
    kind: &VisualItemKind,
    focused_fn: Option<usize>,
) {
    match kind {
        VisualItemKind::Spacer | VisualItemKind::PageBreak => {}
        VisualItemKind::TextLine {
            segments,
            is_quote,
            is_code,
            ..
        } => {
            if *is_code {
                ctx.fill_rect(
                    col_x - READER_SPEC.code_pad_x,
                    top_y + 1.0,
                    col_w + READER_SPEC.code_pad_x * 2.0,
                    (item_h - 2.0).max(4.0),
                    4.0,
                    t.fg1,
                );
            }
            if *is_quote {
                ctx.fill_rect(
                    col_x + 4.0,
                    top_y,
                    READER_SPEC.quote_bar_w,
                    item_h,
                    2.0,
                    t.fg2,
                );
            }

            let cy = top_y + item_h * 0.5;
            for seg in segments {
                let sx = col_x + seg.x_rel;
                if let Some(fn_idx) = seg.footnote_idx {
                    let is_focused = focused_fn == Some(fn_idx);
                    let pill_h = (seg.font_size + 8.0).min(item_h - 2.0);
                    let pill_y = cy - pill_h * 0.5;
                    let pill_w = (seg.width - 2.0).max(16.0);
                    if is_focused {
                        ctx.fill_rect(
                            sx + 1.0,
                            pill_y,
                            pill_w,
                            pill_h,
                            READER_SPEC.footnote_pill_radius,
                            t.fg,
                        );
                        ctx.draw_text(
                            sx + 1.0 + pill_w * 0.5,
                            cy,
                            &seg.text,
                            seg.font_size,
                            FontWeight::Bold,
                            TextAlign::Center,
                            t.bg,
                        );
                    } else {
                        ctx.fill_rect(
                            sx + 1.0,
                            pill_y,
                            pill_w,
                            pill_h,
                            READER_SPEC.footnote_pill_radius,
                            t.fg1,
                        );
                        ctx.stroke_rect(
                            sx + 1.0,
                            pill_y,
                            pill_w,
                            pill_h,
                            READER_SPEC.footnote_pill_radius,
                            2.0,
                            t.fg2,
                        );
                        ctx.draw_text(
                            sx + 1.0 + pill_w * 0.5,
                            cy,
                            &seg.text,
                            seg.font_size,
                            FontWeight::Bold,
                            TextAlign::Center,
                            t.fg,
                        );
                    }
                } else {
                    if seg.style == InlineStyle::Code && !*is_code {
                        let bh = (seg.font_size + 6.0).min(item_h - 2.0);
                        ctx.fill_rect(sx - 2.0, cy - bh * 0.5, seg.width + 4.0, bh, 4.0, t.fg1);
                    }
                    let color = match seg.style {
                        InlineStyle::Italic if !*is_code => t.fg2,
                        _ if *is_quote => t.fg2,
                        _ => t.fg,
                    };
                    ctx.draw_text_styled(
                        sx,
                        cy,
                        &seg.text,
                        seg.font_size,
                        seg.weight,
                        TextAlign::Left,
                        color,
                    );
                }
            }
        }
        VisualItemKind::Illustration {
            image_idx,
            disp_w,
            disp_h,
            caption,
        } => {
            let iw = *disp_w as f32;
            let ih = *disp_h as f32;
            let ix = (col_x + (col_w - iw) * 0.5).round();
            let iy = (top_y + READER_SPEC.image_v_pad).round();

            if let Some(entry) = img_cache.get_or_rasterize(
                doc,
                *image_idx,
                *disp_w,
                *disp_h,
                t.bg,
                t.fg,
                t.fg1,
                t.fg2,
            ) {
                ctx.draw_rgba_image(
                    entry.tex_key,
                    entry.width,
                    entry.height,
                    &entry.rgba,
                    ix,
                    iy,
                    iw,
                    ih,
                    READER_SPEC.image_corner_radius,
                );
                ctx.stroke_rect(
                    ix,
                    iy,
                    iw,
                    ih,
                    READER_SPEC.image_corner_radius,
                    READER_SPEC.image_border_stroke,
                    t.fg1,
                );
            }
            if !caption.is_empty() {
                ctx.draw_text(
                    col_x + col_w * 0.5,
                    iy + ih + 16.0,
                    caption,
                    20.0,
                    FontWeight::Medium,
                    TextAlign::Center,
                    t.fg2,
                );
            }
        }
    }
}

/// Renders the full-width bottom progress bar (replacing the old vertical right-side scrollbar).
fn draw_bottom_progress_strip(
    ctx: &HostContext,
    t: PluginTheme,
    sw: f32,
    sh: f32,
    progress: f32,
    readout: &str,
) {
    let strip_y = sh - READER_SPEC.bottom_strip_h;
    ctx.fill_rect(0.0, strip_y, sw, READER_SPEC.bottom_strip_h, 0.0, t.bg);

    let cy = strip_y + READER_SPEC.bottom_strip_h * 0.5 + 1.0;
    let mx = READER_SPEC.screen_margin_x;
    let lbl_w = ctx.measure_text(
        readout,
        READER_SPEC.progress_label_font,
        FontWeight::SemiBold,
    );
    ctx.draw_text(
        mx,
        cy,
        readout,
        READER_SPEC.progress_label_font,
        FontWeight::SemiBold,
        TextAlign::Left,
        t.fg2,
    );

    let bar_x = (mx + lbl_w + READER_SPEC.progress_label_gap).round();
    let bar_w = (sw - mx - bar_x).max(64.0);
    let bar_h = READER_SPEC.progress_bar_h;
    let bar_y = (cy - bar_h * 0.5).round();

    // Exact horizontal twin of UIPod's `Scrollbar` (`[312]`): `r=8`, `stroke=4 inside` (`fg1`), `4px` inset progressive fill (`fg`)
    ctx.stroke_rect(
        bar_x,
        bar_y,
        bar_w,
        bar_h,
        READER_SPEC.progress_bar_radius,
        READER_SPEC.progress_bar_stroke,
        t.fg1,
    );
    let inset = READER_SPEC.progress_bar_inset;
    let inner_max_w = (bar_w - inset * 2.0).max(0.0);
    let inner_h = (bar_h - inset * 2.0).max(4.0);
    let fill_w = (inner_max_w * progress.clamp(0.0, 1.0)).round();
    if fill_w >= 2.0 {
        ctx.fill_rect(
            bar_x + inset,
            bar_y + inset,
            fill_w,
            inner_h,
            inner_h * 0.5,
            t.fg,
        );
    }
}

/// Renders the floating interactive Footnote popup card (`Up/Down` scrollable, `B` closes).
#[allow(clippy::too_many_arguments)]
fn draw_footnote_popup_card(
    ctx: &HostContext,
    doc: &EbookDoc,
    view: &mut ReaderViewState,
    font: ReaderFont,
    t: PluginTheme,
    sw: f32,
    sh: f32,
    fn_idx: usize,
    total_vis_fns: usize,
) {
    let Some(note) = doc.footnotes.get(fn_idx) else {
        return;
    };

    let card_w = (sw - READER_SPEC.fn_card_margin_x * 2.0).min(READER_SPEC.fn_card_max_w);
    let inner_w = (card_w - READER_SPEC.fn_card_pad_x * 2.0).max(160.0);
    let body_fsz = READER_SPEC.fn_body_font;
    let line_h = (body_fsz * 1.36).round();
    let body_style = font.style_flag();

    let wrapped = wrap_footnote_lines(&note.paragraphs, body_fsz, inner_w, |txt, sz, w| {
        ctx.measure_text_styled(txt, sz, w | body_style)
    });

    let max_card_h = (sh * READER_SPEC.fn_card_max_h_ratio).clamp(220.0, 520.0);
    let header_band_h = 50.0;
    let footer_band_h = 40.0;
    let max_body_lines =
        (((max_card_h - header_band_h - footer_band_h) / line_h).floor() as usize).max(2);
    let vis_lines = wrapped.len().min(max_body_lines);
    let max_scroll = wrapped.len().saturating_sub(vis_lines);
    if view.footnote_scroll_line > max_scroll {
        view.footnote_scroll_line = max_scroll;
    }

    let card_h = header_band_h + (vis_lines as f32) * line_h + footer_band_h;
    let card_x = ((sw - card_w) * 0.5).round();
    let card_y = ((sh - card_h) * 0.5).round();

    // Subtle 4-shade drop shadow + crisp card frame
    ctx.fill_rect(
        card_x + 6.0,
        card_y + 6.0,
        card_w,
        card_h,
        READER_SPEC.fn_card_radius,
        t.fg1,
    );
    ctx.fill_rect(
        card_x,
        card_y,
        card_w,
        card_h,
        READER_SPEC.fn_card_radius,
        t.bg,
    );
    ctx.stroke_rect(
        card_x,
        card_y,
        card_w,
        card_h,
        READER_SPEC.fn_card_radius,
        READER_SPEC.fn_card_stroke,
        t.fg,
    );

    let title_str = format!("{} {}", ctx.tr("Footnote", "Виноска"), note.title);
    ctx.draw_text(
        card_x + READER_SPEC.fn_card_pad_x,
        card_y + 26.0,
        &title_str,
        READER_SPEC.fn_title_font,
        FontWeight::Bold,
        TextAlign::Left,
        t.fg,
    );
    if max_scroll > 0 {
        let sc_lbl = format!(
            "{}/{}",
            view.footnote_scroll_line + vis_lines,
            wrapped.len()
        );
        ctx.draw_text(
            card_x + card_w - READER_SPEC.fn_card_pad_x,
            card_y + 26.0,
            &sc_lbl,
            READER_SPEC.fn_hint_font,
            FontWeight::SemiBold,
            TextAlign::Right,
            t.fg2,
        );
    }
    ctx.draw_line(
        card_x + READER_SPEC.fn_card_pad_x,
        card_y + header_band_h - 4.0,
        card_x + card_w - READER_SPEC.fn_card_pad_x,
        card_y + header_band_h - 4.0,
        2.0,
        t.fg1,
    );

    let body_top = card_y + header_band_h + 2.0;
    for (i, line) in wrapped
        .iter()
        .skip(view.footnote_scroll_line)
        .take(vis_lines)
        .enumerate()
    {
        if line.is_empty() {
            continue;
        }
        ctx.draw_text_styled(
            card_x + READER_SPEC.fn_card_pad_x,
            body_top + (i as f32 + 0.5) * line_h,
            line,
            body_fsz,
            body_style,
            TextAlign::Left,
            t.fg,
        );
    }

    let hint = if total_vis_fns > 1 && max_scroll > 0 {
        ctx.tr(
            "Up/Down: Scroll  ·  A: Next Note  ·  B: Close",
            "Вгору/Вниз: Прокрутка  ·  A: Наст. виноска  ·  B: Закрити",
        )
    } else if total_vis_fns > 1 {
        ctx.tr("A: Next Note  ·  B: Close", "A: Наст. виноска  ·  B: Закрити")
    } else if max_scroll > 0 {
        ctx.tr(
            "Up/Down: Scroll  ·  B: Close",
            "Вгору/Вниз: Прокрутка  ·  B: Закрити",
        )
    } else {
        ctx.tr("B: Close", "B: Закрити")
    };
    ctx.draw_text(
        card_x + card_w * 0.5,
        card_y + card_h - 20.0,
        hint,
        READER_SPEC.fn_hint_font,
        FontWeight::SemiBold,
        TextAlign::Center,
        t.fg2,
    );
}

fn run_text_viewer(ctx: &mut HostContext, file_path: Option<&Path>) -> i32 {
    let uk = ctx.is_ukrainian();
    let (doc, bmark_key) = match file_path {
        Some(p) => {
            let raw_name = p.file_name().and_then(|s| s.to_str()).unwrap_or("Document");
            let is_ephemeral_info = raw_name.eq_ignore_ascii_case("uipod_info.txt");
            let meta_len = fs::metadata(p).map(|m| m.len()).unwrap_or(0);
            if meta_len > MAX_BOOK_BYTES {
                ctx.show_splash(
                    ctx.tr("Book exceeds 64 MB limit", "Файл перевищує ліміт 64 МБ"),
                    1600,
                );
                return -1;
            }
            let mut parsed = {
                let bytes = match fs::read(p) {
                    Ok(b) => b,
                    Err(_) => {
                        ctx.show_splash(ctx.tr("Cannot read file", "Помилка читання файлу"), 1400);
                        return -1;
                    }
                };
                match parse_document(raw_name, &bytes) {
                    Ok(d) => d,
                    Err(err) => {
                        ctx.show_splash(&err.localized_message(uk), 2000);
                        error_document(raw_name, &err, uk)
                    }
                }
            };
            if raw_name.eq_ignore_ascii_case("about.txt") {
                parsed.title = ctx.tr("About UIPod", "Про UIPod").to_string();
            } else if is_ephemeral_info {
                parsed.title = ctx.tr("Info", "Інформація").to_string();
            }
            let key = if is_ephemeral_info {
                None
            } else {
                Some(fnv1a_book_key(&p.to_string_lossy()))
            };
            (parsed, key)
        }
        None => {
            let md = default_guide_markdown(uk);
            let mut parsed = parse_document("reader_guide.md", md.as_bytes())
                .unwrap_or_else(|e| error_document("reader_guide.md", &e, uk));
            parsed.title = ctx.tr("E-Book Reader", "Читалка книг").to_string();
            (parsed, None)
        }
    };

    let mut prefs = load_prefs(ctx);
    let initial_offset = load_book_offset(ctx, &bmark_key);
    let mut view = ReaderViewState::new(initial_offset);
    view.ui_hidden = prefs.hide_ui;

    let mut img_cache = DitheredImageCache::new(16);
    let mut scroll_vel = 0.0f32;
    let mut a_press_ms: Option<u64> = None;
    let mut a_hold_fired = false;
    let mut last_frame_ms = ctx.ticks_ms();

    loop {
        let now_ms = ctx.ticks_ms();
        let dt_sec = ((now_ms.saturating_sub(last_frame_ms)) as f32 / 1000.0).clamp(0.001, 0.12);
        last_frame_ms = now_ms;

        view.tick_auto_hide(&prefs, now_ms);
        let chrome_vis = view.is_chrome_visible(&prefs, now_ms);
        let t = ctx.theme();

        let (sw, sh) = ctx.begin_responsive_frame(t.bg, chrome_vis, &doc.title);
        view.ensure_layout(&doc, &prefs, sw, sh, |txt, sz, wt| {
            ctx.measure_text_styled(txt, sz, wt)
        });

        let vis_fns = view.visible_footnotes(prefs.mode);
        if !vis_fns.is_empty() && view.focused_fn_slot >= vis_fns.len() {
            view.focused_fn_slot = 0;
        }
        let focused_fn = vis_fns.get(view.focused_fn_slot).copied();

        match prefs.mode {
            ReadingMode::Paginated => {
                let page_top_y = view.viewport_top_y;
                if let Some(pg) = view.pages.get(view.current_page).copied() {
                    let mut y = page_top_y;
                    for it in &view.items[pg.start_item..pg.end_item] {
                        draw_visual_item(
                            ctx,
                            &doc,
                            &mut img_cache,
                            t,
                            view.col_x,
                            view.col_w,
                            y,
                            it.height,
                            &it.kind,
                            focused_fn,
                        );
                        y += it.height;
                    }
                }
            }
            ReadingMode::Scroll => {
                let clip_top = view.viewport_top_y - 4.0;
                let clip_bot = sh - READER_SPEC.bottom_strip_h;
                let base_screen_y = view.viewport_top_y;

                let start_idx = match view.items.binary_search_by(|it| {
                    (base_screen_y + it.y_scroll + it.height - view.scroll_px)
                        .partial_cmp(&clip_top)
                        .unwrap_or(core::cmp::Ordering::Equal)
                }) {
                    Ok(i) | Err(i) => i.saturating_sub(1),
                };

                for it in &view.items[start_idx..] {
                    let sy = base_screen_y + it.y_scroll - view.scroll_px;
                    if sy > clip_bot {
                        break;
                    }
                    if sy + it.height < clip_top {
                        continue;
                    }
                    // Avoid drawing partial top text over the shared Header band when `chrome_vis == true`
                    if chrome_vis && sy < READER_SPEC.header_content_top_y - 8.0 {
                        continue;
                    }
                    draw_visual_item(
                        ctx,
                        &doc,
                        &mut img_cache,
                        t,
                        view.col_x,
                        view.col_w,
                        sy,
                        it.height,
                        &it.kind,
                        focused_fn,
                    );
                }
            }
        }

        // Bottom full-width progress strip is ALWAYS permanently visible (even when `Hide UI = On`)
        let (prog, readout) = view.progress_metrics(&prefs);
        draw_bottom_progress_strip(ctx, t, sw, sh, prog, &readout);

        if let Some(open_fn) = view.open_footnote_idx {
            draw_footnote_popup_card(
                ctx,
                &doc,
                &mut view,
                prefs.font,
                t,
                sw,
                sh,
                open_fn,
                vis_fns.len(),
            );
        }

        ctx.present();

        // Poll at 16 ms when holding scroll keys or timing `Hold A` (650 ms -> Reader Options); else idle cleanly
        let need_fast_tick = scroll_vel.abs() > 1.0 || (a_press_ms.is_some() && !a_hold_fired);
        let inp = ctx.poll_input(if need_fast_tick { 16 } else { 180 });
        let poll_ms = ctx.ticks_ms();

        if inp.should_exit {
            view.refresh_anchor_from_mode(prefs.mode);
            save_book_offset(ctx, &bmark_key, view.anchor_offset);
            return 0;
        }

        // Background music controls (`START` Play/Pause, `L2`/`R2` Prev/Next track) work at all times!
        if inp.pressed(btn::START) {
            ctx.audio_toggle_play_pause();
        }
        if inp.pressed(btn::L2) {
            ctx.audio_prev();
            show_track_skip_toast(ctx, false);
        } else if inp.pressed(btn::R2) {
            ctx.audio_next();
            show_track_skip_toast(ctx, true);
        }

        // If Footnote popup card is currently open, handle `B` (close), `Up/Down` (scroll), `A` (next visible note)
        if view.open_footnote_idx.is_some() {
            if inp.pressed(btn::B) {
                view.open_footnote_idx = None;
                view.footnote_scroll_line = 0;
                continue;
            }
            if inp.pressed(btn::UP) {
                view.footnote_scroll_line = view.footnote_scroll_line.saturating_sub(1);
                continue;
            }
            if inp.pressed(btn::DOWN) {
                view.footnote_scroll_line = view.footnote_scroll_line.saturating_add(1);
                continue;
            }
            if inp.pressed(btn::A) {
                if vis_fns.len() > 1 {
                    view.focused_fn_slot = (view.focused_fn_slot + 1) % vis_fns.len();
                    view.open_footnote_idx = vis_fns.get(view.focused_fn_slot).copied();
                    view.footnote_scroll_line = 0;
                } else {
                    view.open_footnote_idx = None;
                    view.footnote_scroll_line = 0;
                }
                continue;
            }
        }

        if inp.pressed(btn::B) {
            view.refresh_anchor_from_mode(prefs.mode);
            save_book_offset(ctx, &bmark_key, view.anchor_offset);
            return 0;
        }

        // Solo `SELECT` (emitted strictly on solo release without `VOL ±`!) toggles Paginated <-> Infinite Scroll!
        if inp.pressed(btn::SELECT) {
            view.refresh_anchor_from_mode(prefs.mode);
            let spot = view.anchor_offset;
            prefs.mode = prefs.mode.toggle();
            save_prefs(ctx, &prefs);
            view.sync_to_anchor(spot);
            save_book_offset(ctx, &bmark_key, view.anchor_offset);
            scroll_vel = 0.0;
            let toast = match prefs.mode {
                ReadingMode::Paginated => {
                    ctx.tr("Reading Mode: Pages (↔)", "Режим: Сторінки (↔)")
                }
                ReadingMode::Scroll => {
                    ctx.tr("Reading Mode: Scroll (↕)", "Режим: Прокрутка (↕)")
                }
            };
            ctx.show_splash(toast, 1400);
            continue;
        }

        // Track `Hold A` (`>= 650 ms` -> opens `Reader Options` context menu) vs short `A` tap (opens Footnote card)
        if inp.pressed(btn::A) && a_press_ms.is_none() {
            a_press_ms = Some(poll_ms);
            a_hold_fired = false;
        }
        let hold_a_menu = if let Some(t0) = a_press_ms {
            if inp.held(btn::A) && !a_hold_fired && poll_ms.saturating_sub(t0) >= 650 {
                a_hold_fired = true;
                a_press_ms = None;
                true
            } else {
                false
            }
        } else {
            false
        };
        let short_a_tap = if !inp.held(btn::A) {
            let was_short = a_press_ms.take().is_some() && !a_hold_fired;
            a_hold_fired = false;
            was_short
        } else {
            false
        };

        // Both `MENU` and `Hold A` open `Reader Options` (`Chapters`, `Reading Mode`, `Font`, `Font Size`, `Hide UI`, `Progress Label`)
        if inp.menu_requested || inp.pressed(btn::MENU) || hold_a_menu {
            a_press_ms = None;
            a_hold_fired = false;
            view.open_footnote_idx = None;
            scroll_vel = 0.0;
            view.refresh_anchor_from_mode(prefs.mode);
            let should_quit = open_reader_options_menu(ctx, &doc, &mut view, &mut prefs, &bmark_key);
            if should_quit {
                view.refresh_anchor_from_mode(prefs.mode);
                save_book_offset(ctx, &bmark_key, view.anchor_offset);
                return 0;
            }
            last_frame_ms = ctx.ticks_ms();
            continue;
        }

        // Short `A` tap opens visible Footnote popup card on current screen
        if short_a_tap && !vis_fns.is_empty() {
            let target_fn = vis_fns[view.focused_fn_slot.min(vis_fns.len() - 1)];
            view.open_footnote_idx = Some(target_fn);
            view.footnote_scroll_line = 0;
            continue;
        }

        // `X` cycles font size quickly while preserving exact paragraph offset
        if inp.pressed(btn::X) {
            view.refresh_anchor_from_mode(prefs.mode);
            prefs.size_idx = (prefs.size_idx + 1) % READER_FONT_SIZES.len();
            save_prefs(ctx, &prefs);
            view.invalidate_layout();
            continue;
        }

        let click_t = ctx.ticks_ms();
        match prefs.mode {
            ReadingMode::Paginated => {
                if inp.pressed(btn::LEFT | btn::L) {
                    if view.step_page(-1, &prefs, click_t) {
                        save_book_offset(ctx, &bmark_key, view.anchor_offset);
                    }
                } else if inp.pressed(btn::RIGHT | btn::R) {
                    if view.step_page(1, &prefs, click_t) {
                        save_book_offset(ctx, &bmark_key, view.anchor_offset);
                    }
                } else if inp.pressed(btn::UP) {
                    if vis_fns.len() > 1 && view.focused_fn_slot > 0 {
                        view.focused_fn_slot -= 1;
                    } else if view.step_page(-1, &prefs, click_t) {
                        save_book_offset(ctx, &bmark_key, view.anchor_offset);
                    }
                } else if inp.pressed(btn::DOWN) {
                    if vis_fns.len() > 1 && view.focused_fn_slot + 1 < vis_fns.len() {
                        view.focused_fn_slot += 1;
                    } else if view.step_page(1, &prefs, click_t) {
                        save_book_offset(ctx, &bmark_key, view.anchor_offset);
                    }
                }
            }
            ReadingMode::Scroll => {
                let line_step =
                    (READER_FONT_SIZES[prefs.size_idx] * READER_LINE_SPACINGS[prefs.spacing_idx])
                        .round();
                if inp.pressed(btn::LEFT | btn::L) {
                    scroll_vel = 0.0;
                    if view.scroll_by_px(-(view.viewport_h - line_step).max(line_step), &prefs, click_t) {
                        save_book_offset(ctx, &bmark_key, view.anchor_offset);
                    }
                } else if inp.pressed(btn::RIGHT | btn::R) {
                    scroll_vel = 0.0;
                    if view.scroll_by_px((view.viewport_h - line_step).max(line_step), &prefs, click_t) {
                        save_book_offset(ctx, &bmark_key, view.anchor_offset);
                    }
                } else if inp.held(btn::UP) {
                    let target_v = -560.0;
                    scroll_vel = (scroll_vel + (target_v - scroll_vel) * (dt_sec * 9.0).min(1.0))
                        .min(-220.0);
                    let delta = if inp.pressed(btn::UP) && scroll_vel.abs() < 260.0 {
                        -line_step
                    } else {
                        scroll_vel * dt_sec
                    };
                    let _ = view.scroll_by_px(delta, &prefs, click_t);
                } else if inp.held(btn::DOWN) {
                    let target_v = 560.0;
                    scroll_vel = (scroll_vel + (target_v - scroll_vel) * (dt_sec * 9.0).min(1.0))
                        .max(220.0);
                    let delta = if inp.pressed(btn::DOWN) && scroll_vel.abs() < 260.0 {
                        line_step
                    } else {
                        scroll_vel * dt_sec
                    };
                    let _ = view.scroll_by_px(delta, &prefs, click_t);
                } else if inp.pressed(btn::UP) {
                    let _ = view.scroll_by_px(-line_step, &prefs, click_t);
                } else if inp.pressed(btn::DOWN) {
                    let _ = view.scroll_by_px(line_step, &prefs, click_t);
                } else if scroll_vel.abs() > 1.0 {
                    scroll_vel *= (1.0 - dt_sec * 14.0).max(0.0);
                    if scroll_vel.abs() < 12.0 {
                        scroll_vel = 0.0;
                        save_book_offset(ctx, &bmark_key, view.anchor_offset);
                    } else {
                        let _ = view.scroll_by_px(scroll_vel * dt_sec, &prefs, click_t);
                    }
                }
            }
        }
    }
}

uipod_declare_plugin! {
    id: "text_viewer",
    name: "Text Viewer",
    name_uk: "Переглядач тексту",
    author: "UIPod",
    version: "0.3.0",
    category: PluginCategory::Viewer,
    kind: PluginKind::Utility,
    extensions: "epub,fb2,fb2.zip,mobi,prc,azw,txt,lrc,md,markdown,log,cfg,ini,json,cue,nfo",
    entry: run_text_viewer,
}
