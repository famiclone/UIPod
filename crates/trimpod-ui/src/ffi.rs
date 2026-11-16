//! Thin C ABI (`extern "C"`) over `UiEngine` for the Strangler-Fig migration stage.
//!
//! Contains zero `#include` / bindgen dependencies on Rockbox internals.

use std::borrow::Cow;
use std::ffi::{c_char, c_void, CStr};
use std::path::Path;
use std::sync::Mutex;

use crate::renderer::{HEADER_BAND_ROWS, HEIGHT, WIDTH};
use crate::{
    FileIconKind, HeaderState, ListRow, ListScreenState, NowPlayingState, PlayMode,
    RightAccessory, ThemeColors, UiEngine,
};

// Safety: UiEngine is accessed exclusively from the serialized Rockbox thread holding the SDL GL context.
unsafe impl Send for SyncEngineWrap {}
struct SyncEngineWrap(UiEngine);

static SYNC_ENGINE: Mutex<Option<SyncEngineWrap>> = Mutex::new(None);

pub type TrimpodRsGlGetProc = unsafe extern "C" fn(*const c_char) -> *const c_void;

#[repr(C)]
#[derive(Clone, Copy)]
pub struct TrimpodRsHeader {
    pub title: *const c_char,
    pub legend: *const c_char,
    pub play_mode: u32,
    pub battery_percent: u8,
    pub is_charging: bool,
    pub is_hold: bool,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct TrimpodRsListRow {
    pub label: *const c_char,
    pub right_value: *const c_char,
    pub file_tag: *const c_char,
    pub selected: bool,
    /// `0 = None`, `1 = Chevron`, `2 = Toggle(false)`, `3 = Toggle(true)`, `4 = Value(right_value)`
    pub right_kind: u8,
}

#[repr(C)]
pub struct TrimpodRsListScreen {
    pub bg_rgb: u32,
    pub fg_rgb: u32,
    pub header: TrimpodRsHeader,
    pub rows: *const TrimpodRsListRow,
    pub row_count: usize,
    pub total_items: usize,
    pub start_item: usize,
    pub visible_items: usize,
}

#[repr(C)]
pub struct TrimpodRsNowPlaying {
    pub bg_rgb: u32,
    pub fg_rgb: u32,
    pub header: TrimpodRsHeader,
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

/// Initializes the global Rust GPU UI engine on the active OpenGL ES 3.0 context.
#[no_mangle]
pub unsafe extern "C" fn trimpod_rs_ui_init(
    get_proc: Option<TrimpodRsGlGetProc>,
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
            eprintln!("[trimpod-ui] init error: {err}");
            false
        }
    }
}

/// Returns `true` if `trimpod_rs_ui_init` has succeeded.
#[no_mangle]
pub extern "C" fn trimpod_rs_ui_is_ready() -> bool {
    SYNC_ENGINE.lock().map(|g| g.is_some()).unwrap_or(false)
}

/// Releases GPU resources owned by the Rust UI engine.
#[no_mangle]
pub extern "C" fn trimpod_rs_ui_shutdown() {
    if let Ok(mut guard) = SYNC_ENGINE.lock() {
        *guard = None;
    }
}

/// Renders only the top `Header` band (`1024 x 124`) into `fb_bgr` (`{b, g, r}`).
#[no_mangle]
pub unsafe extern "C" fn trimpod_rs_ui_render_header(
    bg_rgb: u32,
    fg_rgb: u32,
    header: *const TrimpodRsHeader,
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
    };

    let fb_slice = std::slice::from_raw_parts_mut(fb_bgr, WIDTH * HEADER_BAND_ROWS * 3);
    wrap.0.render_header_into_bgr(ThemeColors { bg_rgb, fg_rgb }, &hs, fb_slice);
    true
}

/// Renders a full `1024 x 768` list/menu screen into `fb_bgr` (`{b, g, r}`).
#[no_mangle]
pub unsafe extern "C" fn trimpod_rs_ui_render_list(
    screen: *const TrimpodRsListScreen,
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
            4 => RightAccessory::Value(&rvals[i]),
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
        },
        header: HeaderState {
            title: &title,
            legend: &legend,
            play_mode: decode_play_mode(sc.header.play_mode),
            battery_percent: sc.header.battery_percent,
            is_charging: sc.header.is_charging,
            is_hold: sc.header.is_hold,
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
pub unsafe extern "C" fn trimpod_rs_ui_render_now_playing(
    np: *const TrimpodRsNowPlaying,
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
        },
        header: HeaderState {
            title: &h_title,
            legend: &h_legend,
            play_mode: decode_play_mode(n.header.play_mode),
            battery_percent: n.header.battery_percent,
            is_charging: n.header.is_charging,
            is_hold: n.header.is_hold,
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
    };

    let fb_slice = std::slice::from_raw_parts_mut(fb_bgr, WIDTH * HEIGHT * 3);
    wrap.0.render_now_playing_into_bgr(&state, fb_slice);
    true
}
