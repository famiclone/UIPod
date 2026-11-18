//! Thin C ABI (`extern "C"`) over `UiEngine` for the Strangler-Fig migration stage.
//!
//! Contains zero `#include` / bindgen dependencies on Rockbox internals.

use std::borrow::Cow;
use std::ffi::{c_char, c_void, CStr};
use std::path::Path;
use std::sync::Mutex;

use crate::renderer::{HEADER_BAND_ROWS, HEIGHT, WIDTH};
use crate::viz_gl::{VizEngine, VIZ_PRESETS};
use crate::{
    FileIconKind, HeaderSourceIcon, HeaderState, KeyboardKey, KeyboardState, ListRow,
    ListScreenState, NowPlayingState, PlayMode, RightAccessory, ThemeColors, UiEngine,
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
    /// `0=None`, `1=MicroSd`, `2=Navidrome`, `3=Jellyfin`
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
    match raw {
        1 => HeaderSourceIcon::MicroSd,
        2 => HeaderSourceIcon::Navidrome,
        3 => HeaderSourceIcon::Jellyfin,
        _ => HeaderSourceIcon::None,
    }
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
    if let Ok(mut guard) = SYNC_VIZ.lock() {
        if let Some(wrap) = guard.as_mut() {
            return wrap.0.toggle_grain();
        }
    }
    false
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
    wrap.0
        .render_frame(pcm_slice, sample_rate, dt, win_w, win_h, theme, fade);
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
