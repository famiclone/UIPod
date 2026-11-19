//! UTF-8 / Cyrillic / CJK word-wrapping `Text & Lyrics Viewer` plugin for UIPod (`v0.2.0`).
//!
//! Associated extensions: `.txt`, `.lrc`, `.md`, `.log`, `.cfg`, `.ini`, `.json`, `.cue`, `.nfo`.
//! Exits directly with `B` (saving reading position automatically), while `MENU` opens
//! navigation, bookmarks, and typography options (`Font Size`, `Line Spacing`).

use std::fs;
use std::path::Path;

use uipod_plugin_sdk::{
    btn, uipod_declare_plugin, FontWeight, HostContext, MenuItem, MenuOutcome, PluginCategory,
    PluginKind, TextAlign, CONTENT_TOP_Y, SCREEN_HEIGHT, SCREEN_WIDTH,
};

const FONT_SIZES: [f32; 3] = [28.0, 34.0, 42.0];
const LINE_SPACINGS: [f32; 3] = [1.24, 1.38, 1.56];
const MAX_READ_BYTES: usize = 512 * 1024; // 512 KiB cap for instant wrapping

#[derive(Clone)]
struct WrappedLine {
    text: String,
    heading_level: u8, // 0 = body, 1 = `# `, 2 = `## ` / `### `
}

