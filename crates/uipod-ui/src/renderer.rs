//! OpenGL ES 3.0 GPU renderer using `femtovg` + `taffy` + offscreen FBO readback.

#[cfg(not(target_arch = "wasm32"))]
use std::ffi::c_void;
use std::path::Path;

use femtovg::renderer::OpenGl;
use femtovg::{
    Align, Baseline, Canvas, Color, FontId, ImageFlags, ImageId, Paint, Path as VgPath,
    PixelFormat, RenderTarget,
};
use glow::HasContext;
use imgref::ImgRef;
use rgb::RGBA8;

use crate::hud::{HudCardPayload, HudRenderItem, HudStack};
use crate::icons::{
    color_with_alpha, draw_arrow_down_64, draw_arrow_s, draw_battery, draw_chevron_left_s,
    draw_chevron_s, draw_filetype_icon, draw_fx_64, draw_header_source_icon, draw_hold_lock,
    draw_hud_card_frame, draw_icon_note_scaled, draw_infinity_badge, draw_play_mode_icon,
    draw_scrollbar_v, draw_seekbar_track_h, draw_seekbar_track_h_with_loop, draw_toggle,
    draw_volume_popup, draw_volume_popup_alpha, fill_rounded_rect, stroke_rounded_rect_inside,
};
use crate::layout::{
    compute_header_layout, compute_header_layout_sized, compute_list_layout_sized,
    compute_now_playing_alt_layout_sized, compute_now_playing_layout_sized, horizontal_margin,
    HeaderBoxes, LogicalMetrics, SCREEN_H, SCREEN_W,
};
use crate::{
    FileIconKind, GraphicEqState, HeaderSourceIcon, HeaderState, KeyboardState, ListRow,
    ListScreenState, NowPlayingState, NsfApuPill, NsfChannelsState, RightAccessory, ThemeColors,
};

pub const WIDTH: usize = 1024;
pub const HEIGHT: usize = 768;
/// Top scanlines occupied by the shared `Header` (`y = 0..116`, padded to 120).
pub const HEADER_BAND_ROWS: usize = 120;

/// Static weight instances of `Geologica` (`Regular=400`, `Medium=500`, `SemiBold=600`, `Bold=700`).
static FONT_GEOLOGICA_REGULAR: &[u8] =
    include_bytes!("../../../assets/fonts/Geologica-Regular.ttf");
static FONT_GEOLOGICA_MEDIUM: &[u8] =
    include_bytes!("../../../assets/fonts/Geologica-Medium.ttf");
static FONT_GEOLOGICA_SEMIBOLD: &[u8] =
    include_bytes!("../../../assets/fonts/Geologica-SemiBold.ttf");
static FONT_GEOLOGICA_BOLD: &[u8] = include_bytes!("../../../assets/fonts/Geologica-Bold.ttf");

/// Editorial book serif family (`Noto Serif` Regular/Bold/Italic/BoldItalic, `SIL OFL 1.1`)
/// covering Latin + complete Cyrillic (including Ukrainian `Ґ, ґ, Є, є, І, і, Ї, ї`).
static FONT_NOTO_SERIF_REGULAR: &[u8] =
    include_bytes!("../../../assets/fonts/NotoSerif-Regular.ttf");
static FONT_NOTO_SERIF_BOLD: &[u8] = include_bytes!("../../../assets/fonts/NotoSerif-Bold.ttf");
static FONT_NOTO_SERIF_ITALIC: &[u8] =
    include_bytes!("../../../assets/fonts/NotoSerif-Italic.ttf");
static FONT_NOTO_SERIF_BOLD_ITALIC: &[u8] =
    include_bytes!("../../../assets/fonts/NotoSerif-BoldItalic.ttf");

/// In `femtovg`'s `OpenGl` backend, `RenderTarget::Image` without `FLIP_Y` maps canvas `y = 0`
/// to the top of the FBO (`gl_y = height - 1`), so `glReadPixels` row `0` is the bottom scanline.
const FBO_ORIGIN_BOTTOM: bool = true;

#[derive(Clone, Copy)]
struct ResolvedTheme {
    bg: Color,
    fg: Color,
    fg1: Color,
    fg2: Color,
}

#[inline]
fn rgb_u32_to_color(rgb: u32) -> Color {
    let r = ((rgb >> 16) & 0xFF) as u8;
    let g = ((rgb >> 8) & 0xFF) as u8;
    let b = (rgb & 0xFF) as u8;
    Color::rgb(r, g, b)
}

#[inline]
fn resolve_theme(t: ThemeColors) -> ResolvedTheme {
    ResolvedTheme {
        bg: rgb_u32_to_color(t.bg_rgb),
        fg: rgb_u32_to_color(t.fg_rgb),
        fg1: rgb_u32_to_color(t.fg1_rgb),
        fg2: rgb_u32_to_color(t.fg2_rgb),
    }
}

