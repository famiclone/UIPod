//! Native Manual Dual-Deck `DJ Mixer` plugin for UIPod (`Extras -> Plugins -> Apps -> DJ Mixer`).
//!
//! Features:
//! - Side-by-side Dual-Deck layout (plus optional Single-Deck CDJ view) rendered in UIPod's
//!   4-shade vector UI (`Geologica` typography, crisp `1px`/`2px` cards, standard status header).
//! - Zoomed 1-bar (`4-beat`, `24` discrete bars) beat-quantized waveform per deck with zero
//!   high-density pixel mush and live RAM-pinned Beat Loop highlight overlays.
//! - Real-time lock-free control of Equal-Power Crossfader + `180 Hz` Bass Swap, resonant bipolar
//!   `LPF/HPF` Macro Filter (with `0` center detent), WSOLA `Master Tempo` / `Vinyl` Beat Sync,
//!   click-free RAM `Cue` / `Stutter`, and `L2/R2` Quick Track Loader (`Queue` / `Library` / `Favourites`).

use std::path::Path;

use uipod_plugin_sdk::{
    btn, uipod_declare_plugin, DjCueAction, DjDeckId, DjTempoMode, DjTrackSource, FontWeight,
    HostContext, LibraryTrack, MenuItem, MenuOutcome, PluginCategory, PluginDjDeckTelemetry,
    PluginDjTelemetry, PluginKind, PluginTheme, TextAlign, CONTENT_TOP_Y,
};

/// Declarative 1:1 Layout Specification (`1024x768` reference canvas, scaled responsively).
struct LayoutSpec {
    margin_x: f32,
    deck_top_y: f32,
    deck_gap: f32,
    deck_h: f32,
    deck_radius: f32,
    deck_pad_x: f32,
    deck_focus_stroke: f32,
    deck_idle_stroke: f32,
    deck_header_cy_off: f32,
    deck_loop_cy_off: f32,
    deck_meta_cy_off: f32,
    deck_prog_y_off: f32,
    deck_prog_h: f32,
    wave_y_off: f32,
    wave_h: f32,
    wave_bar_gap: f32,
    wave_bar_radius: f32,

    pills_y: f32,
    pills_h: f32,
    pills_gap: f32,
    pills_radius: f32,
    pill_sync_w: f32,
    pill_filter_w: f32,
    pill_bass_w: f32,

    xfader_margin_x: f32,
    xfader_y: f32,
    xfader_h: f32,
    xfader_radius: f32,
    xfader_track_inset: f32,
    xfader_track_h: f32,
    xfader_cap_w: f32,
    xfader_cap_h: f32,
    xfader_cap_radius: f32,

    hint_div_y: f32,
    hint_text_cy: f32,

    font_deck_title: f32,
    font_loop_badge: f32,
    font_meta: f32,
    font_pill: f32,
    font_xfader_ab: f32,
    font_hint: f32,
}