fn fnv1a_hex(s: &str) -> String {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in s.bytes() {
        h ^= b as u64;
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    format!("bmark_{h:016x}.txt")
}

fn parse_markdown_heading(line: &str) -> (u8, &str) {
    if let Some(rest) = line.strip_prefix("# ") {
        (1, rest.trim())
    } else if let Some(rest) = line.strip_prefix("## ") {
        (2, rest.trim())
    } else if let Some(rest) = line.strip_prefix("### ") {
        (2, rest.trim())
    } else {
        (0, line)
    }
}

fn wrap_paragraphs(ctx: &HostContext, raw: &str, font_size: f32, max_w: f32) -> Vec<WrappedLine> {
    let mut lines = Vec::new();
    for raw_line in raw.lines() {
        let expanded = raw_line.trim_end_matches('\r').replace('\t', "    ");
        if expanded.trim().is_empty() {
            lines.push(WrappedLine {
                text: String::new(),
                heading_level: 0,
            });
            continue;
        }

        let (heading_level, body) = parse_markdown_heading(&expanded);
        let weight = match heading_level {
            1 => FontWeight::Bold,
            2 => FontWeight::SemiBold,
            _ => FontWeight::Regular,
        };
        let measure_sz = if heading_level == 1 {
            font_size + 4.0
        } else {
            font_size
        };

        if ctx.measure_text(body, measure_sz, weight) <= max_w {
            lines.push(WrappedLine {
                text: body.to_string(),
                heading_level,
            });
            continue;
        }

        let mut cur = String::new();
        for word in body.split_inclusive(' ') {
            let candidate = if cur.is_empty() {
                word.to_string()
            } else {
                let mut s = cur.clone();
                s.push_str(word);
                s
            };
            if ctx.measure_text(&candidate, measure_sz, weight) <= max_w {
                cur = candidate;
            } else {
                if !cur.is_empty() {
                    lines.push(WrappedLine {
                        text: cur.trim_end().to_string(),
                        heading_level,
                    });
                    cur.clear();
                }
                if ctx.measure_text(word, measure_sz, weight) > max_w {
                    let mut chunk = String::new();
                    for ch in word.chars() {
                        chunk.push(ch);
                        if ctx.measure_text(&chunk, measure_sz, weight) > max_w
                            && chunk.chars().count() > 1
                        {
                            let popped = chunk.pop().unwrap();
                            lines.push(WrappedLine {
                                text: chunk,
                                heading_level,
                            });
                            chunk = popped.to_string();
                        }
                    }
                    cur = chunk;
                } else {
                    cur = word.to_string();
                }
            }
        }
        if !cur.is_empty() {
            lines.push(WrappedLine {
                text: cur.trim_end().to_string(),
                heading_level,
            });
        }
    }
    if lines.is_empty() {
        lines.push(WrappedLine {
            text: String::new(),
            heading_level: 0,
        });
    }
    lines
}

fn default_guide_text(uk: bool) -> String {
    if uk {
        String::from(
            "# Переглядач тексту та лірики UIPod\n\
             \n\
             Відкривайте будь-який файл .txt, .lrc, .md, .log, .cfg, .ini, .json або .cue через меню «Огляд» (Browse) — натисніть A на файлі.\n\
             \n\
             ## Керування під час читання\n\
             • Вгору / Вниз — прокрутка по рядках\n\
             • Вліво / Вправо або L / R — сторінка назад / вперед\n\
             • X — швидка зміна розміру шрифту Geologica\n\
             • B — зберегти позицію та вийти (утримання B — на головну)\n\
             • Menu — меню читалки (На початок, У кінець, Закладка, Налаштування)\n\
             • Start — Play / Pause поточної музики\n\
             \n\
             ## Встановлення сторонніх плагінів\n\
             Скопіюйте скомпільовані файли .uipod у теку /mnt/SDCARD/Plugins/UIPod на вашій SD-картці.",
        )
    } else {
        String::from(
            "# UIPod Text & Lyrics Viewer\n\
             \n\
             Open any .txt, .lrc, .md, .log, .cfg, .ini, .json, or .cue file from Main -> Browse by pressing A on the file.\n\
             \n\
             ## Reader Controls\n\
             • Up / Down — line scroll\n\
             • Left / Right or L / R — page up / page down\n\
             • X — cycle Geologica font size\n\
             • B — save reading position & exit (hold B for Main Menu)\n\
             • Menu — reader menu (Jump, Bookmark, Options)\n\
             • Start — Play / Pause background music\n\
             \n\
             ## Installing Custom Plugins\n\
             Drop compiled .uipod plugin binaries into /mnt/SDCARD/Plugins/UIPod on your SD card.",
        )
    }
}

struct ReaderSettings {
    size_idx: usize,
    spacing_idx: usize,
}

impl ReaderSettings {
    fn load(ctx: &HostContext) -> Self {
        if let Some(raw) = ctx
            .load_plugin_data("text_viewer", "settings.txt")
            .and_then(|b| String::from_utf8(b).ok())
        {
            let mut it = raw.split_whitespace();
            let s = it
                .next()
                .and_then(|v| v.parse::<usize>().ok())
                .unwrap_or(1)
                .min(FONT_SIZES.len() - 1);
            let sp = it
                .next()
                .and_then(|v| v.parse::<usize>().ok())
                .unwrap_or(1)
                .min(LINE_SPACINGS.len() - 1);
            return Self {
                size_idx: s,
                spacing_idx: sp,
            };
        }
        let legacy_sz = ctx
            .load_plugin_data("text_viewer", "fontsize.txt")
            .and_then(|b| String::from_utf8(b).ok())
            .and_then(|s| s.trim().parse::<usize>().ok())
            .unwrap_or(1)
            .min(FONT_SIZES.len() - 1);
        Self {
            size_idx: legacy_sz,
            spacing_idx: 1,
        }
    }

    fn save(&self, ctx: &HostContext) {
        let s = format!("{} {}\n", self.size_idx, self.spacing_idx);
        let _ = ctx.save_plugin_data("text_viewer", "settings.txt", s.as_bytes());
    }
}

fn save_bookmark(ctx: &HostContext, bmark_key: &Option<String>, top_line: usize) {
    if let Some(ref k) = bmark_key {
        let _ = ctx.save_plugin_data("text_viewer", k, top_line.to_string().as_bytes());
    }
}

/// Opens `Options` submenu. Returns `(force_exit, rewrap_needed)`.
fn open_reader_options(ctx: &HostContext, cfg: &mut ReaderSettings) -> (bool, bool) {
    let mut sel = 0usize;
    let mut rewrap = false;
    loop {
        let sz_lbl = match cfg.size_idx {
            0 => ctx.tr("Small", "Малий"),
            1 => ctx.tr("Medium", "Середній"),
            _ => ctx.tr("Large", "Великий"),
        };
        let sp_lbl = match cfg.spacing_idx {
            0 => ctx.tr("Compact", "Щільний"),
            1 => ctx.tr("Normal", "Звичайний"),
            _ => ctx.tr("Relaxed", "Широкий"),
        };
        let items = [
            MenuItem::option(ctx.tr("Font Size", "Розмір шрифту"), sz_lbl),
            MenuItem::option(ctx.tr("Line Spacing", "Міжрядковий інтервал"), sp_lbl),
        ];
        match ctx.show_menu(ctx.tr("Options", "Налаштування"), &items, sel) {
            MenuOutcome::Selected(0) => {
                sel = 0;
                cfg.size_idx = (cfg.size_idx + 1) % FONT_SIZES.len();
                cfg.save(ctx);
                rewrap = true;
            }
            MenuOutcome::Selected(1) => {
                sel = 1;
                cfg.spacing_idx = (cfg.spacing_idx + 1) % LINE_SPACINGS.len();
                cfg.save(ctx);
            }
            MenuOutcome::Selected(_) | MenuOutcome::Cancelled => return (false, rewrap),
            MenuOutcome::ForceExit => return (true, rewrap),
        }
    }
}

fn run_text_viewer(ctx: &mut HostContext, file_path: Option<&Path>) -> i32 {
    let (doc_title, raw_text, bmark_key) = match file_path {
        Some(p) => {
            let raw_name = p.file_name().and_then(|s| s.to_str()).unwrap_or("Document");
            let is_ephemeral_info = raw_name.eq_ignore_ascii_case("uipod_info.txt");
            let title = if raw_name.eq_ignore_ascii_case("about.txt") {
                ctx.tr("About UIPod", "Про UIPod").to_string()
            } else if is_ephemeral_info {
                ctx.tr("Info", "Інформація").to_string()
            } else {
                raw_name.to_string()
            };
            let bytes = match fs::read(p) {
                Ok(mut b) => {
                    if b.len() > MAX_READ_BYTES {
                        b.truncate(MAX_READ_BYTES);
                    }
                    b
                }
                Err(_) => {
                    ctx.show_splash(ctx.tr("Cannot read file", "Помилка читання файлу"), 1200);
                    return -1;
                }
            };
            let slice = if bytes.starts_with(&[0xEF, 0xBB, 0xBF]) {
                &bytes[3..]
            } else {
                &bytes[..]
            };
            let text = String::from_utf8_lossy(slice).into_owned();
            let key = if is_ephemeral_info {
                None
            } else {
                Some(fnv1a_hex(&p.to_string_lossy()))
            };
            (title, text, key)
        }
        None => (
            ctx.tr("Text Viewer", "Переглядач тексту").to_string(),
            default_guide_text(ctx.is_ukrainian()),
            None,
        ),
    };

    let mut cfg = ReaderSettings::load(ctx);

    let mut top_line: usize = bmark_key
        .as_ref()
        .and_then(|k| ctx.load_plugin_data("text_viewer", k))
        .and_then(|b| String::from_utf8(b).ok())
        .and_then(|s| s.trim().parse::<usize>().ok())
        .unwrap_or(0);

    let text_x = 32.0_f32;
    let content_h = (SCREEN_HEIGHT as f32) - CONTENT_TOP_Y - 56.0;
    let max_text_w = (SCREEN_WIDTH as f32) - 64.0 - 36.0;

    let mut wrapped = wrap_paragraphs(ctx, &raw_text, FONT_SIZES[cfg.size_idx], max_text_w);

    loop {
        let fsz = FONT_SIZES[cfg.size_idx];
        let line_h = (fsz * LINE_SPACINGS[cfg.spacing_idx]).round();
        let vis_lines = ((content_h / line_h).floor() as usize).max(1);
        let max_top = wrapped.len().saturating_sub(vis_lines);
        if top_line > max_top {
            top_line = max_top;
        }

        let t = ctx.theme();
        ctx.begin_frame(t.bg, true, &doc_title);

        for i in 0..vis_lines {
            let idx = top_line + i;
            if idx >= wrapped.len() {
                break;
            }
            let cy = CONTENT_TOP_Y + (i as f32 + 0.5) * line_h;
            let row = &wrapped[idx];
            if row.text.is_empty() {
                continue;
            }
            let (draw_sz, weight) = match row.heading_level {
                1 => (fsz + 4.0, FontWeight::Bold),
                2 => (fsz, FontWeight::SemiBold),
                _ => (fsz, FontWeight::Regular),
            };
            ctx.draw_text(
                text_x,
                cy,
                &row.text,
                draw_sz,
                weight,
                TextAlign::Left,
                t.fg,
            );
        }

        if wrapped.len() > vis_lines {
            ctx.draw_scrollbar(
                (SCREEN_WIDTH as f32) - 48.0,
                CONTENT_TOP_Y,
                16.0,
                content_h,
                wrapped.len(),
                top_line,
                vis_lines,
            );
        }

        let pct = if max_top == 0 {
            100
        } else {
            (top_line * 100) / max_top
        };
        let footer = format!(
            "{} {}/{} ({pct}%)   •   {}",
            ctx.tr("Line", "Рядок"),
            (top_line + 1).min(wrapped.len()),
            wrapped.len(),
            ctx.tr(
                "B: Back  •  X: Size  •  Menu: Options",
                "B: Назад  •  X: Шрифт  •  Menu: Меню"
            )
        );
        ctx.draw_text(
            text_x,
            (SCREEN_HEIGHT as f32) - 26.0,
            &footer,
            23.0,
            FontWeight::Medium,
            TextAlign::Left,
            t.fg2,
        );

        ctx.present();

        let inp = ctx.poll_input(250);
        if inp.should_exit || inp.pressed(btn::B) {
            save_bookmark(ctx, &bmark_key, top_line);
            return 0;
        }

        if inp.pressed(btn::START) {
            ctx.audio_toggle_play_pause();
        }

        if inp.menu_requested || inp.pressed(btn::MENU) {
            let mut sel = 0usize;
            loop {
                let items = [
                    MenuItem::action(ctx.tr("Continue Reading", "Читати далі")),
                    MenuItem::action(ctx.tr("Go to Beginning", "На початок")),
                    MenuItem::action(ctx.tr("Go to End", "У кінець")),
                    MenuItem::action(ctx.tr("Save Bookmark", "Зберегти закладку")),
                    MenuItem::submenu(ctx.tr("Options", "Налаштування")),
                    MenuItem::action(ctx.tr("Exit", "Вихід")),
                ];
                match ctx.show_menu(&doc_title, &items, sel) {
                    MenuOutcome::Selected(0) | MenuOutcome::Cancelled => break,
                    MenuOutcome::Selected(1) => {
                        top_line = 0;
                        break;
                    }
                    MenuOutcome::Selected(2) => {
                        top_line = max_top;
                        break;
                    }
                    MenuOutcome::Selected(3) => {
                        sel = 3;
                        save_bookmark(ctx, &bmark_key, top_line);
                        ctx.show_splash(ctx.tr("Bookmark saved", "Закладку збережено"), 700);
                    }
                    MenuOutcome::Selected(4) => {
                        sel = 4;
                        let frac_num = top_line;
                        let frac_den = wrapped.len().max(1);
                        let (force_exit, rewrap) = open_reader_options(ctx, &mut cfg);
                        if rewrap {
                            wrapped = wrap_paragraphs(
                                ctx,
                                &raw_text,
                                FONT_SIZES[cfg.size_idx],
                                max_text_w,
                            );
                            top_line = (frac_num * wrapped.len()) / frac_den;
                        }
                        if force_exit {
                            save_bookmark(ctx, &bmark_key, top_line);
                            return 0;
                        }
                    }
                    MenuOutcome::Selected(_) | MenuOutcome::ForceExit => {
                        save_bookmark(ctx, &bmark_key, top_line);
                        return 0;
                    }
                }
            }
            continue;
        }

        if inp.pressed(btn::UP) {
            top_line = top_line.saturating_sub(1);
        } else if inp.pressed(btn::DOWN) {
            top_line = (top_line + 1).min(max_top);
        } else if inp.pressed(btn::LEFT | btn::L) {
            top_line = top_line.saturating_sub(vis_lines.saturating_sub(1).max(1));
        } else if inp.pressed(btn::RIGHT | btn::R) {
            top_line = (top_line + vis_lines.saturating_sub(1).max(1)).min(max_top);
        } else if inp.pressed(btn::X) {
            let frac_num = top_line;
            let frac_den = wrapped.len().max(1);
            cfg.size_idx = (cfg.size_idx + 1) % FONT_SIZES.len();
            cfg.save(ctx);
            wrapped = wrap_paragraphs(ctx, &raw_text, FONT_SIZES[cfg.size_idx], max_text_w);
            top_line = (frac_num * wrapped.len()) / frac_den;
        }
    }
}

uipod_declare_plugin! {
    id: "text_viewer",
    name: "Text Viewer",
    name_uk: "Переглядач тексту",
    author: "UIPod",
    version: "0.2.1",
    category: PluginCategory::Viewer,
    kind: PluginKind::Utility,
    extensions: "txt,lrc,md,log,cfg,ini,json,cue,nfo",
    entry: run_text_viewer,
}
