//! Official pure-Rust SDK and versioned C-ABI for UIPod native plugins (`.uipod` / `.so`).
//!
//! Modelled directly on Rockbox's `.rock` + `struct plugin_api*` architecture, upgraded
//! for UIPod's 1024x768 OpenGL ES 3.0 GPU vector canvas, `Geologica` typography,
//! 4-shade iPod Mini theme palette (`bg`, `fg`, `fg1`, `fg2`), and J2ME-style native
//! system menus (`PluginKind::Utility` vs `PluginKind::MenuDriven`).

use std::ffi::{c_char, CStr, CString};
use std::fs;
use std::path::{Path, PathBuf};

/// Magic identifier `"UIPP"` (`0x5549_5050`) stored in [`UIPodPluginDescriptor::magic`].
pub const UIPOD_PLUGIN_MAGIC: u32 = 0x5549_5050;

/// Host/Plugin ABI version checked by the loader before invoking `run`.
pub const UIPOD_PLUGIN_API_VERSION: u32 = 6;

/// Symbol name exported by every `.uipod` shared library.
pub const UIPOD_PLUGIN_ENTRY_SYMBOL: &[u8] = b"uipod_plugin_descriptor\0";

pub const SCREEN_WIDTH: usize = 1024;
pub const SCREEN_HEIGHT: usize = 768;
/// Y coordinate where usable content begins when `show_header == true` (`Header` ends at `y = 116`).
pub const CONTENT_TOP_Y: f32 = 136.0;

/// Plugin category matching Rockbox's `Games` / `Apps` / `Demos` / `Viewers` split.
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PluginCategory {
    Game = 0,
    App = 1,
    Demo = 2,
    Viewer = 3,
}

impl PluginCategory {
    pub const fn from_u32(v: u32) -> Self {
        match v {
            0 => Self::Game,
            1 => Self::App,
            2 => Self::Demo,
            _ => Self::Viewer,
        }
    }
}

/// Plugin lifecycle & exit model:
/// - [`PluginKind::Utility`]: lightweight utility/demo (e.g. Stopwatch, LCD Plasma) closed directly via `B`.
/// - [`PluginKind::MenuDriven`]: J2ME-style full-featured game/viewer where `B` remains an ordinary
///   in-plugin button and `MENU` / `START` opens the native UIPod System Menu (`New Game`, `Save Game`,
///   `Options`, `Exit`).
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PluginKind {
    Utility = 0,
    MenuDriven = 1,
}

impl PluginKind {
    pub const fn from_u32(v: u32) -> Self {
        match v {
            1 => Self::MenuDriven,
            _ => Self::Utility,
        }
    }
}

/// Hardware button bitmasks on TrimUI Brick.
pub mod btn {
    pub const A: u32 = 0x0000_0001;
    pub const B: u32 = 0x0000_0002;
    pub const X: u32 = 0x0000_0004;
    pub const Y: u32 = 0x0000_0008;
    pub const UP: u32 = 0x0000_0010;
    pub const DOWN: u32 = 0x0000_0020;
    pub const LEFT: u32 = 0x0000_0040;
    pub const RIGHT: u32 = 0x0000_0080;
    pub const START: u32 = 0x0000_0100;
    pub const SELECT: u32 = 0x0000_0200;
    pub const L: u32 = 0x0000_0400;
    pub const R: u32 = 0x0000_0800;
    pub const L2: u32 = 0x0000_1000;
    pub const R2: u32 = 0x0000_2000;
    pub const MENU: u32 = 0x0001_0000;
}

/// `Geologica` font weight selector for GPU canvas text rendering.
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FontWeight {
    Regular = 0,
    Medium = 1,
    SemiBold = 2,
    Bold = 3,
}

/// Horizontal text alignment for [`HostContext::draw_text`].
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TextAlign {
    Left = 0,
    Center = 1,
    Right = 2,
}