#[inline]
fn fnv1a_art_hash(bytes: &[u8], mono: bool, theme: ThemeColors) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    h ^= u64::from(mono);
    h = h.wrapping_mul(0x0100_0000_01b3);
    if mono {
        h ^= ((theme.bg_rgb as u64) << 24) ^ (theme.fg_rgb as u64);
        h = h.wrapping_mul(0x0100_0000_01b3);
        h ^= ((theme.fg1_rgb as u64) << 24) ^ (theme.fg2_rgb as u64);
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    // Stride by 4 bytes for ultra-fast cache invalidation (<0.02 ms on Cortex-A53)
    for chunk in bytes.chunks(4) {
        let mut word = 0u32;
        for (i, &b) in chunk.iter().enumerate() {
            word |= (b as u32) << (i * 8);
        }
        h ^= word as u64;
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    h
}

#[inline]
fn rgb_u32_with_luma(rgb: u32) -> (u32, RGBA8) {
    let r = ((rgb >> 16) & 0xFF) as u8;
    let g = ((rgb >> 8) & 0xFF) as u8;
    let b = (rgb & 0xFF) as u8;
    let luma = 54u32 * (r as u32) + 183u32 * (g as u32) + 19u32 * (b as u32);
    (luma, RGBA8::new(r, g, b, 255))
}

/// Quantizes a `256x256` `RGB24` album cover into the active theme's exact 4 iPod Mini LCD shades
/// (`bg_rgb`, `fg1_rgb`, `fg2_rgb`, `fg_rgb` sorted darkest -> lightest) with
/// 2nd–98th percentile auto-levels and 4x4 Bayer ordered dithering.
pub fn quantize_mono_art_4shade(rgb24: &[u8], theme: ThemeColors, out_rgba: &mut [RGBA8]) {
    const BAYER4: [[u8; 4]; 4] = [
        [0, 8, 2, 10],
        [12, 4, 14, 6],
        [3, 11, 1, 9],
        [15, 7, 13, 5],
    ];

    let mut shades = [
        rgb_u32_with_luma(theme.bg_rgb),
        rgb_u32_with_luma(theme.fg1_rgb),
        rgb_u32_with_luma(theme.fg2_rgb),
        rgb_u32_with_luma(theme.fg_rgb),
    ];
    shades.sort_by_key(|&(luma, _)| luma);
    let palette = [shades[0].1, shades[1].1, shades[2].1, shades[3].1];

    let npx = 256 * 256;
    let mut hist = [0u32; 256];
    for px in rgb24[..npx * 3].chunks_exact(3) {
        let l = ((54u32 * px[0] as u32 + 183u32 * px[1] as u32 + 19u32 * px[2] as u32 + 128)
            >> 8) as usize;
        hist[l] += 1;
    }

    let clip = (npx / 50) as u32; // 2nd and 98th percentiles
    let mut acc = 0u32;
    let mut lo = 0i32;
    for (i, &c) in hist.iter().enumerate() {
        acc += c;
        if acc >= clip {
            lo = i as i32;
            break;
        }
    }
    acc = 0;
    let mut hi = 255i32;
    for (i, &c) in hist.iter().enumerate().rev() {
        acc += c;
        if acc >= clip {
            hi = i as i32;
            break;
        }
    }
    if hi - lo < 48 {
        lo = 0;
        hi = 255;
    }
    let inv_span = 1.0 / ((hi - lo).max(1) as f32);

    for y in 0..256usize {
        let bayer_row = &BAYER4[y & 3];
        for x in 0..256usize {
            let i = y * 256 + x;
            let px = &rgb24[i * 3..i * 3 + 3];
            let raw_l = ((54u32 * px[0] as u32
                + 183u32 * px[1] as u32
                + 19u32 * px[2] as u32
                + 128)
                >> 8) as i32;
            let norm = ((raw_l - lo) as f32 * inv_span).clamp(0.0, 1.0);
            let g_l = norm.powf(1.10);

            let tone = if g_l <= 0.10 {
                0
            } else if g_l >= 0.90 {
                3
            } else {
                let bval = (bayer_row[x & 3] as f32 / 16.0) - 0.5 + (1.0 / 32.0);
                let v = (g_l * 3.0 + bval * 0.48).round().clamp(0.0, 3.0);
                v as usize
            };
            out_rgba[i] = palette[tone];
        }
    }
}

/// Pure-Rust GPU UI engine backed by `taffy` + `femtovg` on OpenGL ES 3.0.
pub struct UiEngine {
    canvas: Canvas<OpenGl>,
    gl: glow::Context,
    fbo_image: ImageId,
    fbo_w: usize,
    fbo_h: usize,
    prev_fbo_image: Option<ImageId>,
    prev_fbo_w: usize,
    prev_fbo_h: usize,
    compat_fbo_image: Option<ImageId>,
    fonts_regular: Vec<FontId>,
    fonts_medium: Vec<FontId>,
    fonts_semibold: Vec<FontId>,
    fonts_bold: Vec<FontId>,
    serif_regular: Vec<FontId>,
    serif_bold: Vec<FontId>,
    serif_italic: Vec<FontId>,
    serif_bold_italic: Vec<FontId>,
    readback_rgba: Vec<u8>,
    art_image: Option<ImageId>,
    art_hash: Option<u64>,
    art_rgba_scratch: Vec<RGBA8>,
    plugin_images: Vec<(u64, u32, u32, ImageId)>,
    last_theme: ThemeColors,
    hud_stack: HudStack,
}

impl UiEngine {
    /// Initializes the `femtovg` OpenGL ES 3.0 renderer, `glow` readback context,
    /// embedded `Geologica` font family, optional CJK fallback font, and `1024x768` FBO.
    ///
    /// # Safety
    /// A valid OpenGL ES 3.0 (or Desktop OpenGL 3.2+) context must be current on the calling thread,
    /// and `get_proc_address` must return valid function pointers for the active context.
    #[cfg(not(target_arch = "wasm32"))]
    pub unsafe fn new<F>(
        mut get_proc_address: F,
        cjk_font_path: Option<&Path>,
    ) -> Result<Self, String>
    where
        F: FnMut(&std::ffi::CStr) -> *const c_void,
    {
        let renderer = OpenGl::new_from_function_cstr(|s| get_proc_address(s))
            .map_err(|e| format!("femtovg OpenGl init failed: {e:?}"))?;
        let gl = glow::Context::from_loader_function_cstr(|s| get_proc_address(s));
        Self::init_from_parts(renderer, gl, cjk_font_path)
    }

    /// Initializes the `femtovg` WebGL2 renderer and `glow` context directly from an HTML `<canvas>`.
    #[cfg(target_arch = "wasm32")]
    pub fn new_from_html_canvas(canvas_el: &web_sys::HtmlCanvasElement) -> Result<Self, String> {
        use wasm_bindgen::JsCast;

        let renderer = OpenGl::new_from_html_canvas(canvas_el)
            .map_err(|e| format!("femtovg WebGL2 init failed: {e:?}"))?;

        let attrs = web_sys::WebGlContextAttributes::new();
        attrs.set_stencil(true);
        attrs.set_antialias(false);
        let webgl2 = canvas_el
            .get_context_with_context_options("webgl2", &attrs)
            .map_err(|e| format!("getContext('webgl2') failed: {e:?}"))?
            .ok_or_else(|| "WebGL2 context unavailable".to_string())?
            .dyn_into::<web_sys::WebGl2RenderingContext>()
            .map_err(|_| "Canvas context is not WebGL2RenderingContext".to_string())?;
        let gl = glow::Context::from_webgl2_context(webgl2);
        Self::init_from_parts(renderer, gl, None)
    }

    fn init_from_parts(
        renderer: OpenGl,
        gl: glow::Context,
        cjk_font_path: Option<&Path>,
    ) -> Result<Self, String> {
        let mut canvas =
            Canvas::new(renderer).map_err(|e| format!("femtovg Canvas init failed: {e:?}"))?;

        let f_reg = canvas
            .add_font_mem(FONT_GEOLOGICA_REGULAR)
            .map_err(|e| format!("Geologica-Regular load failed: {e:?}"))?;
        let f_med = canvas
            .add_font_mem(FONT_GEOLOGICA_MEDIUM)
            .map_err(|e| format!("Geologica-Medium load failed: {e:?}"))?;
        let f_semi = canvas
            .add_font_mem(FONT_GEOLOGICA_SEMIBOLD)
            .map_err(|e| format!("Geologica-SemiBold load failed: {e:?}"))?;
        let f_bold = canvas
            .add_font_mem(FONT_GEOLOGICA_BOLD)
            .map_err(|e| format!("Geologica-Bold load failed: {e:?}"))?;

        let s_reg = canvas
            .add_font_mem(FONT_NOTO_SERIF_REGULAR)
            .map_err(|e| format!("NotoSerif-Regular load failed: {e:?}"))?;
        let s_bold = canvas
            .add_font_mem(FONT_NOTO_SERIF_BOLD)
            .map_err(|e| format!("NotoSerif-Bold load failed: {e:?}"))?;
        let s_ital = canvas
            .add_font_mem(FONT_NOTO_SERIF_ITALIC)
            .map_err(|e| format!("NotoSerif-Italic load failed: {e:?}"))?;
        let s_bi = canvas
            .add_font_mem(FONT_NOTO_SERIF_BOLD_ITALIC)
            .map_err(|e| format!("NotoSerif-BoldItalic load failed: {e:?}"))?;

        #[cfg(not(target_arch = "wasm32"))]
        let (cjk_id, cjk_serif_id) = match cjk_font_path {
            Some(p) => {
                let sans_cjk = if p.exists() {
                    canvas.add_font(p).ok()
                } else {
                    None
                };
                let serif_cjk = p
                    .parent()
                    .map(|dir| dir.join("NotoSerifCJK-Regular.ttf"))
                    .and_then(|sp| if sp.exists() { canvas.add_font(&sp).ok() } else { None })
                    .or(sans_cjk);
                (sans_cjk, serif_cjk)
            }
            None => (None, None),
        };
        #[cfg(target_arch = "wasm32")]
        let (cjk_id, cjk_serif_id): (Option<FontId>, Option<FontId>) = {
            let _ = cjk_font_path;
            (None, None)
        };

        let make_chain = |primary: FontId| -> Vec<FontId> {
            match cjk_id {
                Some(fallback) => vec![primary, fallback],
                None => vec![primary],
            }
        };
        let make_serif_chain = |primary: FontId| -> Vec<FontId> {
            match cjk_serif_id {
                Some(fallback) => vec![primary, fallback],
                None => vec![primary],
            }
        };

        let fbo_image = canvas
            .create_image_empty(
                WIDTH,
                HEIGHT,
                PixelFormat::Rgba8,
                ImageFlags::FLIP_Y | ImageFlags::NEAREST,
            )
            .map_err(|e| format!("FBO image creation failed: {e:?}"))?;

        let mut engine = Self {
            canvas,
            gl,
            fbo_image,
            fbo_w: WIDTH,
            fbo_h: HEIGHT,
            prev_fbo_image: None,
            prev_fbo_w: 0,
            prev_fbo_h: 0,
            compat_fbo_image: None,
            fonts_regular: make_chain(f_reg),
            fonts_medium: make_chain(f_med),
            fonts_semibold: make_chain(f_semi),
            fonts_bold: make_chain(f_bold),
            serif_regular: make_serif_chain(s_reg),
            serif_bold: make_serif_chain(s_bold),
            serif_italic: make_serif_chain(s_ital),
            serif_bold_italic: make_serif_chain(s_bi),
            readback_rgba: vec![0u8; WIDTH * HEIGHT * 4],
            art_image: None,
            art_hash: None,
            art_rgba_scratch: vec![RGBA8::new(0, 0, 0, 255); 256 * 256],
            plugin_images: Vec::new(),
            last_theme: ThemeColors::default(),
            hud_stack: HudStack::default(),
        };

        // Set constant 1024x768 canvas size once, warm up FBO creation, then sync back to Screen.
        engine.canvas.set_size(WIDTH as u32, HEIGHT as u32, 1.0);
        engine
            .canvas
            .set_render_target(RenderTarget::Image(fbo_image));
        engine
            .canvas
            .clear_rect(0, 0, WIDTH as u32, HEIGHT as u32, Color::black());
        engine.canvas.flush();
        engine.canvas.set_render_target(RenderTarget::Screen);
        engine.canvas.flush();
        engine.restore_gl_state();

        Ok(engine)
    }

    /// Registers an in-memory CJK fallback `.ttf` (e.g. `NotoSansCJK-Regular.ttf` fetched
    /// asynchronously in `uipod-wasm`) behind all four `Geologica` weights.
    pub fn add_cjk_fallback_bytes(&mut self, font_bytes: &[u8]) -> Result<(), String> {
        let fallback = self
            .canvas
            .add_font_mem(font_bytes)
            .map_err(|e| format!("CJK fallback font load failed: {e:?}"))?;
        for chain in [
            &mut self.fonts_regular,
            &mut self.fonts_medium,
            &mut self.fonts_semibold,
            &mut self.fonts_bold,
        ] {
            chain.truncate(1);
            chain.push(fallback);
        }
        // Provide immediate CJK fallback behind Serif too until `NotoSerifCJK-Regular.ttf` loads
        for sc in [
            &mut self.serif_regular,
            &mut self.serif_bold,
            &mut self.serif_italic,
            &mut self.serif_bold_italic,
        ] {
            if sc.len() == 1 {
                sc.push(fallback);
            }
        }
        Ok(())
    }

    /// Registers an in-memory Mincho/Songti CJK Serif fallback `.ttf` (`NotoSerifCJK-Regular.ttf`)
    /// behind all four `Noto Serif` faces (`Regular`, `Bold`, `Italic`, `BoldItalic`).
    pub fn add_serif_cjk_fallback_bytes(&mut self, font_bytes: &[u8]) -> Result<(), String> {
        let fallback = self
            .canvas
            .add_font_mem(font_bytes)
            .map_err(|e| format!("CJK Serif fallback font load failed: {e:?}"))?;
        for chain in [
            &mut self.serif_regular,
            &mut self.serif_bold,
            &mut self.serif_italic,
            &mut self.serif_bold_italic,
        ] {
            chain.truncate(1);
            chain.push(fallback);
        }
        Ok(())
    }

    /// Truncates `text` with `"..."` on UTF-8 character boundaries so its rendered width fits `max_width`.
    fn truncate_ellipsis(&mut self, text: &str, max_width: f32, paint: &Paint) -> String {
        if text.is_empty() || max_width <= 4.0 {
            return String::new();
        }
        if let Ok(m) = self.canvas.measure_text(0.0, 0.0, text, paint) {
            if m.width() <= max_width + 0.5 {
                return text.to_string();
            }
        }

        let char_offsets: Vec<usize> = text.char_indices().map(|(i, _)| i).collect();
        let mut lo = 0usize;
        let mut hi = char_offsets.len();
        let mut best = String::from("...");

        while lo <= hi {
            let mid = (lo + hi) / 2;
            let byte_end = if mid < char_offsets.len() {
                char_offsets[mid]
            } else {
                text.len()
            };
            let prefix = text[..byte_end].trim_end();
            let candidate = format!("{prefix}...");
            let fits = self
                .canvas
                .measure_text(0.0, 0.0, &candidate, paint)
                .map(|m| m.width() <= max_width)
                .unwrap_or(false);
            if fits {
                best = candidate;
                if mid == hi {
                    break;
                }
                lo = mid + 1;
            } else {
                if mid == 0 {
                    break;
                }
                hi = mid - 1;
            }
        }
        best
    }

    /// Returns the trailing UTF-8 slice of `text` whose rendered width fits within `max_width`.
    fn visible_tail_utf8(&mut self, text: &str, max_width: f32, paint: &Paint) -> String {
        if text.is_empty() || max_width <= 4.0 {
            return String::new();
        }
        if let Ok(m) = self.canvas.measure_text(0.0, 0.0, text, paint) {
            if m.width() <= max_width + 0.5 {
                return text.to_string();
            }
        }
        for (byte_idx, _) in text.char_indices() {
            let sub = &text[byte_idx..];
            if let Ok(m) = self.canvas.measure_text(0.0, 0.0, sub, paint) {
                if m.width() <= max_width + 0.5 {
                    return sub.to_string();
                }
            }
        }
        String::new()
    }

    /// Optical baseline Y for mixed-case labels (leaves clean clearance for descenders like g/p/y/у/р).
    #[inline]
    fn text_row_baseline(center_y: f32, font_size: f32) -> f32 {
        center_y + font_size * 0.315
    }

    /// Optical cap-height baseline Y for uppercase badges & digits (`EQ`, `FLAC`, `54`).
    #[inline]
    fn cap_centered_baseline(center_y: f32, font_size: f32) -> f32 {
        center_y + font_size * 0.355
    }

    pub fn ensure_fbo_size(&mut self, phys_w: u32, phys_h: u32) {
        let pw = (phys_w as usize).clamp(320, 4096);
        let ph = (phys_h as usize).clamp(240, 4096);
        if pw == self.fbo_w && ph == self.fbo_h {
            return;
        }
        if let Ok(new_fbo) = self.canvas.create_image_empty(
            pw,
            ph,
            PixelFormat::Rgba8,
            ImageFlags::FLIP_Y | ImageFlags::NEAREST,
        ) {
            self.canvas.delete_image(self.fbo_image);
            self.fbo_image = new_fbo;
            self.fbo_w = pw;
            self.fbo_h = ph;
            if let Some(prev) = self.prev_fbo_image.take() {
                self.canvas.delete_image(prev);
                self.prev_fbo_w = 0;
                self.prev_fbo_h = 0;
            }
        }
    }

    fn begin_offscreen_sized(&mut self, m: LogicalMetrics, clear_logical_h: f32, bg: Color) {
        self.ensure_fbo_size(m.phys_w, m.phys_h);
        self.canvas.reset();
        self.canvas.reset_scissor();
        self.canvas.set_size(self.fbo_w as u32, self.fbo_h as u32, 1.0);
        self.canvas
            .set_render_target(RenderTarget::Image(self.fbo_image));

        if clear_logical_h >= m.screen_h - 0.5 {
            self.canvas
                .clear_rect(0, 0, self.fbo_w as u32, self.fbo_h as u32, bg);
            self.canvas.scale(m.scale_x, m.scale_y);
            let mut bg_rect = VgPath::new();
            bg_rect.rect(0.0, 0.0, m.screen_w, m.screen_h);
            self.canvas.fill_path(&bg_rect, &Paint::color(bg));
        } else {
            self.canvas.scale(m.scale_x, m.scale_y);
            self.canvas
                .scissor(0.0, 0.0, m.screen_w, clear_logical_h.max(1.0));
            let mut bg_rect = VgPath::new();
            bg_rect.rect(0.0, 0.0, m.screen_w, clear_logical_h.max(1.0));
            self.canvas.fill_path(&bg_rect, &Paint::color(bg));
        }
    }

    fn begin_offscreen(&mut self, clear_rows: usize, bg: Color) {
        let m = LogicalMetrics::from_physical(WIDTH as u32, HEIGHT as u32);
        self.begin_offscreen_sized(m, clear_rows.min(HEIGHT) as f32, bg);
    }

    /// Reads the top `top_rows` scanlines (`y = 0..top_rows` in top-down screen space)
    /// from the currently bound `femtovg` FBO into `self.readback_rgba`, then restores GL state.
    fn finish_and_read_top_rows(&mut self, top_rows: usize) {
        let rows = top_rows.clamp(1, HEIGHT);
        self.canvas.flush();

        let byte_len = WIDTH * rows * 4;
        let gl_y = if FBO_ORIGIN_BOTTOM {
            (HEIGHT - rows) as i32
        } else {
            0
        };

        unsafe {
            self.gl.pixel_store_i32(glow::PACK_ALIGNMENT, 4);
            self.gl.read_pixels(
                0,
                gl_y,
                WIDTH as i32,
                rows as i32,
                glow::RGBA,
                glow::UNSIGNED_BYTE,
                glow::PixelPackData::Slice(&mut self.readback_rgba[..byte_len]),
            );
        }

        // Synchronize femtovg's internal render-target tracker back to Screen before unbinding GL state.
        self.canvas.set_render_target(RenderTarget::Screen);
        self.canvas.flush();
        self.restore_gl_state();
    }

    /// Unpacked `RGBA8` top-down copy of the last `top_rows` rendered scanlines (used by tests/previews).
    fn unpack_top_rows_to_rgba(&self, top_rows: usize, out_rgba: &mut [u8]) {
        let rows = top_rows.clamp(1, HEIGHT);
        let row_bytes_src = WIDTH * 4;
        assert!(out_rgba.len() >= rows * row_bytes_src);

        for y in 0..rows {
            let src_row = if FBO_ORIGIN_BOTTOM { rows - 1 - y } else { y };
            let s0 = src_row * row_bytes_src;
            let d0 = y * row_bytes_src;
            out_rgba[d0..d0 + row_bytes_src]
                .copy_from_slice(&self.readback_rgba[s0..s0 + row_bytes_src]);
        }
    }

    /// Unpacks the top `top_rows` rendered scanlines into Rockbox's 24-bit `{b, g, r}` framebuffer.
    fn unpack_top_rows_to_bgr(&self, top_rows: usize, out_bgr: &mut [u8]) {
        let rows = top_rows.clamp(1, HEIGHT);
        let row_bytes_src = WIDTH * 4;
        let row_bytes_dst = WIDTH * 3;
        assert!(out_bgr.len() >= rows * row_bytes_dst);

        for y in 0..rows {
            let src_row = if FBO_ORIGIN_BOTTOM { rows - 1 - y } else { y };
            let src_line =
                &self.readback_rgba[src_row * row_bytes_src..(src_row + 1) * row_bytes_src];
            let dst_line = &mut out_bgr[y * row_bytes_dst..(y + 1) * row_bytes_dst];
            for (rgba, bgr) in src_line.chunks_exact(4).zip(dst_line.chunks_exact_mut(3)) {
                bgr[0] = rgba[2]; // B
                bgr[1] = rgba[1]; // G
                bgr[2] = rgba[0]; // R
            }
        }
    }

    /// Restores clean OpenGL ES 3.0 state so `window-sdl.c` sees default bindings.
    fn restore_gl_state(&self) {
        unsafe {
            self.gl.bind_framebuffer(glow::FRAMEBUFFER, None);
            self.gl.disable(glow::STENCIL_TEST);
            self.gl.disable(glow::SCISSOR_TEST);
            self.gl.disable(glow::CULL_FACE);
            self.gl.disable(glow::DEPTH_TEST);
            self.gl.disable(glow::BLEND);
            self.gl.color_mask(true, true, true, true);
            self.gl.pixel_store_i32(glow::UNPACK_ALIGNMENT, 4);
            self.gl.pixel_store_i32(glow::PACK_ALIGNMENT, 4);
            self.gl.active_texture(glow::TEXTURE0);
            self.gl.bind_vertex_array(None);
            self.gl.bind_buffer(glow::ARRAY_BUFFER, None);
            self.gl.use_program(None);
        }
    }

    /// Draws the shared `Header` (`[3105]`: `960x80` at `(32, 32)` + `4px` bottom border in `fg1`).
    fn draw_header_component(
        &mut self,
        boxes: &HeaderBoxes,
        state: &HeaderState,
        pal: ResolvedTheme,
    ) {
        let regular = self.fonts_regular.clone();
        let semibold = self.fonts_semibold.clone();
        let icon_y = boxes.right_slot.y;

        // Right-side Battery [1564] (64x64): shell in fg2, bar in fg1, cavity/bolt cutout in bg
        draw_battery(
            &mut self.canvas,
            boxes.right_slot.x,
            icon_y,
            state.battery_percent,
            state.is_charging,
            pal.fg2,
            pal.fg1,
            pal.bg,
        );

        // Frame 24 [3140]: 32px (`space-4`) gap between Battery and Frame 23 status badges,
        // and 16px (`space-2`) gap between badges inside Frame 23 [3130].
        let has_right_badges =
            state.source_icon != HeaderSourceIcon::None || state.is_downloading || state.is_fx;
        let mut cursor_right = if has_right_badges {
            boxes.right_slot.x - 32.0
        } else {
            boxes.right_slot.x - 16.0
        };

        if state.source_icon != HeaderSourceIcon::None {
            let sx = cursor_right - 64.0;
            draw_header_source_icon(
                &mut self.canvas,
                sx,
                icon_y,
                state.source_icon,
                pal.fg2,
                pal.bg,
            );
            cursor_right = sx - 16.0;
        }

        if state.is_downloading {
            if state.download_total > 0 {
                // Group 3 [3110]/[3129]: 64x64 arrow + 4px gap + Geologica Regular 32px queue count
                let remaining = state.download_total.saturating_sub(state.download_done).max(1);
                let dl_label = format!("{remaining}");
                let mut dp = Paint::color(pal.fg2);
                dp.set_font(&regular);
                dp.set_font_size(32.0);
                dp.set_text_align(Align::Right);
                dp.set_text_baseline(Baseline::Alphabetic);
                let text_w = self
                    .canvas
                    .measure_text(0.0, 0.0, &dl_label, &dp)
                    .map(|m| m.width())
                    .unwrap_or(40.0);
                let base_y = Self::cap_centered_baseline(boxes.right_slot.center_y(), 32.0);
                let _ = self.canvas.fill_text(cursor_right, base_y, &dl_label, &dp);
                let ax = cursor_right - text_w - 4.0 - 64.0;
                draw_arrow_down_64(&mut self.canvas, ax, icon_y, pal.fg2);
                cursor_right = ax - 16.0;
            } else {
                let ax = cursor_right - 64.0;
                draw_arrow_down_64(&mut self.canvas, ax, icon_y, pal.fg2);
                cursor_right = ax - 16.0;
            }
        }

        if state.is_fx {
            // `icon_fx_64` [3187] directly to the left of the download icon (or source/battery)
            let fx_x = cursor_right - 64.0;
            draw_fx_64(&mut self.canvas, fx_x, icon_y, pal.fg2, pal.bg);
            cursor_right = fx_x - 16.0;
        }

        let right_guard_x = cursor_right;

        if !state.legend.trim().is_empty() {
            // Header button-legend (%Tl) overrides left icon + centered title
            let mut lp = Paint::color(pal.fg);
            lp.set_font(&semibold);
            lp.set_font_size(40.0);
            lp.set_text_align(Align::Left);
            lp.set_text_baseline(Baseline::Alphabetic);
            let max_w = (right_guard_x - boxes.container.x).max(0.0);
            let txt = self.truncate_ellipsis(state.legend, max_w, &lp);
            let base_y = Self::text_row_baseline(boxes.title_slot.center_y(), 40.0);
            let _ = self.canvas.fill_text(boxes.container.x, base_y, &txt, &lp);
        } else {
            // Group 4 [3128]: Play-mode icon [90] (45x48 at y+8) + optional 16px gap + `icon_lock_64` [2968]
            draw_play_mode_icon(
                &mut self.canvas,
                boxes.left_slot.x,
                boxes.left_slot.y,
                state.play_mode,
                pal.fg2,
            );
            let left_guard_x = if state.is_hold {
                let lock_x = boxes.container.x + 61.0;
                draw_hold_lock(&mut self.canvas, lock_x, icon_y, pal.fg2, pal.bg);
                lock_x + 64.0 + 16.0
            } else {
                boxes.left_slot.x + 48.0 + 16.0
            };

            // Centered screen title: Geologica SemiBold 48px in fg
            let title = if state.title.trim().is_empty() {
                "UIPod"
            } else {
                state.title
            };
            let mut tp = Paint::color(pal.fg);
            tp.set_font(&semibold);
            tp.set_font_size(48.0);
            tp.set_text_align(Align::Center);
            tp.set_text_baseline(Baseline::Alphabetic);

            let title_cx = boxes.container.center_x();
            let half_avail = (title_cx - left_guard_x)
                .min(right_guard_x - title_cx)
                .max(60.0);
            let fitted = self.truncate_ellipsis(title, half_avail * 2.0, &tp);
            let base_y = Self::text_row_baseline(boxes.title_slot.center_y(), 48.0);
            let _ = self.canvas.fill_text(title_cx, base_y, &fitted, &tp);
        }

        // 4px outside bottom stroke spanning (32, 112, 960, 4) in fg1 (`VariableID:42`)
        let mut line = VgPath::new();
        line.rect(boxes.container.x, boxes.border_y, boxes.container.w, 4.0);
        self.canvas.fill_path(&line, &Paint::color(pal.fg1));
    }

    /// Draws a single `MenuItem` row (`h=60`, `r=8`, `padding=[0,16,0,16]` per `ui.figcraft` `[353]`).
    fn draw_menu_item(&mut self, bx: crate::layout::LayoutBox, row: &ListRow, pal: ResolvedTheme) {
        let text_ink = if row.selected {
            fill_rounded_rect(&mut self.canvas, bx.x, bx.y, bx.w, bx.h, 8.0, pal.fg);
            pal.bg
        } else {
            pal.fg
        };

        let bold = self.fonts_bold.clone();
        let semibold = self.fonts_semibold.clone();
        let medium = self.fonts_medium.clone();

        // Left filetype icon (32x32 at x + 16, gap = 16 before label) if present:
        // On unselected rows, `icon_dir` ([712]/[716]) uses `fg`, while `icon_filetype` ([849]/[925]) uses `fg2`.
        let text_x = if row.file_icon != FileIconKind::None {
            let (f_ink, f_cutout) = if row.selected {
                (pal.bg, pal.fg)
            } else if matches!(
                row.file_icon,
                FileIconKind::Folder | FileIconKind::AddFolder
            ) {
                (pal.fg, pal.bg)
            } else {
                (pal.fg2, pal.bg)
            };
            draw_filetype_icon(
                &mut self.canvas,
                bx.x + 16.0,
                bx.y + (bx.h - 32.0) * 0.5,
                &row.file_icon,
                f_ink,
                f_cutout,
                &bold,
            );
            bx.x + 16.0 + 32.0 + 16.0
        } else {
            bx.x + 16.0
        };

        // Right accessory (icon_arrow_s chevron, toggle switch, Active4 <Value>, or Active5 plain Value)
        let inner_right = bx.x + bx.w - 16.0;
        let right_limit = match row.right {
            RightAccessory::None => inner_right,
            RightAccessory::Chevron => {
                let cx = inner_right - 32.0;
                let cy = bx.y + (bx.h - 32.0) * 0.5;
                let chev_ink = if row.selected { pal.bg } else { pal.fg2 };
                draw_arrow_s(&mut self.canvas, cx, cy, chev_ink);
                cx - 16.0
            }
            RightAccessory::Toggle(enabled) => {
                let tx = inner_right - 64.0;
                let ty = bx.y + (bx.h - 32.0) * 0.5;
                // [50] enabled=True knob is fg1 inside fg pill when unselected; inverted to fg2 inside bg pill when selected
                let (t_ink, t_knob) = if row.selected {
                    (pal.bg, pal.fg2)
                } else {
                    (pal.fg, pal.fg1)
                };
                draw_toggle(&mut self.canvas, tx, ty, enabled, t_ink, t_knob);
                tx - 16.0
            }
            RightAccessory::CycleValue(val) => {
                let trimmed = val.trim();
                if trimmed.is_empty() {
                    inner_right
                } else {
                    let mut vp = Paint::color(text_ink);
                    vp.set_font(&medium);
                    vp.set_font_size(36.0);
                    vp.set_text_align(Align::Right);
                    vp.set_text_baseline(Baseline::Alphabetic);
                    let base_y = Self::text_row_baseline(bx.center_y(), 36.0);

                    if row.selected {
                        // ui.figcraft [1293]/[1186] Type=Active4: [icon_play_s_left] 10px [Value] 10px [icon_play_s]
                        let arrow_y = bx.y + (bx.h - 32.0) * 0.5;
                        let right_arrow_x = inner_right - 32.0;
                        draw_chevron_s(&mut self.canvas, right_arrow_x, arrow_y, text_ink);

                        let val_right_x = right_arrow_x - 10.0;
                        let max_val_w = (bx.w * 0.46).max(80.0);
                        let val_fitted = self.truncate_ellipsis(trimmed, max_val_w, &vp);
                        let val_w = self
                            .canvas
                            .measure_text(0.0, 0.0, &val_fitted, &vp)
                            .map(|m| m.width())
                            .unwrap_or(0.0);
                        let _ = self.canvas.fill_text(val_right_x, base_y, &val_fitted, &vp);

                        let left_arrow_x = val_right_x - val_w - 10.0 - 32.0;
                        draw_chevron_left_s(&mut self.canvas, left_arrow_x, arrow_y, text_ink);

                        (left_arrow_x - 16.0).max(text_x)
                    } else {
                        // ui.figcraft [1318] Type=NotActive4: right-aligned Value without arrows
                        let max_val_w = (bx.w * 0.50).max(80.0);
                        let val_fitted = self.truncate_ellipsis(trimmed, max_val_w, &vp);
                        let val_w = self
                            .canvas
                            .measure_text(0.0, 0.0, &val_fitted, &vp)
                            .map(|m| m.width())
                            .unwrap_or(0.0);
                        let _ = self.canvas.fill_text(inner_right, base_y, &val_fitted, &vp);
                        (inner_right - val_w - 24.0).max(text_x)
                    }
                }
            }
            RightAccessory::Value(val) => {
                // ui.figcraft [2035] Type=Active5 / [1318] NotActive4: plain right-aligned value without < > arrows
                let trimmed = val.trim();
                if trimmed.is_empty() {
                    inner_right
                } else if let Some(prefix) = trimmed
                    .strip_suffix(" ♪]")
                    .or_else(|| trimmed.strip_suffix(" \u{266A}]"))
                {
                    // Vector `[24 ♪]` chiptune count badge using ui.figcraft `icon_note_16` [404]
                    let mut vp = Paint::color(text_ink);
                    vp.set_font(&medium);
                    vp.set_font_size(36.0);
                    vp.set_text_align(Align::Right);
                    vp.set_text_baseline(Baseline::Alphabetic);
                    let base_y = Self::text_row_baseline(bx.center_y(), 36.0);

                    let rbracket = "]";
                    let rb_w = self
                        .canvas
                        .measure_text(0.0, 0.0, rbracket, &vp)
                        .map(|m| m.width())
                        .unwrap_or(12.0);
                    let _ = self.canvas.fill_text(inner_right, base_y, rbracket, &vp);

                    let note_sz = 25.0;
                    let note_x = inner_right - rb_w - 4.0 - note_sz;
                    let note_y = bx.center_y() - note_sz * 0.5;
                    draw_icon_note_scaled(&mut self.canvas, note_x, note_y, note_sz, text_ink);

                    let prefix_x = note_x - 6.0;
                    let prefix_w = self
                        .canvas
                        .measure_text(0.0, 0.0, prefix, &vp)
                        .map(|m| m.width())
                        .unwrap_or(48.0);
                    let _ = self.canvas.fill_text(prefix_x, base_y, prefix, &vp);
                    (prefix_x - prefix_w - 24.0).max(text_x)
                } else {
                    let mut vp = Paint::color(text_ink);
                    vp.set_font(&medium);
                    vp.set_font_size(36.0);
                    vp.set_text_align(Align::Right);
                    vp.set_text_baseline(Baseline::Alphabetic);
                    let base_y = Self::text_row_baseline(bx.center_y(), 36.0);
                    let max_val_w = (bx.w * 0.50).max(80.0);
                    let val_fitted = self.truncate_ellipsis(trimmed, max_val_w, &vp);
                    let val_w = self
                        .canvas
                        .measure_text(0.0, 0.0, &val_fitted, &vp)
                        .map(|m| m.width())
                        .unwrap_or(0.0);
                    let _ = self.canvas.fill_text(inner_right, base_y, &val_fitted, &vp);
                    (inner_right - val_w - 24.0).max(text_x)
                }
            }
            RightAccessory::ValueChevron(val) => {
                let cx = inner_right - 32.0;
                let cy = bx.y + (bx.h - 32.0) * 0.5;
                let chev_ink = if row.selected { pal.bg } else { pal.fg2 };
                draw_arrow_s(&mut self.canvas, cx, cy, chev_ink);
                let val_right = cx - 12.0;
                let trimmed = val.trim();
                if trimmed.is_empty() {
                    val_right - 4.0
                } else {
                    let mut vp = Paint::color(text_ink);
                    vp.set_font(&medium);
                    vp.set_font_size(36.0);
                    vp.set_text_align(Align::Right);
                    vp.set_text_baseline(Baseline::Alphabetic);
                    let base_y = Self::text_row_baseline(bx.center_y(), 36.0);
                    let max_val_w = (bx.w * 0.42).max(80.0);
                    let val_fitted = self.truncate_ellipsis(trimmed, max_val_w, &vp);
                    let val_w = self
                        .canvas
                        .measure_text(0.0, 0.0, &val_fitted, &vp)
                        .map(|m| m.width())
                        .unwrap_or(0.0);
                    let _ = self.canvas.fill_text(val_right, base_y, &val_fitted, &vp);
                    (val_right - val_w - 20.0).max(text_x)
                }
            }
        };

        // Main item label: Geologica SemiBold 48px
        let avail_w = (right_limit - text_x).max(0.0);
        if avail_w > 8.0 && !row.label.is_empty() {
            let mut lp = Paint::color(text_ink);
            lp.set_font(&semibold);
            lp.set_font_size(48.0);
            lp.set_text_align(Align::Left);
            lp.set_text_baseline(Baseline::Alphabetic);
            let fitted = self.truncate_ellipsis(row.label, avail_w, &lp);
            let base_y = Self::text_row_baseline(bx.center_y(), 48.0);
            let _ = self.canvas.fill_text(text_x, base_y, &fitted, &lp);
        }
    }

    fn ensure_album_art_uploaded(
        &mut self,
        rgb24: &[u8],
        mono: bool,
        theme: ThemeColors,
    ) -> Option<ImageId> {
        if rgb24.len() < 256 * 256 * 3 {
            return None;
        }
        let hash = fnv1a_art_hash(&rgb24[..256 * 256 * 3], mono, theme);
        if self.art_hash == Some(hash) {
            if let Some(id) = self.art_image {
                return Some(id);
            }
        }

        if mono {
            quantize_mono_art_4shade(&rgb24[..256 * 256 * 3], theme, &mut self.art_rgba_scratch);
        } else {
            for (src, dst) in rgb24[..256 * 256 * 3]
                .chunks_exact(3)
                .zip(self.art_rgba_scratch.iter_mut())
            {
                *dst = RGBA8::new(src[0], src[1], src[2], 255);
            }
        }

        let img_ref = ImgRef::new(&self.art_rgba_scratch, 256, 256);
        let id = match self.art_image {
            Some(existing) => {
                let _ = self.canvas.update_image(existing, img_ref, 0, 0);
                existing
            }
            None => self
                .canvas
                .create_image(img_ref, ImageFlags::NEAREST)
                .ok()?,
        };
        self.art_image = Some(id);
        self.art_hash = Some(hash);
        Some(id)
    }

    /// Draws `AlbumCover` `Type=NoCover` (`[1400]`): `r=8` `stroke=4 inside` (`fg1`),
    /// outer disc (`fg2`), inner label (`fg1`), center spindle (`fg`).
    fn draw_album_art_placeholder(&mut self, x: f32, y: f32, w: f32, h: f32, pal: ResolvedTheme) {
        stroke_rounded_rect_inside(&mut self.canvas, x, y, w, h, 8.0, 4.0, pal.fg1);
        let cx = x + w * 0.5;
        let cy = y + h * 0.5;
        let scale = (w.min(h) / 256.0).max(0.25);

        let mut outer_disc = VgPath::new();
        outer_disc.circle(cx, cy, 90.0 * scale);
        self.canvas.fill_path(&outer_disc, &Paint::color(pal.fg2));

        let mut label_cut = VgPath::new();
        label_cut.circle(cx, cy, 39.0 * scale);
        self.canvas.fill_path(&label_cut, &Paint::color(pal.fg1));

        let mut spindle = VgPath::new();
        spindle.circle(cx, cy, 4.0 * scale);
        self.canvas.fill_path(&spindle, &Paint::color(pal.fg));
    }

    /// Draws procedural 4-shade vector Famicom HVC cartridge artwork (`famicom_cart_spec`)
    /// inside `(x, y, w, h)` with top grip ribs, molded triangle arrow, stepped 60-pin skirt,
    /// recessed front sticker label, scissor-clipped `Geologica` Game Title + Artist, and
    /// high-contrast corner chip badge (`2A03` / `VRC6` / `MMC5` / `FDS` / `N163` / `5B` / `VRC7`).
    #[allow(clippy::too_many_arguments)]
    fn draw_famicom_cartridge(
        &mut self,
        x: f32,
        y: f32,
        w: f32,
        h: f32,
        game_title: &str,
        artist: &str,
        chip_badge: &str,
        pal: ResolvedTheme,
    ) {
        use crate::layout::famicom_cart_spec as fc;

        let s = (w.min(h) / fc::REF_SIZE).max(0.25);
        let ox = x + (w - fc::REF_SIZE * s) * 0.5;
        let oy = y + (h - fc::REF_SIZE * s) * 0.5;

        // Outer album cover frame (`fg1` inside stroke matching `[1400]`)
        stroke_rounded_rect_inside(
            &mut self.canvas,
            x,
            y,
            w,
            h,
            fc::FRAME_RADIUS,
            fc::FRAME_STROKE,
            pal.fg1,
        );

        // 1. Stepped bottom 60-pin connector skirt (`fg2` fill, `fg` stroke, `fg1` gold-finger pins)
        let sk_x = ox + fc::SKIRT_X * s;
        let sk_y = oy + fc::SKIRT_Y * s;
        let sk_w = fc::SKIRT_W * s;
        let sk_h = fc::SKIRT_H * s;
        fill_rounded_rect(
            &mut self.canvas,
            sk_x,
            sk_y,
            sk_w,
            sk_h,
            fc::SKIRT_RADIUS * s,
            pal.fg2,
        );
        stroke_rounded_rect_inside(
            &mut self.canvas,
            sk_x,
            sk_y,
            sk_w,
            sk_h,
            fc::SKIRT_RADIUS * s,
            (fc::CART_STROKE * s).max(1.5),
            pal.fg,
        );

        let pins_total_w = (fc::PIN_COUNT as f32) * fc::PIN_W * s
            + ((fc::PIN_COUNT - 1) as f32) * fc::PIN_GAP * s;
        let pin_start_x = sk_x + (sk_w - pins_total_w) * 0.5;
        let pin_y = sk_y + fc::PIN_TOP_INSET * s;
        let mut pins_path = VgPath::new();
        for i in 0..fc::PIN_COUNT {
            let px = pin_start_x + (i as f32) * (fc::PIN_W + fc::PIN_GAP) * s;
            pins_path.rect(px, pin_y, fc::PIN_W * s, fc::PIN_H * s);
        }
        self.canvas.fill_path(&pins_path, &Paint::color(pal.fg1));

        // 2. Main horizontal HVC cassette shell (`fg2` fill + crisp `fg` border)
        let cx = ox + fc::CART_X * s;
        let cy = oy + fc::CART_Y * s;
        let cw = fc::CART_W * s;
        let ch = fc::CART_H * s;
        fill_rounded_rect(
            &mut self.canvas,
            cx,
            cy,
            cw,
            ch,
            fc::CART_RADIUS * s,
            pal.fg2,
        );
        stroke_rounded_rect_inside(
            &mut self.canvas,
            cx,
            cy,
            cw,
            ch,
            fc::CART_RADIUS * s,
            (fc::CART_STROKE * s).max(1.5),
            pal.fg,
        );

        // 3. Top grip ribs (`fg1`) + molded downward insertion triangle (`fg1`)
        let mut ribs = VgPath::new();
        for r in 0..fc::GRIP_RIB_COUNT {
            let ry = oy + (fc::GRIP_Y0 + (r as f32) * fc::GRIP_RIB_STEP) * s;
            ribs.rounded_rect(
                ox + fc::GRIP_X * s,
                ry,
                fc::GRIP_W * s,
                fc::GRIP_RIB_H * s,
                1.5 * s,
            );
        }
        let acx = ox + fc::ARROW_CX * s;
        let ay0 = oy + fc::ARROW_TOP_Y * s;
        ribs.move_to(acx - fc::ARROW_HALF_W * s, ay0);
        ribs.line_to(acx + fc::ARROW_HALF_W * s, ay0);
        ribs.line_to(acx, ay0 + fc::ARROW_H * s);
        ribs.close();
        self.canvas.fill_path(&ribs, &Paint::color(pal.fg1));

        // 4. Recessed front sticker label (`bg` fill, `fg1` header band, `fg` border)
        let lx = ox + fc::LABEL_X * s;
        let ly = oy + fc::LABEL_Y * s;
        let lw = fc::LABEL_W * s;
        let lh = fc::LABEL_H * s;
        let lr = fc::LABEL_RADIUS * s;
        fill_rounded_rect(&mut self.canvas, lx, ly, lw, lh, lr, pal.bg);

        let band_h = fc::LABEL_BAND_H * s;
        self.canvas.scissor(lx, ly, lw, band_h);
        fill_rounded_rect(&mut self.canvas, lx, ly, lw, band_h + lr, lr, pal.fg1);
        self.canvas.reset_scissor();

        stroke_rounded_rect_inside(
            &mut self.canvas,
            lx,
            ly,
            lw,
            lh,
            lr,
            (fc::LABEL_STROKE * s).max(1.5),
            pal.fg,
        );

        let bold = self.fonts_bold.clone();
        let semibold = self.fonts_semibold.clone();
        let regular = self.fonts_regular.clone();

        // Top-left HVC cassette stamp + top-right expansion chip badge (`2A03`, `VRC6`, `MMC5`, `FDS`, ...)
        let badge_txt = if chip_badge.trim().is_empty() {
            "2A03"
        } else {
            chip_badge.trim()
        };
        let badge_fs = (fc::BADGE_FONT_SIZE * s).max(10.0);
        let mut bp = Paint::color(pal.bg);
        bp.set_font(&bold);
        bp.set_font_size(badge_fs);
        bp.set_text_align(Align::Center);
        bp.set_text_baseline(Baseline::Alphabetic);
        let btw = self
            .canvas
            .measure_text(0.0, 0.0, badge_txt, &bp)
            .map(|m| m.width())
            .unwrap_or(36.0 * s);
        let bw = (btw + fc::BADGE_PAD_X * 2.0 * s).max(40.0 * s);
        let bh = fc::BADGE_H * s;
        let bx = lx + lw - fc::BADGE_MARGIN_R * s - bw;
        let by = ly + (band_h - bh) * 0.5;
        fill_rounded_rect(
            &mut self.canvas,
            bx,
            by,
            bw,
            bh,
            fc::BADGE_RADIUS * s,
            pal.fg,
        );
        let badge_base_y = Self::cap_centered_baseline(by + bh * 0.5, badge_fs);
        let _ = self
            .canvas
            .fill_text(bx + bw * 0.5, badge_base_y, badge_txt, &bp);

        let hdr_fs = (fc::HEADER_FONT_SIZE * s).max(9.0);
        let mut hp = Paint::color(pal.fg);
        hp.set_font(&semibold);
        hp.set_font_size(hdr_fs);
        hp.set_text_align(Align::Left);
        hp.set_text_baseline(Baseline::Alphabetic);
        let hdr_base_y = Self::cap_centered_baseline(ly + band_h * 0.5, hdr_fs);
        let hdr_avail = (bx - (lx + fc::LABEL_PAD_X * s) - 6.0 * s).max(0.0);
        if hdr_avail > 20.0 * s {
            let hdr_txt = self.truncate_ellipsis("FAMICOM HVC", hdr_avail, &hp);
            let _ = self
                .canvas
                .fill_text(lx + fc::LABEL_PAD_X * s, hdr_base_y, &hdr_txt, &hp);
        }

        // 5. Scissor-clipped Game Title + Artist inside label body (guarantees zero text bleed!)
        let pad_x = fc::LABEL_PAD_X * s;
        let inner_x = lx + pad_x;
        let inner_w = (lw - pad_x * 2.0).max(16.0);
        self.canvas
            .scissor(lx + 3.0 * s, ly + band_h, lw - 6.0 * s, lh - band_h - 3.0 * s);

        let game = if game_title.trim().is_empty() {
            "NSF Cartridge"
        } else {
            game_title.trim()
        };
        let title_fs = (fc::TITLE_FONT_SIZE * s).max(12.0);
        let mut tp = Paint::color(pal.fg);
        tp.set_font(&semibold);
        tp.set_font_size(title_fs);
        tp.set_text_align(Align::Left);
        tp.set_text_baseline(Baseline::Alphabetic);
        let fitted_game = self.truncate_ellipsis(game, inner_w, &tp);
        let title_base_y = Self::text_row_baseline(oy + fc::TITLE_CENTER_Y * s, title_fs);
        let _ = self
            .canvas
            .fill_text(inner_x, title_base_y, &fitted_game, &tp);

        let art_str = if artist.trim().is_empty() {
            "2A03 Sound Engine"
        } else {
            artist.trim()
        };
        let artist_fs = (fc::ARTIST_FONT_SIZE * s).max(10.0);
        let mut ap = Paint::color(pal.fg2);
        ap.set_font(&regular);
        ap.set_font_size(artist_fs);
        ap.set_text_align(Align::Left);
        ap.set_text_baseline(Baseline::Alphabetic);
        let fitted_artist = self.truncate_ellipsis(art_str, inner_w, &ap);
        let artist_base_y = Self::text_row_baseline(oy + fc::ARTIST_CENTER_Y * s, artist_fs);
        let _ = self
            .canvas
            .fill_text(inner_x, artist_base_y, &fitted_artist, &ap);

        let mut rule = VgPath::new();
        rule.rect(
            inner_x,
            oy + fc::ACCENT_RULE_Y * s,
            inner_w,
            (fc::ACCENT_RULE_H * s).max(1.0),
        );
        self.canvas.fill_path(&rule, &Paint::color(pal.fg1));

        self.canvas.reset_scissor();
    }

    /// Draws the compact 4-shade hardware APU channel strip (`P1 P2 TR NS DP` + expansion chips)
    /// when `Large Art` (`alt_layout`) is off and an `.nsf` track is playing.
    /// Muted channels are dimmed and crossed-out in Shade 1 (`fg1`).
    fn draw_nsf_apu_strip(
        &mut self,
        x: f32,
        y: f32,
        max_w: f32,
        channels: &[NsfApuPill],
        pal: ResolvedTheme,
    ) {
        use crate::layout::nsf_now_playing_spec as spec;

        if channels.is_empty() || max_w < 80.0 {
            return;
        }
        let semibold = self.fonts_semibold.clone();
        let mut cursor_x = x;
        let right_edge = x + max_w;

        for ch in channels {
            let code = ch.short_code.trim();
            if code.is_empty() {
                continue;
            }

            let mut tp = Paint::color(if ch.muted {
                pal.fg1
            } else if ch.active {
                pal.fg
            } else {
                pal.fg2
            });
            tp.set_font(&semibold);
            tp.set_font_size(spec::CHIP_FONT_SIZE);
            tp.set_text_align(Align::Center);
            tp.set_text_baseline(Baseline::Alphabetic);

            let tw = self
                .canvas
                .measure_text(0.0, 0.0, code, &tp)
                .map(|m| m.width())
                .unwrap_or(28.0);
            let chip_w = (tw + spec::CHIP_PAD_X * 2.0)
                .max(spec::CHIP_MIN_W)
                .round();
            if cursor_x + chip_w > right_edge {
                break;
            }

            let border_col = if ch.muted {
                pal.fg1
            } else if ch.active {
                pal.fg
            } else {
                pal.fg2
            };
            stroke_rounded_rect_inside(
                &mut self.canvas,
                cursor_x,
                y,
                chip_w,
                spec::CHIP_H,
                spec::CHIP_RADIUS,
                spec::CHIP_STROKE,
                border_col,
            );

            let text_cy = if !ch.muted && ch.active {
                y + spec::CHIP_H * 0.45
            } else {
                y + spec::CHIP_H * 0.5
            };
            let base_y = Self::cap_centered_baseline(text_cy, spec::CHIP_FONT_SIZE);
            let _ = self
                .canvas
                .fill_text(cursor_x + chip_w * 0.5, base_y, code, &tp);

            if ch.muted {
                // Dimmed & crossed-out in Shade 1 (`fg1`) so muted channels are unmistakable
                let mut strike = VgPath::new();
                strike.rect(
                    cursor_x + 6.0,
                    y + (spec::CHIP_H - spec::MUTED_STRIKE_H) * 0.5,
                    (chip_w - 12.0).max(4.0),
                    spec::MUTED_STRIKE_H,
                );
                self.canvas.fill_path(&strike, &Paint::color(pal.fg1));
            } else if ch.active || ch.volume > 0.02 {
                let meter_max_w = (chip_w - spec::CHIP_METER_INSET_X * 2.0).max(4.0);
                let fill_w = (meter_max_w * ch.volume.clamp(0.15, 1.0)).round();
                fill_rounded_rect(
                    &mut self.canvas,
                    cursor_x + spec::CHIP_METER_INSET_X,
                    y + spec::CHIP_H - spec::CHIP_METER_BOTTOM_INSET - spec::CHIP_METER_H,
                    fill_w,
                    spec::CHIP_METER_H,
                    2.0,
                    pal.fg2,
                );
            }

            cursor_x += chip_w + spec::CHIP_GAP_X;
        }
    }

    fn draw_list_screen_sized(&mut self, state: &ListScreenState, m: LogicalMetrics) {
        self.last_theme = state.theme;
        let pal = resolve_theme(state.theme);
        self.begin_offscreen_sized(m, m.screen_h, pal.bg);

        let vis = if state.visible_items > 0 {
            state.visible_items
        } else {
            m.visible_rows
        };
        let show_scrollbar = state.total_items > vis;
        let boxes =
            compute_list_layout_sized(m.screen_w, m.screen_h, state.rows.len(), vis, show_scrollbar);

        self.draw_header_component(&boxes.header, &state.header, pal);

        for (row_data, row_box) in state.rows.iter().zip(boxes.rows.iter()) {
            self.draw_menu_item(*row_box, row_data, pal);
        }

        if let Some(sb) = boxes.scrollbar {
            draw_scrollbar_v(
                &mut self.canvas,
                sb.x,
                sb.y,
                sb.w,
                sb.h,
                state.total_items,
                state.start_item,
                vis,
                pal.fg1,
                pal.fg,
            );
        }
    }

    fn draw_list_screen(&mut self, state: &ListScreenState) {
        let m = LogicalMetrics::from_physical(WIDTH as u32, HEIGHT as u32);
        self.draw_list_screen_sized(state, m);
    }

    #[allow(clippy::too_many_arguments)]
    fn draw_dj_status_pill_right(
        &mut self,
        right_x: f32,
        top_y: f32,
        label: &str,
        progress: f32,
        bass_swap_flash: bool,
        centered: bool,
        center_x: f32,
        pal: ResolvedTheme,
    ) {
        use crate::layout::dj_status_spec as djs;

        let trimmed = label.trim();
        if trimmed.is_empty() {
            return;
        }
        let semibold = self.fonts_semibold.clone();
        let mut tp = Paint::color(if bass_swap_flash { pal.bg } else { pal.fg });
        tp.set_font(&semibold);
        tp.set_font_size(djs::FONT_SIZE);
        tp.set_text_align(Align::Center);
        tp.set_text_baseline(Baseline::Alphabetic);

        let tw = self
            .canvas
            .measure_text(0.0, 0.0, trimmed, &tp)
            .map(|m| m.width())
            .unwrap_or(180.0);
        let pw = (tw + djs::PILL_PAD_X * 2.0).round();
        let ph = djs::PILL_H;
        let px = if centered {
            (center_x - pw * 0.5).round()
        } else {
            (right_x - pw).round()
        };
        let py = top_y.round();

        if bass_swap_flash {
            fill_rounded_rect(&mut self.canvas, px, py, pw, ph, djs::PILL_RADIUS, pal.fg);
        } else {
            stroke_rounded_rect_inside(
                &mut self.canvas,
                px,
                py,
                pw,
                ph,
                djs::PILL_RADIUS,
                djs::PILL_STROKE,
                pal.fg2,
            );
        }

        let prog = progress.clamp(0.0, 1.0);
        if prog > 0.01 {
            let max_bw = (pw - djs::PROGRESS_INSET_X * 2.0).max(0.0);
            let bw = (max_bw * prog).round();
            if bw >= 2.0 {
                let by = py + ph - djs::PROGRESS_BOTTOM_INSET - djs::PROGRESS_BAR_H;
                let bar_col = if bass_swap_flash { pal.fg1 } else { pal.fg1 };
                fill_rounded_rect(
                    &mut self.canvas,
                    px + djs::PROGRESS_INSET_X,
                    by,
                    bw,
                    djs::PROGRESS_BAR_H,
                    1.5,
                    bar_col,
                );
            }
        }

        let base_y = Self::cap_centered_baseline(py + ph * 0.5 - 1.0, djs::FONT_SIZE);
        let _ = self.canvas.fill_text(px + pw * 0.5, base_y, trimmed, &tp);
    }

    fn draw_now_playing_screen_sized(&mut self, state: &NowPlayingState, m: LogicalMetrics) {
        self.last_theme = state.theme;
        let pal = resolve_theme(state.theme);
        self.begin_offscreen_sized(m, m.screen_h, pal.bg);

        if state.alt_layout {
            let alt = compute_now_playing_alt_layout_sized(m.screen_w, m.screen_h);
            let semibold = self.fonts_semibold.clone();

            let art_drawn = if let Some(rgb24) = state.art_rgb24 {
                if let Some(img_id) =
                    self.ensure_album_art_uploaded(rgb24, state.mono_album_art, state.theme)
                {
                    let paint = Paint::image(
                        img_id,
                        alt.art_box.x,
                        alt.art_box.y,
                        alt.art_box.w,
                        alt.art_box.h,
                        0.0,
                        1.0,
                    );
                    let mut rect = VgPath::new();
                    rect.rounded_rect(
                        alt.art_box.x,
                        alt.art_box.y,
                        alt.art_box.w,
                        alt.art_box.h,
                        8.0,
                    );
                    self.canvas.fill_path(&rect, &paint);
                    true
                } else {
                    false
                }
            } else {
                false
            };
            if !art_drawn {
                if state.is_nsf {
                    self.draw_famicom_cartridge(
                        alt.art_box.x,
                        alt.art_box.y,
                        alt.art_box.w,
                        alt.art_box.h,
                        state.album,
                        state.artist,
                        state.nsf_chip_badge,
                        pal,
                    );
                } else {
                    self.draw_album_art_placeholder(
                        alt.art_box.x,
                        alt.art_box.y,
                        alt.art_box.w,
                        alt.art_box.h,
                        pal,
                    );
                }
            }

            draw_seekbar_track_h_with_loop(
                &mut self.canvas,
                alt.seekbar_track.x,
                alt.seekbar_track.y,
                alt.seekbar_track.w,
                alt.seekbar_track.h,
                state.progress,
                state.nsf_loop_progress,
                pal.fg1,
                pal.fg2,
                pal.fg,
            );

            if !state.dj_status_label.trim().is_empty() {
                use crate::layout::dj_status_spec as djs;
                self.draw_dj_status_pill_right(
                    (m.screen_w + 320.0) * 0.5,
                    djs::ALT_TOP_Y,
                    state.dj_status_label,
                    state.dj_progress,
                    state.dj_bass_swap_flash,
                    true,
                    m.screen_w * 0.5,
                    pal,
                );
            }

            let label_base_y = Self::cap_centered_baseline(alt.seekbar_labels.center_y(), 32.0);
            let mut lp = Paint::color(pal.fg);
            lp.set_font(&semibold);
            lp.set_font_size(32.0);
            lp.set_text_baseline(Baseline::Alphabetic);

            if !state.center_status_label.trim().is_empty() {
                lp.set_text_align(Align::Center);
                let _ = self.canvas.fill_text(
                    alt.seekbar_labels.center_x(),
                    label_base_y,
                    state.center_status_label,
                    &lp,
                );
            } else {
                if !state.elapsed_label.is_empty() {
                    lp.set_text_align(Align::Left);
                    let _ = self.canvas.fill_text(
                        alt.seekbar_labels.x,
                        label_base_y,
                        state.elapsed_label,
                        &lp,
                    );
                }
                let mut rem_right = alt.seekbar_labels.x + alt.seekbar_labels.w;
                if state.nsf_endless_loop {
                    use crate::layout::nsf_now_playing_spec as spec;
                    let bx = rem_right - spec::ENDLESS_BADGE_W;
                    let by = alt.seekbar_labels.center_y() - spec::ENDLESS_BADGE_H * 0.5;
                    draw_infinity_badge(
                        &mut self.canvas,
                        bx,
                        by,
                        spec::ENDLESS_BADGE_W,
                        spec::ENDLESS_BADGE_H,
                        pal.fg2,
                        pal.fg,
                    );
                    rem_right = bx - spec::ENDLESS_BADGE_GAP_X;
                }
                if !state.remaining_label.is_empty() {
                    lp.set_text_align(Align::Right);
                    let _ = self
                        .canvas
                        .fill_text(rem_right, label_base_y, state.remaining_label, &lp);
                }
            }
            return;
        }

        let np = compute_now_playing_layout_sized(m.screen_w, m.screen_h);
        self.draw_header_component(&np.header, &state.header, pal);

        let regular = self.fonts_regular.clone();
        let semibold = self.fonts_semibold.clone();

        // 1. Sub-header row [247] (y = 144, h = 40): "21 of 500" (Regular 32px, ls=2%) + optional [EQ] badge
        if !state.track_pos_label.is_empty() {
            let mut pp = Paint::color(pal.fg);
            pp.set_font(&regular);
            pp.set_font_size(32.0);
            pp.set_letter_spacing(32.0 * 0.02); // 2% letter-spacing per [248]
            pp.set_text_align(Align::Left);
            pp.set_text_baseline(Baseline::Alphabetic);
            let base_y = Self::cap_centered_baseline(np.subheader_row.center_y(), 32.0);
            let _ = self
                .canvas
                .fill_text(np.subheader_row.x, base_y, state.track_pos_label, &pp);
        }

        let mut sub_right = np.subheader_row.x + np.subheader_row.w;
        if state.eq_enabled {
            // [249]: 55x34 at right edge (x = 937, y = 147), r=4, stroke=2 inside
            let eq_w = 55.0;
            let eq_h = 34.0;
            let eq_x = sub_right - eq_w;
            let eq_y = np.subheader_row.y + (np.subheader_row.h - eq_h) * 0.5;
            stroke_rounded_rect_inside(
                &mut self.canvas,
                eq_x,
                eq_y,
                eq_w,
                eq_h,
                4.0,
                2.0,
                pal.fg,
            );

            let mut eqp = Paint::color(pal.fg);
            eqp.set_font(&semibold);
            eqp.set_font_size(24.0);
            eqp.set_text_align(Align::Center);
            eqp.set_text_baseline(Baseline::Alphabetic);
            let base_y = Self::cap_centered_baseline(eq_y + eq_h * 0.5, 24.0);
            let _ = self
                .canvas
                .fill_text(eq_x + eq_w * 0.5, base_y, "EQ", &eqp);
            sub_right = eq_x - crate::layout::dj_status_spec::PILL_GAP_X;
        }

        if !state.dj_status_label.trim().is_empty() {
            use crate::layout::dj_status_spec as djs;
            let py = np.subheader_row.y + (np.subheader_row.h - djs::PILL_H) * 0.5;
            self.draw_dj_status_pill_right(
                sub_right,
                py,
                state.dj_status_label,
                state.dj_progress,
                state.dj_bass_swap_flash,
                false,
                0.0,
                pal,
            );
        }

        // 2. Album Art [252] (32, 256, 256, 256): r=8 image without stroke (placeholder keeps 4px border)
        let art_drawn = if let Some(rgb24) = state.art_rgb24 {
            if let Some(img_id) =
                self.ensure_album_art_uploaded(rgb24, state.mono_album_art, state.theme)
            {
                let paint = Paint::image(
                    img_id,
                    np.art_box.x,
                    np.art_box.y,
                    np.art_box.w,
                    np.art_box.h,
                    0.0,
                    1.0,
                );
                let mut rect = VgPath::new();
                rect.rounded_rect(np.art_box.x, np.art_box.y, np.art_box.w, np.art_box.h, 8.0);
                self.canvas.fill_path(&rect, &paint);
                true
            } else {
                false
            }
        } else {
            false
        };
        if !art_drawn {
            if state.is_nsf {
                self.draw_famicom_cartridge(
                    np.art_box.x,
                    np.art_box.y,
                    np.art_box.w,
                    np.art_box.h,
                    state.album,
                    state.artist,
                    state.nsf_chip_badge,
                    pal,
                );
            } else {
                self.draw_album_art_placeholder(
                    np.art_box.x,
                    np.art_box.y,
                    np.art_box.w,
                    np.art_box.h,
                    pal,
                );
            }
        }

        // 3. Metadata column [253]: Title (SemiBold 48), Artist (Regular 36 ls=2%), Album (Regular 36 ls=2%), Specs row
        if !state.title.is_empty() {
            let mut tp = Paint::color(pal.fg);
            tp.set_font(&semibold);
            tp.set_font_size(48.0);
            tp.set_text_align(Align::Left);
            tp.set_text_baseline(Baseline::Alphabetic);
            let fitted = self.truncate_ellipsis(state.title, np.title_box.w, &tp);
            let base_y = Self::text_row_baseline(np.title_box.center_y(), 48.0);
            let _ = self.canvas.fill_text(np.title_box.x, base_y, &fitted, &tp);
        }

        if !state.artist.is_empty() {
            let mut ap = Paint::color(pal.fg);
            ap.set_font(&regular);
            ap.set_font_size(36.0);
            ap.set_letter_spacing(36.0 * 0.02);
            ap.set_text_align(Align::Left);
            ap.set_text_baseline(Baseline::Alphabetic);
            let fitted = self.truncate_ellipsis(state.artist, np.artist_box.w, &ap);
            let base_y = Self::text_row_baseline(np.artist_box.center_y(), 36.0);
            let _ = self
                .canvas
                .fill_text(np.artist_box.x, base_y, &fitted, &ap);
        }

        if !state.album.is_empty() {
            let mut alp = Paint::color(pal.fg);
            alp.set_font(&regular);
            alp.set_font_size(36.0);
            alp.set_letter_spacing(36.0 * 0.02);
            alp.set_text_align(Align::Left);
            alp.set_text_baseline(Baseline::Alphabetic);
            let fitted = self.truncate_ellipsis(state.album, np.album_box.w, &alp);
            let base_y = Self::text_row_baseline(np.album_box.center_y(), 36.0);
            let _ = self
                .canvas
                .fill_text(np.album_box.x, base_y, &fitted, &alp);
        }

        // Audio specs row [258]/[259]: [CODEC] badge + 21px gap + bitdepth + 21px gap + samplerate
        let mut cursor_x = np.specs_row.x;
        let specs_cy = np.specs_row.center_y();
        let specs_base_y = Self::cap_centered_baseline(specs_cy, 24.0);

        let codec = state.codec_label.trim();
        if !codec.is_empty() {
            let mut cp = Paint::color(pal.fg);
            cp.set_font(&semibold);
            cp.set_font_size(24.0);
            cp.set_text_align(Align::Center);
            cp.set_text_baseline(Baseline::Alphabetic);

            let tw = self
                .canvas
                .measure_text(0.0, 0.0, codec, &cp)
                .map(|m| m.width())
                .unwrap_or(50.0);
            let badge_w = (tw + 20.0).max(52.0).round();
            let badge_h = 34.0;
            let badge_y = np.specs_row.y + (np.specs_row.h - badge_h) * 0.5;
            stroke_rounded_rect_inside(
                &mut self.canvas,
                cursor_x,
                badge_y,
                badge_w,
                badge_h,
                4.0,
                2.0,
                pal.fg,
            );
            let _ = self
                .canvas
                .fill_text(cursor_x + badge_w * 0.5, specs_base_y, codec, &cp);
            cursor_x += badge_w + 21.0;
        }

        let mut sp = Paint::color(pal.fg);
        sp.set_font(&regular);
        sp.set_font_size(24.0);
        sp.set_text_align(Align::Left);
        sp.set_text_baseline(Baseline::Alphabetic);

        let bitdepth = state.bitdepth_label.trim();
        if !bitdepth.is_empty() {
            let _ = self.canvas.fill_text(cursor_x, specs_base_y, bitdepth, &sp);
            let bw = self
                .canvas
                .measure_text(0.0, 0.0, bitdepth, &sp)
                .map(|m| m.width())
                .unwrap_or(56.0);
            cursor_x += bw + 21.0;
        }

        let srate = state.samplerate_label.trim();
        if !srate.is_empty() {
            let _ = self.canvas.fill_text(cursor_x, specs_base_y, srate, &sp);
        }

        // 3b. Compact 4-shade hardware APU channel strip (`P1 P2 TR NS DP ...`) when Large Art is off
        if state.is_nsf && !state.nsf_channels.is_empty() {
            use crate::layout::nsf_now_playing_spec as spec;
            let strip_bottom_max = (np.seekbar_track.y - 14.0).max(0.0);
            let strip_y = (np.art_box.y + np.art_box.h + spec::STRIP_TOP_OFFSET_FROM_ART)
                .min(strip_bottom_max - spec::CHIP_H);
            if strip_y > np.art_box.y + np.art_box.h + 4.0 {
                self.draw_nsf_apu_strip(
                    np.seekbar_track.x,
                    strip_y,
                    np.seekbar_track.w,
                    state.nsf_channels,
                    pal,
                );
            }
        }

        // 4. Seekbar [640] (32, 594, 960, 84): horizontal track 960x24 (`fg1` border, `fg2` fill, optional loop tick)
        draw_seekbar_track_h_with_loop(
            &mut self.canvas,
            np.seekbar_track.x,
            np.seekbar_track.y,
            np.seekbar_track.w,
            np.seekbar_track.h,
            state.progress,
            state.nsf_loop_progress,
            pal.fg1,
            pal.fg2,
            pal.fg,
        );

        let label_base_y = Self::cap_centered_baseline(np.seekbar_labels.center_y(), 32.0);
        let mut lp = Paint::color(pal.fg);
        lp.set_font(&semibold);
        lp.set_font_size(32.0);
        lp.set_text_baseline(Baseline::Alphabetic);

        if !state.center_status_label.trim().is_empty() {
            lp.set_text_align(Align::Center);
            let _ = self.canvas.fill_text(
                np.seekbar_labels.center_x(),
                label_base_y,
                state.center_status_label,
                &lp,
            );
        } else {
            if !state.elapsed_label.is_empty() {
                lp.set_text_align(Align::Left);
                let _ = self
                    .canvas
                    .fill_text(np.seekbar_labels.x, label_base_y, state.elapsed_label, &lp);
            }
            let mut rem_right = np.seekbar_labels.x + np.seekbar_labels.w;
            if state.nsf_endless_loop {
                use crate::layout::nsf_now_playing_spec as spec;
                let bx = rem_right - spec::ENDLESS_BADGE_W;
                let by = np.seekbar_labels.center_y() - spec::ENDLESS_BADGE_H * 0.5;
                draw_infinity_badge(
                    &mut self.canvas,
                    bx,
                    by,
                    spec::ENDLESS_BADGE_W,
                    spec::ENDLESS_BADGE_H,
                    pal.fg2,
                    pal.fg,
                );
                rem_right = bx - spec::ENDLESS_BADGE_GAP_X;
            }
            if !state.remaining_label.is_empty() {
                lp.set_text_align(Align::Right);
                let _ = self
                    .canvas
                    .fill_text(rem_right, label_base_y, state.remaining_label, &lp);
            }
        }
    }

    fn draw_now_playing_screen(&mut self, state: &NowPlayingState) {
        let m = LogicalMetrics::from_physical(WIDTH as u32, HEIGHT as u32);
        self.draw_now_playing_screen_sized(state, m);
    }

    fn draw_keyboard_screen_sized(&mut self, state: &KeyboardState, m: LogicalMetrics) {
        self.last_theme = state.theme;
        let pal = resolve_theme(state.theme);
        self.begin_offscreen_sized(m, m.screen_h, pal.bg);

        let semibold = self.fonts_semibold.clone();
        let margin = horizontal_margin(m.screen_w);
        let inner_w = (m.screen_w - margin * 2.0).max(320.0);

        // 1. Input box: r=8, 4px inside border in fg1, text in fg, caret in fg2
        let fx = margin;
        let fy = 32.0_f32;
        let fw = inner_w;
        let fh = 96.0_f32;
        stroke_rounded_rect_inside(&mut self.canvas, fx, fy, fw, fh, 8.0, 4.0, pal.fg1);

        let mut ip = Paint::color(pal.fg);
        ip.set_font(&semibold);
        ip.set_font_size(46.0);
        ip.set_text_align(Align::Left);
        ip.set_text_baseline(Baseline::Alphabetic);

        let text_pad = 20.0_f32;
        let vis = self.visible_tail_utf8(state.text, fw - text_pad * 2.0 - 16.0, &ip);
        let tw = if vis.is_empty() {
            0.0
        } else {
            self.canvas
                .measure_text(0.0, 0.0, &vis, &ip)
                .map(|m| m.width())
                .unwrap_or(0.0)
        };
        if !vis.is_empty() {
            let base_y = Self::text_row_baseline(fy + fh * 0.5, 46.0);
            let _ = self.canvas.fill_text(fx + text_pad, base_y, &vis, &ip);
        }
        let caret_x = (fx + text_pad + tw + 4.0).min(fx + fw - text_pad - 6.0);
        fill_rounded_rect(&mut self.canvas, caret_x, fy + 20.0, 6.0, fh - 40.0, 3.0, pal.fg2);

        // 2. Staggered 5-row QWERTY key grid adapting cleanly to `(m.screen_w, m.screen_h)`
        let gap = 12.0_f32;
        let grid_top = 156.0_f32;
        let avail_grid_h = (m.screen_h - grid_top - 32.0).max(400.0);
        let key_h = ((avail_grid_h - 4.0 * gap) / 5.0).clamp(80.0, 140.0);
        let row_step = key_h + gap;
        let char_key_w = (inner_w - 9.0 * gap) / 10.0;

        for (r_idx, row) in state.rows.iter().enumerate() {
            let n = row.len();
            if n == 0 {
                continue;
            }
            let kw = if r_idx == 4 {
                (inner_w - ((n - 1) as f32) * gap) / (n as f32)
            } else {
                char_key_w
            };
            let row_w = (n as f32) * kw + ((n - 1) as f32) * gap;
            let start_x = (m.screen_w - row_w) * 0.5;
            let y = grid_top + (r_idx as f32) * row_step;

            for (c_idx, key) in row.iter().enumerate() {
                let x = start_x + (c_idx as f32) * (kw + gap);
                let text_color = if key.selected {
                    fill_rounded_rect(&mut self.canvas, x, y, kw, key_h, 8.0, pal.fg);
                    pal.bg
                } else if key.is_active_toggle {
                    fill_rounded_rect(&mut self.canvas, x, y, kw, key_h, 8.0, pal.fg2);
                    pal.bg
                } else {
                    stroke_rounded_rect_inside(
                        &mut self.canvas,
                        x,
                        y,
                        kw,
                        key_h,
                        8.0,
                        4.0,
                        pal.fg1,
                    );
                    if key.is_modifier {
                        pal.fg2
                    } else {
                        pal.fg
                    }
                };

                if !key.label.is_empty() {
                    let fsz = if key.is_modifier { 34.0 } else { 44.0 };
                    let mut kp = Paint::color(text_color);
                    kp.set_font(&semibold);
                    kp.set_font_size(fsz);
                    kp.set_text_align(Align::Center);
                    kp.set_text_baseline(Baseline::Alphabetic);
                    let base_y = Self::text_row_baseline(y + key_h * 0.5, fsz);
                    let _ = self
                        .canvas
                        .fill_text(x + kw * 0.5, base_y, key.label, &kp);
                }
            }
        }
    }

    fn draw_keyboard_screen(&mut self, state: &KeyboardState) {
        let m = LogicalMetrics::from_physical(WIDTH as u32, HEIGHT as u32);
        self.draw_keyboard_screen_sized(state, m);
    }

    fn draw_graphic_eq_screen_sized(&mut self, state: &GraphicEqState, m: LogicalMetrics) {
        const COL_LABELS: [&str; 11] = [
            "Pre", "31", "63", "125", "250", "500", "1k", "2k", "4k", "8k", "16k",
        ];
        const HEADER_PREFIXES: [&str; 11] = [
            "Pre", "31Hz", "63Hz", "125Hz", "250Hz", "500Hz", "1kHz", "2kHz", "4kHz", "8kHz",
            "16kHz",
        ];

        self.last_theme = state.theme;
        let pal = resolve_theme(state.theme);
        self.begin_offscreen_sized(m, m.screen_h, pal.bg);

        let sel = state.selected_col.min(10);
        let cur_tenths: i32 = if sel == 0 {
            state.preamp_tenths_db as i32
        } else {
            state.band_gains_tenths_db[sel - 1] as i32
        };

        let adj_title = if state.adjusting {
            let prefix = HEADER_PREFIXES[sel];
            if cur_tenths > 0 {
                format!("{prefix}: +{}.{}", cur_tenths / 10, cur_tenths % 10)
            } else if cur_tenths < 0 {
                let av = -cur_tenths;
                format!("{prefix}: -{}.{}", av / 10, av % 10)
            } else {
                format!("{prefix}: 0.0")
            }
        } else {
            String::new()
        };

        let mut hdr = state.header;
        if state.adjusting {
            hdr.title = &adj_title;
        }
        let header_boxes = compute_header_layout_sized(m.screen_w, m.screen_h);
        self.draw_header_component(&header_boxes, &hdr, pal);

        // Exact ui.figcraft [3215] Graphic Equalizer geometry:
        // Header ends at y=112, Frame 28 paddingTop=32 -> sliders start at y=144.
        // Assembly height on 1024x768 is 572 (track_h=516, gap=16, label_h=40, underline at +572).
        let margin = horizontal_margin(m.screen_w);
        let content_x = margin;
        let content_w = (m.screen_w - margin * 2.0).max(360.0);
        let top_y = 144.0_f32;
        let assembly_h = (m.screen_h - top_y - 52.0).max(280.0);
        let track_h = (assembly_h - 56.0).max(220.0);
        let label_cy = top_y + track_h + 16.0 + 20.0;
        let underline_y = top_y + track_h + 56.0;

        let pre_gap = 32.0_f32;
        let unit_w = (content_w - pre_gap) / 11.0;
        let pre_col_w = unit_w * (87.27273 / 84.36364);
        let band_col_w = (content_w - pre_col_w - pre_gap) / 10.0;

        let semibold = self.fonts_semibold.clone();
        let label_base_y = Self::cap_centered_baseline(label_cy, 32.0);

        for col in 0..11usize {
            let (col_x, col_w) = if col == 0 {
                (content_x, pre_col_w)
            } else {
                (
                    content_x + pre_col_w + pre_gap + ((col - 1) as f32) * band_col_w,
                    band_col_w,
                )
            };
            let is_sel = col == sel;
            let tenths = if col == 0 {
                state.preamp_tenths_db as f32
            } else {
                state.band_gains_tenths_db[col - 1] as f32
            };

            // Map -12.0 dB..+12.0 dB (-120..+120 tenths) onto 0.0..=1.0 (0.0 dB = 0.5 center)
            let norm = ((tenths + 120.0) / 240.0).clamp(0.0, 1.0);

            let track_w = 24.0_f32;
            let track_x = (col_x + (col_w - track_w) * 0.5).round();
            let stroke_col = if is_sel { pal.fg2 } else { pal.fg1 };
            stroke_rounded_rect_inside(
                &mut self.canvas,
                track_x,
                top_y,
                track_w,
                track_h,
                12.0,
                4.0,
                stroke_col,
            );

            let max_fill_h = (track_h - 8.0).max(16.0);
            let raw_fill_h = (norm * max_fill_h).round();
            if raw_fill_h > 0.5 {
                let fill_h = raw_fill_h.clamp(16.0, max_fill_h);
                let fill_y = top_y + track_h - 4.0 - fill_h;
                let fill_col = if state.bypassed {
                    pal.fg1
                } else if is_sel {
                    pal.fg
                } else {
                    pal.fg2
                };
                fill_rounded_rect(
                    &mut self.canvas,
                    track_x + 4.0,
                    fill_y,
                    16.0,
                    fill_h,
                    8.0,
                    fill_col,
                );
            }

            let txt_col = if is_sel { pal.fg } else { pal.fg2 };
            let mut tp = Paint::color(txt_col);
            tp.set_font(&semibold);
            tp.set_font_size(32.0);
            tp.set_text_align(Align::Center);
            tp.set_text_baseline(Baseline::Alphabetic);

            let cx = col_x + col_w * 0.5;
            let lbl = COL_LABELS[col];
            let _ = self.canvas.fill_text(cx, label_base_y, lbl, &tp);

            if is_sel {
                let tw = self
                    .canvas
                    .measure_text(0.0, 0.0, lbl, &tp)
                    .map(|m| m.width())
                    .unwrap_or(40.0);
                let ul_w = (tw + 4.0).max(44.0).round();
                let ul_x = (cx - ul_w * 0.5).round();
                fill_rounded_rect(
                    &mut self.canvas,
                    ul_x,
                    underline_y - 2.0,
                    ul_w,
                    4.0,
                    2.0,
                    pal.fg,
                );
            }
        }
    }

    fn draw_graphic_eq_screen(&mut self, state: &GraphicEqState) {
        let m = LogicalMetrics::from_physical(WIDTH as u32, HEIGHT as u32);
        self.draw_graphic_eq_screen_sized(state, m);
    }

    /// Draws the interactive `Now Playing -> Channels` live APU channel toggle screen (`1024x768`).
    fn draw_nsf_channels_screen_sized(&mut self, state: &NsfChannelsState, m: LogicalMetrics) {
        use crate::layout::nsf_channels_spec as cs;

        self.last_theme = state.theme;
        let pal = resolve_theme(state.theme);
        self.begin_offscreen_sized(m, m.screen_h, pal.bg);

        let has_enable_all = !state.enable_all_label.trim().is_empty();
        let total_items = state.channels.len() + usize::from(has_enable_all);

        let has_footer = !state.footer_hint.trim().is_empty();
        let avail_list_h = (m.screen_h - 144.0 - 32.0 - if has_footer { cs::FOOTER_H + 12.0 } else { 0.0 })
            .max(240.0);
        let auto_vis = (((avail_list_h + cs::ROW_GAP) / (cs::ROW_HEIGHT + cs::ROW_GAP)).floor()
            as usize)
            .clamp(5, 16);
        let vis = if state.visible_items > 0 {
            state.visible_items.min(auto_vis)
        } else {
            auto_vis
        };

        let show_scrollbar = total_items > vis;
        let hdr = compute_header_layout_sized(m.screen_w, m.screen_h);
        self.draw_header_component(&hdr, &state.header, pal);

        let mx = horizontal_margin(m.screen_w);
        let content_w = (m.screen_w - mx * 2.0).max(320.0);
        let list_w = if show_scrollbar {
            content_w - 32.0
        } else {
            content_w
        };
        let list_top_y = 144.0;

        let max_start = total_items.saturating_sub(vis);
        let start = state.start_item.min(max_start);
        let end = (start + vis).min(total_items);

        let bold = self.fonts_bold.clone();
        let semibold = self.fonts_semibold.clone();
        let medium = self.fonts_medium.clone();
        let regular = self.fonts_regular.clone();

        for (slot, item_idx) in (start..end).enumerate() {
            let rx = mx;
            let ry = list_top_y + (slot as f32) * (cs::ROW_HEIGHT + cs::ROW_GAP);
            let rw = list_w;
            let rh = cs::ROW_HEIGHT;

            if item_idx < state.channels.len() {
                let ch = &state.channels[item_idx];
                let row_ink = if ch.selected {
                    fill_rounded_rect(&mut self.canvas, rx, ry, rw, rh, cs::ROW_RADIUS, pal.fg);
                    pal.bg
                } else if ch.enabled {
                    pal.fg
                } else {
                    pal.fg2
                };

                // 1. Left hardware short-code badge (`P1`, `TR`, `SAW`, ...)
                let bx = rx + cs::ROW_PAD_X;
                let by = ry + (rh - cs::CODE_BADGE_H) * 0.5;
                let badge_border = if ch.selected {
                    pal.bg
                } else if ch.enabled {
                    pal.fg2
                } else {
                    pal.fg1
                };
                stroke_rounded_rect_inside(
                    &mut self.canvas,
                    bx,
                    by,
                    cs::CODE_BADGE_W,
                    cs::CODE_BADGE_H,
                    cs::CODE_BADGE_RADIUS,
                    cs::CODE_BADGE_STROKE,
                    badge_border,
                );
                let mut cp = Paint::color(if ch.selected {
                    pal.bg
                } else if ch.enabled {
                    pal.fg
                } else {
                    pal.fg1
                });
                cp.set_font(&bold);
                cp.set_font_size(cs::CODE_FONT_SIZE);
                cp.set_text_align(Align::Center);
                cp.set_text_baseline(Baseline::Alphabetic);
                let code_base_y =
                    Self::cap_centered_baseline(by + cs::CODE_BADGE_H * 0.5, cs::CODE_FONT_SIZE);
                let _ = self.canvas.fill_text(
                    bx + cs::CODE_BADGE_W * 0.5,
                    code_base_y,
                    ch.short_code,
                    &cp,
                );

                // 2. Right `[ON]` / `[OFF]` state label + `64x32` 4-shade `toggle` pill switch
                let inner_right = rx + rw - cs::ROW_PAD_X;
                let tx = inner_right - cs::TOGGLE_W;
                let ty = ry + (rh - cs::TOGGLE_H) * 0.5;
                let (t_ink, t_knob) = if ch.selected {
                    (pal.bg, pal.fg2)
                } else {
                    (pal.fg, pal.fg1)
                };
                draw_toggle(&mut self.canvas, tx, ty, ch.enabled, t_ink, t_knob);

                let state_str = if ch.enabled { "[ON]" } else { "[OFF]" };
                let mut sp = Paint::color(if ch.selected {
                    pal.bg
                } else if ch.enabled {
                    pal.fg
                } else {
                    pal.fg1
                });
                sp.set_font(&semibold);
                sp.set_font_size(cs::STATE_TEXT_FONT_SIZE);
                sp.set_text_align(Align::Right);
                sp.set_text_baseline(Baseline::Alphabetic);
                let state_right_x = tx - cs::STATE_TEXT_GAP_X;
                let state_base_y =
                    Self::cap_centered_baseline(ry + rh * 0.5, cs::STATE_TEXT_FONT_SIZE);
                let state_w = self
                    .canvas
                    .measure_text(0.0, 0.0, state_str, &sp)
                    .map(|m| m.width())
                    .unwrap_or(68.0);
                let _ = self
                    .canvas
                    .fill_text(state_right_x, state_base_y, state_str, &sp);

                // 3. Live `ApuFrameTelemetry` activity meter bar right beside `[ON]`/`[OFF]`
                let meter_x = state_right_x - state_w - cs::METER_GAP_X - cs::METER_W;
                let meter_y = ry + (rh - cs::METER_H) * 0.5;
                let meter_border = if ch.selected {
                    pal.fg1
                } else {
                    pal.fg1
                };
                stroke_rounded_rect_inside(
                    &mut self.canvas,
                    meter_x,
                    meter_y,
                    cs::METER_W,
                    cs::METER_H,
                    cs::METER_RADIUS,
                    cs::METER_STROKE,
                    meter_border,
                );
                let vol = if ch.active {
                    ch.volume.clamp(0.08, 1.0)
                } else {
                    ch.volume.clamp(0.0, 1.0)
                };
                let max_fill_w = (cs::METER_W - cs::METER_INSET * 2.0).max(0.0);
                let fill_w = (max_fill_w * vol).round();
                if fill_w >= 2.0 {
                    let fill_col = if !ch.enabled {
                        pal.fg1
                    } else if ch.selected {
                        pal.bg
                    } else {
                        pal.fg2
                    };
                    fill_rounded_rect(
                        &mut self.canvas,
                        meter_x + cs::METER_INSET,
                        meter_y + cs::METER_INSET,
                        fill_w,
                        cs::METER_H - cs::METER_INSET * 2.0,
                        (cs::METER_H - cs::METER_INSET * 2.0) * 0.5,
                        fill_col,
                    );
                }

                // 4. Channel Display Name + optional `[SOLO]` pill
                let name_x = bx + cs::CODE_BADGE_W + cs::CODE_GAP_RIGHT;
                let mut name_right_limit = (meter_x - cs::METER_GAP_X).max(name_x);
                if ch.soloed {
                    let mut sol_p = Paint::color(if ch.selected { pal.fg } else { pal.bg });
                    sol_p.set_font(&bold);
                    sol_p.set_font_size(cs::SOLO_FONT_SIZE);
                    sol_p.set_text_align(Align::Center);
                    sol_p.set_text_baseline(Baseline::Alphabetic);
                    let stw = self
                        .canvas
                        .measure_text(0.0, 0.0, "SOLO", &sol_p)
                        .map(|m| m.width())
                        .unwrap_or(52.0);
                    let sbw = (stw + cs::SOLO_BADGE_PAD_X * 2.0).round();
                    let sbx = name_right_limit - sbw;
                    let sby = ry + (rh - cs::SOLO_BADGE_H) * 0.5;
                    if sbx > name_x + 80.0 {
                        fill_rounded_rect(
                            &mut self.canvas,
                            sbx,
                            sby,
                            sbw,
                            cs::SOLO_BADGE_H,
                            5.0,
                            if ch.selected { pal.bg } else { pal.fg },
                        );
                        let sol_base_y = Self::cap_centered_baseline(
                            sby + cs::SOLO_BADGE_H * 0.5,
                            cs::SOLO_FONT_SIZE,
                        );
                        let _ = self.canvas.fill_text(
                            sbx + sbw * 0.5,
                            sol_base_y,
                            "SOLO",
                            &sol_p,
                        );
                        name_right_limit = sbx - cs::SOLO_GAP_X;
                    }
                }

                let avail_name_w = (name_right_limit - name_x).max(0.0);
                if avail_name_w > 12.0 {
                    let mut np = Paint::color(row_ink);
                    np.set_font(&medium);
                    np.set_font_size(cs::NAME_FONT_SIZE);
                    np.set_text_align(Align::Left);
                    np.set_text_baseline(Baseline::Alphabetic);
                    let fitted = self.truncate_ellipsis(ch.display_name, avail_name_w, &np);
                    let base_y = Self::text_row_baseline(ry + rh * 0.5, cs::NAME_FONT_SIZE);
                    let _ = self.canvas.fill_text(name_x, base_y, &fitted, &np);
                }
            } else {
                // Bottom convenience row: `Enable All Channels`
                let action_row = ListRow {
                    label: state.enable_all_label,
                    selected: state.enable_all_selected,
                    file_icon: FileIconKind::None,
                    right: RightAccessory::None,
                };
                self.draw_menu_item(
                    crate::layout::LayoutBox {
                        x: rx,
                        y: ry,
                        w: rw,
                        h: rh,
                    },
                    &action_row,
                    pal,
                );
            }
        }

        if show_scrollbar {
            let sb_h = (vis as f32) * cs::ROW_HEIGHT + ((vis.saturating_sub(1)) as f32) * cs::ROW_GAP;
            draw_scrollbar_v(
                &mut self.canvas,
                mx + content_w - 16.0,
                list_top_y,
                16.0,
                sb_h,
                total_items,
                start,
                vis,
                pal.fg1,
                pal.fg,
            );
        }

        if has_footer {
            let fy = m.screen_h - 32.0 - cs::FOOTER_H * 0.5;
            let mut fp = Paint::color(pal.fg2);
            fp.set_font(&regular);
            fp.set_font_size(cs::FOOTER_FONT_SIZE);
            fp.set_text_align(Align::Center);
            fp.set_text_baseline(Baseline::Alphabetic);
            let fitted = self.truncate_ellipsis(state.footer_hint, content_w, &fp);
            let fbase_y = Self::cap_centered_baseline(fy, cs::FOOTER_FONT_SIZE);
            let _ = self
                .canvas
                .fill_text(m.screen_w * 0.5, fbase_y, &fitted, &fp);
        }
    }

    fn draw_nsf_channels_screen(&mut self, state: &NsfChannelsState) {
        let m = LogicalMetrics::from_physical(WIDTH as u32, HEIGHT as u32);
        self.draw_nsf_channels_screen_sized(state, m);
    }

    /// Renders only the top `Header` band (`1024x120`) into Rockbox's `{b, g, r}` framebuffer,
    /// leaving rows `120..768` untouched for non-list screens (dialogs, sliders).
    pub fn render_header_into_bgr(
        &mut self,
        theme: ThemeColors,
        state: &HeaderState,
        fb_bgr: &mut [u8],
    ) {
        let pal = resolve_theme(theme);
        self.begin_offscreen(HEADER_BAND_ROWS, pal.bg);
        let boxes = compute_header_layout();
        self.draw_header_component(&boxes, state, pal);
        self.finish_and_read_top_rows(HEADER_BAND_ROWS);
        self.unpack_top_rows_to_bgr(HEADER_BAND_ROWS, fb_bgr);
    }

    /// Renders a full `1024x768` list/menu screen (`Header` + `MenuItem` rows + `Scrollbar`) into `{b, g, r}`.
    pub fn render_list_into_bgr(&mut self, state: &ListScreenState, fb_bgr: &mut [u8]) {
        self.draw_list_screen(state);
        self.finish_and_read_top_rows(HEIGHT);
        self.unpack_top_rows_to_bgr(HEIGHT, fb_bgr);
    }

    /// Renders the full `1024x768` `Now Playing` screen into Rockbox's `{b, g, r}` framebuffer.
    pub fn render_now_playing_into_bgr(&mut self, state: &NowPlayingState, fb_bgr: &mut [u8]) {
        self.draw_now_playing_screen(state);
        self.finish_and_read_top_rows(HEIGHT);
        self.unpack_top_rows_to_bgr(HEIGHT, fb_bgr);
    }

    /// Renders the full `1024x768` QWERTY keyboard into Rockbox's `{b, g, r}` framebuffer.
    pub fn render_keyboard_into_bgr(&mut self, state: &KeyboardState, fb_bgr: &mut [u8]) {
        self.draw_keyboard_screen(state);
        self.finish_and_read_top_rows(HEIGHT);
        self.unpack_top_rows_to_bgr(HEIGHT, fb_bgr);
    }

    /// Renders the full `1024x768` `Graphic Equalizer` screen into Rockbox's `{b, g, r}` framebuffer.
    pub fn render_graphic_eq_into_bgr(&mut self, state: &GraphicEqState, fb_bgr: &mut [u8]) {
        self.draw_graphic_eq_screen(state);
        self.finish_and_read_top_rows(HEIGHT);
        self.unpack_top_rows_to_bgr(HEIGHT, fb_bgr);
    }

    /// Renders the full `1024x768` `Channels` live toggle screen into Rockbox's `{b, g, r}` framebuffer.
    pub fn render_nsf_channels_into_bgr(&mut self, state: &NsfChannelsState, fb_bgr: &mut [u8]) {
        self.draw_nsf_channels_screen(state);
        self.finish_and_read_top_rows(HEIGHT);
        self.unpack_top_rows_to_bgr(HEIGHT, fb_bgr);
    }

    /// Renders the full `1024x768` `Channels` live toggle screen and returns top-down `RGBA8` bytes.
    pub fn render_nsf_channels_rgba(&mut self, state: &NsfChannelsState) -> Vec<u8> {
        self.draw_nsf_channels_screen(state);
        self.finish_and_read_top_rows(HEIGHT);
        let mut out = vec![0u8; WIDTH * HEIGHT * 4];
        self.unpack_top_rows_to_rgba(HEIGHT, &mut out);
        out
    }

    /// Renders a full `1024x768` list/menu screen and returns top-down `RGBA8` bytes (for PNG previews/tests).
    pub fn render_list_rgba(&mut self, state: &ListScreenState) -> Vec<u8> {
        self.draw_list_screen(state);
        self.finish_and_read_top_rows(HEIGHT);
        let mut out = vec![0u8; WIDTH * HEIGHT * 4];
        self.unpack_top_rows_to_rgba(HEIGHT, &mut out);
        out
    }

    /// Renders the full `1024x768` `Now Playing` screen and returns top-down `RGBA8` bytes (for PNG previews/tests).
    pub fn render_now_playing_rgba(&mut self, state: &NowPlayingState) -> Vec<u8> {
        self.draw_now_playing_screen(state);
        self.finish_and_read_top_rows(HEIGHT);
        let mut out = vec![0u8; WIDTH * HEIGHT * 4];
        self.unpack_top_rows_to_rgba(HEIGHT, &mut out);
        out
    }

    /// Renders the full `1024x768` `Now Playing` screen with the resting `VolumeBarPopup` (`#10977`) and returns `RGBA8`.
    pub fn render_volume_popup_rgba(&mut self, state: &NowPlayingState, level: f32) -> Vec<u8> {
        use crate::layout::volume_popup_spec as vp;

        self.draw_now_playing_screen(state);
        let pal = resolve_theme(state.theme);
        let px = (WIDTH as f32 - vp::WIDTH) * 0.5;
        draw_volume_popup(
            &mut self.canvas,
            px,
            vp::REST_Y,
            level,
            pal.bg,
            pal.fg,
            pal.fg1,
            pal.fg2,
        );
        self.finish_and_read_top_rows(HEIGHT);
        let mut out = vec![0u8; WIDTH * HEIGHT * 4];
        self.unpack_top_rows_to_rgba(HEIGHT, &mut out);
        out
    }

    /// Renders the full `1024x768` QWERTY keyboard and returns top-down `RGBA8` bytes (for PNG previews/tests).
    pub fn render_keyboard_rgba(&mut self, state: &KeyboardState) -> Vec<u8> {
        self.draw_keyboard_screen(state);
        self.finish_and_read_top_rows(HEIGHT);
        let mut out = vec![0u8; WIDTH * HEIGHT * 4];
        self.unpack_top_rows_to_rgba(HEIGHT, &mut out);
        out
    }

    /* ---- Direct GPU Screen Presentation & Transitions (Zero Readback) --- */

    /// Copies the currently rendered `fbo_image` into an offscreen snapshot FBO at `(self.fbo_w, self.fbo_h)`
    /// so subsequent frames can animate UIPod screen transitions (`Slide`, `Cover`, `Fade`,
    /// `Pixels`, `Blinds`) entirely on the GPU at native screen resolution.
    pub fn snapshot_fbo_for_transition(&mut self) {
        let fw = self.fbo_w.max(1);
        let fh = self.fbo_h.max(1);
        if self.prev_fbo_w != fw || self.prev_fbo_h != fh {
            if let Some(old) = self.prev_fbo_image.take() {
                self.canvas.delete_image(old);
            }
            self.prev_fbo_w = 0;
            self.prev_fbo_h = 0;
        }
        let prev = match self.prev_fbo_image {
            Some(id) => id,
            None => match self.canvas.create_image_empty(
                fw,
                fh,
                PixelFormat::Rgba8,
                ImageFlags::FLIP_Y | ImageFlags::NEAREST,
            ) {
                Ok(id) => {
                    self.prev_fbo_image = Some(id);
                    self.prev_fbo_w = fw;
                    self.prev_fbo_h = fh;
                    id
                }
                Err(_) => return,
            },
        };
        self.canvas.set_size(fw as u32, fh as u32, 1.0);
        self.canvas.set_render_target(RenderTarget::Image(prev));
        self.canvas.reset_transform();
        self.canvas.reset_scissor();
        let paint = Paint::image(
            self.fbo_image,
            0.0,
            0.0,
            fw as f32,
            fh as f32,
            0.0,
            1.0,
        );
        let mut quad = VgPath::new();
        quad.rect(0.0, 0.0, fw as f32, fh as f32);
        self.canvas.fill_path(&quad, &paint);
        self.canvas.flush();
        self.canvas.set_render_target(RenderTarget::Screen);
        self.canvas.flush();
        self.restore_gl_state();
    }

    /// Updates ONLY the top `Header` band inside `self.fbo_image` at native `(win_w, win_h)`
    /// without disturbing the list/Now Playing content below `y = 120`.
    pub fn render_header_to_fbo_sized(
        &mut self,
        theme: ThemeColors,
        state: &HeaderState,
        win_w: u32,
        win_h: u32,
    ) {
        self.last_theme = theme;
        let m = LogicalMetrics::from_physical(win_w, win_h);
        let pal = resolve_theme(theme);
        self.begin_offscreen_sized(m, HEADER_BAND_ROWS as f32, pal.bg);
        let boxes = compute_header_layout_sized(m.screen_w, m.screen_h);
        self.draw_header_component(&boxes, state, pal);
        self.canvas.reset_scissor();
        self.canvas.flush();
        self.canvas.set_render_target(RenderTarget::Screen);
        self.canvas.flush();
        self.restore_gl_state();
    }

    /// Renders a responsive list/menu screen directly into `fbo_image` at `(win_w, win_h)`.
    pub fn render_list_to_fbo_sized(&mut self, state: &ListScreenState, win_w: u32, win_h: u32) {
        let m = LogicalMetrics::from_physical(win_w, win_h);
        self.draw_list_screen_sized(state, m);
        self.canvas.flush();
        self.canvas.set_render_target(RenderTarget::Screen);
        self.canvas.flush();
        self.restore_gl_state();
    }

    /// Renders a list/menu screen directly into `fbo_image` at canonical `1024x768`.
    pub fn render_list_to_fbo(&mut self, state: &ListScreenState) {
        self.render_list_to_fbo_sized(state, WIDTH as u32, HEIGHT as u32);
    }

    /// Renders the responsive `Now Playing` screen directly into `fbo_image` at `(win_w, win_h)`.
    pub fn render_now_playing_to_fbo_sized(
        &mut self,
        state: &NowPlayingState,
        win_w: u32,
        win_h: u32,
    ) {
        let m = LogicalMetrics::from_physical(win_w, win_h);
        self.draw_now_playing_screen_sized(state, m);
        self.canvas.flush();
        self.canvas.set_render_target(RenderTarget::Screen);
        self.canvas.flush();
        self.restore_gl_state();
    }

    /// Renders the `Now Playing` screen directly into `fbo_image` at canonical `1024x768`.
    pub fn render_now_playing_to_fbo(&mut self, state: &NowPlayingState) {
        self.render_now_playing_to_fbo_sized(state, WIDTH as u32, HEIGHT as u32);
    }

    /// Renders the responsive QWERTY keyboard directly into `fbo_image` at `(win_w, win_h)`.
    pub fn render_keyboard_to_fbo_sized(
        &mut self,
        state: &KeyboardState,
        win_w: u32,
        win_h: u32,
    ) {
        let m = LogicalMetrics::from_physical(win_w, win_h);
        self.draw_keyboard_screen_sized(state, m);
        self.canvas.flush();
        self.canvas.set_render_target(RenderTarget::Screen);
        self.canvas.flush();
        self.restore_gl_state();
    }

    /// Renders the QWERTY keyboard directly into `fbo_image` at canonical `1024x768`.
    pub fn render_keyboard_to_fbo(&mut self, state: &KeyboardState) {
        self.render_keyboard_to_fbo_sized(state, WIDTH as u32, HEIGHT as u32);
    }

    /// Renders the responsive `Graphic Equalizer` screen directly into `fbo_image` at `(win_w, win_h)`.
    pub fn render_graphic_eq_to_fbo_sized(
        &mut self,
        state: &GraphicEqState,
        win_w: u32,
        win_h: u32,
    ) {
        let m = LogicalMetrics::from_physical(win_w, win_h);
        self.draw_graphic_eq_screen_sized(state, m);
        self.canvas.flush();
        self.canvas.set_render_target(RenderTarget::Screen);
        self.canvas.flush();
        self.restore_gl_state();
    }

    /// Renders the `Graphic Equalizer` screen directly into `fbo_image` at canonical `1024x768`.
    pub fn render_graphic_eq_to_fbo(&mut self, state: &GraphicEqState) {
        self.render_graphic_eq_to_fbo_sized(state, WIDTH as u32, HEIGHT as u32);
    }

    /// Renders the responsive `Channels` live APU toggle screen directly into `fbo_image` at `(win_w, win_h)`.
    pub fn render_nsf_channels_to_fbo_sized(
        &mut self,
        state: &NsfChannelsState,
        win_w: u32,
        win_h: u32,
    ) {
        let m = LogicalMetrics::from_physical(win_w, win_h);
        self.draw_nsf_channels_screen_sized(state, m);
        self.canvas.flush();
        self.canvas.set_render_target(RenderTarget::Screen);
        self.canvas.flush();
        self.restore_gl_state();
    }

    /// Renders the `Channels` live APU toggle screen directly into `fbo_image` at canonical `1024x768`.
    pub fn render_nsf_channels_to_fbo(&mut self, state: &NsfChannelsState) {
        self.render_nsf_channels_to_fbo_sized(state, WIDTH as u32, HEIGHT as u32);
    }

    /// Presents `fbo_image` directly to the window backbuffer, optionally blending from the
    /// last `snapshot_fbo_for_transition()` (`0=Off`, `1=Slide`, `2=Cover`, `3=Fade`,
    /// `4=Pixels`, `5=Blinds`) while optionally keeping the shared top `Header` pinned.
    pub fn present_fbo_transition_ex(
        &mut self,
        win_w: u32,
        win_h: u32,
        kind: u8,
        progress: f32,
        dir: i8,
        pin_header: bool,
    ) {
        let t = progress.clamp(0.0, 1.0);
        if kind == 0 || t >= 1.0 || self.prev_fbo_image.is_none() {
            self.plugin_present_gpu_screen(win_w, win_h);
            return;
        }
        let prev = self.prev_fbo_image.unwrap();
        let sw = win_w.max(1) as f32;
        let sh = win_h.max(1) as f32;
        self.canvas.set_size(win_w.max(1), win_h.max(1), 1.0);
        self.canvas.set_render_target(RenderTarget::Screen);
        self.canvas.reset_transform();
        self.canvas.reset_scissor();

        let ease = 1.0 - (1.0 - t).powi(3);
        let sign = if dir < 0 { -1.0f32 } else { 1.0f32 };

        let header_phys_h = if pin_header {
            let m = LogicalMetrics::from_physical(win_w, win_h);
            ((HEADER_BAND_ROWS as f32) * m.scale_y).round().clamp(0.0, sh)
        } else {
            0.0
        };

        if header_phys_h > 0.0 {
            self.canvas
                .scissor(0.0, header_phys_h, sw, (sh - header_phys_h).max(1.0));
        }

        match kind {
            1 => {
                // Slide: both old and new move together
                let ox_prev = -sign * sw * ease;
                let ox_curr = sign * sw * (1.0 - ease);
                let p_prev = Paint::image(prev, ox_prev, 0.0, sw, sh, 0.0, 1.0);
                let mut q1 = VgPath::new();
                q1.rect(ox_prev, 0.0, sw, sh);
                self.canvas.fill_path(&q1, &p_prev);

                let p_curr = Paint::image(self.fbo_image, ox_curr, 0.0, sw, sh, 0.0, 1.0);
                let mut q2 = VgPath::new();
                q2.rect(ox_curr, 0.0, sw, sh);
                self.canvas.fill_path(&q2, &p_curr);
            }
            2 => {
                // Cover: incoming slides over stationary previous
                let p_prev = Paint::image(prev, 0.0, 0.0, sw, sh, 0.0, 1.0);
                let mut q1 = VgPath::new();
                q1.rect(0.0, 0.0, sw, sh);
                self.canvas.fill_path(&q1, &p_prev);

                let ox_curr = sign * sw * (1.0 - ease);
                let p_curr = Paint::image(self.fbo_image, ox_curr, 0.0, sw, sh, 0.0, 1.0);
                let mut q2 = VgPath::new();
                q2.rect(ox_curr, 0.0, sw, sh);
                self.canvas.fill_path(&q2, &p_curr);
            }
            3 => {
                // Fade: crossfade
                let p_prev = Paint::image(prev, 0.0, 0.0, sw, sh, 0.0, 1.0);
                let mut q1 = VgPath::new();
                q1.rect(0.0, 0.0, sw, sh);
                self.canvas.fill_path(&q1, &p_prev);

                let p_curr = Paint::image(self.fbo_image, 0.0, 0.0, sw, sh, 0.0, ease);
                let mut q2 = VgPath::new();
                q2.rect(0.0, 0.0, sw, sh);
                self.canvas.fill_path(&q2, &p_curr);
            }
            4 => {
                // Pixels: ordered 4x4 Bayer tile reveal
                const ORDER: [u8; 16] = [
                    0, 8, 2, 10, 12, 4, 14, 6, 3, 11, 1, 9, 15, 7, 13, 5,
                ];
                let p_prev = Paint::image(prev, 0.0, 0.0, sw, sh, 0.0, 1.0);
                let mut q1 = VgPath::new();
                q1.rect(0.0, 0.0, sw, sh);
                self.canvas.fill_path(&q1, &p_prev);

                let cols = 16usize;
                let rows = 12usize;
                let cw = sw / cols as f32;
                let ch = sh / rows as f32;
                let p_curr = Paint::image(self.fbo_image, 0.0, 0.0, sw, sh, 0.0, 1.0);
                let mut reveal = VgPath::new();
                let thresh = (ease * 16.5) as u8;
                for r in 0..rows {
                    for c in 0..cols {
                        let o = ORDER[((r & 3) << 2) | (c & 3)];
                        if o < thresh {
                            reveal.rect(c as f32 * cw, r as f32 * ch, cw + 0.5, ch + 0.5);
                        }
                    }
                }
                self.canvas.fill_path(&reveal, &p_curr);
            }
            _ => {
                // Blinds: horizontal venetian slats
                let p_prev = Paint::image(prev, 0.0, 0.0, sw, sh, 0.0, 1.0);
                let mut q1 = VgPath::new();
                q1.rect(0.0, 0.0, sw, sh);
                self.canvas.fill_path(&q1, &p_prev);

                let slats = 12usize;
                let slat_h = sh / slats as f32;
                let p_curr = Paint::image(self.fbo_image, 0.0, 0.0, sw, sh, 0.0, 1.0);
                let mut reveal = VgPath::new();
                for i in 0..slats {
                    let h = slat_h * ease;
                    reveal.rect(0.0, i as f32 * slat_h, sw, h + 0.5);
                }
                self.canvas.fill_path(&reveal, &p_curr);
            }
        }

        if header_phys_h > 0.0 {
            self.canvas.reset_scissor();
            let p_hdr = Paint::image(self.fbo_image, 0.0, 0.0, sw, sh, 0.0, 1.0);
            let mut hq = VgPath::new();
            hq.rect(0.0, 0.0, sw, header_phys_h);
            self.canvas.fill_path(&hq, &p_hdr);
        }

        let now_ms = self.hud_stack.clock_ms();
        self.draw_hud_stack_on_screen_internal(win_w, win_h, now_ms);

        self.canvas.flush();
        self.canvas
            .set_size(self.fbo_w as u32, self.fbo_h as u32, 1.0);
        self.restore_gl_state();
    }

    pub fn present_fbo_transition(
        &mut self,
        win_w: u32,
        win_h: u32,
        kind: u8,
        progress: f32,
        dir: i8,
    ) {
        self.present_fbo_transition_ex(win_w, win_h, kind, progress, dir, false);
    }

    /* ---- Immediate-Mode Plugin GPU Canvas (Responsive + Fixed-Aspect) ---- */

    #[inline]
    fn font_chain_by_weight(&self, weight: u8) -> Vec<FontId> {
        if (weight & 0x08) != 0 {
            let is_italic = (weight & 0x04) != 0;
            let is_bold = (weight & 0x03) >= 2;
            match (is_bold, is_italic) {
                (false, false) => self.serif_regular.clone(),
                (true, false) => self.serif_bold.clone(),
                (false, true) => self.serif_italic.clone(),
                (true, true) => self.serif_bold_italic.clone(),
            }
        } else {
            match weight & 0x03 {
                0 => self.fonts_regular.clone(),
                1 => self.fonts_medium.clone(),
                2 => self.fonts_semibold.clone(),
                _ => self.fonts_bold.clone(),
            }
        }
    }

    pub fn plugin_begin_ex(
        &mut self,
        theme: ThemeColors,
        bg_rgb: u32,
        header: Option<&HeaderState>,
        win_w: u32,
        win_h: u32,
        responsive: bool,
    ) -> (f32, f32) {
        self.last_theme = theme;
        let m = LogicalMetrics::from_physical(win_w, win_h);
        let bg = rgb_u32_to_color(bg_rgb);
        self.begin_offscreen_sized(m, m.screen_h, bg);
        if let Some(hdr) = header {
            let boxes = compute_header_layout_sized(m.screen_w, m.screen_h);
            self.draw_header_component(&boxes, hdr, resolve_theme(theme));
        }

        if responsive {
            if header.is_some() {
                self.canvas.scissor(
                    0.0,
                    HEADER_BAND_ROWS as f32,
                    m.screen_w,
                    (m.screen_h - HEADER_BAND_ROWS as f32).max(1.0),
                );
            }
            (m.screen_w, m.screen_h)
        } else {
            self.canvas.reset_transform();
            self.canvas.reset_scissor();
            let fit_scale = ((m.phys_w as f32) / SCREEN_W).min((m.phys_h as f32) / SCREEN_H);
            let ox = ((m.phys_w as f32) - SCREEN_W * fit_scale) * 0.5;
            let oy = ((m.phys_h as f32) - SCREEN_H * fit_scale) * 0.5;
            self.canvas.translate(ox, oy);
            self.canvas.scale(fit_scale, fit_scale);
            if header.is_some() {
                let top_clip = (((HEADER_BAND_ROWS as f32) * m.scale_y - oy) / fit_scale)
                    .clamp(0.0, SCREEN_H);
                self.canvas
                    .scissor(0.0, top_clip, SCREEN_W, (SCREEN_H - top_clip).max(1.0));
            } else {
                self.canvas.scissor(0.0, 0.0, SCREEN_W, SCREEN_H);
            }
            (SCREEN_W, SCREEN_H)
        }
    }

    pub fn plugin_begin(
        &mut self,
        theme: ThemeColors,
        bg_rgb: u32,
        header: Option<&HeaderState>,
    ) {
        let _ = self.plugin_begin_ex(theme, bg_rgb, header, WIDTH as u32, HEIGHT as u32, false);
    }

    pub fn plugin_fill_rect(&mut self, x: f32, y: f32, w: f32, h: f32, radius: f32, rgb: u32) {
        fill_rounded_rect(&mut self.canvas, x, y, w, h, radius.max(0.0), rgb_u32_to_color(rgb));
    }

    pub fn plugin_stroke_rect(
        &mut self,
        x: f32,
        y: f32,
        w: f32,
        h: f32,
        radius: f32,
        stroke_w: f32,
        rgb: u32,
    ) {
        stroke_rounded_rect_inside(
            &mut self.canvas,
            x,
            y,
            w,
            h,
            radius.max(0.0),
            stroke_w.max(1.0),
            rgb_u32_to_color(rgb),
        );
    }

    pub fn plugin_fill_circle(&mut self, cx: f32, cy: f32, r: f32, rgb: u32) {
        if r <= 0.0 {
            return;
        }
        let mut p = VgPath::new();
        p.circle(cx, cy, r);
        self.canvas
            .fill_path(&p, &Paint::color(rgb_u32_to_color(rgb)));
    }

    pub fn plugin_draw_line(
        &mut self,
        x0: f32,
        y0: f32,
        x1: f32,
        y1: f32,
        stroke_w: f32,
        rgb: u32,
    ) {
        let mut p = VgPath::new();
        p.move_to(x0, y0);
        p.line_to(x1, y1);
        let mut paint = Paint::color(rgb_u32_to_color(rgb));
        paint.set_line_width(stroke_w.max(1.0));
        paint.set_line_cap(femtovg::LineCap::Butt);
        self.canvas.stroke_path(&p, &paint);
    }

    pub fn plugin_draw_text(
        &mut self,
        x: f32,
        center_y: f32,
        text: &str,
        font_size: f32,
        weight: u8,
        align: u8,
        rgb: u32,
    ) -> f32 {
        if text.is_empty() || font_size <= 1.0 {
            return 0.0;
        }
        let fonts = self.font_chain_by_weight(weight);
        let mut tp = Paint::color(rgb_u32_to_color(rgb));
        tp.set_font(&fonts);
        tp.set_font_size(font_size);
        tp.set_text_align(match align {
            1 => Align::Center,
            2 => Align::Right,
            _ => Align::Left,
        });
        tp.set_text_baseline(Baseline::Alphabetic);
        let base_y = Self::text_row_baseline(center_y, font_size);
        self.canvas
            .fill_text(x, base_y, text, &tp)
            .map(|m| m.width())
            .unwrap_or(0.0)
    }

    pub fn plugin_measure_text(&mut self, text: &str, font_size: f32, weight: u8) -> f32 {
        if text.is_empty() || font_size <= 1.0 {
            return 0.0;
        }
        let fonts = self.font_chain_by_weight(weight);
        let mut tp = Paint::color(Color::white());
        tp.set_font(&fonts);
        tp.set_font_size(font_size);
        self.canvas
            .measure_text(0.0, 0.0, text, &tp)
            .map(|m| m.width())
            .unwrap_or(0.0)
    }

    pub fn plugin_draw_progress_bar(
        &mut self,
        theme: ThemeColors,
        x: f32,
        y: f32,
        w: f32,
        h: f32,
        progress: f32,
    ) {
        let pal = resolve_theme(theme);
        draw_seekbar_track_h(&mut self.canvas, x, y, w, h, progress, pal.fg1, pal.fg2);
    }

    pub fn plugin_draw_scrollbar(
        &mut self,
        theme: ThemeColors,
        x: f32,
        y: f32,
        w: f32,
        h: f32,
        total: usize,
        start: usize,
        visible: usize,
    ) {
        let pal = resolve_theme(theme);
        draw_scrollbar_v(
            &mut self.canvas,
            x,
            y,
            w,
            h,
            total,
            start,
            visible,
            pal.fg1,
            pal.fg,
        );
    }

    /// Uploads (or reuses via 16-slot LRU cache) a pre-dithered `RGBA8` image and draws it
    /// with crisp `ImageFlags::NEAREST` sampling onto the active plugin GPU canvas.
    #[allow(clippy::too_many_arguments)]
    pub fn plugin_draw_rgba_image(
        &mut self,
        tex_key: u64,
        src_w: u32,
        src_h: u32,
        rgba: &[u8],
        dst_x: f32,
        dst_y: f32,
        dst_w: f32,
        dst_h: f32,
        radius: f32,
    ) {
        if src_w == 0 || src_h == 0 || dst_w <= 0.5 || dst_h <= 0.5 {
            return;
        }
        let needed = (src_w as usize)
            .saturating_mul(src_h as usize)
            .saturating_mul(4);
        if rgba.len() < needed {
            return;
        }

        let img_id = if let Some(pos) = self
            .plugin_images
            .iter()
            .position(|&(k, w, h, _)| k == tex_key && w == src_w && h == src_h)
        {
            let entry = self.plugin_images.remove(pos);
            let id = entry.3;
            self.plugin_images.push(entry);
            id
        } else {
            let mut pixels = Vec::with_capacity((src_w as usize) * (src_h as usize));
            for px in rgba[..needed].chunks_exact(4) {
                pixels.push(RGBA8::new(px[0], px[1], px[2], px[3]));
            }
            let img_ref = ImgRef::new(&pixels, src_w as usize, src_h as usize);
            let Ok(new_id) = self.canvas.create_image(img_ref, ImageFlags::NEAREST) else {
                return;
            };
            if self.plugin_images.len() >= 16 {
                let (_, _, _, old_id) = self.plugin_images.remove(0);
                self.canvas.delete_image(old_id);
            }
            self.plugin_images.push((tex_key, src_w, src_h, new_id));
            new_id
        };

        let paint = Paint::image(img_id, dst_x, dst_y, dst_w, dst_h, 0.0, 1.0);
        let mut path = VgPath::new();
        if radius > 0.5 {
            path.rounded_rect(dst_x, dst_y, dst_w, dst_h, radius);
        } else {
            path.rect(dst_x, dst_y, dst_w, dst_h);
        }
        self.canvas.fill_path(&path, &paint);
    }

    pub fn plugin_present_into_bgr(&mut self, fb_bgr: &mut [u8]) {
        self.plugin_sync_fbo_into_bgr(fb_bgr);
    }

    /* ---- Unified Stacked HUD System (`Volume`, `Toast`, `Progress`, `Confirmation`) ---- */

    pub fn hud_volume(&mut self, level_norm: f32, now_ms: u64) {
        self.hud_stack.trigger_volume(level_norm, now_ms);
    }

    pub fn hud_toast(&mut self, key: &str, message: &str, hold_ms: u64, now_ms: u64) {
        self.hud_stack.trigger_toast(key, message, hold_ms, now_ms);
    }

    pub fn hud_progress_begin(
        &mut self,
        title: &str,
        detail: &str,
        progress: Option<f32>,
        now_ms: u64,
    ) -> u32 {
        self.hud_stack.progress_begin(title, detail, progress, now_ms)
    }

    pub fn hud_progress_update(
        &mut self,
        handle: u32,
        title: &str,
        detail: &str,
        progress: Option<f32>,
        now_ms: u64,
    ) -> bool {
        self.hud_stack
            .progress_update(handle, title, detail, progress, now_ms)
    }

    pub fn hud_progress_end(&mut self, handle: u32, now_ms: u64) -> bool {
        self.hud_stack.progress_end(handle, now_ms)
    }

    pub fn hud_confirm_push(
        &mut self,
        question: &str,
        detail: &str,
        confirm_label: &str,
        cancel_label: &str,
        now_ms: u64,
    ) -> u32 {
        self.hud_stack
            .confirm_push(question, detail, confirm_label, cancel_label, now_ms)
    }

    pub fn hud_confirm_resolve(&mut self, id: u32, now_ms: u64) -> bool {
        self.hud_stack.confirm_resolve(id, now_ms)
    }

    pub fn hud_has_active_confirm(&self) -> bool {
        self.hud_stack.has_active_confirm()
    }

    pub fn hud_top_confirm_id(&self) -> Option<u32> {
        self.hud_stack.top_confirm_id()
    }

    pub fn hud_set_clock(&mut self, now_ms: u64) {
        self.hud_stack.set_clock(now_ms);
    }

    pub fn hud_clock_ms(&self) -> u64 {
        self.hud_stack.clock_ms()
    }

    pub fn hud_needs_tick(&mut self, now_ms: u64) -> bool {
        self.hud_stack.needs_frame_tick(now_ms)
    }

    pub fn hud_is_visible(&mut self, now_ms: u64) -> bool {
        self.hud_stack.is_visible(now_ms)
    }

    /// Backwards-compatible convenience aliases forwarding into `HudStack`.
    pub fn trigger_volume_popup(&mut self, level_norm: f32, now_ms: u64) {
        self.hud_volume(level_norm, now_ms);
    }

    pub fn trigger_hud_toast(&mut self, message: &str, now_ms: u64) {
        self.hud_toast("", message, 0, now_ms);
    }

    pub fn set_volume_popup_clock(&mut self, now_ms: u64) {
        self.hud_set_clock(now_ms);
    }

    pub fn set_active_theme(&mut self, theme: ThemeColors) {
        self.last_theme = theme;
    }

    pub fn is_volume_popup_animating(&mut self, now_ms: u64) -> bool {
        self.hud_needs_tick(now_ms)
    }

    fn draw_hud_toast_card(
        &mut self,
        line1: &str,
        line2: &str,
        screen_w: f32,
        py: f32,
        alpha: f32,
        pal: ResolvedTheme,
    ) {
        use crate::layout::hud_stack_spec as hs;
        use crate::layout::hud_toast_spec as ht;

        let semibold = self.fonts_semibold.clone();
        let medium = self.fonts_medium.clone();

        let mut tp1 = Paint::color(color_with_alpha(pal.fg, alpha));
        tp1.set_font(&semibold);
        tp1.set_font_size(ht::FONT_SIZE);
        tp1.set_text_align(Align::Center);
        tp1.set_text_baseline(Baseline::Alphabetic);

        let max_box_w = (screen_w - ht::SCREEN_MARGIN_X).max(ht::MIN_WIDTH);
        let max_text_w = (max_box_w - ht::INNER_INSET_X * 2.0).max(40.0);
        let fitted1 = self.truncate_ellipsis(line1, max_text_w, &tp1);
        let w1 = self
            .canvas
            .measure_text(0.0, 0.0, &fitted1, &tp1)
            .map(|m| m.width())
            .unwrap_or(200.0);

        if line2.is_empty() {
            let box_w = (w1 + ht::INNER_INSET_X * 2.0)
                .ceil()
                .clamp(ht::MIN_WIDTH, max_box_w);
            let px = ((screen_w - box_w) * 0.5).round();
            draw_hud_card_frame(
                &mut self.canvas,
                px,
                py,
                box_w,
                ht::HEIGHT,
                ht::CORNER_RADIUS,
                ht::STROKE_WIDTH,
                pal.bg,
                pal.fg2,
                alpha,
            );
            let base_y = Self::text_row_baseline(py + ht::HEIGHT * 0.5, ht::FONT_SIZE);
            let _ = self
                .canvas
                .fill_text(px + box_w * 0.5, base_y, &fitted1, &tp1);
        } else {
            let mut tp2 = Paint::color(color_with_alpha(pal.fg2, alpha));
            tp2.set_font(&medium);
            tp2.set_font_size(hs::TOAST_LINE2_FONT_SIZE);
            tp2.set_text_align(Align::Center);
            tp2.set_text_baseline(Baseline::Alphabetic);

            let fitted2 = self.truncate_ellipsis(line2, max_text_w, &tp2);
            let w2 = self
                .canvas
                .measure_text(0.0, 0.0, &fitted2, &tp2)
                .map(|m| m.width())
                .unwrap_or(160.0);

            let box_w = (w1.max(w2) + ht::INNER_INSET_X * 2.0)
                .ceil()
                .clamp(ht::MIN_WIDTH, max_box_w);
            let px = ((screen_w - box_w) * 0.5).round();
            draw_hud_card_frame(
                &mut self.canvas,
                px,
                py,
                box_w,
                hs::TOAST_TWO_LINE_HEIGHT,
                ht::CORNER_RADIUS,
                ht::STROKE_WIDTH,
                pal.bg,
                pal.fg2,
                alpha,
            );
            let b1 = Self::text_row_baseline(py + hs::TOAST_LINE1_CENTER_Y, ht::FONT_SIZE);
            let b2 =
                Self::text_row_baseline(py + hs::TOAST_LINE2_CENTER_Y, hs::TOAST_LINE2_FONT_SIZE);
            let _ = self.canvas.fill_text(px + box_w * 0.5, b1, &fitted1, &tp1);
            let _ = self.canvas.fill_text(px + box_w * 0.5, b2, &fitted2, &tp2);
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn draw_hud_progress_card(
        &mut self,
        title: &str,
        detail: &str,
        progress: Option<f32>,
        screen_w: f32,
        py: f32,
        alpha: f32,
        now_ms: u64,
        pal: ResolvedTheme,
    ) {
        use crate::layout::hud_stack_spec as hs;

        let semibold = self.fonts_semibold.clone();
        let medium = self.fonts_medium.clone();

        let max_box_w = (screen_w - hs::SCREEN_MARGIN_X).max(hs::PROGRESS_MIN_WIDTH);
        let max_inner_w = (max_box_w - hs::PROGRESS_INNER_INSET_X * 2.0).max(120.0);

        let mut tp_title = Paint::color(color_with_alpha(pal.fg, alpha));
        tp_title.set_font(&semibold);
        tp_title.set_font_size(hs::PROGRESS_TITLE_FONT_SIZE);
        tp_title.set_text_baseline(Baseline::Alphabetic);

        let mut tp_det = Paint::color(color_with_alpha(pal.fg2, alpha));
        tp_det.set_font(&medium);
        tp_det.set_font_size(hs::PROGRESS_DETAIL_FONT_SIZE);
        tp_det.set_text_align(Align::Right);
        tp_det.set_text_baseline(Baseline::Alphabetic);

        let det_trimmed = detail.trim();
        let det_w = if det_trimmed.is_empty() {
            0.0
        } else {
            self.canvas
                .measure_text(0.0, 0.0, det_trimmed, &tp_det)
                .map(|m| m.width())
                .unwrap_or(64.0)
        };

        let avail_title_w = if det_w > 0.0 {
            (max_inner_w - det_w - hs::PROGRESS_LABEL_GAP_X).max(80.0)
        } else {
            max_inner_w
        };
        let fitted_title = self.truncate_ellipsis(title, avail_title_w, &tp_title);
        let title_w = self
            .canvas
            .measure_text(0.0, 0.0, &fitted_title, &tp_title)
            .map(|m| m.width())
            .unwrap_or(200.0);

        let content_w = if det_w > 0.0 {
            title_w + hs::PROGRESS_LABEL_GAP_X + det_w
        } else {
            title_w
        };
        let box_w = (content_w + hs::PROGRESS_INNER_INSET_X * 2.0)
            .ceil()
            .clamp(hs::PROGRESS_MIN_WIDTH, max_box_w);
        let px = ((screen_w - box_w) * 0.5).round();

        draw_hud_card_frame(
            &mut self.canvas,
            px,
            py,
            box_w,
            hs::PROGRESS_HEIGHT,
            hs::CORNER_RADIUS,
            hs::STROKE_WIDTH,
            pal.bg,
            pal.fg2,
            alpha,
        );

        let inner_x = px + hs::PROGRESS_INNER_INSET_X;
        let inner_w = box_w - hs::PROGRESS_INNER_INSET_X * 2.0;
        let label_base = Self::text_row_baseline(
            py + hs::PROGRESS_LABEL_CENTER_Y,
            hs::PROGRESS_TITLE_FONT_SIZE,
        );

        if det_w > 0.0 {
            tp_title.set_text_align(Align::Left);
            let _ = self
                .canvas
                .fill_text(inner_x, label_base, &fitted_title, &tp_title);
            let det_base = Self::text_row_baseline(
                py + hs::PROGRESS_LABEL_CENTER_Y,
                hs::PROGRESS_DETAIL_FONT_SIZE,
            );
            let _ = self
                .canvas
                .fill_text(inner_x + inner_w, det_base, det_trimmed, &tp_det);
        } else {
            tp_title.set_text_align(Align::Center);
            let _ = self.canvas.fill_text(
                px + box_w * 0.5,
                label_base,
                &fitted_title,
                &tp_title,
            );
        }

        // 4-Shade capsule progress bar
        let track_y = py + hs::PROGRESS_TRACK_Y;
        let track_h = hs::PROGRESS_TRACK_HEIGHT;
        stroke_rounded_rect_inside(
            &mut self.canvas,
            inner_x,
            track_y,
            inner_w,
            track_h,
            track_h * 0.5,
            hs::STROKE_WIDTH,
            color_with_alpha(pal.fg1, alpha),
        );

        let max_fill = (inner_w - hs::STROKE_WIDTH * 2.0).max(0.0);
        let fill_h = (track_h - hs::STROKE_WIDTH * 2.0).max(2.0);
        let fill_c = color_with_alpha(pal.fg, alpha);

        match progress {
            Some(p) => {
                let clamped = p.clamp(0.0, 1.0);
                if clamped > 0.001 && max_fill >= fill_h {
                    let fill_w = (max_fill * clamped).clamp(fill_h, max_fill);
                    fill_rounded_rect(
                        &mut self.canvas,
                        inner_x + hs::STROKE_WIDTH,
                        track_y + hs::STROKE_WIDTH,
                        fill_w,
                        fill_h,
                        fill_h * 0.5,
                        fill_c,
                    );
                }
            }
            None => {
                if max_fill >= fill_h {
                    let shuttle_w =
                        (max_fill * hs::INDETERMINATE_SHUTTLE_RATIO).clamp(fill_h * 2.0, max_fill);
                    let travel = (max_fill - shuttle_w).max(0.0);
                    let phase = 0.5
                        - 0.5
                            * ((now_ms as f32 / hs::INDETERMINATE_PERIOD_MS)
                                * std::f32::consts::TAU)
                                .cos();
                    let sx = inner_x + hs::STROKE_WIDTH + travel * phase;
                    fill_rounded_rect(
                        &mut self.canvas,
                        sx,
                        track_y + hs::STROKE_WIDTH,
                        shuttle_w,
                        fill_h,
                        fill_h * 0.5,
                        fill_c,
                    );
                }
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn draw_hud_confirm_button(
        &mut self,
        x: f32,
        y: f32,
        w: f32,
        h: f32,
        key_char: &str,
        label: &str,
        primary: bool,
        alpha: f32,
        pal: ResolvedTheme,
    ) {
        use crate::layout::hud_stack_spec as hs;

        let semibold = self.fonts_semibold.clone();
        let bold = self.fonts_bold.clone();

        if primary {
            fill_rounded_rect(
                &mut self.canvas,
                x,
                y,
                w,
                h,
                hs::CONFIRM_BTN_RADIUS,
                color_with_alpha(pal.fg, alpha),
            );
        } else {
            draw_hud_card_frame(
                &mut self.canvas,
                x,
                y,
                w,
                h,
                hs::CONFIRM_BTN_RADIUS,
                hs::CONFIRM_BTN_STROKE,
                pal.bg,
                pal.fg2,
                alpha,
            );
        }

        let mut tp_lbl = Paint::color(color_with_alpha(if primary { pal.bg } else { pal.fg }, alpha));
        tp_lbl.set_font(&semibold);
        tp_lbl.set_font_size(hs::CONFIRM_BTN_FONT_SIZE);
        tp_lbl.set_text_align(Align::Left);
        tp_lbl.set_text_baseline(Baseline::Alphabetic);

        let lbl_w = self
            .canvas
            .measure_text(0.0, 0.0, label, &tp_lbl)
            .map(|m| m.width())
            .unwrap_or(64.0);
        let group_w = hs::CONFIRM_KEY_BADGE_SIZE + hs::CONFIRM_KEY_GAP_X + lbl_w;
        let gx = x + ((w - group_w) * 0.5).max(hs::CONFIRM_BTN_PAD_X * 0.5);
        let ky = y + (h - hs::CONFIRM_KEY_BADGE_SIZE) * 0.5;

        fill_rounded_rect(
            &mut self.canvas,
            gx,
            ky,
            hs::CONFIRM_KEY_BADGE_SIZE,
            hs::CONFIRM_KEY_BADGE_SIZE,
            hs::CONFIRM_KEY_BADGE_RADIUS,
            color_with_alpha(if primary { pal.bg } else { pal.fg2 }, alpha),
        );

        let mut tp_key = Paint::color(color_with_alpha(if primary { pal.fg } else { pal.bg }, alpha));
        tp_key.set_font(&bold);
        tp_key.set_font_size(hs::CONFIRM_KEY_BADGE_FONT_SIZE);
        tp_key.set_text_align(Align::Center);
        tp_key.set_text_baseline(Baseline::Alphabetic);

        let key_base =
            Self::text_row_baseline(y + h * 0.5, hs::CONFIRM_KEY_BADGE_FONT_SIZE);
        let _ = self.canvas.fill_text(
            gx + hs::CONFIRM_KEY_BADGE_SIZE * 0.5,
            key_base,
            key_char,
            &tp_key,
        );

        let lbl_base = Self::text_row_baseline(y + h * 0.5, hs::CONFIRM_BTN_FONT_SIZE);
        let _ = self.canvas.fill_text(
            gx + hs::CONFIRM_KEY_BADGE_SIZE + hs::CONFIRM_KEY_GAP_X,
            lbl_base,
            label,
            &tp_lbl,
        );
    }

    #[allow(clippy::too_many_arguments)]
    fn draw_hud_confirm_card(
        &mut self,
        question: &str,
        detail: &str,
        confirm_label: &str,
        cancel_label: &str,
        screen_w: f32,
        py: f32,
        alpha: f32,
        pal: ResolvedTheme,
    ) {
        use crate::layout::hud_stack_spec as hs;

        let semibold = self.fonts_semibold.clone();
        let medium = self.fonts_medium.clone();

        let has_detail = !detail.trim().is_empty();
        let box_h = if has_detail {
            hs::CONFIRM_HEIGHT_DETAIL
        } else {
            hs::CONFIRM_HEIGHT_SINGLE
        };
        let max_box_w = (screen_w - hs::SCREEN_MARGIN_X).max(hs::CONFIRM_MIN_WIDTH);
        let max_inner_w = (max_box_w - hs::CONFIRM_INNER_INSET_X * 2.0).max(200.0);

        let mut tp_q = Paint::color(color_with_alpha(pal.fg, alpha));
        tp_q.set_font(&semibold);
        tp_q.set_font_size(hs::CONFIRM_QUESTION_FONT_SIZE);
        tp_q.set_text_align(Align::Center);
        tp_q.set_text_baseline(Baseline::Alphabetic);

        let fitted_q = self.truncate_ellipsis(question, max_inner_w, &tp_q);
        let q_w = self
            .canvas
            .measure_text(0.0, 0.0, &fitted_q, &tp_q)
            .map(|m| m.width())
            .unwrap_or(240.0);

        let mut tp_d = Paint::color(color_with_alpha(pal.fg2, alpha));
        tp_d.set_font(&medium);
        tp_d.set_font_size(hs::CONFIRM_DETAIL_FONT_SIZE);
        tp_d.set_text_align(Align::Center);
        tp_d.set_text_baseline(Baseline::Alphabetic);

        let (fitted_d, d_w) = if has_detail {
            let fd = self.truncate_ellipsis(detail, max_inner_w, &tp_d);
            let dw = self
                .canvas
                .measure_text(0.0, 0.0, &fd, &tp_d)
                .map(|m| m.width())
                .unwrap_or(0.0);
            (fd, dw)
        } else {
            (String::new(), 0.0)
        };

        let mut tp_btn_meas = Paint::color(pal.fg);
        tp_btn_meas.set_font(&semibold);
        tp_btn_meas.set_font_size(hs::CONFIRM_BTN_FONT_SIZE);
        let c_lbl_w = self
            .canvas
            .measure_text(0.0, 0.0, cancel_label, &tp_btn_meas)
            .map(|m| m.width())
            .unwrap_or(80.0);
        let a_lbl_w = self
            .canvas
            .measure_text(0.0, 0.0, confirm_label, &tp_btn_meas)
            .map(|m| m.width())
            .unwrap_or(80.0);
        let btn_overhead =
            hs::CONFIRM_KEY_BADGE_SIZE + hs::CONFIRM_KEY_GAP_X + hs::CONFIRM_BTN_PAD_X * 2.0;
        let btn_w = (c_lbl_w.max(a_lbl_w) + btn_overhead)
            .ceil()
            .max(hs::CONFIRM_BTN_MIN_WIDTH);
        let btns_total_w = btn_w * 2.0 + hs::CONFIRM_BTN_GAP_X;

        let inner_need = q_w.max(d_w).max(btns_total_w);
        let box_w = (inner_need + hs::CONFIRM_INNER_INSET_X * 2.0)
            .ceil()
            .clamp(hs::CONFIRM_MIN_WIDTH, max_box_w);
        let px = ((screen_w - box_w) * 0.5).round();

        draw_hud_card_frame(
            &mut self.canvas,
            px,
            py,
            box_w,
            box_h,
            hs::CORNER_RADIUS,
            hs::STROKE_WIDTH,
            pal.bg,
            pal.fg2,
            alpha,
        );

        let cx = px + box_w * 0.5;
        if has_detail {
            let d_base =
                Self::text_row_baseline(py + hs::CONFIRM_DETAIL_CENTER_Y, hs::CONFIRM_DETAIL_FONT_SIZE);
            let q_base = Self::text_row_baseline(
                py + hs::CONFIRM_QUESTION_DETAIL_CENTER_Y,
                hs::CONFIRM_QUESTION_FONT_SIZE,
            );
            let _ = self.canvas.fill_text(cx, d_base, &fitted_d, &tp_d);
            let _ = self.canvas.fill_text(cx, q_base, &fitted_q, &tp_q);
        } else {
            let q_base = Self::text_row_baseline(
                py + hs::CONFIRM_QUESTION_SINGLE_CENTER_Y,
                hs::CONFIRM_QUESTION_FONT_SIZE,
            );
            let _ = self.canvas.fill_text(cx, q_base, &fitted_q, &tp_q);
        }

        let eff_btn_w = btn_w.min((box_w - hs::CONFIRM_INNER_INSET_X * 2.0 - hs::CONFIRM_BTN_GAP_X) * 0.5);
        let row_w = eff_btn_w * 2.0 + hs::CONFIRM_BTN_GAP_X;
        let row_x = px + (box_w - row_w) * 0.5;
        let btn_y = py + box_h - hs::CONFIRM_BTN_BOTTOM_INSET - hs::CONFIRM_BTN_HEIGHT;

        self.draw_hud_confirm_button(
            row_x,
            btn_y,
            eff_btn_w,
            hs::CONFIRM_BTN_HEIGHT,
            "B",
            cancel_label,
            false,
            alpha,
            pal,
        );
        self.draw_hud_confirm_button(
            row_x + eff_btn_w + hs::CONFIRM_BTN_GAP_X,
            btn_y,
            eff_btn_w,
            hs::CONFIRM_BTN_HEIGHT,
            "A",
            confirm_label,
            true,
            alpha,
            pal,
        );
    }

    fn draw_single_hud_item(
        &mut self,
        item: &HudRenderItem,
        screen_w: f32,
        now_ms: u64,
        pal: ResolvedTheme,
    ) {
        use crate::layout::volume_popup_spec as vp;

        match &item.payload {
            HudCardPayload::Volume { level_norm } => {
                let px = ((screen_w - vp::WIDTH) * 0.5).round();
                draw_volume_popup_alpha(
                    &mut self.canvas,
                    px,
                    item.y,
                    *level_norm,
                    pal.bg,
                    pal.fg,
                    pal.fg1,
                    pal.fg2,
                    item.opacity,
                );
            }
            HudCardPayload::Toast { line1, line2, .. } => {
                self.draw_hud_toast_card(line1, line2, screen_w, item.y, item.opacity, pal);
            }
            HudCardPayload::Progress {
                title,
                detail,
                progress,
                ..
            } => {
                self.draw_hud_progress_card(
                    title,
                    detail,
                    *progress,
                    screen_w,
                    item.y,
                    item.opacity,
                    now_ms,
                    pal,
                );
            }
            HudCardPayload::Confirmation {
                question,
                detail,
                confirm_label,
                cancel_label,
                ..
            } => {
                self.draw_hud_confirm_card(
                    question,
                    detail,
                    confirm_label,
                    cancel_label,
                    screen_w,
                    item.y,
                    item.opacity,
                    pal,
                );
            }
        }
    }

    fn draw_hud_stack_on_screen_internal(&mut self, win_w: u32, win_h: u32, now_ms: u64) {
        let items = self.hud_stack.collect_render_items(now_ms);
        if items.is_empty() {
            return;
        }
        let m = LogicalMetrics::from_physical(win_w, win_h);
        let pal = resolve_theme(self.last_theme);

        self.canvas.reset_transform();
        self.canvas.reset_scissor();
        self.canvas.scale(m.scale_x, m.scale_y);
        for item in &items {
            self.draw_single_hud_item(item, m.screen_w, now_ms, pal);
        }
        self.canvas.reset_transform();
    }

    /// Draws the Unified Stacked HUD directly onto `RenderTarget::Screen` over an already-rendered
    /// frame (such as the 4-shade GPU visualizer or legacy surface) without clearing the backbuffer.
    pub fn render_volume_popup_over_screen(
        &mut self,
        win_w: u32,
        win_h: u32,
        theme: ThemeColors,
        now_ms: u64,
    ) -> bool {
        self.last_theme = theme;
        if !self.hud_is_visible(now_ms) {
            return false;
        }
        let sw = win_w.max(1);
        let sh = win_h.max(1);
        self.canvas.set_size(sw, sh, 1.0);
        self.canvas.set_render_target(RenderTarget::Screen);
        self.draw_hud_stack_on_screen_internal(sw, sh, now_ms);
        self.canvas.flush();
        self.canvas
            .set_size(self.fbo_w as u32, self.fbo_h as u32, 1.0);
        self.restore_gl_state();
        true
    }

    /// Zero-copy GPU presentation: flushes `self.fbo_image` and blits it straight to
    /// `RenderTarget::Screen` at `(win_w, win_h)`, compositing the Unified Stacked HUD on top.
    pub fn plugin_present_gpu_screen(&mut self, win_w: u32, win_h: u32) {
        self.canvas.reset_scissor();
        self.canvas.flush();

        let sw = win_w.max(1);
        let sh = win_h.max(1);
        self.canvas.set_size(sw, sh, 1.0);
        self.canvas.set_render_target(RenderTarget::Screen);
        self.canvas.reset_transform();
        self.canvas.reset_scissor();

        let paint = Paint::image(self.fbo_image, 0.0, 0.0, sw as f32, sh as f32, 0.0, 1.0);
        let mut quad = VgPath::new();
        quad.rect(0.0, 0.0, sw as f32, sh as f32);
        self.canvas.fill_path(&quad, &paint);
        let now_ms = self.hud_stack.clock_ms();
        self.draw_hud_stack_on_screen_internal(sw, sh, now_ms);
        self.canvas.flush();

        self.canvas
            .set_size(self.fbo_w as u32, self.fbo_h as u32, 1.0);
        self.restore_gl_state();
    }

    /// Presents `self.fbo_image` faded toward `target_rgb` (`fade=1.0` normal, `0.0` solid `target_rgb`),
    /// used when fading seamlessly from Now Playing into the 4-shade GPU visualizer!
    pub fn present_fbo_fade_rgb(&mut self, win_w: u32, win_h: u32, fade: f32, target_rgb: u32) {
        self.canvas.reset_scissor();
        self.canvas.flush();

        let sw = win_w.max(1);
        let sh = win_h.max(1);
        let f = fade.clamp(0.0, 1.0);
        let bg = rgb_u32_to_color(target_rgb);

        self.canvas.set_size(sw, sh, 1.0);
        self.canvas.set_render_target(RenderTarget::Screen);
        self.canvas.reset_transform();
        self.canvas.reset_scissor();
        self.canvas.clear_rect(0, 0, sw, sh, bg);

        let mut bg_q = VgPath::new();
        bg_q.rect(0.0, 0.0, sw as f32, sh as f32);
        self.canvas.fill_path(&bg_q, &Paint::color(bg));

        if f > 0.001 {
            let paint = Paint::image(self.fbo_image, 0.0, 0.0, sw as f32, sh as f32, 0.0, f);
            let mut quad = VgPath::new();
            quad.rect(0.0, 0.0, sw as f32, sh as f32);
            self.canvas.fill_path(&quad, &paint);
        }
        self.canvas.flush();

        self.canvas
            .set_size(self.fbo_w as u32, self.fbo_h as u32, 1.0);
        self.restore_gl_state();
    }

    /// Flushes `self.fbo_image` without presenting to the window (used when display panel is off).
    pub fn plugin_flush_fbo_only(&mut self) {
        self.canvas.reset_scissor();
        self.canvas.flush();
        self.canvas.set_render_target(RenderTarget::Screen);
        self.canvas.flush();
        self.restore_gl_state();
    }

    /// Zero-copy GPU presentation for hardware video playback (`video_through`):
    /// clears the GL window backbuffer to `RGBA(0, 0, 0, 0)` so the hardware video plane
    /// underneath shows through, and blits only the bottom `bottom_rows_768` scanlines
    /// (`Progress`, `Volume`, or `.srt` `Subtitle` strip) from `self.fbo_image`.
    pub fn present_video_overlay_gpu(&mut self, win_w: u32, win_h: u32, bottom_rows_768: u32) {
        self.canvas.reset_scissor();
        self.canvas.flush();

        let sw = win_w.max(1);
        let sh = win_h.max(1);
        self.canvas.set_size(sw, sh, 1.0);
        self.canvas.set_render_target(RenderTarget::Screen);
        self.canvas.reset_transform();
        self.canvas.reset_scissor();
        self.canvas
            .clear_rect(0, 0, sw, sh, Color::rgba(0, 0, 0, 0));

        let rows = bottom_rows_768.min(HEIGHT as u32);
        if rows > 0 {
            let strip_h = ((rows as f32) * (sh as f32) / (HEIGHT as f32))
                .round()
                .max(1.0);
            let strip_y = (sh as f32 - strip_h).max(0.0);
            let paint = Paint::image(self.fbo_image, 0.0, 0.0, sw as f32, sh as f32, 0.0, 1.0);
            let mut quad = VgPath::new();
            quad.rect(0.0, strip_y, sw as f32, strip_h);
            self.canvas.fill_path(&quad, &paint);
        }
        self.canvas.flush();

        self.canvas
            .set_size(self.fbo_w as u32, self.fbo_h as u32, 1.0);
        self.restore_gl_state();
    }

    /// Synchronizes the most recently rendered `self.fbo_image` into Rockbox's CPU `1024x768` `{b, g, r}`
    /// framebuffer on demand (always blitting into `compat_fbo_image` so `finish_and_read_top_rows`
    /// executes a non-empty `flush()` with `compat`'s FBO bound before `glReadPixels`).
    pub fn plugin_sync_fbo_into_bgr(&mut self, fb_bgr: &mut [u8]) {
        self.canvas.reset_scissor();
        self.canvas.flush();

        let compat = match self.compat_fbo_image {
            Some(id) => id,
            None => match self.canvas.create_image_empty(
                WIDTH,
                HEIGHT,
                PixelFormat::Rgba8,
                ImageFlags::FLIP_Y,
            ) {
                Ok(id) => {
                    self.compat_fbo_image = Some(id);
                    id
                }
                Err(_) => return,
            },
        };
        self.canvas.reset();
        self.canvas.set_size(WIDTH as u32, HEIGHT as u32, 1.0);
        self.canvas.set_render_target(RenderTarget::Image(compat));
        let paint = Paint::image(
            self.fbo_image,
            0.0,
            0.0,
            WIDTH as f32,
            HEIGHT as f32,
            0.0,
            1.0,
        );
        let mut q = VgPath::new();
        q.rect(0.0, 0.0, WIDTH as f32, HEIGHT as f32);
        self.canvas.fill_path(&q, &paint);
        self.finish_and_read_top_rows(HEIGHT);
        self.unpack_top_rows_to_bgr(HEIGHT, fb_bgr);
        self.canvas
            .set_size(self.fbo_w as u32, self.fbo_h as u32, 1.0);
    }
}

const _: () = {
    assert!(SCREEN_W as usize == WIDTH);
    assert!(SCREEN_H as usize == HEIGHT);
};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn monochrome_art_uses_exact_four_theme_shades() {
        let mut rgb = vec![0u8; 256 * 256 * 3];
        for y in 0..256usize {
            for x in 0..256usize {
                let v = ((x + y) / 2) as u8;
                let i = (y * 256 + x) * 3;
                rgb[i] = v;
                rgb[i + 1] = v;
                rgb[i + 2] = v;
            }
        }
        let mut out = vec![RGBA8::new(0, 0, 0, 255); 256 * 256];
        let theme = ThemeColors::default();
        quantize_mono_art_4shade(&rgb, theme, &mut out);

        let c_fg = RGBA8::new(0x3C, 0x36, 0x37, 255);
        let c_fg2 = RGBA8::new(0x59, 0x59, 0x59, 255);
        let c_fg1 = RGBA8::new(0xA8, 0xA8, 0xA8, 255);
        let c_bg = RGBA8::new(0xD6, 0xD8, 0xD5, 255);

        let mut seen = [false; 4];
        for &px in &out {
            if px == c_fg {
                seen[0] = true;
            } else if px == c_fg2 {
                seen[1] = true;
            } else if px == c_fg1 {
                seen[2] = true;
            } else if px == c_bg {
                seen[3] = true;
            } else {
                panic!("unexpected shade outside 4-tone theme palette: {px:?}");
            }
        }
        assert_eq!(seen, [true, true, true, true]);
    }

    #[test]
    fn hud_toast_shares_volume_popup_timing_and_coexists_smoothly() {
        let mut vol = HudStack::default();
        let mut toast = HudStack::default();

        vol.trigger_volume(0.65, 1000);
        toast.trigger_toast("", "Nothing to resume", 2000, 1000);

        // Exact 1:1 entrance (0..180ms), hold (180..2000ms), and exit (2000..2200ms) curve parity
        for &t in &[1000u64, 1060, 1120, 1180, 2000, 2999, 3050, 3150, 3200] {
            let vp = vol.collect_render_items(t);
            let tp = toast.collect_render_items(t);
            assert_eq!(vp.len(), tp.len(), "count mismatch at t={t}");
            if let (Some(a), Some(b)) = (vp.first(), tp.first()) {
                assert!((a.y - b.y).abs() < 1e-5, "y curve mismatch at t={t}");
                assert!(
                    (a.opacity - b.opacity).abs() < 1e-5,
                    "opacity curve mismatch at t={t}"
                );
            }
        }
        assert!(!toast.is_visible(3205));

        // Repeated toast triggers with identical key keep position pinned at REST_Y and extend hold
        let mut hud = HudStack::default();
        hud.trigger_toast("", "Nothing to resume", 2000, 5000);
        assert!((hud.collect_render_items(5500)[0].y - 22.0).abs() < 1e-5);
        hud.trigger_toast("", "Nothing to resume", 2000, 6800);
        assert!((hud.collect_render_items(6800)[0].y - 22.0).abs() < 1e-5);
        assert!((hud.collect_render_items(8700)[0].y - 22.0).abs() < 1e-5);

        // Adjusting volume while toast is active stacks Volume at index 0 and pushes Toast down to index 1!
        hud.trigger_volume(0.5, 7500);
        assert_eq!(hud.active_count(), 2);
        assert!(hud.active_payload_at(0).unwrap().is_volume());
        assert!(hud.active_payload_at(1).unwrap().is_toast());
    }

    #[test]
    fn geologica_and_noto_cjk_fallback_cover_en_ua_and_cjk_without_overlap_drift() {
        let noto_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../assets/fonts/NotoSansCJK-Regular.ttf");
        let noto_bytes = std::fs::read(&noto_path).expect("read NotoSansCJK-Regular.ttf");
        let serif_cjk_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../assets/fonts/NotoSerifCJK-Regular.ttf");
        let serif_cjk_bytes =
            std::fs::read(&serif_cjk_path).expect("read NotoSerifCJK-Regular.ttf");

        // Verify valid static TrueType sfnt (`0x00010000`) and non-empty font tables
        assert_eq!(&noto_bytes[..4], &[0x00, 0x01, 0x00, 0x00]);
        assert_eq!(&serif_cjk_bytes[..4], &[0x00, 0x01, 0x00, 0x00]);
        assert!(FONT_GEOLOGICA_REGULAR.len() > 50_000);
        assert!(FONT_GEOLOGICA_SEMIBOLD.len() > 50_000);
        assert!(FONT_NOTO_SERIF_REGULAR.len() > 150_000);
        assert!(FONT_NOTO_SERIF_BOLD.len() > 150_000);
        assert!(FONT_NOTO_SERIF_ITALIC.len() > 150_000);
        assert!(FONT_NOTO_SERIF_BOLD_ITALIC.len() > 150_000);

        // Parse CMAP Format 4 / 12 via minimal big-endian SFNT walker to prove exact glyph resolution:
        // Geologica covers EN + UA (`Касета`) and yields 0 (`.notdef`) for CJK (`カセット`, `音楽`, `「」`),
        // which then fall through cleanly to `NotoSansCJK-Regular.ttf`!
        fn has_glyph_in_ttf(ttf: &[u8], cp: u32) -> bool {
            let num_tables = u16::from_be_bytes([ttf[4], ttf[5]]) as usize;
            let mut cmap_off = None;
            for i in 0..num_tables {
                let rec = 12 + i * 16;
                if &ttf[rec..rec + 4] == b"cmap" {
                    cmap_off = Some(u32::from_be_bytes([
                        ttf[rec + 8],
                        ttf[rec + 9],
                        ttf[rec + 10],
                        ttf[rec + 11],
                    ]) as usize);
                    break;
                }
            }
            let base = cmap_off.expect("cmap table");
            let n_sub = u16::from_be_bytes([ttf[base + 2], ttf[base + 3]]) as usize;
            // Prefer Format 12 if present, else Format 4
            let mut f12 = None;
            let mut f4 = None;
            for i in 0..n_sub {
                let r = base + 4 + i * 8;
                let sub = base
                    + u32::from_be_bytes([ttf[r + 4], ttf[r + 5], ttf[r + 6], ttf[r + 7]])
                        as usize;
                let fmt = u16::from_be_bytes([ttf[sub], ttf[sub + 1]]);
                if fmt == 12 {
                    f12 = Some(sub);
                } else if fmt == 4 {
                    f4 = Some(sub);
                }
            }
            if let Some(sub) = f12 {
                let n_groups =
                    u32::from_be_bytes([ttf[sub + 12], ttf[sub + 13], ttf[sub + 14], ttf[sub + 15]])
                        as usize;
                for g in 0..n_groups {
                    let o = sub + 16 + g * 12;
                    let start = u32::from_be_bytes([ttf[o], ttf[o + 1], ttf[o + 2], ttf[o + 3]]);
                    let end =
                        u32::from_be_bytes([ttf[o + 4], ttf[o + 5], ttf[o + 6], ttf[o + 7]]);
                    let gid =
                        u32::from_be_bytes([ttf[o + 8], ttf[o + 9], ttf[o + 10], ttf[o + 11]]);
                    if cp >= start && cp <= end {
                        return (gid + (cp - start)) != 0;
                    }
                }
                return false;
            }
            if let Some(sub) = f4 {
                if cp > 0xFFFF {
                    return false;
                }
                let seg_count = (u16::from_be_bytes([ttf[sub + 6], ttf[sub + 7]]) / 2) as usize;
                let end_codes = sub + 14;
                let start_codes = end_codes + seg_count * 2 + 2;
                let id_deltas = start_codes + seg_count * 2;
                let id_range_offsets = id_deltas + seg_count * 2;
                for s in 0..seg_count {
                    let end_c =
                        u16::from_be_bytes([ttf[end_codes + s * 2], ttf[end_codes + s * 2 + 1]])
                            as u32;
                    let start_c = u16::from_be_bytes([
                        ttf[start_codes + s * 2],
                        ttf[start_codes + s * 2 + 1],
                    ]) as u32;
                    if cp >= start_c && cp <= end_c {
                        let delta = i16::from_be_bytes([
                            ttf[id_deltas + s * 2],
                            ttf[id_deltas + s * 2 + 1],
                        ]) as i32;
                        let ro_pos = id_range_offsets + s * 2;
                        let ro = u16::from_be_bytes([ttf[ro_pos], ttf[ro_pos + 1]]) as usize;
                        if ro == 0 {
                            return (((cp as i32 + delta) & 0xFFFF) as u16) != 0;
                        }
                        let glyph_idx_addr = ro_pos + ro + ((cp - start_c) as usize) * 2;
                        let raw =
                            u16::from_be_bytes([ttf[glyph_idx_addr], ttf[glyph_idx_addr + 1]]);
                        if raw == 0 {
                            return false;
                        }
                        return (((raw as i32 + delta) & 0xFFFF) as u16) != 0;
                    }
                }
            }
            false
        }

        // Primary Geologica, Noto Serif (Regular/Bold/Italic/BoldItalic), and NotoSansCJK-Regular.ttf
        // all cover full EN + UA Cyrillic (`ҐґЄєІіЇї`)
        for ch in "UIPod Касета Привіт ҐґЄєІіЇї 0123456789".chars().filter(|c| !c.is_whitespace()) {
            assert!(
                has_glyph_in_ttf(FONT_GEOLOGICA_SEMIBOLD, ch as u32),
                "Geologica missing expected EN/UA glyph {ch:?}"
            );
            for (name, face) in [
                ("NotoSerif-Regular", FONT_NOTO_SERIF_REGULAR),
                ("NotoSerif-Bold", FONT_NOTO_SERIF_BOLD),
                ("NotoSerif-Italic", FONT_NOTO_SERIF_ITALIC),
                ("NotoSerif-BoldItalic", FONT_NOTO_SERIF_BOLD_ITALIC),
            ] {
                assert!(
                    has_glyph_in_ttf(face, ch as u32),
                    "{name} missing expected EN/UA Cyrillic glyph {ch:?}"
                );
            }
            assert!(
                has_glyph_in_ttf(&noto_bytes, ch as u32),
                "NotoSansCJK-Regular.ttf missing expected EN/UA Cyrillic glyph {ch:?}"
            );
        }

        // Full Pan-CJK (JP Kana/Kanji, Simplified Chinese, Traditional Chinese, Korean Hangul, CJK punctuation)
        for ch in "カセットあいう音楽日本語简体中文阅读器书签繁體中文閱讀器書籤한국어노래읽기설정「」『』・【】Ａ".chars() {
            assert!(
                !has_glyph_in_ttf(FONT_GEOLOGICA_SEMIBOLD, ch as u32),
                "Expected {ch:?} to bypass Geologica to NotoSansCJK fallback"
            );
            assert!(
                !has_glyph_in_ttf(FONT_NOTO_SERIF_REGULAR, ch as u32),
                "Expected {ch:?} to bypass NotoSerif-Regular to NotoSerifCJK fallback"
            );
            assert!(
                has_glyph_in_ttf(&noto_bytes, ch as u32),
                "NotoSansCJK-Regular.ttf missing CJK glyph {ch:?}"
            );
            assert!(
                has_glyph_in_ttf(&serif_cjk_bytes, ch as u32),
                "NotoSerifCJK-Regular.ttf missing CJK glyph {ch:?}"
            );
        }
    }
}
