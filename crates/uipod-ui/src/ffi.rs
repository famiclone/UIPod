//! Thin C ABI (`extern "C"`) over `UiEngine` for the Strangler-Fig migration stage.
//!
//! Contains zero `#include` / bindgen dependencies on Rockbox internals.

use std::borrow::Cow;
use std::ffi::{c_char, c_void, CStr};
use std::path::Path;
use std::sync::Mutex;

use crate::layout::LogicalMetrics;
use crate::renderer::{HEADER_BAND_ROWS, HEIGHT, WIDTH};
use crate::viz_gl::{VizEngine, VIZ_PRESETS};
use crate::{
    FileIconKind, GraphicEqState, HeaderSourceIcon, HeaderState, KeyboardKey, KeyboardState,
    ListRow, ListScreenState, NowPlayingState, NsfApuPill, NsfChannelRow, NsfChannelsState,
    PlayMode, RightAccessory, ThemeColors, UiEngine,
};

// Safety: UiEngine is accessed exclusively from the serialized Rockbox thread holding the SDL GL context.
unsafe impl Send for SyncEngineWrap {}
struct SyncEngineWrap(UiEngine);

static SYNC_ENGINE: Mutex<Option<SyncEngineWrap>> = Mutex::new(None);

// Safety: VizEngine is accessed exclusively from the thread holding the SDL GL context
// (initialized on main thread, rendered on `viz_render_thread` while `uipod_viz_active` is set).
unsafe impl Send for SyncVizWrap {}
struct SyncVizWrap(VizEngine);

static SYNC_VIZ: Mutex<Option<SyncVizWrap>> = Mutex::new(None);

pub type UIPodRsGlGetProc = unsafe extern "C" fn(*const c_char) -> *const c_void;