/// Active 4-shade iPod Mini theme palette (`0xRRGGBB`).
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PluginTheme {
    pub bg: u32,
    pub fg: u32,
    pub fg1: u32,
    pub fg2: u32,
}

/// Snapshot of button edges, held buttons, and system requests from a single poll step.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct PluginInput {
    /// Bitmask of buttons newly pressed (or key-repeated) during this poll.
    pub pressed: u32,
    /// Real-time bitmask of buttons currently held down (`button_status()`).
    pub held: u32,
    /// `true` if the user held `B` for Home (in `Utility` plugins), connected USB, or triggered poweroff.
    pub should_exit: bool,
    /// `true` if the user pressed `MENU` or `START` requesting the J2ME-style system menu.
    pub menu_requested: bool,
}

impl PluginInput {
    #[inline]
    pub const fn pressed(&self, mask: u32) -> bool {
        (self.pressed & mask) != 0
    }

    #[inline]
    pub const fn held(&self, mask: u32) -> bool {
        (self.held & mask) != 0
    }
}

/// C-ABI descriptor for a single row in the native UIPod System Menu (`HostContext::show_menu`).
#[repr(C)]
pub struct UIPodPluginMenuItem {
    pub label: *const c_char,
    pub value: *const c_char,
    pub has_chevron: bool,
}

/// High-level Rust builder for a row in a J2ME-style plugin system menu.
#[derive(Debug, Clone, Copy)]
pub struct MenuItem<'a> {
    pub label: &'a str,
    pub value: Option<&'a str>,
    pub has_chevron: bool,
}

impl<'a> MenuItem<'a> {
    /// Plain command row (e.g. `"Continue"`, `"New Game"`, `"Save Game"`, `"Exit"`).
    #[inline]
    pub const fn action(label: &'a str) -> Self {
        Self {
            label,
            value: None,
            has_chevron: false,
        }
    }

    /// Setting/choice row showing its current value on the right (e.g. `"Ghost Piece"` -> `"On"`).
    #[inline]
    pub const fn option(label: &'a str, value: &'a str) -> Self {
        Self {
            label,
            value: Some(value),
            has_chevron: false,
        }
    }

    /// Submenu row with a right-side chevron `>` (e.g. `"Options"`).
    #[inline]
    pub const fn submenu(label: &'a str) -> Self {
        Self {
            label,
            value: None,
            has_chevron: true,
        }
    }
}

/// Result returned when the native UIPod System Menu closes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MenuOutcome {
    /// User activated row `index` with `A`.
    Selected(usize),
    /// User dismissed the menu with `B` or `MENU` to return to the plugin.
    Cancelled,
    /// System interrupt (Hold-`B` Home inside menu, USB connected, or poweroff) — save & exit immediately.
    ForceExit,
}

/// Function-pointer table passed from the UIPod host into every running plugin
/// (the direct Rust equivalent of Rockbox's `struct plugin_api`).
#[repr(C)]
pub struct UIPodPluginApi {
    pub api_version: u32,

    /* ---- Timing, Localization & Persistent Storage ---- */
    pub get_ticks_ms: extern "C" fn() -> u64,
    pub sleep_ms: extern "C" fn(ms: u32),
    /// `0 = English`, `1 = Ukrainian`
    pub get_language: extern "C" fn() -> u32,
    pub get_data_dir: unsafe extern "C" fn(out: *mut c_char, cap: usize) -> usize,

