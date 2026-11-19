//! iPod-style `Stopwatch & Timer` utility plugin for UIPod.

use std::path::Path;

use uipod_plugin_sdk::{
    btn, uipod_declare_plugin, FontWeight, HostContext, PluginCategory, PluginKind, TextAlign,
    CONTENT_TOP_Y, SCREEN_WIDTH,
};

fn format_clock_ms(ms: u64, show_centis: bool) -> String {
    let total_sec = ms / 1000;
    let mins = total_sec / 60;
    let secs = total_sec % 60;
    let centis = (ms % 1000) / 10;
    if mins >= 60 {
        let hrs = mins / 60;
        let rem_m = mins % 60;
        if show_centis {
            format!("{hrs:02}:{rem_m:02}:{secs:02}.{centis:02}")
        } else {
            format!("{hrs:02}:{rem_m:02}:{secs:02}")
        }
    } else if show_centis {
        format!("{mins:02}:{secs:02}.{centis:02}")
    } else {
        format!("{mins:02}:{secs:02}")
    }
}

fn run_stopwatch(ctx: &mut HostContext, _file: Option<&Path>) -> i32 {
    // mode 0 = Stopwatch, mode 1 = Timer
    let mut timer_mode = false;
    let mut sw_running = false;
    let mut sw_accum_ms: u64 = 0;
    let mut sw_start_tick: u64 = 0;
    let mut laps: Vec<(u64, u64)> = Vec::new(); // (lap_split_ms, total_ms)
    let mut lap_scroll: usize = 0;

    let mut tm_running = false;
    let mut tm_preset_sec: u64 = 5 * 60;
    let mut tm_rem_ms: u64 = tm_preset_sec * 1000;
    let mut tm_last_tick: u64 = 0;
    let mut tm_finished = false;

    loop {
        let now = ctx.ticks_ms();
        let active = sw_running || tm_running;
        let inp = ctx.poll_input(if active { 25 } else { 200 });
        if inp.should_exit || inp.pressed(btn::B) {
            return 0;
        }

        let tick_now = ctx.ticks_ms();
        if tm_running {
            let dt = tick_now.saturating_sub(tm_last_tick);
            tm_last_tick = tick_now;
            if dt >= tm_rem_ms {
                tm_rem_ms = 0;
                tm_running = false;
                tm_finished = true;
            } else {
                tm_rem_ms -= dt;
            }
        }

        if inp.pressed(btn::L | btn::R | btn::SELECT) {
            timer_mode = !timer_mode;
        }

        if !timer_mode {
            let cur_total = if sw_running {
                sw_accum_ms + tick_now.saturating_sub(sw_start_tick)
            } else {
                sw_accum_ms
            };

            if inp.pressed(btn::A | btn::START) {
                if sw_running {
                    sw_accum_ms = cur_total;
                    sw_running = false;
                } else {
                    sw_start_tick = tick_now;
                    sw_running = true;
                }
            } else if inp.pressed(btn::X | btn::Y) {
                if sw_running {
                    let prev_total = laps.first().map(|&(_, tot)| tot).unwrap_or(0);
                    let split = cur_total.saturating_sub(prev_total);
                    laps.insert(0, (split, cur_total));
                    lap_scroll = 0;
                } else {
                    sw_accum_ms = 0;
                    laps.clear();
                    lap_scroll = 0;
                }
            } else if inp.pressed(btn::UP) && lap_scroll > 0 {
                lap_scroll -= 1;
            } else if inp.pressed(btn::DOWN) && lap_scroll + 5 < laps.len() {
                lap_scroll += 1;
            }
        } else {
            if inp.pressed(btn::A | btn::START) {
                if tm_finished {
                    tm_finished = false;
                    tm_rem_ms = tm_preset_sec * 1000;
                } else if tm_running {
                    tm_running = false;
                } else if tm_rem_ms > 0 {
                    tm_last_tick = tick_now;
                    tm_running = true;
                }
            } else if inp.pressed(btn::X | btn::Y) {
                tm_running = false;
                tm_finished = false;
                tm_rem_ms = tm_preset_sec * 1000;
            } else if !tm_running {
                if inp.pressed(btn::UP | btn::RIGHT) {
                    tm_preset_sec = (tm_preset_sec + 60).min(180 * 60);
                    tm_rem_ms = tm_preset_sec * 1000;
                    tm_finished = false;
                } else if inp.pressed(btn::DOWN | btn::LEFT) {
                    tm_preset_sec = tm_preset_sec.saturating_sub(60).max(60);
                    tm_rem_ms = tm_preset_sec * 1000;
                    tm_finished = false;
                }
            }
        }

        // Draw frame
        let t = ctx.theme();
        let hdr_title = if timer_mode {
            ctx.tr("Timer", "Таймер")
        } else {
            ctx.tr("Stopwatch", "Секундомір")
        };
        let (w, _h) = ctx.begin_responsive_frame(t.bg, true, hdr_title);
        let _ = SCREEN_WIDTH;
        // Mode tabs pill at top (Stopwatch | Timer)
        let tab_y = CONTENT_TOP_Y + 4.0;
        let tab_w = 220.0_f32;
        let tab_h = 44.0_f32;
        let tab0_x = w * 0.5 - tab_w - 8.0;
        let tab1_x = w * 0.5 + 8.0;

        if !timer_mode {
            ctx.fill_rect(tab0_x, tab_y, tab_w, tab_h, 8.0, t.fg);
            ctx.stroke_rect(tab1_x, tab_y, tab_w, tab_h, 8.0, 3.0, t.fg1);
        } else {
            ctx.stroke_rect(tab0_x, tab_y, tab_w, tab_h, 8.0, 3.0, t.fg1);
            ctx.fill_rect(tab1_x, tab_y, tab_w, tab_h, 8.0, t.fg);
        }
        ctx.draw_text(
            tab0_x + tab_w * 0.5,
            tab_y + tab_h * 0.5,
            ctx.tr("Stopwatch", "Секундомір"),
            26.0,
            FontWeight::SemiBold,
            TextAlign::Center,
            if !timer_mode { t.bg } else { t.fg2 },
        );
        ctx.draw_text(
            tab1_x + tab_w * 0.5,
            tab_y + tab_h * 0.5,
            ctx.tr("Timer", "Таймер"),
            26.0,
            FontWeight::SemiBold,
            TextAlign::Center,
            if timer_mode { t.bg } else { t.fg2 },
        );

        if !timer_mode {
            let total_ms = if sw_running {
                sw_accum_ms + ctx.ticks_ms().saturating_sub(sw_start_tick)
            } else {
                sw_accum_ms
            };
            let main_str = format_clock_ms(total_ms, true);

            // Big digital readout card
            ctx.stroke_rect(32.0, tab_y + 62.0, w - 64.0, 152.0, 12.0, 4.0, t.fg1);
            ctx.draw_text(
                w * 0.5,
                tab_y + 138.0,
                &main_str,
                92.0,
                FontWeight::Bold,
                TextAlign::Center,
                t.fg,
            );

            // Controls hint row
            let hint = if sw_running {
                ctx.tr(
                    "A: Pause   •   X: Lap   •   L/R: Timer",
                    "A: Пауза   •   X: Коло   •   L/R: Таймер",
                )
            } else {
                ctx.tr(
                    "A: Start   •   X: Reset   •   L/R: Timer",
                    "A: Старт   •   X: Скинути   •   L/R: Таймер",
                )
            };
            ctx.draw_text(
                w * 0.5,
                tab_y + 244.0,
                hint,
                26.0,
                FontWeight::Medium,
                TextAlign::Center,
                t.fg2,
            );

            // Laps table
            let list_top = tab_y + 276.0;
            let row_h = 54.0_f32;
            let vis = 5usize;
            if laps.is_empty() {
                ctx.draw_text(
                    w * 0.5,
                    list_top + 90.0,
                    ctx.tr("(No laps recorded)", "(Кіл ще немає)"),
                    30.0,
                    FontWeight::Regular,
                    TextAlign::Center,
                    t.fg1,
                );
            } else {
                let list_w = if laps.len() > vis {
                    w - 64.0 - 28.0
                } else {
                    w - 64.0
                };
                for i in 0..vis {
                    let idx = lap_scroll + i;
                    if idx >= laps.len() {
                        break;
                    }
                    let lap_num = laps.len() - idx;
                    let (split_ms, tot_ms) = laps[idx];
                    let ry = list_top + (i as f32) * (row_h + 6.0);
                    ctx.stroke_rect(32.0, ry, list_w, row_h, 8.0, 3.0, t.fg1);

                    let left_txt = format!("{} #{lap_num}", ctx.tr("Lap", "Коло"));
                    let right_txt = format!(
                        "+{}   ({})",
                        format_clock_ms(split_ms, true),
                        format_clock_ms(tot_ms, true)
                    );
                    ctx.draw_text(
                        52.0,
                        ry + row_h * 0.5,
                        &left_txt,
                        30.0,
                        FontWeight::SemiBold,
                        TextAlign::Left,
                        t.fg,
                    );
                    ctx.draw_text(
                        32.0 + list_w - 20.0,
                        ry + row_h * 0.5,
                        &right_txt,
                        28.0,
                        FontWeight::Medium,
                        TextAlign::Right,
                        t.fg2,
                    );
                }
                if laps.len() > vis {
                    ctx.draw_scrollbar(
                        w - 48.0,
                        list_top,
                        16.0,
                        vis as f32 * row_h + (vis - 1) as f32 * 6.0,
                        laps.len(),
                        lap_scroll,
                        vis,
                    );
                }
            }
        } else {
            let main_str = format_clock_ms(tm_rem_ms, tm_running);
            let card_y = tab_y + 76.0;
            if tm_finished && ((now / 350) % 2 == 0) {
                ctx.fill_rect(32.0, card_y, w - 64.0, 200.0, 12.0, t.fg);
                ctx.draw_text(
                    w * 0.5,
                    card_y + 100.0,
                    ctx.tr("TIME'S UP!", "ЧАС ВИЙШОВ!"),
                    72.0,
                    FontWeight::Bold,
                    TextAlign::Center,
                    t.bg,
                );
            } else {
                ctx.stroke_rect(32.0, card_y, w - 64.0, 200.0, 12.0, 4.0, t.fg1);
                ctx.draw_text(
                    w * 0.5,
                    card_y + 100.0,
                    &main_str,
                    96.0,
                    FontWeight::Bold,
                    TextAlign::Center,
                    t.fg,
                );
            }

            let frac = if tm_preset_sec > 0 {
                (tm_rem_ms as f32) / ((tm_preset_sec * 1000) as f32)
            } else {
                0.0
            };
            ctx.draw_progress_bar(32.0, card_y + 236.0, w - 64.0, 24.0, frac);

            let hint = if tm_running {
                ctx.tr(
                    "A: Pause   •   X: Reset   •   L/R: Stopwatch",
                    "A: Пауза   •   X: Скинути   •   L/R: Секундомір",
                )
            } else {
                ctx.tr(
                    "Up/Down: ±1 min   •   A: Start   •   X: Reset",
                    "Вгору/Вниз: ±1 хв   •   A: Старт   •   X: Скинути",
                )
            };
            ctx.draw_text(
                w * 0.5,
                card_y + 310.0,
                hint,
                28.0,
                FontWeight::Medium,
                TextAlign::Center,
                t.fg2,
            );
        }

        ctx.present();
    }
}

uipod_declare_plugin! {
    id: "stopwatch",
    name: "Stopwatch",
    name_uk: "Секундомір",
    author: "UIPod",
    version: "0.1.0",
    category: PluginCategory::App,
    kind: PluginKind::Utility,
    extensions: "",
    entry: run_stopwatch,
}