#[repr(C)]
#[derive(Clone, Copy)]
pub struct UIPodRsHeader {
    pub title: *const c_char,
    pub legend: *const c_char,
    pub play_mode: u32,
    pub battery_percent: u8,
    pub is_charging: bool,
    pub is_hold: bool,
    pub is_downloading: bool,
    /// Bits `0..6`: `0=None`, `1=MicroSd`, `2=Navidrome`, `3=Jellyfin`; bit `7` (`0x80`): `is_fx`
    pub source_icon: u8,
    pub download_done: u32,
    pub download_total: u32,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct UIPodRsListRow {
    pub label: *const c_char,
    pub right_value: *const c_char,
    pub file_tag: *const c_char,
    pub selected: bool,
    /// `0=None`, `1=Chevron`, `2=Toggle(false)`, `3=Toggle(true)`, `4=CycleValue(<val>)`, `5=Value(val)`
    pub right_kind: u8,
}

#[repr(C)]
pub struct UIPodRsListScreen {
    pub bg_rgb: u32,
    pub fg_rgb: u32,
    pub fg1_rgb: u32,
    pub fg2_rgb: u32,
    pub header: UIPodRsHeader,
    pub rows: *const UIPodRsListRow,
    pub row_count: usize,
    pub total_items: usize,
    pub start_item: usize,
    pub visible_items: usize,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct UIPodRsNsfApuPill {
    pub short_code: *const c_char,
    pub active: bool,
    pub muted: bool,
    pub volume: f32,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct UIPodRsNsfChannelRow {
    pub short_code: *const c_char,
    pub display_name: *const c_char,
    pub enabled: bool,
    pub soloed: bool,
    pub active: bool,
    pub volume: f32,
    pub selected: bool,
}

#[repr(C)]
pub struct UIPodRsNsfChannels {
    pub bg_rgb: u32,
    pub fg_rgb: u32,
    pub fg1_rgb: u32,
    pub fg2_rgb: u32,
    pub header: UIPodRsHeader,
    pub channels: *const UIPodRsNsfChannelRow,
    pub channel_count: usize,
    pub enable_all_label: *const c_char,
    pub enable_all_selected: bool,
    pub footer_hint: *const c_char,
    pub start_item: usize,
    pub visible_items: usize,
}

#[repr(C)]
pub struct UIPodRsNowPlaying {
    pub bg_rgb: u32,
    pub fg_rgb: u32,
    pub fg1_rgb: u32,
    pub fg2_rgb: u32,
    pub header: UIPodRsHeader,
    pub track_pos_label: *const c_char,
    pub eq_enabled: bool,
    pub title: *const c_char,
    pub artist: *const c_char,
    pub album: *const c_char,
    pub codec_label: *const c_char,
    pub bitdepth_label: *const c_char,
    pub samplerate_label: *const c_char,
    pub progress: f32,
    pub elapsed_label: *const c_char,
    pub remaining_label: *const c_char,
    pub center_status_label: *const c_char,
    pub art_rgb24: *const u8,
    pub mono_album_art: bool,
    pub alt_layout: bool,
    pub is_nsf: bool,
    pub nsf_endless_loop: bool,
    pub nsf_chip_badge: *const c_char,
    /// `< 0.0` when None, `0.0..=1.0` when `loop_point_ms` is known
    pub nsf_loop_progress: f32,
    pub nsf_channels: *const UIPodRsNsfApuPill,
    pub nsf_channel_count: usize,
    pub dj_status_label: *const c_char,
    pub dj_progress: f32,
    pub dj_bass_swap_flash: bool,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct UIPodRsKeyboardKey {
    pub label: *const c_char,
    pub selected: bool,
    pub is_modifier: bool,
    pub is_active_toggle: bool,
}

#[repr(C)]
pub struct UIPodRsKeyboard {
    pub bg_rgb: u32,
    pub fg_rgb: u32,
    pub fg1_rgb: u32,
    pub fg2_rgb: u32,
    pub text: *const c_char,
    pub row_counts: [usize; 5],
    pub keys: *const UIPodRsKeyboardKey,
}

#[repr(C)]
pub struct UIPodRsGraphicEq {
    pub bg_rgb: u32,
    pub fg_rgb: u32,
    pub fg1_rgb: u32,
    pub fg2_rgb: u32,
    pub header: UIPodRsHeader,
    pub preamp_tenths_db: i16,
    pub band_gains_tenths_db: [i16; 10],
    pub selected_col: usize,
    pub adjusting: bool,
    pub bypassed: bool,
}

#[inline]
unsafe fn cstr_lossy<'a>(ptr: *const c_char) -> Cow<'a, str> {
    if ptr.is_null() {
        Cow::Borrowed("")
    } else {
        CStr::from_ptr(ptr).to_string_lossy()
    }
}

#[inline]
fn decode_play_mode(raw: u32) -> PlayMode {
    match raw {
        1 => PlayMode::Play,
        2 => PlayMode::Pause,
        3 => PlayMode::FastForward,
        4 => PlayMode::Rewind,
        _ => PlayMode::Stop,
    }
}

#[inline]
fn decode_source_icon(raw: u8) -> HeaderSourceIcon {
    match raw & 0x7F {
        1 => HeaderSourceIcon::MicroSd,
        2 => HeaderSourceIcon::Navidrome,
        3 => HeaderSourceIcon::Jellyfin,
        _ => HeaderSourceIcon::None,
    }
}

#[inline]
const fn decode_is_fx(raw: u8) -> bool {
    (raw & 0x80) != 0
}

/// Initializes the global Rust GPU UI engine on the active OpenGL ES 3.0 context.
#[no_mangle]
pub unsafe extern "C" fn uipod_rs_ui_init(
    get_proc: Option<UIPodRsGlGetProc>,
    cjk_font_path: *const c_char,
) -> bool {
    let Some(loader) = get_proc else {
        return false;
    };
    let mut guard = match SYNC_ENGINE.lock() {
        Ok(g) => g,
        Err(_) => return false,
    };
    if guard.is_some() {
        return true;
    }

    let cjk_cow = cstr_lossy(cjk_font_path);
    let cjk_opt = if cjk_cow.trim().is_empty() {
        None
    } else {
        Some(Path::new(cjk_cow.as_ref()))
    };

    match UiEngine::new(|s| loader(s.as_ptr()), cjk_opt) {
        Ok(engine) => {
            *guard = Some(SyncEngineWrap(engine));
            true
        }
        Err(err) => {
            eprintln!("[uipod-ui] init error: {err}");
            false
        }
    }
}

/// Returns `true` if `uipod_rs_ui_init` has succeeded.
#[no_mangle]
pub extern "C" fn uipod_rs_ui_is_ready() -> bool {
    SYNC_ENGINE.lock().map(|g| g.is_some()).unwrap_or(false)
}

/// Releases GPU resources owned by the Rust UI engine.
#[no_mangle]
pub extern "C" fn uipod_rs_ui_shutdown() {
    if let Ok(mut guard) = SYNC_ENGINE.lock() {
        *guard = None;
    }
}

/// Renders only the top `Header` band (`1024 x 120`) into `fb_bgr` (`{b, g, r}`).
#[no_mangle]
pub unsafe extern "C" fn uipod_rs_ui_render_header(
    bg_rgb: u32,
    fg_rgb: u32,
    fg1_rgb: u32,
    fg2_rgb: u32,
    header: *const UIPodRsHeader,
    fb_bgr: *mut u8,
) -> bool {
    if header.is_null() || fb_bgr.is_null() {
        return false;
    }
    let mut guard = match SYNC_ENGINE.lock() {
        Ok(g) => g,
        Err(_) => return false,
    };
    let Some(wrap) = guard.as_mut() else {
        return false;
    };

    let h = &*header;
    let title = cstr_lossy(h.title);
    let legend = cstr_lossy(h.legend);
    let hs = HeaderState {
        title: &title,
        legend: &legend,
        play_mode: decode_play_mode(h.play_mode),
        battery_percent: h.battery_percent,
        is_charging: h.is_charging,
        is_hold: h.is_hold,
        is_fx: decode_is_fx(h.source_icon),
        is_downloading: h.is_downloading,
        download_done: h.download_done,
        download_total: h.download_total,
        source_icon: decode_source_icon(h.source_icon),
    };

    let fb_slice = std::slice::from_raw_parts_mut(fb_bgr, WIDTH * HEADER_BAND_ROWS * 3);
    wrap.0.render_header_into_bgr(
        ThemeColors {
            bg_rgb,
            fg_rgb,
            fg1_rgb,
            fg2_rgb,
        },
        &hs,
        fb_slice,
    );
    true
}

/// Renders a full `1024 x 768` list/menu screen into `fb_bgr` (`{b, g, r}`).
#[no_mangle]
pub unsafe extern "C" fn uipod_rs_ui_render_list(
    screen: *const UIPodRsListScreen,
    fb_bgr: *mut u8,
) -> bool {
    if screen.is_null() || fb_bgr.is_null() {
        return false;
    }
    let mut guard = match SYNC_ENGINE.lock() {
        Ok(g) => g,
        Err(_) => return false,
    };
    let Some(wrap) = guard.as_mut() else {
        return false;
    };

    let sc = &*screen;
    let title = cstr_lossy(sc.header.title);
    let legend = cstr_lossy(sc.header.legend);

    let raw_rows = if sc.rows.is_null() || sc.row_count == 0 {
        &[]
    } else {
        std::slice::from_raw_parts(sc.rows, sc.row_count)
    };

    let labels: Vec<Cow<str>> = raw_rows.iter().map(|r| cstr_lossy(r.label)).collect();
    let rvals: Vec<Cow<str>> = raw_rows.iter().map(|r| cstr_lossy(r.right_value)).collect();
    let ftags: Vec<Cow<str>> = raw_rows.iter().map(|r| cstr_lossy(r.file_tag)).collect();

    let mut rows = Vec::with_capacity(raw_rows.len());
    for i in 0..raw_rows.len() {
        let r = &raw_rows[i];
        let right = match r.right_kind {
            1 => RightAccessory::Chevron,
            2 => RightAccessory::Toggle(false),
            3 => RightAccessory::Toggle(true),
            4 => RightAccessory::CycleValue(&rvals[i]),
            5 => RightAccessory::Value(&rvals[i]),
            6 => RightAccessory::ValueChevron(&rvals[i]),
            _ => RightAccessory::None,
        };
        rows.push(ListRow {
            label: &labels[i],
            selected: r.selected,
            file_icon: FileIconKind::from_tag(&ftags[i]),
            right,
        });
    }

    let state = ListScreenState {
        theme: ThemeColors {
            bg_rgb: sc.bg_rgb,
            fg_rgb: sc.fg_rgb,
            fg1_rgb: sc.fg1_rgb,
            fg2_rgb: sc.fg2_rgb,
        },
        header: HeaderState {
            title: &title,
            legend: &legend,
            play_mode: decode_play_mode(sc.header.play_mode),
            battery_percent: sc.header.battery_percent,
            is_charging: sc.header.is_charging,
            is_hold: sc.header.is_hold,
            is_fx: decode_is_fx(sc.header.source_icon),
            is_downloading: sc.header.is_downloading,
            download_done: sc.header.download_done,
            download_total: sc.header.download_total,
            source_icon: decode_source_icon(sc.header.source_icon),
        },
        rows,
        total_items: sc.total_items,
        start_item: sc.start_item,
        visible_items: sc.visible_items,
    };

    let fb_slice = std::slice::from_raw_parts_mut(fb_bgr, WIDTH * HEIGHT * 3);
    wrap.0.render_list_into_bgr(&state, fb_slice);
    true
}

/// Renders the full `1024 x 768` `Now Playing` screen into `fb_bgr` (`{b, g, r}`).
#[no_mangle]
pub unsafe extern "C" fn uipod_rs_ui_render_now_playing(
    np: *const UIPodRsNowPlaying,
    fb_bgr: *mut u8,
) -> bool {
    if np.is_null() || fb_bgr.is_null() {
        return false;
    }
    let mut guard = match SYNC_ENGINE.lock() {
        Ok(g) => g,
        Err(_) => return false,
    };
    let Some(wrap) = guard.as_mut() else {
        return false;
    };

    let n = &*np;
    let h_title = cstr_lossy(n.header.title);
    let h_legend = cstr_lossy(n.header.legend);
    let track_pos = cstr_lossy(n.track_pos_label);
    let title = cstr_lossy(n.title);
    let artist = cstr_lossy(n.artist);
    let album = cstr_lossy(n.album);
    let codec = cstr_lossy(n.codec_label);
    let bitdepth = cstr_lossy(n.bitdepth_label);
    let samplerate = cstr_lossy(n.samplerate_label);
    let elapsed = cstr_lossy(n.elapsed_label);
    let remaining = cstr_lossy(n.remaining_label);
    let center_status = cstr_lossy(n.center_status_label);
    let nsf_badge = cstr_lossy(n.nsf_chip_badge);
    let dj_status = cstr_lossy(n.dj_status_label);

    let raw_pills = if n.nsf_channels.is_null() || n.nsf_channel_count == 0 {
        &[]
    } else {
        std::slice::from_raw_parts(n.nsf_channels, n.nsf_channel_count.min(8))
    };
    let pill_codes: Vec<Cow<str>> = raw_pills.iter().map(|p| cstr_lossy(p.short_code)).collect();
    let pills: Vec<NsfApuPill> = raw_pills
        .iter()
        .enumerate()
        .map(|(i, p)| NsfApuPill {
            short_code: &pill_codes[i],
            active: p.active,
            muted: p.muted,
            volume: p.volume,
        })
        .collect();

    let art_slice = if n.art_rgb24.is_null() {
        None
    } else {
        Some(std::slice::from_raw_parts(n.art_rgb24, 256 * 256 * 3))
    };

    let state = NowPlayingState {
        theme: ThemeColors {
            bg_rgb: n.bg_rgb,
            fg_rgb: n.fg_rgb,
            fg1_rgb: n.fg1_rgb,
            fg2_rgb: n.fg2_rgb,
        },
        header: HeaderState {
            title: &h_title,
            legend: &h_legend,
            play_mode: decode_play_mode(n.header.play_mode),
            battery_percent: n.header.battery_percent,
            is_charging: n.header.is_charging,
            is_hold: n.header.is_hold,
            is_fx: decode_is_fx(n.header.source_icon),
            is_downloading: n.header.is_downloading,
            download_done: n.header.download_done,
            download_total: n.header.download_total,
            source_icon: decode_source_icon(n.header.source_icon),
        },
        track_pos_label: &track_pos,
        eq_enabled: n.eq_enabled,
        title: &title,
        artist: &artist,
        album: &album,
        codec_label: &codec,
        bitdepth_label: &bitdepth,
        samplerate_label: &samplerate,
        progress: n.progress,
        elapsed_label: &elapsed,
        remaining_label: &remaining,
        center_status_label: &center_status,
        art_rgb24: art_slice,
        mono_album_art: n.mono_album_art,
        alt_layout: n.alt_layout,
        is_nsf: n.is_nsf,
        nsf_chip_badge: &nsf_badge,
        nsf_loop_progress: (n.nsf_loop_progress >= 0.0 && n.nsf_loop_progress <= 1.0)
            .then_some(n.nsf_loop_progress),
        nsf_endless_loop: n.nsf_endless_loop,
        nsf_channels: &pills,
        dj_status_label: &dj_status,
        dj_progress: n.dj_progress,
        dj_bass_swap_flash: n.dj_bass_swap_flash,
    };

    let fb_slice = std::slice::from_raw_parts_mut(fb_bgr, WIDTH * HEIGHT * 3);
    wrap.0.render_now_playing_into_bgr(&state, fb_slice);
    true
}

/// Renders the full `1024 x 768` QWERTY keyboard screen into `fb_bgr` (`{b, g, r}`).
#[no_mangle]
pub unsafe extern "C" fn uipod_rs_ui_render_keyboard(
    kbd: *const UIPodRsKeyboard,
    fb_bgr: *mut u8,
) -> bool {
    if kbd.is_null() || fb_bgr.is_null() {
        return false;
    }
    let mut guard = match SYNC_ENGINE.lock() {
        Ok(g) => g,
        Err(_) => return false,
    };
    let Some(wrap) = guard.as_mut() else {
        return false;
    };

    let k = &*kbd;
    let text = cstr_lossy(k.text);
    let total_keys: usize = k.row_counts.iter().sum();
    let raw_keys = if k.keys.is_null() || total_keys == 0 {
        &[]
    } else {
        std::slice::from_raw_parts(k.keys, total_keys)
    };
    let key_labels: Vec<Cow<str>> = raw_keys.iter().map(|rk| cstr_lossy(rk.label)).collect();

    let mut rows: [Vec<KeyboardKey>; 5] = Default::default();
    let mut offset = 0usize;
    for r_idx in 0..5 {
        let cnt = k.row_counts[r_idx];
        let mut row_vec = Vec::with_capacity(cnt);
        for i in 0..cnt {
            if offset + i < raw_keys.len() {
                let rk = &raw_keys[offset + i];
                row_vec.push(KeyboardKey {
                    label: &key_labels[offset + i],
                    selected: rk.selected,
                    is_modifier: rk.is_modifier,
                    is_active_toggle: rk.is_active_toggle,
                });
            }
        }
        offset += cnt;
        rows[r_idx] = row_vec;
    }

    let state = KeyboardState {
        theme: ThemeColors {
            bg_rgb: k.bg_rgb,
            fg_rgb: k.fg_rgb,
            fg1_rgb: k.fg1_rgb,
            fg2_rgb: k.fg2_rgb,
        },
        text: &text,
        rows,
    };

    let fb_slice = std::slice::from_raw_parts_mut(fb_bgr, WIDTH * HEIGHT * 3);
    wrap.0.render_keyboard_into_bgr(&state, fb_slice);
    true
}

/* ---- Plugin GPU Canvas FFI Entry Points -------------------------------- */

#[no_mangle]
pub unsafe extern "C" fn uipod_rs_ui_plugin_begin(
    bg_rgb: u32,
    fg_rgb: u32,
    fg1_rgb: u32,
    fg2_rgb: u32,
    clear_rgb: u32,
    header: *const UIPodRsHeader,
) -> bool {
    let mut guard = match SYNC_ENGINE.lock() {
        Ok(g) => g,
        Err(_) => return false,
    };
    let Some(wrap) = guard.as_mut() else {
        return false;
    };

    let theme = ThemeColors {
        bg_rgb,
        fg_rgb,
        fg1_rgb,
        fg2_rgb,
    };

    if header.is_null() {
        wrap.0.plugin_begin(theme, clear_rgb, None);
    } else {
        let h = &*header;
        let title = cstr_lossy(h.title);
        let legend = cstr_lossy(h.legend);
        let hs = HeaderState {
            title: &title,
            legend: &legend,
            play_mode: decode_play_mode(h.play_mode),
            battery_percent: h.battery_percent,
            is_charging: h.is_charging,
            is_hold: h.is_hold,
            is_fx: decode_is_fx(h.source_icon),
            is_downloading: h.is_downloading,
            download_done: h.download_done,
            download_total: h.download_total,
            source_icon: decode_source_icon(h.source_icon),
        };
        wrap.0.plugin_begin(theme, clear_rgb, Some(&hs));
    }
    true
}

#[no_mangle]
pub extern "C" fn uipod_rs_ui_plugin_fill_rect(
    x: f32,
    y: f32,
    w: f32,
    h: f32,
    radius: f32,
    rgb: u32,
) {
    if let Ok(mut guard) = SYNC_ENGINE.lock() {
        if let Some(wrap) = guard.as_mut() {
            wrap.0.plugin_fill_rect(x, y, w, h, radius, rgb);
        }
    }
}

#[no_mangle]
pub extern "C" fn uipod_rs_ui_plugin_stroke_rect(
    x: f32,
    y: f32,
    w: f32,
    h: f32,
    radius: f32,
    stroke_w: f32,
    rgb: u32,
) {
    if let Ok(mut guard) = SYNC_ENGINE.lock() {
        if let Some(wrap) = guard.as_mut() {
            wrap.0.plugin_stroke_rect(x, y, w, h, radius, stroke_w, rgb);
        }
    }
}

#[no_mangle]
pub extern "C" fn uipod_rs_ui_plugin_fill_circle(cx: f32, cy: f32, r: f32, rgb: u32) {
    if let Ok(mut guard) = SYNC_ENGINE.lock() {
        if let Some(wrap) = guard.as_mut() {
            wrap.0.plugin_fill_circle(cx, cy, r, rgb);
        }
    }
}

#[no_mangle]
pub extern "C" fn uipod_rs_ui_plugin_draw_line(
    x0: f32,
    y0: f32,
    x1: f32,
    y1: f32,
    stroke_w: f32,
    rgb: u32,
) {
    if let Ok(mut guard) = SYNC_ENGINE.lock() {
        if let Some(wrap) = guard.as_mut() {
            wrap.0.plugin_draw_line(x0, y0, x1, y1, stroke_w, rgb);
        }
    }
}

#[no_mangle]
pub unsafe extern "C" fn uipod_rs_ui_plugin_draw_text(
    x: f32,
    center_y: f32,
    text: *const c_char,
    font_size: f32,
    weight: u8,
    align: u8,
    rgb: u32,
) -> f32 {
    if text.is_null() {
        return 0.0;
    }
    let s = cstr_lossy(text);
    if let Ok(mut guard) = SYNC_ENGINE.lock() {
        if let Some(wrap) = guard.as_mut() {
            return wrap
                .0
                .plugin_draw_text(x, center_y, &s, font_size, weight, align, rgb);
        }
    }
    0.0
}

#[no_mangle]
pub unsafe extern "C" fn uipod_rs_ui_plugin_measure_text(
    text: *const c_char,
    font_size: f32,
    weight: u8,
) -> f32 {
    if text.is_null() {
        return 0.0;
    }
    let s = cstr_lossy(text);
    if let Ok(mut guard) = SYNC_ENGINE.lock() {
        if let Some(wrap) = guard.as_mut() {
            return wrap.0.plugin_measure_text(&s, font_size, weight);
        }
    }
    0.0
}

#[no_mangle]
pub extern "C" fn uipod_rs_ui_plugin_draw_progress_bar(
    bg_rgb: u32,
    fg_rgb: u32,
    fg1_rgb: u32,
    fg2_rgb: u32,
    x: f32,
    y: f32,
    w: f32,
    h: f32,
    progress: f32,
) {
    if let Ok(mut guard) = SYNC_ENGINE.lock() {
        if let Some(wrap) = guard.as_mut() {
            wrap.0.plugin_draw_progress_bar(
                ThemeColors {
                    bg_rgb,
                    fg_rgb,
                    fg1_rgb,
                    fg2_rgb,
                },
                x,
                y,
                w,
                h,
                progress,
            );
        }
    }
}

#[no_mangle]
pub extern "C" fn uipod_rs_ui_plugin_draw_scrollbar(
    bg_rgb: u32,
    fg_rgb: u32,
    fg1_rgb: u32,
    fg2_rgb: u32,
    x: f32,
    y: f32,
    w: f32,
    h: f32,
    total: usize,
    start: usize,
    visible: usize,
) {
    if let Ok(mut guard) = SYNC_ENGINE.lock() {
        if let Some(wrap) = guard.as_mut() {
            wrap.0.plugin_draw_scrollbar(
                ThemeColors {
                    bg_rgb,
                    fg_rgb,
                    fg1_rgb,
                    fg2_rgb,
                },
                x,
                y,
                w,
                h,
                total,
                start,
                visible,
            );
        }
    }
}

#[no_mangle]
pub unsafe extern "C" fn uipod_rs_ui_plugin_draw_rgba_image(
    tex_key: u64,
    src_w: u32,
    src_h: u32,
    rgba_ptr: *const u8,
    rgba_len: usize,
    dst_x: f32,
    dst_y: f32,
    dst_w: f32,
    dst_h: f32,
    radius: f32,
) {
    if rgba_ptr.is_null() || src_w == 0 || src_h == 0 {
        return;
    }
    let needed = (src_w as usize)
        .saturating_mul(src_h as usize)
        .saturating_mul(4);
    if rgba_len < needed || needed == 0 {
        return;
    }
    let rgba = std::slice::from_raw_parts(rgba_ptr, needed);
    if let Ok(mut guard) = SYNC_ENGINE.lock() {
        if let Some(wrap) = guard.as_mut() {
            wrap.0.plugin_draw_rgba_image(
                tex_key, src_w, src_h, rgba, dst_x, dst_y, dst_w, dst_h, radius,
            );
        }
    }
}

#[no_mangle]
pub unsafe extern "C" fn uipod_rs_ui_plugin_present(fb_bgr: *mut u8) -> bool {
    if fb_bgr.is_null() {
        return false;
    }
    let mut guard = match SYNC_ENGINE.lock() {
        Ok(g) => g,
        Err(_) => return false,
    };
    let Some(wrap) = guard.as_mut() else {
        return false;
    };
    let fb_slice = std::slice::from_raw_parts_mut(fb_bgr, WIDTH * HEIGHT * 3);
    wrap.0.plugin_present_into_bgr(fb_slice);
    true
}

#[no_mangle]
pub extern "C" fn uipod_rs_ui_plugin_present_gpu(win_w: u32, win_h: u32) -> bool {
    let mut guard = match SYNC_ENGINE.lock() {
        Ok(g) => g,
        Err(_) => return false,
    };
    let Some(wrap) = guard.as_mut() else {
        return false;
    };
    wrap.0.plugin_present_gpu_screen(win_w, win_h);
    true
}

#[no_mangle]
pub extern "C" fn uipod_rs_ui_present_video_overlay_gpu(
    win_w: u32,
    win_h: u32,
    bottom_rows_768: u32,
) -> bool {
    let mut guard = match SYNC_ENGINE.lock() {
        Ok(g) => g,
        Err(_) => return false,
    };
    let Some(wrap) = guard.as_mut() else {
        return false;
    };
    wrap.0
        .present_video_overlay_gpu(win_w, win_h, bottom_rows_768);
    true
}

#[no_mangle]
pub extern "C" fn uipod_rs_ui_plugin_flush_fbo() -> bool {
    let mut guard = match SYNC_ENGINE.lock() {
        Ok(g) => g,
        Err(_) => return false,
    };
    let Some(wrap) = guard.as_mut() else {
        return false;
    };
    wrap.0.plugin_flush_fbo_only();
    true
}

#[no_mangle]
pub unsafe extern "C" fn uipod_rs_ui_plugin_sync_fb(fb_bgr: *mut u8) -> bool {
    if fb_bgr.is_null() {
        return false;
    }
    let mut guard = match SYNC_ENGINE.lock() {
        Ok(g) => g,
        Err(_) => return false,
    };
    let Some(wrap) = guard.as_mut() else {
        return false;
    };
    let fb_slice = std::slice::from_raw_parts_mut(fb_bgr, WIDTH * HEIGHT * 3);
    wrap.0.plugin_sync_fbo_into_bgr(fb_slice);
    true
}

/* ---- Native-HiDPI Responsive GPU Screen & Transition Entry Points ------ */

#[no_mangle]
pub extern "C" fn uipod_rs_ui_visible_rows_for_window(win_w: u32, win_h: u32) -> usize {
    LogicalMetrics::from_physical(win_w, win_h).visible_rows
}

#[no_mangle]
pub unsafe extern "C" fn uipod_rs_ui_render_header_gpu(
    bg_rgb: u32,
    fg_rgb: u32,
    fg1_rgb: u32,
    fg2_rgb: u32,
    header: *const UIPodRsHeader,
    win_w: u32,
    win_h: u32,
) -> bool {
    if header.is_null() {
        return false;
    }
    let mut guard = match SYNC_ENGINE.lock() {
        Ok(g) => g,
        Err(_) => return false,
    };
    let Some(wrap) = guard.as_mut() else {
        return false;
    };

    let h = &*header;
    let title = cstr_lossy(h.title);
    let legend = cstr_lossy(h.legend);
    let hs = HeaderState {
        title: &title,
        legend: &legend,
        play_mode: decode_play_mode(h.play_mode),
        battery_percent: h.battery_percent,
        is_charging: h.is_charging,
        is_hold: h.is_hold,
        is_fx: decode_is_fx(h.source_icon),
        is_downloading: h.is_downloading,
        download_done: h.download_done,
        download_total: h.download_total,
        source_icon: decode_source_icon(h.source_icon),
    };

    wrap.0.render_header_to_fbo_sized(
        ThemeColors {
            bg_rgb,
            fg_rgb,
            fg1_rgb,
            fg2_rgb,
        },
        &hs,
        win_w,
        win_h,
    );
    true
}

#[no_mangle]
pub unsafe extern "C" fn uipod_rs_ui_render_list_gpu(
    screen: *const UIPodRsListScreen,
    win_w: u32,
    win_h: u32,
) -> bool {
    if screen.is_null() {
        return false;
    }
    let mut guard = match SYNC_ENGINE.lock() {
        Ok(g) => g,
        Err(_) => return false,
    };
    let Some(wrap) = guard.as_mut() else {
        return false;
    };

    let sc = &*screen;
    let title = cstr_lossy(sc.header.title);
    let legend = cstr_lossy(sc.header.legend);

    let raw_rows = if sc.rows.is_null() || sc.row_count == 0 {
        &[]
    } else {
        std::slice::from_raw_parts(sc.rows, sc.row_count)
    };

    let labels: Vec<Cow<str>> = raw_rows.iter().map(|r| cstr_lossy(r.label)).collect();
    let rvals: Vec<Cow<str>> = raw_rows.iter().map(|r| cstr_lossy(r.right_value)).collect();
    let ftags: Vec<Cow<str>> = raw_rows.iter().map(|r| cstr_lossy(r.file_tag)).collect();

    let mut rows = Vec::with_capacity(raw_rows.len());
    for i in 0..raw_rows.len() {
        let r = &raw_rows[i];
        let right = match r.right_kind {
            1 => RightAccessory::Chevron,
            2 => RightAccessory::Toggle(false),
            3 => RightAccessory::Toggle(true),
            4 => RightAccessory::CycleValue(&rvals[i]),
            5 => RightAccessory::Value(&rvals[i]),
            6 => RightAccessory::ValueChevron(&rvals[i]),
            _ => RightAccessory::None,
        };
        rows.push(ListRow {
            label: &labels[i],
            selected: r.selected,
            file_icon: FileIconKind::from_tag(&ftags[i]),
            right,
        });
    }

    let state = ListScreenState {
        theme: ThemeColors {
            bg_rgb: sc.bg_rgb,
            fg_rgb: sc.fg_rgb,
            fg1_rgb: sc.fg1_rgb,
            fg2_rgb: sc.fg2_rgb,
        },
        header: HeaderState {
            title: &title,
            legend: &legend,
            play_mode: decode_play_mode(sc.header.play_mode),
            battery_percent: sc.header.battery_percent,
            is_charging: sc.header.is_charging,
            is_hold: sc.header.is_hold,
            is_fx: decode_is_fx(sc.header.source_icon),
            is_downloading: sc.header.is_downloading,
            download_done: sc.header.download_done,
            download_total: sc.header.download_total,
            source_icon: decode_source_icon(sc.header.source_icon),
        },
        rows,
        total_items: sc.total_items,
        start_item: sc.start_item,
        visible_items: sc.visible_items,
    };

    wrap.0.render_list_to_fbo_sized(&state, win_w, win_h);
    true
}

#[no_mangle]
pub unsafe extern "C" fn uipod_rs_ui_render_now_playing_gpu(
    np: *const UIPodRsNowPlaying,
    win_w: u32,
    win_h: u32,
) -> bool {
    if np.is_null() {
        return false;
    }
    let mut guard = match SYNC_ENGINE.lock() {
        Ok(g) => g,
        Err(_) => return false,
    };
    let Some(wrap) = guard.as_mut() else {
        return false;
    };

    let n = &*np;
    let h_title = cstr_lossy(n.header.title);
    let h_legend = cstr_lossy(n.header.legend);
    let track_pos = cstr_lossy(n.track_pos_label);
    let title = cstr_lossy(n.title);
    let artist = cstr_lossy(n.artist);
    let album = cstr_lossy(n.album);
    let codec = cstr_lossy(n.codec_label);
    let bitdepth = cstr_lossy(n.bitdepth_label);
    let samplerate = cstr_lossy(n.samplerate_label);
    let elapsed = cstr_lossy(n.elapsed_label);
    let remaining = cstr_lossy(n.remaining_label);
    let center_status = cstr_lossy(n.center_status_label);
    let nsf_badge = cstr_lossy(n.nsf_chip_badge);
    let dj_status = cstr_lossy(n.dj_status_label);

    let raw_pills = if n.nsf_channels.is_null() || n.nsf_channel_count == 0 {
        &[]
    } else {
        std::slice::from_raw_parts(n.nsf_channels, n.nsf_channel_count.min(8))
    };
    let pill_codes: Vec<Cow<str>> = raw_pills.iter().map(|p| cstr_lossy(p.short_code)).collect();
    let pills: Vec<NsfApuPill> = raw_pills
        .iter()
        .enumerate()
        .map(|(i, p)| NsfApuPill {
            short_code: &pill_codes[i],
            active: p.active,
            muted: p.muted,
            volume: p.volume,
        })
        .collect();

    let art_slice = if n.art_rgb24.is_null() {
        None
    } else {
        Some(std::slice::from_raw_parts(n.art_rgb24, 256 * 256 * 3))
    };

    let state = NowPlayingState {
        theme: ThemeColors {
            bg_rgb: n.bg_rgb,
            fg_rgb: n.fg_rgb,
            fg1_rgb: n.fg1_rgb,
            fg2_rgb: n.fg2_rgb,
        },
        header: HeaderState {
            title: &h_title,
            legend: &h_legend,
            play_mode: decode_play_mode(n.header.play_mode),
            battery_percent: n.header.battery_percent,
            is_charging: n.header.is_charging,
            is_hold: n.header.is_hold,
            is_fx: decode_is_fx(n.header.source_icon),
            is_downloading: n.header.is_downloading,
            download_done: n.header.download_done,
            download_total: n.header.download_total,
            source_icon: decode_source_icon(n.header.source_icon),
        },
        track_pos_label: &track_pos,
        eq_enabled: n.eq_enabled,
        title: &title,
        artist: &artist,
        album: &album,
        codec_label: &codec,
        bitdepth_label: &bitdepth,
        samplerate_label: &samplerate,
        progress: n.progress,
        elapsed_label: &elapsed,
        remaining_label: &remaining,
        center_status_label: &center_status,
        art_rgb24: art_slice,
        mono_album_art: n.mono_album_art,
        alt_layout: n.alt_layout,
        is_nsf: n.is_nsf,
        nsf_chip_badge: &nsf_badge,
        nsf_loop_progress: (n.nsf_loop_progress >= 0.0 && n.nsf_loop_progress <= 1.0)
            .then_some(n.nsf_loop_progress),
        nsf_endless_loop: n.nsf_endless_loop,
        nsf_channels: &pills,
        dj_status_label: &dj_status,
        dj_progress: n.dj_progress,
        dj_bass_swap_flash: n.dj_bass_swap_flash,
    };

    wrap.0.render_now_playing_to_fbo_sized(&state, win_w, win_h);
    true
}

#[no_mangle]
pub unsafe extern "C" fn uipod_rs_ui_render_keyboard_gpu(
    kbd: *const UIPodRsKeyboard,
    win_w: u32,
    win_h: u32,
) -> bool {
    if kbd.is_null() {
        return false;
    }
    let mut guard = match SYNC_ENGINE.lock() {
        Ok(g) => g,
        Err(_) => return false,
    };
    let Some(wrap) = guard.as_mut() else {
        return false;
    };

    let k = &*kbd;
    let text = cstr_lossy(k.text);
    let total_keys: usize = k.row_counts.iter().sum();
    let raw_keys = if k.keys.is_null() || total_keys == 0 {
        &[]
    } else {
        std::slice::from_raw_parts(k.keys, total_keys)
    };
    let key_labels: Vec<Cow<str>> = raw_keys.iter().map(|rk| cstr_lossy(rk.label)).collect();

    let mut rows: [Vec<KeyboardKey>; 5] = Default::default();
    let mut offset = 0usize;
    for r_idx in 0..5 {
        let cnt = k.row_counts[r_idx];
        let mut row_vec = Vec::with_capacity(cnt);
        for i in 0..cnt {
            if offset + i < raw_keys.len() {
                let rk = &raw_keys[offset + i];
                row_vec.push(KeyboardKey {
                    label: &key_labels[offset + i],
                    selected: rk.selected,
                    is_modifier: rk.is_modifier,
                    is_active_toggle: rk.is_active_toggle,
                });
            }
        }
        offset += cnt;
        rows[r_idx] = row_vec;
    }

    let state = KeyboardState {
        theme: ThemeColors {
            bg_rgb: k.bg_rgb,
            fg_rgb: k.fg_rgb,
            fg1_rgb: k.fg1_rgb,
            fg2_rgb: k.fg2_rgb,
        },
        text: &text,
        rows,
    };

    wrap.0.render_keyboard_to_fbo_sized(&state, win_w, win_h);
    true
}

#[no_mangle]
pub unsafe extern "C" fn uipod_rs_ui_render_graphic_eq(
    eq: *const UIPodRsGraphicEq,
    fb_bgr: *mut u8,
) -> bool {
    if eq.is_null() || fb_bgr.is_null() {
        return false;
    }
    let mut guard = match SYNC_ENGINE.lock() {
        Ok(g) => g,
        Err(_) => return false,
    };
    let Some(wrap) = guard.as_mut() else {
        return false;
    };

    let e = &*eq;
    let h_title = cstr_lossy(e.header.title);
    let h_legend = cstr_lossy(e.header.legend);
    let state = GraphicEqState {
        theme: ThemeColors {
            bg_rgb: e.bg_rgb,
            fg_rgb: e.fg_rgb,
            fg1_rgb: e.fg1_rgb,
            fg2_rgb: e.fg2_rgb,
        },
        header: HeaderState {
            title: &h_title,
            legend: &h_legend,
            play_mode: decode_play_mode(e.header.play_mode),
            battery_percent: e.header.battery_percent,
            is_charging: e.header.is_charging,
            is_hold: e.header.is_hold,
            is_fx: decode_is_fx(e.header.source_icon),
            is_downloading: e.header.is_downloading,
            download_done: e.header.download_done,
            download_total: e.header.download_total,
            source_icon: decode_source_icon(e.header.source_icon),
        },
        preamp_tenths_db: e.preamp_tenths_db,
        band_gains_tenths_db: e.band_gains_tenths_db,
        selected_col: e.selected_col,
        adjusting: e.adjusting,
        bypassed: e.bypassed,
    };

    let fb_slice = std::slice::from_raw_parts_mut(fb_bgr, WIDTH * HEIGHT * 3);
    wrap.0.render_graphic_eq_into_bgr(&state, fb_slice);
    true
}

#[no_mangle]
pub unsafe extern "C" fn uipod_rs_ui_render_graphic_eq_gpu(
    eq: *const UIPodRsGraphicEq,
    win_w: u32,
    win_h: u32,
) -> bool {
    if eq.is_null() {
        return false;
    }
    let mut guard = match SYNC_ENGINE.lock() {
        Ok(g) => g,
        Err(_) => return false,
    };
    let Some(wrap) = guard.as_mut() else {
        return false;
    };

    let e = &*eq;
    let h_title = cstr_lossy(e.header.title);
    let h_legend = cstr_lossy(e.header.legend);
    let state = GraphicEqState {
        theme: ThemeColors {
            bg_rgb: e.bg_rgb,
            fg_rgb: e.fg_rgb,
            fg1_rgb: e.fg1_rgb,
            fg2_rgb: e.fg2_rgb,
        },
        header: HeaderState {
            title: &h_title,
            legend: &h_legend,
            play_mode: decode_play_mode(e.header.play_mode),
            battery_percent: e.header.battery_percent,
            is_charging: e.header.is_charging,
            is_hold: e.header.is_hold,
            is_fx: decode_is_fx(e.header.source_icon),
            is_downloading: e.header.is_downloading,
            download_done: e.header.download_done,
            download_total: e.header.download_total,
            source_icon: decode_source_icon(e.header.source_icon),
        },
        preamp_tenths_db: e.preamp_tenths_db,
        band_gains_tenths_db: e.band_gains_tenths_db,
        selected_col: e.selected_col,
        adjusting: e.adjusting,
        bypassed: e.bypassed,
    };

    wrap.0.render_graphic_eq_to_fbo_sized(&state, win_w, win_h);
    true
}

#[no_mangle]
pub unsafe extern "C" fn uipod_rs_ui_render_nsf_channels(
    ch_sc: *const UIPodRsNsfChannels,
    fb_bgr: *mut u8,
) -> bool {
    if ch_sc.is_null() || fb_bgr.is_null() {
        return false;
    }
    let mut guard = match SYNC_ENGINE.lock() {
        Ok(g) => g,
        Err(_) => return false,
    };
    let Some(wrap) = guard.as_mut() else {
        return false;
    };

    let c = &*ch_sc;
    let h_title = cstr_lossy(c.header.title);
    let h_legend = cstr_lossy(c.header.legend);
    let enable_all = cstr_lossy(c.enable_all_label);
    let footer = cstr_lossy(c.footer_hint);

    let raw_ch = if c.channels.is_null() || c.channel_count == 0 {
        &[]
    } else {
        std::slice::from_raw_parts(c.channels, c.channel_count.min(16))
    };
    let codes: Vec<Cow<str>> = raw_ch.iter().map(|r| cstr_lossy(r.short_code)).collect();
    let names: Vec<Cow<str>> = raw_ch.iter().map(|r| cstr_lossy(r.display_name)).collect();
    let rows: Vec<NsfChannelRow> = raw_ch
        .iter()
        .enumerate()
        .map(|(i, r)| NsfChannelRow {
            short_code: &codes[i],
            display_name: &names[i],
            enabled: r.enabled,
            soloed: r.soloed,
            active: r.active,
            volume: r.volume,
            selected: r.selected,
        })
        .collect();

    let state = NsfChannelsState {
        theme: ThemeColors {
            bg_rgb: c.bg_rgb,
            fg_rgb: c.fg_rgb,
            fg1_rgb: c.fg1_rgb,
            fg2_rgb: c.fg2_rgb,
        },
        header: HeaderState {
            title: &h_title,
            legend: &h_legend,
            play_mode: decode_play_mode(c.header.play_mode),
            battery_percent: c.header.battery_percent,
            is_charging: c.header.is_charging,
            is_hold: c.header.is_hold,
            is_fx: decode_is_fx(c.header.source_icon),
            is_downloading: c.header.is_downloading,
            download_done: c.header.download_done,
            download_total: c.header.download_total,
            source_icon: decode_source_icon(c.header.source_icon),
        },
        channels: &rows,
        enable_all_label: &enable_all,
        enable_all_selected: c.enable_all_selected,
        footer_hint: &footer,
        start_item: c.start_item,
        visible_items: c.visible_items,
    };

    let fb_slice = std::slice::from_raw_parts_mut(fb_bgr, WIDTH * HEIGHT * 3);
    wrap.0.render_nsf_channels_into_bgr(&state, fb_slice);
    true
}

#[no_mangle]
pub unsafe extern "C" fn uipod_rs_ui_render_nsf_channels_gpu(
    ch_sc: *const UIPodRsNsfChannels,
    win_w: u32,
    win_h: u32,
) -> bool {
    if ch_sc.is_null() {
        return false;
    }
    let mut guard = match SYNC_ENGINE.lock() {
        Ok(g) => g,
        Err(_) => return false,
    };
    let Some(wrap) = guard.as_mut() else {
        return false;
    };

    let c = &*ch_sc;
    let h_title = cstr_lossy(c.header.title);
    let h_legend = cstr_lossy(c.header.legend);
    let enable_all = cstr_lossy(c.enable_all_label);
    let footer = cstr_lossy(c.footer_hint);

    let raw_ch = if c.channels.is_null() || c.channel_count == 0 {
        &[]
    } else {
        std::slice::from_raw_parts(c.channels, c.channel_count.min(16))
    };
    let codes: Vec<Cow<str>> = raw_ch.iter().map(|r| cstr_lossy(r.short_code)).collect();
    let names: Vec<Cow<str>> = raw_ch.iter().map(|r| cstr_lossy(r.display_name)).collect();
    let rows: Vec<NsfChannelRow> = raw_ch
        .iter()
        .enumerate()
        .map(|(i, r)| NsfChannelRow {
            short_code: &codes[i],
            display_name: &names[i],
            enabled: r.enabled,
            soloed: r.soloed,
            active: r.active,
            volume: r.volume,
            selected: r.selected,
        })
        .collect();

    let state = NsfChannelsState {
        theme: ThemeColors {
            bg_rgb: c.bg_rgb,
            fg_rgb: c.fg_rgb,
            fg1_rgb: c.fg1_rgb,
            fg2_rgb: c.fg2_rgb,
        },
        header: HeaderState {
            title: &h_title,
            legend: &h_legend,
            play_mode: decode_play_mode(c.header.play_mode),
            battery_percent: c.header.battery_percent,
            is_charging: c.header.is_charging,
            is_hold: c.header.is_hold,
            is_fx: decode_is_fx(c.header.source_icon),
            is_downloading: c.header.is_downloading,
            download_done: c.header.download_done,
            download_total: c.header.download_total,
            source_icon: decode_source_icon(c.header.source_icon),
        },
        channels: &rows,
        enable_all_label: &enable_all,
        enable_all_selected: c.enable_all_selected,
        footer_hint: &footer,
        start_item: c.start_item,
        visible_items: c.visible_items,
    };

    wrap.0.render_nsf_channels_to_fbo_sized(&state, win_w, win_h);
    true
}

#[no_mangle]
pub extern "C" fn uipod_rs_ui_snapshot_transition() -> bool {
    let mut guard = match SYNC_ENGINE.lock() {
        Ok(g) => g,
        Err(_) => return false,
    };
    let Some(wrap) = guard.as_mut() else {
        return false;
    };
    wrap.0.snapshot_fbo_for_transition();
    true
}

#[no_mangle]
pub extern "C" fn uipod_rs_ui_present_transition_gpu(
    win_w: u32,
    win_h: u32,
    kind: u8,
    progress: f32,
    dir: i8,
    pin_header: bool,
) -> bool {
    let mut guard = match SYNC_ENGINE.lock() {
        Ok(g) => g,
        Err(_) => return false,
    };
    let Some(wrap) = guard.as_mut() else {
        return false;
    };
    wrap.0
        .present_fbo_transition_ex(win_w, win_h, kind, progress, dir, pin_header);
    true
}

#[no_mangle]
pub extern "C" fn uipod_rs_ui_present_fade_rgb_gpu(
    win_w: u32,
    win_h: u32,
    fade: f32,
    target_rgb: u32,
) -> bool {
    let mut guard = match SYNC_ENGINE.lock() {
        Ok(g) => g,
        Err(_) => return false,
    };
    let Some(wrap) = guard.as_mut() else {
        return false;
    };
    wrap.0.present_fbo_fade_rgb(win_w, win_h, fade, target_rgb);
    true
}

#[no_mangle]
pub unsafe extern "C" fn uipod_rs_ui_plugin_begin_ex(
    bg_rgb: u32,
    fg_rgb: u32,
    fg1_rgb: u32,
    fg2_rgb: u32,
    clear_rgb: u32,
    header: *const UIPodRsHeader,
    win_w: u32,
    win_h: u32,
    responsive: bool,
    out_logical_w: *mut f32,
    out_logical_h: *mut f32,
) -> bool {
    let mut guard = match SYNC_ENGINE.lock() {
        Ok(g) => g,
        Err(_) => return false,
    };
    let Some(wrap) = guard.as_mut() else {
        return false;
    };

    let theme = ThemeColors {
        bg_rgb,
        fg_rgb,
        fg1_rgb,
        fg2_rgb,
    };

    let (lw, lh) = if header.is_null() {
        wrap.0
            .plugin_begin_ex(theme, clear_rgb, None, win_w, win_h, responsive)
    } else {
        let h = &*header;
        let title = cstr_lossy(h.title);
        let legend = cstr_lossy(h.legend);
        let hs = HeaderState {
            title: &title,
            legend: &legend,
            play_mode: decode_play_mode(h.play_mode),
            battery_percent: h.battery_percent,
            is_charging: h.is_charging,
            is_hold: h.is_hold,
            is_fx: decode_is_fx(h.source_icon),
            is_downloading: h.is_downloading,
            download_done: h.download_done,
            download_total: h.download_total,
            source_icon: decode_source_icon(h.source_icon),
        };
        wrap.0
            .plugin_begin_ex(theme, clear_rgb, Some(&hs), win_w, win_h, responsive)
    };

    if !out_logical_w.is_null() {
        *out_logical_w = lw;
    }
    if !out_logical_h.is_null() {
        *out_logical_h = lh;
    }
    true
}

// ---- Unified Stacked HUD System C ABI (`uipod_rs_hud_*`) --------------------

#[no_mangle]
pub extern "C" fn uipod_rs_hud_volume(level_norm: f32, now_ms: u64) {
    if let Ok(mut guard) = SYNC_ENGINE.lock() {
        if let Some(wrap) = guard.as_mut() {
            wrap.0.hud_volume(level_norm, now_ms);
        }
    }
}

#[no_mangle]
pub unsafe extern "C" fn uipod_rs_hud_toast(
    key: *const c_char,
    message: *const c_char,
    hold_ms: u64,
    now_ms: u64,
) {
    if message.is_null() {
        return;
    }
    let msg = cstr_lossy(message);
    if msg.trim().is_empty() {
        return;
    }
    let k = cstr_lossy(key);
    if let Ok(mut guard) = SYNC_ENGINE.lock() {
        if let Some(wrap) = guard.as_mut() {
            wrap.0.hud_toast(&k, &msg, hold_ms, now_ms);
        }
    }
}

#[no_mangle]
pub unsafe extern "C" fn uipod_rs_hud_progress_begin(
    title: *const c_char,
    detail: *const c_char,
    progress: f32,
    now_ms: u64,
) -> u32 {
    let t = cstr_lossy(title);
    let d = cstr_lossy(detail);
    let prog = (progress >= 0.0).then_some(progress);
    if let Ok(mut guard) = SYNC_ENGINE.lock() {
        if let Some(wrap) = guard.as_mut() {
            return wrap.0.hud_progress_begin(&t, &d, prog, now_ms);
        }
    }
    0
}

#[no_mangle]
pub unsafe extern "C" fn uipod_rs_hud_progress_update(
    handle: u32,
    title: *const c_char,
    detail: *const c_char,
    progress: f32,
    now_ms: u64,
) -> bool {
    if handle == 0 {
        return false;
    }
    let t = cstr_lossy(title);
    let d = cstr_lossy(detail);
    let prog = (progress >= 0.0).then_some(progress);
    if let Ok(mut guard) = SYNC_ENGINE.lock() {
        if let Some(wrap) = guard.as_mut() {
            return wrap.0.hud_progress_update(handle, &t, &d, prog, now_ms);
        }
    }
    false
}

#[no_mangle]
pub extern "C" fn uipod_rs_hud_progress_end(handle: u32, now_ms: u64) -> bool {
    if handle == 0 {
        return false;
    }
    if let Ok(mut guard) = SYNC_ENGINE.lock() {
        if let Some(wrap) = guard.as_mut() {
            return wrap.0.hud_progress_end(handle, now_ms);
        }
    }
    false
}

#[no_mangle]
pub unsafe extern "C" fn uipod_rs_hud_confirm_push(
    question: *const c_char,
    detail: *const c_char,
    confirm_label: *const c_char,
    cancel_label: *const c_char,
    now_ms: u64,
) -> u32 {
    let q = cstr_lossy(question);
    let d = cstr_lossy(detail);
    let a = cstr_lossy(confirm_label);
    let b = cstr_lossy(cancel_label);
    if let Ok(mut guard) = SYNC_ENGINE.lock() {
        if let Some(wrap) = guard.as_mut() {
            return wrap.0.hud_confirm_push(&q, &d, &a, &b, now_ms);
        }
    }
    0
}

#[no_mangle]
pub extern "C" fn uipod_rs_hud_confirm_resolve(id: u32, now_ms: u64) -> bool {
    if id == 0 {
        return false;
    }
    if let Ok(mut guard) = SYNC_ENGINE.lock() {
        if let Some(wrap) = guard.as_mut() {
            return wrap.0.hud_confirm_resolve(id, now_ms);
        }
    }
    false
}

#[no_mangle]
pub extern "C" fn uipod_rs_hud_has_confirm() -> bool {
    if let Ok(guard) = SYNC_ENGINE.lock() {
        if let Some(wrap) = guard.as_ref() {
            return wrap.0.hud_has_active_confirm();
        }
    }
    false
}

#[no_mangle]
pub extern "C" fn uipod_rs_hud_set_clock(now_ms: u64) {
    if let Ok(mut guard) = SYNC_ENGINE.lock() {
        if let Some(wrap) = guard.as_mut() {
            wrap.0.hud_set_clock(now_ms);
        }
    }
}

#[no_mangle]
pub extern "C" fn uipod_rs_hud_needs_tick(now_ms: u64) -> bool {
    if let Ok(mut guard) = SYNC_ENGINE.lock() {
        if let Some(wrap) = guard.as_mut() {
            return wrap.0.hud_needs_tick(now_ms);
        }
    }
    false
}

#[no_mangle]
pub extern "C" fn uipod_rs_hud_is_visible(now_ms: u64) -> bool {
    if let Ok(mut guard) = SYNC_ENGINE.lock() {
        if let Some(wrap) = guard.as_mut() {
            return wrap.0.hud_is_visible(now_ms);
        }
    }
    false
}

#[no_mangle]
pub extern "C" fn uipod_rs_hud_render_over_screen(
    win_w: u32,
    win_h: u32,
    bg_rgb: u32,
    fg_rgb: u32,
    fg1_rgb: u32,
    fg2_rgb: u32,
    now_ms: u64,
) -> bool {
    let mut guard = match SYNC_ENGINE.lock() {
        Ok(g) => g,
        Err(_) => return false,
    };
    let Some(wrap) = guard.as_mut() else {
        return false;
    };
    wrap.0.render_volume_popup_over_screen(
        win_w,
        win_h,
        ThemeColors {
            bg_rgb,
            fg_rgb,
            fg1_rgb,
            fg2_rgb,
        },
        now_ms,
    )
}

#[no_mangle]
pub extern "C" fn uipod_rs_ui_trigger_volume_popup(level_norm: f32, now_ms: u64) {
    uipod_rs_hud_volume(level_norm, now_ms);
}

#[no_mangle]
pub unsafe extern "C" fn uipod_rs_ui_trigger_hud_toast(message: *const c_char, now_ms: u64) {
    uipod_rs_hud_toast(std::ptr::null(), message, 0, now_ms);
}

#[no_mangle]
pub extern "C" fn uipod_rs_ui_set_volume_popup_clock(now_ms: u64) {
    uipod_rs_hud_set_clock(now_ms);
}

#[no_mangle]
pub extern "C" fn uipod_rs_ui_is_volume_popup_animating(now_ms: u64) -> bool {
    uipod_rs_hud_needs_tick(now_ms)
}

#[no_mangle]
pub extern "C" fn uipod_rs_ui_render_volume_popup_over_screen(
    win_w: u32,
    win_h: u32,
    bg_rgb: u32,
    fg_rgb: u32,
    fg1_rgb: u32,
    fg2_rgb: u32,
    now_ms: u64,
) -> bool {
    uipod_rs_hud_render_over_screen(win_w, win_h, bg_rgb, fg_rgb, fg1_rgb, fg2_rgb, now_ms)
}

// ---- 4-Shade Audio-Reactive Visualizer C ABI (`uipod_rs_viz_*`) ------------

unsafe fn write_cstr_buf(s: &str, out: *mut c_char, cap: usize) -> usize {
    if out.is_null() || cap == 0 {
        return 0;
    }
    let bytes = s.as_bytes();
    let n = bytes.len().min(cap - 1);
    std::ptr::copy_nonoverlapping(bytes.as_ptr() as *const c_char, out, n);
    *out.add(n) = 0;
    n
}

#[no_mangle]
pub extern "C" fn uipod_rs_viz_preset_count() -> usize {
    VIZ_PRESETS.len()
}

#[no_mangle]
pub unsafe extern "C" fn uipod_rs_viz_preset_id(
    idx: usize,
    out: *mut c_char,
    cap: usize,
) -> usize {
    let Some(p) = VIZ_PRESETS.get(idx) else {
        if !out.is_null() && cap > 0 {
            *out = 0;
        }
        return 0;
    };
    write_cstr_buf(p.id, out, cap)
}

#[no_mangle]
pub unsafe extern "C" fn uipod_rs_viz_preset_name(
    idx: usize,
    is_ukrainian: bool,
    out: *mut c_char,
    cap: usize,
) -> usize {
    let Some(p) = VIZ_PRESETS.get(idx) else {
        if !out.is_null() && cap > 0 {
            *out = 0;
        }
        return 0;
    };
    let label = if is_ukrainian { p.name_uk } else { p.name_en };
    write_cstr_buf(label, out, cap)
}

#[no_mangle]
pub unsafe extern "C" fn uipod_rs_viz_init(get_proc: Option<UIPodRsGlGetProc>) -> bool {
    let Some(loader) = get_proc else {
        return false;
    };
    let mut guard = match SYNC_VIZ.lock() {
        Ok(g) => g,
        Err(_) => return false,
    };
    if guard.is_some() {
        return true;
    }
    match VizEngine::new(|cname| loader(cname.as_ptr())) {
        Ok(engine) => {
            *guard = Some(SyncVizWrap(engine));
            true
        }
        Err(err) => {
            eprintln!("[uipod-viz] init error: {err}");
            false
        }
    }
}

#[no_mangle]
pub extern "C" fn uipod_rs_viz_set_preset(idx: usize) {
    if let Ok(mut guard) = SYNC_VIZ.lock() {
        if let Some(wrap) = guard.as_mut() {
            wrap.0.set_preset(idx);
        }
    }
}

#[no_mangle]
pub extern "C" fn uipod_rs_viz_toggle_grain() -> bool {
    true
}

#[no_mangle]
pub extern "C" fn uipod_rs_viz_set_invert(invert: bool) {
    if let Ok(mut guard) = SYNC_VIZ.lock() {
        if let Some(wrap) = guard.as_mut() {
            wrap.0.set_invert(invert);
        }
    }
}

#[no_mangle]
pub unsafe extern "C" fn uipod_rs_viz_render_frame(
    pcm_interleaved: *const i16,
    frames: usize,
    sample_rate: u32,
    dt: f32,
    win_w: i32,
    win_h: i32,
    bg_rgb: u32,
    fg_rgb: u32,
    fg1_rgb: u32,
    fg2_rgb: u32,
    fade: f32,
    track_progress: f32,
    transport_speed: f32,
) -> bool {
    let mut guard = match SYNC_VIZ.lock() {
        Ok(g) => g,
        Err(_) => return false,
    };
    let Some(wrap) = guard.as_mut() else {
        return false;
    };
    let pcm_slice: &[i16] = if pcm_interleaved.is_null() || frames == 0 {
        &[]
    } else {
        std::slice::from_raw_parts(pcm_interleaved, frames.saturating_mul(2))
    };
    let theme = ThemeColors {
        bg_rgb,
        fg_rgb,
        fg1_rgb,
        fg2_rgb,
    };
    wrap.0.render_frame(
        pcm_slice,
        sample_rate,
        dt,
        win_w,
        win_h,
        theme,
        fade,
        track_progress,
        transport_speed,
    );
    true
}

// ---- Pure-Rust Album Art Image Decoder C ABI (`uipod_rs_image_*`) ----------

#[no_mangle]
pub unsafe extern "C" fn uipod_rs_image_decode_rgb(
    bytes: *const u8,
    len: usize,
    out_w: *mut i32,
    out_h: *mut i32,
    out_byte_len: *mut usize,
) -> *mut u8 {
    if bytes.is_null() || len < 16 || out_w.is_null() || out_h.is_null() || out_byte_len.is_null() {
        return std::ptr::null_mut();
    }
    let slice = std::slice::from_raw_parts(bytes, len);
    let Some((w, h, mut rgb)) = crate::image_decode::decode_image_rgb24(slice) else {
        return std::ptr::null_mut();
    };
    let expected = (w as usize).saturating_mul(h as usize).saturating_mul(3);
    if expected == 0 || rgb.len() < expected {
        return std::ptr::null_mut();
    }
    rgb.truncate(expected);
    let boxed: Box<[u8]> = rgb.into_boxed_slice();
    let byte_len = boxed.len();
    let raw_ptr = Box::into_raw(boxed) as *mut u8;
    *out_w = w as i32;
    *out_h = h as i32;
    *out_byte_len = byte_len;
    raw_ptr
}

#[no_mangle]
pub unsafe extern "C" fn uipod_rs_image_free_rgb(ptr: *mut u8, byte_len: usize) {
    if !ptr.is_null() && byte_len > 0 {
        let slice_ptr = std::ptr::slice_from_raw_parts_mut(ptr, byte_len);
        drop(Box::from_raw(slice_ptr));
    }
}