    /* ---- 4-Color Theme & GPU Vector 2D Canvas (`1024x768`) ---- */
    pub get_theme: extern "C" fn() -> PluginTheme,
    pub canvas_begin: unsafe extern "C" fn(bg_rgb: u32, show_header: bool, title: *const c_char),
    pub canvas_fill_rect:
        extern "C" fn(x: f32, y: f32, w: f32, h: f32, radius: f32, rgb: u32),
    pub canvas_stroke_rect:
        extern "C" fn(x: f32, y: f32, w: f32, h: f32, radius: f32, stroke_w: f32, rgb: u32),
    pub canvas_fill_circle: extern "C" fn(cx: f32, cy: f32, r: f32, rgb: u32),
    pub canvas_draw_line:
        extern "C" fn(x0: f32, y0: f32, x1: f32, y1: f32, stroke_w: f32, rgb: u32),
    pub canvas_draw_text: unsafe extern "C" fn(
        x: f32,
        y: f32,
        text: *const c_char,
        font_size: f32,
        weight: u8,
        align: u8,
        rgb: u32,
    ) -> f32,
    pub canvas_measure_text:
        unsafe extern "C" fn(text: *const c_char, font_size: f32, weight: u8) -> f32,
    pub canvas_draw_progress_bar:
        extern "C" fn(x: f32, y: f32, w: f32, h: f32, progress: f32),
    pub canvas_draw_scrollbar: extern "C" fn(
        x: f32,
        y: f32,
        w: f32,
        h: f32,
        total: usize,
        start: usize,
        visible: usize,
    ),
    pub canvas_present: extern "C" fn(),

    /* ---- Direct Raw 24-bit `{B, G, R}` Framebuffer (`1024 * 768 * 3`) ---- */
    pub fb_ptr: extern "C" fn() -> *mut u8,
    pub fb_present: extern "C" fn(),

    /* ---- Input, Native J2ME-Style System Menu, Keyboard & Dialogs ---- */
    pub poll_input: extern "C" fn(timeout_ms: u32) -> PluginInput,
    pub show_menu: unsafe extern "C" fn(
        title: *const c_char,
        items: *const UIPodPluginMenuItem,
        count: usize,
        initial_sel: usize,
    ) -> i32,
    pub keyboard_input:
        unsafe extern "C" fn(buf: *mut c_char, cap: usize, allow_symbols: bool) -> bool,
    pub show_splash: unsafe extern "C" fn(message: *const c_char, duration_ms: u32),

    /* ---- Background Audio Playback & Metadata ---- */
    /// `0 = Stopped`, `1 = Playing`, `2 = Paused`
    pub audio_status: extern "C" fn() -> u32,
    pub audio_toggle_play_pause: extern "C" fn(),
    pub audio_next: extern "C" fn(),
    pub audio_prev: extern "C" fn(),
    pub audio_seek_ms: extern "C" fn(pos_ms: u32),
    pub audio_current_track: unsafe extern "C" fn(
        path_out: *mut c_char,
        path_cap: usize,
        title_out: *mut c_char,
        title_cap: usize,
        artist_out: *mut c_char,
        artist_cap: usize,
        pos_ms_out: *mut u32,
        len_ms_out: *mut u32,
    ) -> bool,
    /// Reads up to `max_frames` interleaved S16 stereo PCM frames (`L, R` = `2 * i16` per frame):
    /// - `ahead_ms == 0`: live pre-volume PCM currently playing at the ALSA sink.
    /// - `ahead_ms > 0`: lookahead PCM `ahead_ms` ahead in Rockbox's decoded `pcmbuf`
    ///   (compensated for hardware ALSA output latency).
    pub audio_read_pcm:
        unsafe extern "C" fn(ahead_ms: u32, out_stereo: *mut i16, max_frames: usize) -> usize,
    pub audio_play_track: unsafe extern "C" fn(path: *const c_char, start_ms: u32) -> bool,
    pub audio_stop: extern "C" fn(),

    /* ---- Unified Music Library Access ---- */
    pub library_track_count: extern "C" fn() -> usize,
    pub library_track_at: unsafe extern "C" fn(
        index: usize,
        path_out: *mut c_char,
        path_cap: usize,
        artist_out: *mut c_char,
        artist_cap: usize,
        title_out: *mut c_char,
        title_cap: usize,
        duration_ms_out: *mut u32,
    ) -> bool,

