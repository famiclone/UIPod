//! OpenGL ES 3.0 GPU renderer using `femtovg` + `taffy` + offscreen FBO readback.

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

use crate::icons::{
    draw_battery, draw_chevron_left_s, draw_chevron_s, draw_filetype_icon, draw_hold_lock,
    draw_play_mode_icon, draw_scrollbar_v, draw_seekbar_track_h, draw_toggle, fill_rounded_rect,
    stroke_rounded_rect_inside,
};
use crate::layout::{
    compute_header_layout, compute_list_layout, compute_now_playing_layout, HeaderBoxes,
    LIST_VISIBLE_ROWS, SCREEN_H, SCREEN_W,
};
use crate::{
    FileIconKind, HeaderState, ListRow, ListScreenState, NowPlayingState, RightAccessory,
    ThemeColors,
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

/// In `femtovg`'s `OpenGl` backend, `RenderTarget::Image` without `FLIP_Y` maps canvas `y = 0`
/// to the top of the FBO (`gl_y = height - 1`), so `glReadPixels` row `0` is the bottom scanline.
const FBO_ORIGIN_BOTTOM: bool = true;

#[inline]
fn rgb_u32_to_color(rgb: u32) -> Color {
    let r = ((rgb >> 16) & 0xFF) as u8;
    let g = ((rgb >> 8) & 0xFF) as u8;
    let b = (rgb & 0xFF) as u8;
    Color::rgb(r, g, b)
}

#[inline]
fn fnv1a_art_hash(bytes: &[u8], mono: bool) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    h ^= u64::from(mono);
    h = h.wrapping_mul(0x0100_0000_01b3);
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

/// Pure-Rust GPU UI engine backed by `taffy` + `femtovg` on OpenGL ES 3.0.
pub struct UiEngine {
    canvas: Canvas<OpenGl>,
    gl: glow::Context,
    fbo_image: ImageId,
    fonts_regular: Vec<FontId>,
    fonts_medium: Vec<FontId>,
    fonts_semibold: Vec<FontId>,
    fonts_bold: Vec<FontId>,
    readback_rgba: Vec<u8>,
    art_image: Option<ImageId>,
    art_hash: Option<u64>,
    art_rgba_scratch: Vec<RGBA8>,
}

impl UiEngine {
    /// Initializes the `femtovg` OpenGL ES 3.0 renderer, `glow` readback context,
    /// embedded `Geologica` font family, optional CJK fallback font, and `1024x768` FBO.
    ///
    /// # Safety
    /// A valid OpenGL ES 3.0 (or Desktop OpenGL 3.2+) context must be current on the calling thread,
    /// and `get_proc_address` must return valid function pointers for the active context.
    pub unsafe fn new<F>(mut get_proc_address: F, cjk_font_path: Option<&Path>) -> Result<Self, String>
    where
        F: FnMut(&std::ffi::CStr) -> *const c_void,
    {
        let renderer = OpenGl::new_from_function_cstr(|s| get_proc_address(s))
            .map_err(|e| format!("femtovg OpenGl init failed: {e:?}"))?;
        let mut canvas =
            Canvas::new(renderer).map_err(|e| format!("femtovg Canvas init failed: {e:?}"))?;

        let gl = glow::Context::from_loader_function_cstr(|s| get_proc_address(s));

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

        let cjk_id = cjk_font_path.and_then(|p| {
            if p.exists() {
                canvas.add_font(p).ok()
            } else {
                None
            }
        });

        let make_chain = |primary: FontId| -> Vec<FontId> {
            match cjk_id {
                Some(fallback) => vec![primary, fallback],
                None => vec![primary],
            }
        };

        let fbo_image = canvas
            .create_image_empty(WIDTH, HEIGHT, PixelFormat::Rgba8, ImageFlags::empty())
            .map_err(|e| format!("FBO image creation failed: {e:?}"))?;

        let mut engine = Self {
            canvas,
            gl,
            fbo_image,
            fonts_regular: make_chain(f_reg),
            fonts_medium: make_chain(f_med),
            fonts_semibold: make_chain(f_semi),
            fonts_bold: make_chain(f_bold),
            readback_rgba: vec![0u8; WIDTH * HEIGHT * 4],
            art_image: None,
            art_hash: None,
            art_rgba_scratch: vec![RGBA8::new(0, 0, 0, 255); 256 * 256],
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

    fn begin_offscreen(&mut self, clear_rows: usize, bg: Color) {
        self.canvas.reset();
        self.canvas.reset_scissor();
        self.canvas
            .set_render_target(RenderTarget::Image(self.fbo_image));
        let rows = clear_rows.min(HEIGHT);
        self.canvas
            .clear_rect(0, 0, WIDTH as u32, rows as u32, bg);
        // Also draw an explicit opaque background quad inside the command stream.
        let mut bg_rect = VgPath::new();
        bg_rect.rect(0.0, 0.0, WIDTH as f32, rows as f32);
        self.canvas.fill_path(&bg_rect, &Paint::color(bg));
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
            let src_line = &self.readback_rgba[src_row * row_bytes_src..(src_row + 1) * row_bytes_src];
            let dst_line = &mut out_bgr[y * row_bytes_dst..(y + 1) * row_bytes_dst];
            for (rgba, bgr) in src_line.chunks_exact(4).zip(dst_line.chunks_exact_mut(3)) {
                bgr[0] = rgba[2]; // B
                bgr[1] = rgba[1]; // G
                bgr[2] = rgba[0]; // R
            }
        }
    }

    /// Restores clean OpenGL ES 3.0 state so `window-sdl.c` and `projectM` see default bindings.
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

    /// Draws the shared `Header` (`[271]`: `960x80` at `(32, 32)` + `4px` bottom border at `y=112..116`).
    fn draw_header_component(&mut self, boxes: &HeaderBoxes, state: &HeaderState, fg: Color, bg: Color) {
        let semibold = self.fonts_semibold.clone();

        // Right-side Battery [1564] (64x64) + optional Hold padlock to its left
        draw_battery(
            &mut self.canvas,
            boxes.right_slot.x,
            boxes.right_slot.y,
            state.battery_percent,
            state.is_charging,
            fg,
            bg,
        );

        let right_guard_x = if state.is_hold {
            let lock_x = boxes.right_slot.x - 40.0;
            let lock_y = boxes.right_slot.center_y() - 18.0;
            draw_hold_lock(&mut self.canvas, lock_x, lock_y, fg, bg);
            lock_x - 16.0
        } else {
            boxes.right_slot.x - 16.0
        };

        if !state.legend.trim().is_empty() {
            // Header button-legend (%Tl) overrides left icon + centered title
            let mut lp = Paint::color(fg);
            lp.set_font(&semibold);
            lp.set_font_size(40.0);
            lp.set_text_align(Align::Left);
            lp.set_text_baseline(Baseline::Alphabetic);
            let max_w = (right_guard_x - boxes.container.x).max(0.0);
            let txt = self.truncate_ellipsis(state.legend, max_w, &lp);
            let base_y = Self::text_row_baseline(boxes.title_slot.center_y(), 40.0);
            let _ = self.canvas.fill_text(boxes.container.x, base_y, &txt, &lp);
        } else {
            // Left play-mode icon (48x48 inside 85x48 Frame 4 [1572])
            draw_play_mode_icon(
                &mut self.canvas,
                boxes.left_slot.x,
                boxes.left_slot.y,
                state.play_mode,
                fg,
            );

            // Centered screen title: Geologica SemiBold 48px centered in spaceBetween slot (x_center = 522.5)
            let title = if state.title.trim().is_empty() {
                "UIPod Mini"
            } else {
                state.title
            };
            let mut tp = Paint::color(fg);
            tp.set_font(&semibold);
            tp.set_font_size(48.0);
            tp.set_text_align(Align::Center);
            tp.set_text_baseline(Baseline::Alphabetic);

            let title_cx = boxes.title_slot.center_x();
            let half_avail = (title_cx - (boxes.left_slot.x + 56.0))
                .min(right_guard_x - title_cx)
                .max(60.0);
            let fitted = self.truncate_ellipsis(title, half_avail * 2.0, &tp);
            let base_y = Self::text_row_baseline(boxes.title_slot.center_y(), 48.0);
            let _ = self.canvas.fill_text(title_cx, base_y, &fitted, &tp);
        }

        // 4px outside bottom stroke spanning (32, 112, 960, 4)
        let mut line = VgPath::new();
        line.rect(boxes.container.x, boxes.border_y, boxes.container.w, 4.0);
        self.canvas.fill_path(&line, &Paint::color(fg));
    }

    /// Draws a single `MenuItem` row (`h=60`, `r=8`, `padding=[0,16,0,16]` per `ui.figcraft` `[353]`).
    fn draw_menu_item(&mut self, bx: crate::layout::LayoutBox, row: &ListRow, fg: Color, bg: Color) {
        let (ink, cutout) = if row.selected {
            fill_rounded_rect(&mut self.canvas, bx.x, bx.y, bx.w, bx.h, 8.0, fg);
            (bg, fg)
        } else {
            (fg, bg)
        };

        let bold = self.fonts_bold.clone();
        let semibold = self.fonts_semibold.clone();
        let medium = self.fonts_medium.clone();

        // Left filetype icon (32x32 at x + 16, gap = 16 before label) if present
        let text_x = if row.file_icon != FileIconKind::None {
            draw_filetype_icon(
                &mut self.canvas,
                bx.x + 16.0,
                bx.y + (bx.h - 32.0) * 0.5,
                &row.file_icon,
                ink,
                cutout,
                &bold,
            );
            bx.x + 16.0 + 32.0 + 16.0
        } else {
            bx.x + 16.0
        };

        // Right accessory (icon_play_s chevron, toggle switch, or Active4/NotActive4 value)
        let inner_right = bx.x + bx.w - 16.0;
        let right_limit = match row.right {
            RightAccessory::None => inner_right,
            RightAccessory::Chevron => {
                let cx = inner_right - 32.0;
                let cy = bx.y + (bx.h - 32.0) * 0.5;
                draw_chevron_s(&mut self.canvas, cx, cy, ink);
                cx - 16.0
            }
            RightAccessory::Toggle(enabled) => {
                let tx = inner_right - 64.0;
                let ty = bx.y + (bx.h - 32.0) * 0.5;
                draw_toggle(&mut self.canvas, tx, ty, enabled, ink, cutout);
                tx - 16.0
            }
            RightAccessory::Value(val) => {
                let trimmed = val.trim();
                if trimmed.is_empty() {
                    inner_right
                } else {
                    let mut vp = Paint::color(ink);
                    vp.set_font(&medium);
                    vp.set_font_size(36.0);
                    vp.set_text_align(Align::Right);
                    vp.set_text_baseline(Baseline::Alphabetic);
                    let base_y = Self::text_row_baseline(bx.center_y(), 36.0);

                    if row.selected {
                        // ui.figcraft [1293]/[1186] Type=Active4: [icon_play_s_left] 10px [Value] 10px [icon_play_s]
                        let arrow_y = bx.y + (bx.h - 32.0) * 0.5;
                        let right_arrow_x = inner_right - 32.0;
                        draw_chevron_s(&mut self.canvas, right_arrow_x, arrow_y, ink);

                        let val_right_x = right_arrow_x - 10.0;
                        let max_val_w = (bx.w * 0.36).max(80.0);
                        let val_fitted = self.truncate_ellipsis(trimmed, max_val_w, &vp);
                        let val_w = self
                            .canvas
                            .measure_text(0.0, 0.0, &val_fitted, &vp)
                            .map(|m| m.width())
                            .unwrap_or(0.0);
                        let _ = self.canvas.fill_text(val_right_x, base_y, &val_fitted, &vp);

                        let left_arrow_x = val_right_x - val_w - 10.0 - 32.0;
                        draw_chevron_left_s(&mut self.canvas, left_arrow_x, arrow_y, ink);

                        (left_arrow_x - 16.0).max(text_x)
                    } else {
                        // ui.figcraft [1318] Type=NotActive4: right-aligned Value without arrows
                        let max_val_w = (bx.w * 0.42).max(80.0);
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
        };

        // Main item label: Geologica SemiBold 48px
        let avail_w = (right_limit - text_x).max(0.0);
        if avail_w > 8.0 && !row.label.is_empty() {
            let mut lp = Paint::color(ink);
            lp.set_font(&semibold);
            lp.set_font_size(48.0);
            lp.set_text_align(Align::Left);
            lp.set_text_baseline(Baseline::Alphabetic);
            let fitted = self.truncate_ellipsis(row.label, avail_w, &lp);
            let base_y = Self::text_row_baseline(bx.center_y(), 48.0);
            let _ = self.canvas.fill_text(text_x, base_y, &fitted, &lp);
        }
    }

    fn ensure_album_art_uploaded(&mut self, rgb24: &[u8], mono: bool) -> Option<ImageId> {
        if rgb24.len() < 256 * 256 * 3 {
            return None;
        }
        let hash = fnv1a_art_hash(&rgb24[..256 * 256 * 3], mono);
        if self.art_hash == Some(hash) {
            if let Some(id) = self.art_image {
                return Some(id);
            }
        }

        for (src, dst) in rgb24[..256 * 256 * 3]
            .chunks_exact(3)
            .zip(self.art_rgba_scratch.iter_mut())
        {
            if mono {
                // ui.figcraft [252]: saturation = -1.0, contrast = +0.48
                let r = src[0] as f32 / 255.0;
                let g = src[1] as f32 / 255.0;
                let b = src[2] as f32 / 255.0;
                let luma = 0.2126 * r + 0.7152 * g + 0.0722 * b;
                let contrasted = ((luma - 0.5) * 1.48 + 0.5).clamp(0.0, 1.0);
                let v = (contrasted * 255.0 + 0.5) as u8;
                *dst = RGBA8::new(v, v, v, 255);
            } else {
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

    /// Draws `AlbumCover` `Type=NoCover` (`[1400]`): `256x256` `r=8` `stroke=4 inside` + filled vinyl disc (`180` / `78` / `8`).
    fn draw_album_art_placeholder(&mut self, x: f32, y: f32, w: f32, h: f32, fg: Color, bg: Color) {
        stroke_rounded_rect_inside(&mut self.canvas, x, y, w, h, 8.0, 4.0, fg);
        let cx = x + w * 0.5;
        let cy = y + h * 0.5;

        // [1397] Outer vinyl disc: 180x180 (radius 90) filled in fg
        let mut outer_disc = VgPath::new();
        outer_disc.circle(cx, cy, 90.0);
        self.canvas.fill_path(&outer_disc, &Paint::color(fg));

        // [1398] Center label cutout: 78x78 (radius 39) filled in bg
        let mut label_cut = VgPath::new();
        label_cut.circle(cx, cy, 39.0);
        self.canvas.fill_path(&label_cut, &Paint::color(bg));

        // [1399] Spindle dot: 8x8 (radius 4) filled in fg
        let mut spindle = VgPath::new();
        spindle.circle(cx, cy, 4.0);
        self.canvas.fill_path(&spindle, &Paint::color(fg));
    }

    fn draw_list_screen(&mut self, state: &ListScreenState) {
        let bg = rgb_u32_to_color(state.theme.bg_rgb);
        let fg = rgb_u32_to_color(state.theme.fg_rgb);
        self.begin_offscreen(HEIGHT, bg);

        let vis = if state.visible_items > 0 {
            state.visible_items
        } else {
            LIST_VISIBLE_ROWS
        };
        // Only display the right-hand Scrollbar when total_items exceeds visible capacity (> 8),
        // letting MenuItem rows span the full 960px width on screens that fit on a single page.
        let show_scrollbar = state.total_items > vis;
        let boxes = compute_list_layout(state.rows.len(), show_scrollbar);

        self.draw_header_component(&boxes.header, &state.header, fg, bg);

        for (row_data, row_box) in state.rows.iter().zip(boxes.rows.iter()) {
            self.draw_menu_item(*row_box, row_data, fg, bg);
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
                fg,
            );
        }
    }

    fn draw_now_playing_screen(&mut self, state: &NowPlayingState) {
        let bg = rgb_u32_to_color(state.theme.bg_rgb);
        let fg = rgb_u32_to_color(state.theme.fg_rgb);
        self.begin_offscreen(HEIGHT, bg);

        let np = compute_now_playing_layout();
        self.draw_header_component(&np.header, &state.header, fg, bg);

        let regular = self.fonts_regular.clone();
        let semibold = self.fonts_semibold.clone();

        // 1. Sub-header row [247] (y = 144, h = 40): "21 of 500" (Regular 32px, ls=2%) + optional [EQ] badge
        if !state.track_pos_label.is_empty() {
            let mut pp = Paint::color(fg);
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

        if state.eq_enabled {
            // [249]: 55x34 at right edge (x = 937, y = 147), r=4, stroke=2 inside
            let eq_w = 55.0;
            let eq_h = 34.0;
            let eq_x = np.subheader_row.x + np.subheader_row.w - eq_w;
            let eq_y = np.subheader_row.y + (np.subheader_row.h - eq_h) * 0.5;
            stroke_rounded_rect_inside(&mut self.canvas, eq_x, eq_y, eq_w, eq_h, 4.0, 2.0, fg);

            let mut eqp = Paint::color(fg);
            eqp.set_font(&semibold);
            eqp.set_font_size(24.0);
            eqp.set_text_align(Align::Center);
            eqp.set_text_baseline(Baseline::Alphabetic);
            let base_y = Self::cap_centered_baseline(eq_y + eq_h * 0.5, 24.0);
            let _ = self.canvas.fill_text(eq_x + eq_w * 0.5, base_y, "EQ", &eqp);
        }

        // 2. Album Art [1402] Type=Cover (32, 256, 256, 256): r=8 image + 4px inside rounded border [1403]
        let art_drawn = if let Some(rgb24) = state.art_rgb24 {
            if let Some(img_id) = self.ensure_album_art_uploaded(rgb24, state.mono_album_art) {
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
                stroke_rounded_rect_inside(
                    &mut self.canvas,
                    np.art_box.x,
                    np.art_box.y,
                    np.art_box.w,
                    np.art_box.h,
                    8.0,
                    4.0,
                    fg,
                );
                true
            } else {
                false
            }
        } else {
            false
        };
        if !art_drawn {
            self.draw_album_art_placeholder(
                np.art_box.x,
                np.art_box.y,
                np.art_box.w,
                np.art_box.h,
                fg,
                bg,
            );
        }

        // 3. Metadata column [253]: Title (SemiBold 48), Artist (Regular 36 ls=2%), Album (Regular 36 ls=2%), Specs row
        if !state.title.is_empty() {
            let mut tp = Paint::color(fg);
            tp.set_font(&semibold);
            tp.set_font_size(48.0);
            tp.set_text_align(Align::Left);
            tp.set_text_baseline(Baseline::Alphabetic);
            let fitted = self.truncate_ellipsis(state.title, np.title_box.w, &tp);
            let base_y = Self::text_row_baseline(np.title_box.center_y(), 48.0);
            let _ = self.canvas.fill_text(np.title_box.x, base_y, &fitted, &tp);
        }

        if !state.artist.is_empty() {
            let mut ap = Paint::color(fg);
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
            let mut alp = Paint::color(fg);
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
            let mut cp = Paint::color(fg);
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
                fg,
            );
            let _ = self
                .canvas
                .fill_text(cursor_x + badge_w * 0.5, specs_base_y, codec, &cp);
            cursor_x += badge_w + 21.0;
        }

        let mut sp = Paint::color(fg);
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

        // 4. Seekbar [640] (32, 594, 960, 84): horizontal track 960x34 + labels row 960x40
        draw_seekbar_track_h(
            &mut self.canvas,
            np.seekbar_track.x,
            np.seekbar_track.y,
            np.seekbar_track.w,
            np.seekbar_track.h,
            state.progress,
            fg,
        );

        let label_base_y = Self::cap_centered_baseline(np.seekbar_labels.center_y(), 32.0);
        let mut lp = Paint::color(fg);
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
            if !state.remaining_label.is_empty() {
                lp.set_text_align(Align::Right);
                let _ = self.canvas.fill_text(
                    np.seekbar_labels.x + np.seekbar_labels.w,
                    label_base_y,
                    state.remaining_label,
                    &lp,
                );
            }
        }
    }

    /// Renders only the top `Header` band (`1024x124`) into Rockbox's `{b, g, r}` framebuffer,
    /// leaving rows `124..768` untouched for non-list screens (dialogs, sliders, keyboard).
    pub fn render_header_into_bgr(&mut self, theme: ThemeColors, state: &HeaderState, fb_bgr: &mut [u8]) {
        let bg = rgb_u32_to_color(theme.bg_rgb);
        let fg = rgb_u32_to_color(theme.fg_rgb);
        self.begin_offscreen(HEADER_BAND_ROWS, bg);
        let boxes = compute_header_layout();
        self.draw_header_component(&boxes, state, fg, bg);
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
}

const _: () = {
    assert!(SCREEN_W as usize == WIDTH);
    assert!(SCREEN_H as usize == HEIGHT);
};