impl LayoutSpec {
    fn compute(_sw: f32, sh: f32) -> Self {
        let extra_h = (sh - 768.0).max(0.0);
        let deck_h = 330.0 + extra_h * 0.48;
        let deck_top_y = CONTENT_TOP_Y + 4.0;
        let pills_y = deck_top_y + deck_h + 18.0 + extra_h * 0.12;
        let xfader_y = pills_y + 62.0 + 18.0 + extra_h * 0.12;
        let hint_div_y = sh - 64.0;
        Self {
            margin_x: 32.0,
            deck_top_y,
            deck_gap: 24.0,
            deck_h,
            deck_radius: 18.0,
            deck_pad_x: 24.0,
            deck_focus_stroke: 2.5,
            deck_idle_stroke: 1.2,
            deck_header_cy_off: 38.0,
            deck_loop_cy_off: 68.0,
            deck_meta_cy_off: 98.0,
            deck_prog_y_off: 120.0,
            deck_prog_h: 8.0,
            wave_y_off: 144.0,
            wave_h: (deck_h - 166.0).max(120.0),
            wave_bar_gap: 3.5,
            wave_bar_radius: 2.5,

            pills_y,
            pills_h: 62.0,
            pills_gap: 20.0,
            pills_radius: 16.0,
            pill_sync_w: 132.0,
            pill_filter_w: 152.0,
            pill_bass_w: 144.0,

            xfader_margin_x: 48.0,
            xfader_y,
            xfader_h: 108.0,
            xfader_radius: 18.0,
            xfader_track_inset: 92.0,
            xfader_track_h: 12.0,
            xfader_cap_w: 36.0,
            xfader_cap_h: 82.0,
            xfader_cap_radius: 14.0,

            hint_div_y,
            hint_text_cy: sh - 32.0,

            font_deck_title: 34.0,
            font_loop_badge: 20.0,
            font_meta: 22.0,
            font_pill: 21.0,
            font_xfader_ab: 40.0,
            font_hint: 21.0,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum MixerLayoutMode {
    DualDeck = 0,
    SingleDeckCdj = 1,
}

struct QuickLoaderState {
    open: bool,
    target_deck: DjDeckId,
    source: DjTrackSource,
    tracks: Vec<LibraryTrack>,
    selected: usize,
}

impl QuickLoaderState {
    fn new() -> Self {
        Self {
            open: false,
            target_deck: DjDeckId::DeckB,
            source: DjTrackSource::Queue,
            tracks: Vec::new(),
            selected: 0,
        }
    }

    fn reload_tracks(&mut self, ctx: &HostContext) {
        self.tracks.clear();
        let count = ctx.dj_source_track_count(self.source).min(1024);
        for i in 0..count {
            if let Some(t) = ctx.dj_source_track_at(self.source, i) {
                self.tracks.push(t);
            }
        }
        // Auto-fallback to Library if Queue/Favourites is empty on first open
        if self.tracks.is_empty() && self.source != DjTrackSource::Library {
            let lib_cnt = ctx.dj_source_track_count(DjTrackSource::Library).min(1024);
            if lib_cnt > 0 {
                self.source = DjTrackSource::Library;
                for i in 0..lib_cnt {
                    if let Some(t) = ctx.dj_source_track_at(DjTrackSource::Library, i) {
                        self.tracks.push(t);
                    }
                }
            }
        }
        if self.selected >= self.tracks.len() {
            self.selected = self.tracks.len().saturating_sub(1);
        }
    }
}

struct DjMixerState {
    focused_deck: DjDeckId,
    crossfader: f32,
    xfade_hold_sec: f32,
    last_left_tap_ms: u64,
    last_right_tap_ms: u64,

    bass_swap: bool,
    tempo_mode: DjTempoMode,
    layout_mode: MixerLayoutMode,

    deck_filters: [f32; 2],
    filter_detent_until_ms: [u64; 2],
    deck_ratios: [f32; 2],

    cue_preview_held: [bool; 2],
    cue_latched_play: [bool; 2],
    last_b_tap_ms: [u64; 2],

    menu_sel: usize,
    loader: QuickLoaderState,
}

impl DjMixerState {
    fn new() -> Self {
        Self {
            focused_deck: DjDeckId::DeckA,
            crossfader: 0.0,
            xfade_hold_sec: 0.0,
            last_left_tap_ms: 0,
            last_right_tap_ms: 0,
            bass_swap: true,
            tempo_mode: DjTempoMode::MasterTempo,
            layout_mode: MixerLayoutMode::DualDeck,
            deck_filters: [0.0, 0.0],
            filter_detent_until_ms: [0, 0],
            deck_ratios: [1.0, 1.0],
            cue_preview_held: [false, false],
            cue_latched_play: [false, false],
            last_b_tap_ms: [0, 0],
            menu_sel: 0,
            loader: QuickLoaderState::new(),
        }
    }
}

fn format_remaining_mmss(rem_ms: u32) -> String {
    let total_sec = (rem_ms + 500) / 1000;
    let mins = total_sec / 60;
    let secs = total_sec % 60;
    format!("-{mins:02}:{secs:02}")
}

fn format_duration_mmss(dur_ms: u32) -> String {
    if dur_ms == 0 {
        return String::new();
    }
    let total_sec = (dur_ms + 500) / 1000;
    format!("{:02}:{:02}", total_sec / 60, total_sec % 60)
}

fn truncate_to_width(
    ctx: &HostContext,
    text: &str,
    font_size: f32,
    weight: FontWeight,
    max_w: f32,
) -> String {
    if max_w <= 12.0 || text.is_empty() {
        return String::new();
    }
    if ctx.measure_text(text, font_size, weight) <= max_w {
        return text.to_string();
    }
    let ellipsis = "…";
    let ell_w = ctx.measure_text(ellipsis, font_size, weight);
    let target = (max_w - ell_w).max(0.0);
    let mut out = String::new();
    for ch in text.chars() {
        out.push(ch);
        if ctx.measure_text(&out, font_size, weight) > target {
            out.pop();
            break;
        }
    }
    out.push_str(ellipsis);
    out
}

#[allow(clippy::too_many_arguments)]
fn draw_deck_card(
    ctx: &HostContext,
    spec: &LayoutSpec,
    theme: PluginTheme,
    deck: DjDeckId,
    dt: &PluginDjDeckTelemetry,
    focused: bool,
    filter_val: f32,
    x: f32,
    y: f32,
    w: f32,
    h: f32,
) {
    // Card background & focus border (`2.5px` Shade 3 when focused, `1.2px` Shade 2 when unfocused)
    ctx.fill_rect(x, y, w, h, spec.deck_radius, theme.bg);
    if focused {
        ctx.stroke_rect(
            x,
            y,
            w,
            h,
            spec.deck_radius,
            spec.deck_focus_stroke,
            theme.fg,
        );
    } else {
        ctx.stroke_rect(
            x,
            y,
            w,
            h,
            spec.deck_radius,
            spec.deck_idle_stroke,
            theme.fg1,
        );
    }

    let inner_x = x + spec.deck_pad_x;
    let inner_w = (w - spec.deck_pad_x * 2.0).max(60.0);
    let right_x = inner_x + inner_w;

    // 1. Card Header: `DECK A` / `DECK B` + play indicator on left, `128.0 BPM` + `LOOP X` on right
    let hdr_cy = y + spec.deck_header_cy_off;
    let deck_name = match deck {
        DjDeckId::DeckA => "DECK A",
        DjDeckId::DeckB => "DECK B",
    };
    let title_end_x = ctx.draw_text(
        inner_x,
        hdr_cy,
        deck_name,
        spec.font_deck_title,
        FontWeight::Bold,
        TextAlign::Left,
        theme.fg,
    );

    if dt.loaded != 0 {
        // Subtle play/cue state dot next to `DECK A/B`
        let dot_cx = title_end_x + 14.0;
        if dt.playing != 0 {
            ctx.fill_circle(dot_cx, hdr_cy, 5.5, theme.fg);
        } else {
            ctx.stroke_rect(dot_cx - 5.0, hdr_cy - 5.0, 10.0, 10.0, 5.0, 1.6, theme.fg1);
        }
    }

    let bpm_text = if dt.loaded != 0 {
        format!("{:.1} BPM", dt.effective_bpm)
    } else {
        "--.- BPM".to_string()
    };
    ctx.draw_text(
        right_x,
        hdr_cy,
        &bpm_text,
        spec.font_deck_title,
        FontWeight::Bold,
        TextAlign::Right,
        if dt.loaded != 0 { theme.fg } else { theme.fg1 },
    );

    if let Some(loop_lbl) = dt.loop_length().badge_label() {
        ctx.draw_text(
            right_x,
            y + spec.deck_loop_cy_off,
            loop_lbl,
            spec.font_loop_badge,
            FontWeight::SemiBold,
            TextAlign::Right,
            theme.fg,
        );
    } else if filter_val.abs() > 0.05 && !focused {
        // Show inactive deck's non-zero filter badge so the DJ never forgets an open HPF/LPF!
        let flt_badge = if filter_val < 0.0 {
            format!("LPF {:.0}%", (-filter_val * 100.0).round())
        } else {
            format!("HPF +{:.0}%", (filter_val * 100.0).round())
        };
        ctx.draw_text(
            right_x,
            y + spec.deck_loop_cy_off,
            &flt_badge,
            spec.font_loop_badge - 2.0,
            FontWeight::Medium,
            TextAlign::Right,
            theme.fg1,
        );
    }

    // 2. Track Metadata (`Artist — Title`) & Remaining Time (`-MM:SS`)
    let meta_cy = y + spec.deck_meta_cy_off;
    let rem_str = if dt.loaded != 0 {
        format_remaining_mmss(dt.remaining_ms)
    } else {
        "-00:00".to_string()
    };
    let rem_w = ctx.measure_text(&rem_str, spec.font_meta, FontWeight::Medium);
    ctx.draw_text(
        right_x,
        meta_cy,
        &rem_str,
        spec.font_meta,
        FontWeight::Medium,
        TextAlign::Right,
        if dt.loaded != 0 { theme.fg } else { theme.fg1 },
    );

    let raw_meta = if dt.loaded != 0 {
        let artist = dt.artist_str();
        let title = dt.title_str();
        if !artist.is_empty() && !title.is_empty() {
            format!("{artist} — {title}")
        } else if !title.is_empty() {
            title.to_string()
        } else {
            ctx.tr("Untitled Track", "Без назви").to_string()
        }
    } else {
        ctx.tr("Press L2/R2 to Load Track", "L2/R2 — завантажити трек")
            .to_string()
    };
    let clipped_meta = truncate_to_width(
        ctx,
        &raw_meta,
        spec.font_meta,
        FontWeight::Medium,
        inner_w - rem_w - 16.0,
    );
    ctx.draw_text(
        inner_x,
        meta_cy,
        &clipped_meta,
        spec.font_meta,
        FontWeight::Medium,
        TextAlign::Left,
        if dt.loaded != 0 { theme.fg } else { theme.fg1 },
    );

    // 3. Thin Full-Track Progress Bar (`Shade 1` track, `Shade 3` fill + Cue tick)
    let prog_y = y + spec.deck_prog_y_off;
    let prog_h = spec.deck_prog_h;
    let prog_r = prog_h * 0.5;
    ctx.fill_rect(inner_x, prog_y, inner_w, prog_h, prog_r, theme.fg2);
    ctx.stroke_rect(inner_x, prog_y, inner_w, prog_h, prog_r, 1.0, theme.fg1);
    if dt.loaded != 0 && dt.duration_ms > 0 {
        let frac = ((dt.elapsed_ms as f32) / (dt.duration_ms as f32)).clamp(0.0, 1.0);
        let fill_w = (inner_w * frac).max(if frac > 0.002 { prog_h } else { 0.0 });
        if fill_w > 0.5 {
            ctx.fill_rect(inner_x, prog_y, fill_w, prog_h, prog_r, theme.fg);
        }
        // Subtle Cue marker tick
        let cue_frac = ((dt.cue_ms as f32) / (dt.duration_ms as f32)).clamp(0.0, 1.0);
        let cue_x = inner_x + inner_w * cue_frac;
        ctx.draw_line(cue_x, prog_y - 3.0, cue_x, prog_y + prog_h + 3.0, 2.0, theme.fg);
    }

    // 4. Zoomed 1-Bar (`4-Beat`, `24-Bar`) Discrete Waveform
    let wave_x = inner_x;
    let wave_y = y + spec.wave_y_off;
    let wave_w = inner_w;
    let wave_h = (y + h - 22.0 - wave_y).max(spec.wave_h.min(80.0));
    let wave_bottom = wave_y + wave_h;
    let wave_cx = wave_x + wave_w * 0.5;

    // Loop Highlight Overlay behind looped beats
    if let Some(loop_beats) = dt.loop_length().beats_f32() {
        let vis_beats = loop_beats.min(3.4);
        let box_w = (wave_w * (vis_beats / 4.0)).clamp(36.0, wave_w - 24.0);
        let box_x = wave_cx - box_w * 0.5;
        ctx.fill_rect(box_x, wave_y + 2.0, box_w, wave_h - 4.0, 8.0, theme.fg2);
    }

    // Outer left/right boundary hairlines
    ctx.draw_line(wave_x, wave_y + 4.0, wave_x, wave_bottom, 1.0, theme.fg1);
    ctx.draw_line(
        wave_x + wave_w,
        wave_y + 4.0,
        wave_x + wave_w,
        wave_bottom,
        1.0,
        theme.fg1,
    );

    // 4 thin `1px` vertical beatgrid lines aligned to `dt.beat_phase` across `[-2.0 .. +2.0]` beats
    let phase = dt.beat_phase.clamp(0.0, 0.9999);
    for n in -2i32..=2i32 {
        let rel_beats = (n as f32) - phase;
        if rel_beats > -1.98 && rel_beats < 1.98 {
            let gx = wave_cx + (rel_beats * 0.25) * wave_w;
            ctx.draw_line(gx, wave_y + 2.0, gx, wave_bottom, 1.1, theme.fg1);
        }
    }

    // 24 discrete symmetric rectangular bars (`6` per beat) with crisp `3–4px` gaps
    let slot_w = wave_w / 24.0;
    let bar_w = (slot_w - spec.wave_bar_gap).max(4.0);
    let max_bar_h = wave_h - 14.0;
    let center_sub = (phase * 6.0).floor() as i32;

    for i in 0..24usize {
        let pk = dt.bars_peak_24[i];
        let kk = dt.bars_kick_24[i];
        if dt.loaded == 0 || pk == 0 {
            continue;
        }

        // Slot `i = 12` is the active playhead sub-step; `(rel_sub + center_sub) % 6 == 0` is an exact beatgrid downbeat!
        let rel_sub = (i as i32) - 12;
        let is_beat_slot = (center_sub + rel_sub).rem_euclid(6) == 0;

        let norm = (pk as f32) * (1.0 / 255.0);
        let kick_boost = if is_beat_slot && kk >= 115 { 1.12 } else { 1.0 };
        let bar_h = (norm * max_bar_h * kick_boost).clamp(12.0, max_bar_h);
        let bx = wave_x + (i as f32) * slot_w + (slot_w - bar_w) * 0.5;
        let by = wave_bottom - bar_h;

        // 4-shade semantic bar coloring:
        // - Tall Kick / strong beat bars: `Shade 3` (`theme.fg`)
        // - Played / medium groove bars: `Shade 2` (`theme.fg1`)
        // - Subtle / upcoming off-beat bars: `Shade 1` (`theme.fg2`)
        let bar_color = if (is_beat_slot && kk >= 105) || (kk >= 165 && pk >= 135) {
            theme.fg
        } else if i < 12 || pk >= 120 {
            theme.fg1
        } else {
            theme.fg2
        };

        ctx.fill_rect(bx, by, bar_w, bar_h, spec.wave_bar_radius, bar_color);
    }

    // Bold `2px` `Shade 3` center playhead line (drawn on top of bars for maximum pocket clarity)
    ctx.draw_line(wave_cx, wave_y, wave_cx, wave_bottom + 2.0, 2.5, theme.fg);
}

fn draw_status_pills(
    ctx: &HostContext,
    spec: &LayoutSpec,
    theme: PluginTheme,
    sw: f32,
    sync_active: bool,
    focused_filter: f32,
    bass_swap: bool,
) {
    let total_w =
        spec.pill_sync_w + spec.pill_filter_w + spec.pill_bass_w + spec.pills_gap * 2.0;
    let start_x = (sw - total_w) * 0.5;
    let py = spec.pills_y;
    let ph = spec.pills_h;
    let r = spec.pills_radius;

    // 1. `SYNC` pill
    let x_sync = start_x;
    if sync_active {
        ctx.fill_rect(x_sync, py, spec.pill_sync_w, ph, r, theme.fg);
        ctx.draw_text(
            x_sync + spec.pill_sync_w * 0.5,
            py + ph * 0.5,
            "SYNC",
            spec.font_pill + 1.0,
            FontWeight::Bold,
            TextAlign::Center,
            theme.bg,
        );
    } else {
        ctx.fill_rect(x_sync, py, spec.pill_sync_w, ph, r, theme.fg2);
        ctx.draw_text(
            x_sync + spec.pill_sync_w * 0.5,
            py + ph * 0.5,
            "SYNC",
            spec.font_pill + 1.0,
            FontWeight::Bold,
            TextAlign::Center,
            theme.fg,
        );
    }

    // 2. `FILTER` pill (`FILTER: 0` / `LPF -40%` / `HPF +60%`)
    let x_flt = x_sync + spec.pill_sync_w + spec.pills_gap;
    let flt_active = focused_filter.abs() > 0.05;
    ctx.fill_rect(x_flt, py, spec.pill_filter_w, ph, r, theme.fg2);
    if flt_active {
        ctx.stroke_rect(x_flt, py, spec.pill_filter_w, ph, r, 2.0, theme.fg);
    }
    let (line1, line2) = if focused_filter < -0.05 {
        let pct = ((-focused_filter) * 100.0).round() as i32;
        ("LPF".to_string(), format!("-{pct}%"))
    } else if focused_filter > 0.05 {
        let pct = (focused_filter * 100.0).round() as i32;
        ("HPF".to_string(), format!("+{pct}%"))
    } else {
        ("FILTER:".to_string(), "0".to_string())
    };
    ctx.draw_text(
        x_flt + spec.pill_filter_w * 0.5,
        py + ph * 0.34,
        &line1,
        spec.font_pill - 1.0,
        FontWeight::Bold,
        TextAlign::Center,
        theme.fg,
    );
    ctx.draw_text(
        x_flt + spec.pill_filter_w * 0.5,
        py + ph * 0.71,
        &line2,
        spec.font_pill,
        FontWeight::Bold,
        TextAlign::Center,
        theme.fg,
    );

    // 3. `BASS SWAP` pill
    let x_bs = x_flt + spec.pill_filter_w + spec.pills_gap;
    if bass_swap {
        ctx.fill_rect(x_bs, py, spec.pill_bass_w, ph, r, theme.fg2);
        ctx.stroke_rect(x_bs, py, spec.pill_bass_w, ph, r, 1.5, theme.fg1);
        ctx.draw_text(
            x_bs + spec.pill_bass_w * 0.5,
            py + ph * 0.34,
            "BASS",
            spec.font_pill - 1.0,
            FontWeight::Bold,
            TextAlign::Center,
            theme.fg,
        );
        ctx.draw_text(
            x_bs + spec.pill_bass_w * 0.5,
            py + ph * 0.71,
            "SWAP",
            spec.font_pill - 1.0,
            FontWeight::Bold,
            TextAlign::Center,
            theme.fg,
        );
    } else {
        ctx.fill_rect(x_bs, py, spec.pill_bass_w, ph, r, theme.bg);
        ctx.stroke_rect(x_bs, py, spec.pill_bass_w, ph, r, 1.2, theme.fg2);
        ctx.draw_text(
            x_bs + spec.pill_bass_w * 0.5,
            py + ph * 0.34,
            "BASS",
            spec.font_pill - 1.0,
            FontWeight::SemiBold,
            TextAlign::Center,
            theme.fg1,
        );
        ctx.draw_text(
            x_bs + spec.pill_bass_w * 0.5,
            py + ph * 0.71,
            "OFF",
            spec.font_pill - 1.0,
            FontWeight::SemiBold,
            TextAlign::Center,
            theme.fg1,
        );
    }
}

fn draw_crossfader_card(
    ctx: &HostContext,
    spec: &LayoutSpec,
    theme: PluginTheme,
    sw: f32,
    crossfader: f32,
) {
    let cx_x = spec.xfader_margin_x;
    let cx_y = spec.xfader_y;
    let cx_w = (sw - spec.xfader_margin_x * 2.0).max(200.0);
    let cx_h = spec.xfader_h;
    let cy = cx_y + cx_h * 0.5;

    ctx.fill_rect(cx_x, cx_y, cx_w, cx_h, spec.xfader_radius, theme.bg);
    ctx.stroke_rect(cx_x, cx_y, cx_w, cx_h, spec.xfader_radius, 1.5, theme.fg1);

    ctx.draw_text(
        cx_x + 44.0,
        cy,
        "A",
        spec.font_xfader_ab,
        FontWeight::Bold,
        TextAlign::Center,
        theme.fg,
    );
    ctx.draw_text(
        cx_x + cx_w - 44.0,
        cy,
        "B",
        spec.font_xfader_ab,
        FontWeight::Bold,
        TextAlign::Center,
        theme.fg,
    );

    let track_x = cx_x + spec.xfader_track_inset;
    let track_w = (cx_w - spec.xfader_track_inset * 2.0).max(80.0);
    let track_y = cy - spec.xfader_track_h * 0.5;

    ctx.fill_rect(
        track_x,
        track_y,
        track_w,
        spec.xfader_track_h,
        spec.xfader_track_h * 0.5,
        theme.fg2,
    );

    // Subtle center 50/50 calibration tick
    let mid_x = track_x + track_w * 0.5;
    ctx.draw_line(mid_x, cy - 18.0, mid_x, cy + 18.0, 1.2, theme.fg1);

    // Bold Shade 3 rounded rectangular fader cap
    let cap_travel = (track_w - spec.xfader_cap_w).max(1.0);
    let cap_x = track_x + crossfader.clamp(0.0, 1.0) * cap_travel;
    let cap_y = cy - spec.xfader_cap_h * 0.5;
    ctx.fill_rect(
        cap_x,
        cap_y,
        spec.xfader_cap_w,
        spec.xfader_cap_h,
        spec.xfader_cap_radius,
        theme.fg,
    );
}

fn draw_quick_loader_overlay(
    ctx: &HostContext,
    theme: PluginTheme,
    sw: f32,
    sh: f32,
    loader: &QuickLoaderState,
) {
    let mw = (sw - 112.0).clamp(480.0, 912.0);
    let mh = (sh - CONTENT_TOP_Y - 56.0).clamp(360.0, 560.0);
    let mx = (sw - mw) * 0.5;
    let my = CONTENT_TOP_Y + 20.0;

    // Crisp elevated modal card
    ctx.fill_rect(mx + 6.0, my + 6.0, mw, mh, 20.0, theme.fg2);
    ctx.fill_rect(mx, my, mw, mh, 20.0, theme.bg);
    ctx.stroke_rect(mx, my, mw, mh, 20.0, 2.5, theme.fg);

    let deck_lbl = match loader.target_deck {
        DjDeckId::DeckA => "DECK A",
        DjDeckId::DeckB => "DECK B",
    };
    let hdr_str = if ctx.is_ukrainian() {
        format!("Завантажити у {deck_lbl} (пауза)")
    } else {
        format!("Load to {deck_lbl} (Paused)")
    };
    ctx.draw_text(
        mx + 28.0,
        my + 36.0,
        &hdr_str,
        28.0,
        FontWeight::Bold,
        TextAlign::Left,
        theme.fg,
    );

    // Source tabs (`Queue` · `Library` · `Favourites`)
    let tabs = [
        (DjTrackSource::Queue, ctx.tr("Queue", "Черга")),
        (DjTrackSource::Library, ctx.tr("Library", "Медіатека")),
        (DjTrackSource::Favourites, ctx.tr("Favourites", "Улюблені")),
    ];
    let mut tab_x = mx + mw - 28.0;
    for &(src, name) in tabs.iter().rev() {
        let tw = ctx.measure_text(name, 20.0, FontWeight::SemiBold) + 28.0;
        tab_x -= tw;
        let active = loader.source == src;
        ctx.fill_rect(
            tab_x,
            my + 16.0,
            tw,
            40.0,
            12.0,
            if active { theme.fg } else { theme.fg2 },
        );
        ctx.draw_text(
            tab_x + tw * 0.5,
            my + 36.0,
            name,
            20.0,
            FontWeight::SemiBold,
            TextAlign::Center,
            if active { theme.bg } else { theme.fg },
        );
        tab_x -= 10.0;
    }

    ctx.draw_line(mx + 20.0, my + 68.0, mx + mw - 20.0, my + 68.0, 1.2, theme.fg2);

    let list_y = my + 80.0;
    let list_h = mh - 134.0;
    let row_h = 56.0;
    let vis_rows = ((list_h / row_h).floor() as usize).max(1);

    if loader.tracks.is_empty() {
        ctx.draw_text(
            mx + mw * 0.5,
            list_y + list_h * 0.5,
            ctx.tr(
                "No tracks in this source — press ←/→ to switch tab",
                "У цьому джерелі порожньо — ←/→ змінити вкладку",
            ),
            23.0,
            FontWeight::Medium,
            TextAlign::Center,
            theme.fg1,
        );
    } else {
        let total = loader.tracks.len();
        let sel = loader.selected.min(total - 1);
        let start = if total <= vis_rows {
            0
        } else if sel >= vis_rows {
            (sel + 1 - vis_rows).min(total - vis_rows)
        } else {
            0
        };
        let end = (start + vis_rows).min(total);
        let row_w = mw - 64.0;

        for (vis_i, idx) in (start..end).enumerate() {
            let ry = list_y + (vis_i as f32) * row_h;
            let rcy = ry + row_h * 0.5;
            let trk = &loader.tracks[idx];
            let is_sel = idx == sel;

            if is_sel {
                ctx.fill_rect(mx + 20.0, ry + 3.0, row_w, row_h - 6.0, 12.0, theme.fg);
            }
            let fg_col = if is_sel { theme.bg } else { theme.fg };
            let sub_col = if is_sel { theme.bg } else { theme.fg1 };

            let dur_lbl = format_duration_mmss(trk.duration_ms);
            let dur_w = ctx.measure_text(&dur_lbl, 20.0, FontWeight::Medium);
            if !dur_lbl.is_empty() {
                ctx.draw_text(
                    mx + 20.0 + row_w - 16.0,
                    rcy,
                    &dur_lbl,
                    20.0,
                    FontWeight::Medium,
                    TextAlign::Right,
                    sub_col,
                );
            }

            let disp = if !trk.artist.is_empty() && !trk.title.is_empty() {
                format!("{} — {}", trk.artist, trk.title)
            } else if !trk.title.is_empty() {
                trk.title.clone()
            } else {
                trk.path.clone()
            };
            let clipped = truncate_to_width(
                ctx,
                &disp,
                22.0,
                FontWeight::SemiBold,
                row_w - dur_w - 44.0,
            );
            ctx.draw_text(
                mx + 36.0,
                rcy,
                &clipped,
                22.0,
                FontWeight::SemiBold,
                TextAlign::Left,
                fg_col,
            );
        }

        if total > vis_rows {
            ctx.draw_scrollbar(
                mx + mw - 30.0,
                list_y + 4.0,
                10.0,
                list_h - 8.0,
                total,
                start,
                vis_rows,
            );
        }
    }

    ctx.draw_line(
        mx + 20.0,
        my + mh - 48.0,
        mx + mw - 20.0,
        my + mh - 48.0,
        1.0,
        theme.fg2,
    );
    ctx.draw_text(
        mx + mw * 0.5,
        my + mh - 24.0,
        ctx.tr(
            "A: Load Paused  •  SEL: Target Deck  •  ←/→: Source  •  B: Cancel",
            "A: Завантажити  •  SEL: Дека  •  ←/→: Джерело  •  B: Скасувати",
        ),
        19.0,
        FontWeight::Medium,
        TextAlign::Center,
        theme.fg1,
    );
}

fn open_dj_system_menu(
    ctx: &HostContext,
    state: &mut DjMixerState,
    tel: &PluginDjTelemetry,
) -> bool {
    loop {
        let tempo_lbl = match state.tempo_mode {
            DjTempoMode::MasterTempo => "Master Tempo",
            DjTempoMode::Vinyl => "Vinyl",
        };
        let bass_lbl = if state.bass_swap {
            ctx.tr("On", "Увімк.")
        } else {
            ctx.tr("Off", "Вимк.")
        };
        let layout_lbl = match state.layout_mode {
            MixerLayoutMode::DualDeck => ctx.tr("Dual-Deck", "2 Деки"),
            MixerLayoutMode::SingleDeckCdj => ctx.tr("Single-Deck CDJ", "1 Дека CDJ"),
        };

        let items = [
            MenuItem::submenu(ctx.tr("Load Track to Deck A", "Завантажити трек у Деку A")),
            MenuItem::submenu(ctx.tr("Load Track to Deck B", "Завантажити трек у Деку B")),
            MenuItem::option(ctx.tr("Tempo Mode", "Режим темпу"), tempo_lbl),
            MenuItem::option(ctx.tr("Bass Swap", "Обмін басу"), bass_lbl),
            MenuItem::action(ctx.tr("Reset Filter", "Скинути фільтр")),
            MenuItem::option(ctx.tr("Layout", "Макет"), layout_lbl),
            MenuItem::action(ctx.tr("Quit DJ Mixer", "Вийти з DJ Мікшера")),
        ];

        match ctx.show_menu(ctx.tr("DJ Menu", "Меню DJ"), &items, state.menu_sel) {
            MenuOutcome::Cancelled => return false,
            MenuOutcome::ForceExit => return true,
            MenuOutcome::Selected(sel) => {
                state.menu_sel = sel;
                match sel {
                    0 => {
                        state.loader.target_deck = DjDeckId::DeckA;
                        state.loader.open = true;
                        state.loader.reload_tracks(ctx);
                        return false;
                    }
                    1 => {
                        state.loader.target_deck = DjDeckId::DeckB;
                        state.loader.open = true;
                        state.loader.reload_tracks(ctx);
                        return false;
                    }
                    2 => {
                        state.tempo_mode = match state.tempo_mode {
                            DjTempoMode::MasterTempo => DjTempoMode::Vinyl,
                            DjTempoMode::Vinyl => DjTempoMode::MasterTempo,
                        };
                        for d in [DjDeckId::DeckA, DjDeckId::DeckB] {
                            ctx.dj_deck_set_tempo(
                                d,
                                state.deck_ratios[d.index()],
                                state.tempo_mode,
                                0.0,
                            );
                        }
                    }
                    3 => {
                        state.bass_swap = !state.bass_swap;
                        ctx.dj_set_crossfader(state.crossfader, state.bass_swap);
                    }
                    4 => {
                        let fidx = state.focused_deck.index();
                        state.deck_filters[fidx] = 0.0;
                        ctx.dj_deck_set_filter(state.focused_deck, 0.0);
                        let _ = tel;
                        return false;
                    }
                    5 => {
                        state.layout_mode = match state.layout_mode {
                            MixerLayoutMode::DualDeck => MixerLayoutMode::SingleDeckCdj,
                            MixerLayoutMode::SingleDeckCdj => MixerLayoutMode::DualDeck,
                        };
                    }
                    6 => return true,
                    _ => {}
                }
            }
        }
    }
}

fn run_dj_mixer(ctx: &mut HostContext, _open_path: Option<&Path>) -> i32 {
    if !ctx.dj_session_acquire() {
        ctx.show_splash(
            ctx.tr("DJ Engine unavailable", "DJ-рушій недоступний"),
            1500,
        );
        return -1;
    }

    let mut state = DjMixerState::new();
    ctx.dj_set_crossfader(state.crossfader, state.bass_swap);

    let mut last_ms = ctx.ticks_ms();

    loop {
        let input = ctx.poll_input(0);
        if input.should_exit {
            break;
        }

        let now_ms = ctx.ticks_ms();
        let dt = (((now_ms.saturating_sub(last_ms)).min(100)) as f32) * 0.001;
        last_ms = now_ms;

        let tel = ctx.dj_telemetry().unwrap_or_default();

        if input.menu_requested {
            // Release any active momentary bend / stutter before entering modal system menu
            for d in [DjDeckId::DeckA, DjDeckId::DeckB] {
                ctx.dj_deck_set_tempo(d, state.deck_ratios[d.index()], state.tempo_mode, 0.0);
            }
            state.cue_preview_held = [false, false];
            state.loader.open = false;

            if open_dj_system_menu(ctx, &mut state, &tel) {
                break;
            }
            last_ms = ctx.ticks_ms();
            continue;
        }

        // ---- Modal Quick Track Loader (`L2 / R2`) ----
        if state.loader.open {
            if input.pressed(btn::B | btn::L2 | btn::R2) {
                state.loader.open = false;
            } else if input.pressed(btn::SELECT) {
                state.loader.target_deck = state.loader.target_deck.opposite();
            } else if input.pressed(btn::LEFT | btn::L) {
                state.loader.source = match state.loader.source {
                    DjTrackSource::Queue => DjTrackSource::Favourites,
                    DjTrackSource::Library => DjTrackSource::Queue,
                    DjTrackSource::Favourites => DjTrackSource::Library,
                };
                state.loader.selected = 0;
                state.loader.reload_tracks(ctx);
            } else if input.pressed(btn::RIGHT | btn::R) {
                state.loader.source = match state.loader.source {
                    DjTrackSource::Queue => DjTrackSource::Library,
                    DjTrackSource::Library => DjTrackSource::Favourites,
                    DjTrackSource::Favourites => DjTrackSource::Queue,
                };
                state.loader.selected = 0;
                state.loader.reload_tracks(ctx);
            } else if input.pressed(btn::UP) && !state.loader.tracks.is_empty() {
                if state.loader.selected == 0 {
                    state.loader.selected = state.loader.tracks.len() - 1;
                } else {
                    state.loader.selected -= 1;
                }
            } else if input.pressed(btn::DOWN) && !state.loader.tracks.is_empty() {
                state.loader.selected = (state.loader.selected + 1) % state.loader.tracks.len();
            } else if input.pressed(btn::A) && !state.loader.tracks.is_empty() {
                let chosen = state.loader.tracks[state.loader.selected].path.clone();
                let target = state.loader.target_deck;
                state.loader.open = false;
                if ctx.dj_deck_load(target, &chosen, true) {
                    state.deck_filters[target.index()] = 0.0;
                    state.deck_ratios[target.index()] = 1.0;
                    state.focused_deck = target;
                }
                last_ms = ctx.ticks_ms();
            }
        } else {
            // ---- Live Dual-Deck Performance Controls ----
            let focus = state.focused_deck;
            let fidx = focus.index();
            let dt_focus = &tel.decks[fidx];

            // 1. SELECT (solo tap): toggle active control focus DECK A <-> DECK B
            if input.pressed(btn::SELECT) {
                // Zero out momentary bend on outgoing focused deck
                ctx.dj_deck_set_tempo(focus, state.deck_ratios[fidx], state.tempo_mode, 0.0);
                state.focused_deck = focus.opposite();
            }

            // 2. L2 / R2: open Quick Track Loader pre-targeted to silent or focused deck
            if input.pressed(btn::L2 | btn::R2) {
                let target = if input.pressed(btn::L2) && !input.pressed(btn::R2) {
                    if tel.decks[0].playing == 0 {
                        DjDeckId::DeckA
                    } else if tel.decks[1].playing == 0 {
                        DjDeckId::DeckB
                    } else {
                        state.focused_deck
                    }
                } else if tel.decks[1].playing == 0 {
                    DjDeckId::DeckB
                } else if tel.decks[0].playing == 0 {
                    DjDeckId::DeckA
                } else {
                    state.focused_deck
                };
                state.loader.target_deck = target;
                state.loader.open = true;
                state.loader.reload_tracks(ctx);
            }

            // 3. D-Pad Left / Right: Smooth accelerated Crossfader + double-tap Quick Cut
            let held_l = input.held(btn::LEFT) && !input.held(btn::RIGHT);
            let held_r = input.held(btn::RIGHT) && !input.held(btn::LEFT);
            if input.pressed(btn::LEFT) && !held_r {
                if now_ms.saturating_sub(state.last_left_tap_ms) <= 230 {
                    state.crossfader = if state.crossfader > 0.55 { 0.5 } else { 0.0 };
                    ctx.dj_set_crossfader(state.crossfader, state.bass_swap);
                }
                state.last_left_tap_ms = now_ms;
            }
            if input.pressed(btn::RIGHT) && !held_l {
                if now_ms.saturating_sub(state.last_right_tap_ms) <= 230 {
                    state.crossfader = if state.crossfader < 0.45 { 0.5 } else { 1.0 };
                    ctx.dj_set_crossfader(state.crossfader, state.bass_swap);
                }
                state.last_right_tap_ms = now_ms;
            }
            if held_l || held_r {
                state.xfade_hold_sec = (state.xfade_hold_sec + dt).min(1.5);
                let speed = 0.52 + 0.95 * (state.xfade_hold_sec / 0.75).min(1.0);
                let dir = if held_r { 1.0 } else { -1.0 };
                state.crossfader = (state.crossfader + dir * speed * dt).clamp(0.0, 1.0);
                ctx.dj_set_crossfader(state.crossfader, state.bass_swap);
            } else {
                state.xfade_hold_sec = 0.0;
            }

            // 4. D-Pad Up / Down (+ optional Right Stick Y): Bipolar DJ Filter with `0` center detent
            let held_up = input.held(btn::UP) && !input.held(btn::DOWN);
            let held_dn = input.held(btn::DOWN) && !input.held(btn::UP);
            if (held_up || held_dn) && now_ms >= state.filter_detent_until_ms[fidx] {
                let dir = if held_up { 1.0 } else { -1.0 };
                let prev = state.deck_filters[fidx];
                let mut next = (prev + dir * 0.78 * dt).clamp(-1.0, 1.0);
                // Center `0` detent: snap cleanly to `0.0` for 180 ms when crossing zero!
                if (prev < -0.005 && next >= -0.005) || (prev > 0.005 && next <= 0.005) {
                    next = 0.0;
                    state.filter_detent_until_ms[fidx] = now_ms + 180;
                }
                state.deck_filters[fidx] = next;
                ctx.dj_deck_set_filter(focus, next);
            } else if tel.analog_ry.unsigned_abs() > 6_500 {
                // Analog Right Stick Y: live proportional Filter sweep (`Up = -ry -> HPF +`)
                let norm_ry = (-(tel.analog_ry as f32) / 32767.0).clamp(-1.0, 1.0);
                state.deck_filters[fidx] = norm_ry;
                ctx.dj_deck_set_filter(focus, norm_ry);
            }

            // 5. `A`: Play / Pause focused deck (beat-quantized when `SYNC` is active)
            if input.pressed(btn::A) && dt_focus.loaded != 0 {
                if state.cue_preview_held[fidx] {
                    // CDJ Play-Latch while holding Cue!
                    state.cue_latched_play[fidx] = true;
                } else {
                    let next_play = dt_focus.playing == 0;
                    let quant = dt_focus.sync_locked != 0;
                    ctx.dj_deck_set_playing(focus, next_play, quant);
                }
            }

            // 6. `B`: Cue / Stutter / Set Cue (never exits plugin!)
            if input.pressed(btn::B) && dt_focus.loaded != 0 {
                let rapid_stutter = now_ms.saturating_sub(state.last_b_tap_ms[fidx]) <= 340;
                state.last_b_tap_ms[fidx] = now_ms;

                if dt_focus.playing != 0 {
                    if rapid_stutter {
                        ctx.dj_deck_cue(focus, DjCueAction::CueStutter);
                        state.cue_preview_held[fidx] = true;
                        state.cue_latched_play[fidx] = true;
                    } else {
                        ctx.dj_deck_cue(focus, DjCueAction::CueReturn);
                        state.cue_preview_held[fidx] = false;
                        state.cue_latched_play[fidx] = false;
                    }
                } else {
                    let dist_ms = dt_focus.elapsed_ms.abs_diff(dt_focus.cue_ms);
                    if dist_ms > 35 {
                        // Paused away from Cue -> stamp new Cue point right here!
                        ctx.dj_deck_cue(focus, DjCueAction::SetCueHere);
                        ctx.show_splash(ctx.tr("Cue Set", "Cue встановлено"), 900);
                    } else {
                        // Paused at Cue -> instant CDJ Cue Preview / Stutter!
                        ctx.dj_deck_cue(focus, DjCueAction::CueStutter);
                        state.cue_preview_held[fidx] = true;
                        state.cue_latched_play[fidx] = false;
                    }
                }
            }
            if state.cue_preview_held[fidx] && !input.held(btn::B) {
                state.cue_preview_held[fidx] = false;
                if !state.cue_latched_play[fidx] {
                    ctx.dj_deck_cue(focus, DjCueAction::CueReturn);
                }
            }

            // 7. `X`: Cycle seamless RAM Beat Loop (`4 -> 2 -> 1 -> 1/2 -> Off`)
            if input.pressed(btn::X) && dt_focus.loaded != 0 {
                let next_loop = dt_focus.loop_length().next_cycle();
                ctx.dj_deck_set_loop(focus, next_loop);
            }

            // 8. `Y`: Toggle Beat Sync (`Master Tempo` / `Vinyl` + phase lock)
            if input.pressed(btn::Y) && dt_focus.loaded != 0 {
                let next_sync = dt_focus.sync_locked == 0;
                ctx.dj_deck_set_sync(focus, next_sync);
                if !next_sync {
                    state.deck_ratios[fidx] = 1.0;
                }
            }

            // 9. `L1 / R1` (+ optional Left Stick X):
            //    - Playing: Momentary Phase Bend `-4%` / `+4%`
            //    - Paused: Jump `-4` / `+4` beats along beatgrid to set Cue on a drop!
            let mut bend = 0.0f32;
            if dt_focus.playing != 0 {
                if input.held(btn::L) && !input.held(btn::R) {
                    bend = -0.04;
                } else if input.held(btn::R) && !input.held(btn::L) {
                    bend = 0.04;
                } else if tel.analog_lx.unsigned_abs() > 6_500 {
                    bend = ((tel.analog_lx as f32) / 32767.0 * 0.04).clamp(-0.04, 0.04);
                }
            } else {
                if input.pressed(btn::L) {
                    ctx.dj_deck_seek_beats(focus, -4);
                }
                if input.pressed(btn::R) {
                    ctx.dj_deck_seek_beats(focus, 4);
                }
            }

            if dt_focus.sync_locked != 0 && dt_focus.native_bpm > 20.0 {
                state.deck_ratios[fidx] =
                    (dt_focus.effective_bpm / dt_focus.native_bpm).clamp(0.75, 1.25);
            }
            ctx.dj_deck_set_tempo(focus, state.deck_ratios[fidx], state.tempo_mode, bend);
        }

        // ---- Render 60 FPS Native 4-Shade Vector UI ----
        let theme: PluginTheme = ctx.theme();
        let (sw, sh) =
            ctx.begin_responsive_frame(theme.bg, true, ctx.tr("DJ Mixer", "DJ Мікшер"));
        let spec = LayoutSpec::compute(sw, sh);

        match state.layout_mode {
            MixerLayoutMode::DualDeck => {
                let avail_w = (sw - spec.margin_x * 2.0 - spec.deck_gap).max(320.0);
                let card_w = avail_w * 0.5;
                let x_a = spec.margin_x;
                let x_b = spec.margin_x + card_w + spec.deck_gap;

                draw_deck_card(
                    ctx,
                    &spec,
                    theme,
                    DjDeckId::DeckA,
                    &tel.decks[0],
                    state.focused_deck == DjDeckId::DeckA,
                    state.deck_filters[0],
                    x_a,
                    spec.deck_top_y,
                    card_w,
                    spec.deck_h,
                );
                draw_deck_card(
                    ctx,
                    &spec,
                    theme,
                    DjDeckId::DeckB,
                    &tel.decks[1],
                    state.focused_deck == DjDeckId::DeckB,
                    state.deck_filters[1],
                    x_b,
                    spec.deck_top_y,
                    card_w,
                    spec.deck_h,
                );
            }
            MixerLayoutMode::SingleDeckCdj => {
                let fidx = state.focused_deck.index();
                let full_w = (sw - spec.margin_x * 2.0).max(320.0);
                draw_deck_card(
                    ctx,
                    &spec,
                    theme,
                    state.focused_deck,
                    &tel.decks[fidx],
                    true,
                    state.deck_filters[fidx],
                    spec.margin_x,
                    spec.deck_top_y,
                    full_w,
                    spec.deck_h,
                );
            }
        }

        let fidx = state.focused_deck.index();
        draw_status_pills(
            ctx,
            &spec,
            theme,
            sw,
            tel.decks[fidx].sync_locked != 0,
            state.deck_filters[fidx],
            state.bass_swap,
        );
        draw_crossfader_card(ctx, &spec, theme, sw, state.crossfader);

        // Bottom Controls Hint Strip
        ctx.draw_line(
            spec.margin_x,
            spec.hint_div_y,
            sw - spec.margin_x,
            spec.hint_div_y,
            1.0,
            theme.fg2,
        );
        ctx.draw_text(
            sw * 0.5,
            spec.hint_text_cy,
            ctx.tr(
                "SEL: Deck  •  ←/→: Crossfade  •  A: Play  •  B: Cue  •  X: Loop  •  Y: Sync",
                "SEL: Дека  •  ←/→: Кроссфейд  •  A: Грати  •  B: Cue  •  X: Луп  •  Y: Синхр",
            ),
            spec.font_hint,
            FontWeight::SemiBold,
            TextAlign::Center,
            theme.fg,
        );

        if state.loader.open {
            draw_quick_loader_overlay(ctx, theme, sw, sh, &state.loader);
        }

        ctx.present();
    }

    ctx.dj_session_release();
    0
}

uipod_declare_plugin! {
    id: "dj_mixer",
    name: "DJ Mixer",
    name_uk: "DJ Мікшер",
    author: "UIPod",
    version: "1.0.0",
    category: PluginCategory::App,
    kind: PluginKind::MenuDriven,
    extensions: "",
    entry: run_dj_mixer,
}