    /* ---- Responsive Canvas Extension (v6+) ---- */
    pub canvas_begin_responsive: unsafe extern "C" fn(
        bg_rgb: u32,
        show_header: bool,
        title: *const c_char,
        out_w: *mut f32,
        out_h: *mut f32,
    ),
    pub get_responsive_size: unsafe extern "C" fn(out_w: *mut f32, out_h: *mut f32),
}

/// Static plugin metadata & entry-point descriptor exported by every `.uipod` library.
#[repr(C)]
pub struct UIPodPluginDescriptor {
    pub magic: u32,
    pub api_version: u32,
    pub id: *const c_char,
    pub name: *const c_char,
    pub name_uk: *const c_char,
    pub author: *const c_char,
    pub version: *const c_char,
    pub category: u32,
    pub kind: u32,
    /// Comma-separated file extensions handled by a `Viewer` plugin (e.g. `"txt,lrc,md"`).
    pub extensions: *const c_char,
    pub run: unsafe extern "C" fn(api: *const UIPodPluginApi, file_path: *const c_char) -> i32,
}

unsafe impl Sync for UIPodPluginDescriptor {}

/// Metadata about the currently playing background track.
#[derive(Debug, Clone, Default)]
pub struct AudioTrackInfo {
    pub status: u32,
    pub path: String,
    pub title: String,
    pub artist: String,
    pub position_ms: u32,
    pub duration_ms: u32,
}

/// Entry from UIPod's unified Music library (`Local` + `Navidrome` + `Jellyfin`).
#[derive(Debug, Clone, Default)]
pub struct LibraryTrack {
    pub path: String,
    pub artist: String,
    pub title: String,
    pub duration_ms: u32,
}

/// Safe, zero-boilerplate Rust wrapper over [`UIPodPluginApi`] provided to plugin entry functions.
pub struct HostContext<'a> {
    api: &'a UIPodPluginApi,
}

impl<'a> HostContext<'a> {
    /// Wraps a validated host API table pointer.
    ///
    /// # Safety
    /// `api` must point to a valid `UIPodPluginApi` matching `UIPOD_PLUGIN_API_VERSION`.
    pub unsafe fn from_raw(api: &'a UIPodPluginApi) -> Self {
        Self { api }
    }

    #[inline]
    pub fn ticks_ms(&self) -> u64 {
        (self.api.get_ticks_ms)()
    }

    #[inline]
    pub fn sleep_ms(&self, ms: u32) {
        (self.api.sleep_ms)(ms);
    }

    #[inline]
    pub fn is_ukrainian(&self) -> bool {
        (self.api.get_language)() == 1
    }

    #[inline]
    pub fn tr<'s>(&self, en: &'s str, uk: &'s str) -> &'s str {
        if self.is_ukrainian() {
            uk
        } else {
            en
        }
    }

    #[inline]
    pub fn theme(&self) -> PluginTheme {
        (self.api.get_theme)()
    }

    pub fn data_dir(&self) -> PathBuf {
        let mut buf = vec![0u8; 512];
        let n = unsafe { (self.api.get_data_dir)(buf.as_mut_ptr() as *mut c_char, buf.len()) };
        if n == 0 {
            return PathBuf::from("/tmp");
        }
        let s = String::from_utf8_lossy(&buf[..n.min(buf.len())]).into_owned();
        PathBuf::from(s)
    }

    pub fn save_plugin_data(&self, plugin_id: &str, key: &str, data: &[u8]) -> bool {
        let dir = self.data_dir().join(plugin_id);
        if fs::create_dir_all(&dir).is_err() {
            return false;
        }
        fs::write(dir.join(key), data).is_ok()
    }

    pub fn load_plugin_data(&self, plugin_id: &str, key: &str) -> Option<Vec<u8>> {
        let path = self.data_dir().join(plugin_id).join(key);
        fs::read(path).ok()
    }

    /* ---- GPU Vector Canvas ---- */

    pub fn begin_frame(&self, bg_rgb: u32, show_header: bool, title: &str) {
        let c_title = sanitize_cstring(title);
        unsafe { (self.api.canvas_begin)(bg_rgb, show_header, c_title.as_ptr()) };
    }

    /// Starts a responsive full-screen vector frame adapting to the display's aspect ratio
    /// and returns `(logical_width, logical_height)`.
    pub fn begin_responsive_frame(
        &self,
        bg_rgb: u32,
        show_header: bool,
        title: &str,
    ) -> (f32, f32) {
        let c_title = sanitize_cstring(title);
        if self.api.api_version >= 6 {
            let mut w = SCREEN_WIDTH as f32;
            let mut h = SCREEN_HEIGHT as f32;
            unsafe {
                (self.api.canvas_begin_responsive)(
                    bg_rgb,
                    show_header,
                    c_title.as_ptr(),
                    &mut w,
                    &mut h,
                );
            }
            (w.max(320.0), h.max(240.0))
        } else {
            unsafe { (self.api.canvas_begin)(bg_rgb, show_header, c_title.as_ptr()) };
            (SCREEN_WIDTH as f32, SCREEN_HEIGHT as f32)
        }
    }

    /// Queries the current responsive `(logical_width, logical_height)` without starting a frame.
    pub fn responsive_size(&self) -> (f32, f32) {
        if self.api.api_version >= 6 {
            let mut w = SCREEN_WIDTH as f32;
            let mut h = SCREEN_HEIGHT as f32;
            unsafe {
                (self.api.get_responsive_size)(&mut w, &mut h);
            }
            (w.max(320.0), h.max(240.0))
        } else {
            (SCREEN_WIDTH as f32, SCREEN_HEIGHT as f32)
        }
    }

    #[inline]
    pub fn fill_rect(&self, x: f32, y: f32, w: f32, h: f32, radius: f32, rgb: u32) {
        (self.api.canvas_fill_rect)(x, y, w, h, radius, rgb);
    }

    #[inline]
    pub fn stroke_rect(
        &self,
        x: f32,
        y: f32,
        w: f32,
        h: f32,
        radius: f32,
        stroke_w: f32,
        rgb: u32,
    ) {
        (self.api.canvas_stroke_rect)(x, y, w, h, radius, stroke_w, rgb);
    }

    #[inline]
    pub fn fill_circle(&self, cx: f32, cy: f32, r: f32, rgb: u32) {
        (self.api.canvas_fill_circle)(cx, cy, r, rgb);
    }

    #[inline]
    pub fn draw_line(&self, x0: f32, y0: f32, x1: f32, y1: f32, stroke_w: f32, rgb: u32) {
        (self.api.canvas_draw_line)(x0, y0, x1, y1, stroke_w, rgb);
    }

    pub fn draw_text(
        &self,
        x: f32,
        y: f32,
        text: &str,
        font_size: f32,
        weight: FontWeight,
        align: TextAlign,
        rgb: u32,
    ) -> f32 {
        if text.is_empty() {
            return 0.0;
        }
        let c = sanitize_cstring(text);
        unsafe {
            (self.api.canvas_draw_text)(
                x,
                y,
                c.as_ptr(),
                font_size,
                weight as u8,
                align as u8,
                rgb,
            )
        }
    }

    pub fn measure_text(&self, text: &str, font_size: f32, weight: FontWeight) -> f32 {
        if text.is_empty() {
            return 0.0;
        }
        let c = sanitize_cstring(text);
        unsafe { (self.api.canvas_measure_text)(c.as_ptr(), font_size, weight as u8) }
    }

    #[inline]
    pub fn draw_progress_bar(&self, x: f32, y: f32, w: f32, h: f32, progress: f32) {
        (self.api.canvas_draw_progress_bar)(x, y, w, h, progress);
    }

    #[inline]
    pub fn draw_scrollbar(
        &self,
        x: f32,
        y: f32,
        w: f32,
        h: f32,
        total: usize,
        start: usize,
        visible: usize,
    ) {
        (self.api.canvas_draw_scrollbar)(x, y, w, h, total, start, visible);
    }

    #[inline]
    pub fn present(&self) {
        (self.api.canvas_present)();
    }

    /* ---- Direct Framebuffer (`1024 * 768 * 3` `{B, G, R}`) ---- */

    pub fn framebuffer_mut(&mut self) -> Option<&mut [u8]> {
        let ptr = (self.api.fb_ptr)();
        if ptr.is_null() {
            None
        } else {
            Some(unsafe { std::slice::from_raw_parts_mut(ptr, SCREEN_WIDTH * SCREEN_HEIGHT * 3) })
        }
    }

    #[inline]
    pub fn present_framebuffer(&self) {
        (self.api.fb_present)();
    }

    /* ---- Input, Native System Menu & Dialogs ---- */

    #[inline]
    pub fn poll_input(&self, timeout_ms: u32) -> PluginInput {
        (self.api.poll_input)(timeout_ms)
    }

    /// Opens UIPod's native J2ME-style full-screen menu (`gui_synclist` + screen transitions)
    /// populated with `items`, initially highlighting `initial_sel`.
    pub fn show_menu(&self, title: &str, items: &[MenuItem<'_>], initial_sel: usize) -> MenuOutcome {
        if items.is_empty() {
            return MenuOutcome::Cancelled;
        }
        let c_title = sanitize_cstring(title);
        let c_labels: Vec<CString> = items.iter().map(|it| sanitize_cstring(it.label)).collect();
        let c_vals: Vec<Option<CString>> = items
            .iter()
            .map(|it| it.value.map(sanitize_cstring))
            .collect();
        let raw_items: Vec<UIPodPluginMenuItem> = items
            .iter()
            .enumerate()
            .map(|(i, it)| UIPodPluginMenuItem {
                label: c_labels[i].as_ptr(),
                value: c_vals[i]
                    .as_ref()
                    .map_or(std::ptr::null(), |v| v.as_ptr()),
                has_chevron: it.has_chevron,
            })
            .collect();

        let sel = initial_sel.min(raw_items.len() - 1);
        let rc = unsafe {
            (self.api.show_menu)(c_title.as_ptr(), raw_items.as_ptr(), raw_items.len(), sel)
        };
        if rc >= 0 {
            MenuOutcome::Selected(rc as usize)
        } else if rc == -1 {
            MenuOutcome::Cancelled
        } else {
            MenuOutcome::ForceExit
        }
    }

    pub fn prompt_keyboard(&self, initial: &str, allow_symbols: bool) -> Option<String> {
        let mut buf = vec![0u8; 256];
        let bytes = initial.as_bytes();
        let copy_len = bytes.len().min(buf.len() - 1);
        buf[..copy_len].copy_from_slice(&bytes[..copy_len]);
        buf[copy_len] = 0;

        let ok = unsafe {
            (self.api.keyboard_input)(buf.as_mut_ptr() as *mut c_char, buf.len(), allow_symbols)
        };
        if !ok {
            return None;
        }
        let nul = buf.iter().position(|&b| b == 0).unwrap_or(buf.len());
        Some(String::from_utf8_lossy(&buf[..nul]).into_owned())
    }

    pub fn show_splash(&self, message: &str, duration_ms: u32) {
        let c = sanitize_cstring(message);
        unsafe { (self.api.show_splash)(c.as_ptr(), duration_ms) };
    }

    /* ---- Audio Playback ---- */

    #[inline]
    pub fn audio_status(&self) -> u32 {
        (self.api.audio_status)()
    }

    #[inline]
    pub fn audio_toggle_play_pause(&self) {
        (self.api.audio_toggle_play_pause)();
    }

    #[inline]
    pub fn audio_next(&self) {
        (self.api.audio_next)();
    }

    #[inline]
    pub fn audio_prev(&self) {
        (self.api.audio_prev)();
    }

    #[inline]
    pub fn audio_seek_ms(&self, pos_ms: u32) {
        (self.api.audio_seek_ms)(pos_ms);
    }

    pub fn audio_track_info(&self) -> Option<AudioTrackInfo> {
        let status = (self.api.audio_status)();
        if status == 0 {
            return None;
        }
        let mut p_buf = vec![0u8; 512];
        let mut t_buf = vec![0u8; 256];
        let mut a_buf = vec![0u8; 256];
        let mut pos_ms = 0u32;
        let mut len_ms = 0u32;
        let ok = unsafe {
            (self.api.audio_current_track)(
                p_buf.as_mut_ptr() as *mut c_char,
                p_buf.len(),
                t_buf.as_mut_ptr() as *mut c_char,
                t_buf.len(),
                a_buf.as_mut_ptr() as *mut c_char,
                a_buf.len(),
                &mut pos_ms,
                &mut len_ms,
            )
        };
        if !ok {
            return None;
        }
        let p_end = p_buf.iter().position(|&b| b == 0).unwrap_or(p_buf.len());
        let t_end = t_buf.iter().position(|&b| b == 0).unwrap_or(t_buf.len());
        let a_end = a_buf.iter().position(|&b| b == 0).unwrap_or(a_buf.len());
        Some(AudioTrackInfo {
            status,
            path: String::from_utf8_lossy(&p_buf[..p_end]).into_owned(),
            title: String::from_utf8_lossy(&t_buf[..t_end]).into_owned(),
            artist: String::from_utf8_lossy(&a_buf[..a_end]).into_owned(),
            position_ms: pos_ms,
            duration_ms: len_ms,
        })
    }

    /// Reads interleaved stereo S16 samples (`[L0, R0, L1, R1, ...]`) into `out_stereo`.
    /// `out_stereo.len()` should be `2 * frames`. Returns the number of stereo frames read.
    /// - `ahead_ms == 0`: live waveform playing right now.
    /// - `ahead_ms > 0`: lookahead waveform `ahead_ms` in the future from `pcmbuf`.
    #[inline]
    pub fn audio_read_pcm(&self, ahead_ms: u32, out_stereo: &mut [i16]) -> usize {
        let max_frames = out_stereo.len() / 2;
        if max_frames == 0 {
            return 0;
        }
        unsafe { (self.api.audio_read_pcm)(ahead_ms, out_stereo.as_mut_ptr(), max_frames) }
    }

    /// Starts playing `path` at `start_ms` as a non-prompting single-track snippet.
    #[inline]
    pub fn audio_play_track(&self, path: &str, start_ms: u32) -> bool {
        let c = sanitize_cstring(path);
        unsafe { (self.api.audio_play_track)(c.as_ptr(), start_ms) }
    }

    /// Stops playback cleanly.
    #[inline]
    pub fn audio_stop(&self) {
        (self.api.audio_stop)();
    }

    /// Total number of deduplicated Music tracks available in UIPod's library.
    #[inline]
    pub fn library_track_count(&self) -> usize {
        (self.api.library_track_count)()
    }

    /// Reads the `index`-th Music track from UIPod's unified library.
    pub fn library_track_at(&self, index: usize) -> Option<LibraryTrack> {
        let mut p_buf = [0u8; 512];
        let mut a_buf = [0u8; 256];
        let mut t_buf = [0u8; 256];
        let mut dur_ms = 0u32;
        let ok = unsafe {
            (self.api.library_track_at)(
                index,
                p_buf.as_mut_ptr() as *mut c_char,
                p_buf.len(),
                a_buf.as_mut_ptr() as *mut c_char,
                a_buf.len(),
                t_buf.as_mut_ptr() as *mut c_char,
                t_buf.len(),
                &mut dur_ms,
            )
        };
        if !ok {
            return None;
        }
        let p_end = p_buf.iter().position(|&b| b == 0).unwrap_or(p_buf.len());
        let a_end = a_buf.iter().position(|&b| b == 0).unwrap_or(a_buf.len());
        let t_end = t_buf.iter().position(|&b| b == 0).unwrap_or(t_buf.len());
        Some(LibraryTrack {
            path: String::from_utf8_lossy(&p_buf[..p_end]).into_owned(),
            artist: String::from_utf8_lossy(&a_buf[..a_end]).into_owned(),
            title: String::from_utf8_lossy(&t_buf[..t_end]).into_owned(),
            duration_ms: dur_ms,
        })
    }
}

#[inline]
fn sanitize_cstring(s: &str) -> CString {
    match CString::new(s) {
        Ok(c) => c,
        Err(_) => {
            let cleaned: Vec<u8> = s.bytes().filter(|&b| b != 0).collect();
            CString::new(cleaned).unwrap_or_default()
        }
    }
}

#[doc(hidden)]
pub unsafe fn invoke_plugin_entry<F>(
    api: *const UIPodPluginApi,
    file_path: *const c_char,
    entry: F,
) -> i32
where
    F: FnOnce(&mut HostContext, Option<&Path>) -> i32,
{
    if api.is_null() {
        return -1;
    }
    let api_ref = &*api;
    if api_ref.api_version != UIPOD_PLUGIN_API_VERSION {
        return -2;
    }
    let mut ctx = HostContext::from_raw(api_ref);
    let opt_str = if file_path.is_null() {
        None
    } else {
        let s = CStr::from_ptr(file_path).to_string_lossy();
        if s.trim().is_empty() {
            None
        } else {
            Some(s.into_owned())
        }
    };
    let res = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        entry(&mut ctx, opt_str.as_deref().map(Path::new))
    }));
    res.unwrap_or(-3)
}

/// Declares and exports a `.uipod` native plugin with zero `unsafe` boilerplate.
#[macro_export]
macro_rules! uipod_declare_plugin {
    (
        id: $id:literal,
        name: $name:literal,
        name_uk: $name_uk:literal,
        author: $author:literal,
        version: $version:literal,
        category: $cat:expr,
        kind: $kind:expr,
        extensions: $exts:literal,
        entry: $entry_fn:path $(,)?
    ) => {
        unsafe extern "C" fn __uipod_plugin_trampoline(
            api: *const $crate::UIPodPluginApi,
            file_path: *const ::std::ffi::c_char,
        ) -> i32 {
            $crate::invoke_plugin_entry(api, file_path, $entry_fn)
        }

        static __UIPOD_PLUGIN_DESC: $crate::UIPodPluginDescriptor =
            $crate::UIPodPluginDescriptor {
                magic: $crate::UIPOD_PLUGIN_MAGIC,
                api_version: $crate::UIPOD_PLUGIN_API_VERSION,
                id: concat!($id, "\0").as_ptr() as *const ::std::ffi::c_char,
                name: concat!($name, "\0").as_ptr() as *const ::std::ffi::c_char,
                name_uk: concat!($name_uk, "\0").as_ptr() as *const ::std::ffi::c_char,
                author: concat!($author, "\0").as_ptr() as *const ::std::ffi::c_char,
                version: concat!($version, "\0").as_ptr() as *const ::std::ffi::c_char,
                category: ($cat) as u32,
                kind: ($kind) as u32,
                extensions: concat!($exts, "\0").as_ptr() as *const ::std::ffi::c_char,
                run: __uipod_plugin_trampoline,
            };

        #[no_mangle]
        pub extern "C" fn uipod_plugin_descriptor() -> *const $crate::UIPodPluginDescriptor {
            &__UIPOD_PLUGIN_DESC
        }
    };
    (
        id: $id:literal,
        name: $name:literal,
        name_uk: $name_uk:literal,
        author: $author:literal,
        version: $version:literal,
        category: $cat:expr,
        extensions: $exts:literal,
        entry: $entry_fn:path $(,)?
    ) => {
        $crate::uipod_declare_plugin! {
            id: $id,
            name: $name,
            name_uk: $name_uk,
            author: $author,
            version: $version,
            category: $cat,
            kind: $crate::PluginKind::Utility,
            extensions: $exts,
            entry: $entry_fn,
        }
    };
}
