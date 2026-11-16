//! Vector primitives and composite UI icons faithfully implementing `assets/icons/ui.figcraft`.

use femtovg::{Align, Baseline, Canvas, Color, FontId, LineCap, LineJoin, Paint, Path, Renderer};

use crate::{FileIconKind, PlayMode};

/// Builds a closed polygon path with circular corner rounding of `radius` at every vertex.
pub fn rounded_polygon_path(pts: &[(f32, f32)], radius: f32) -> Path {
    let mut path = Path::new();
    let n = pts.len();
    if n < 3 {
        return path;
    }
    let (x_last, y_last) = pts[n - 1];
    let (x0, y0) = pts[0];
    path.move_to((x_last + x0) * 0.5, (y_last + y0) * 0.5);
    for i in 0..n {
        let (vx, vy) = pts[i];
        let (nx, ny) = pts[(i + 1) % n];
        path.arc_to(vx, vy, nx, ny, radius);
    }
    path.close();
    path
}

/// Draws an inside-aligned rounded rectangle stroke (`stroke_align: inside` in FigCraft).
pub fn stroke_rounded_rect_inside<R: Renderer>(
    canvas: &mut Canvas<R>,
    x: f32,
    y: f32,
    w: f32,
    h: f32,
    outer_radius: f32,
    stroke_w: f32,
    color: Color,
) {
    let clamped_outer = outer_radius.min(w * 0.5).min(h * 0.5);
    let half = stroke_w * 0.5;
    let rw = (w - stroke_w).max(0.0);
    let rh = (h - stroke_w).max(0.0);
    let rr = (clamped_outer - half).max(0.0);
    let mut path = Path::new();
    path.rounded_rect(x + half, y + half, rw, rh, rr);
    let mut paint = Paint::color(color);
    paint.set_line_width(stroke_w);
    canvas.stroke_path(&path, &paint);
}

/// Draws a filled rounded rectangle.
pub fn fill_rounded_rect<R: Renderer>(
    canvas: &mut Canvas<R>,
    x: f32,
    y: f32,
    w: f32,
    h: f32,
    radius: f32,
    color: Color,
) {
    if w <= 0.0 || h <= 0.0 {
        return;
    }
    let mut path = Path::new();
    path.rounded_rect(x, y, w, h, radius.min(w * 0.5).min(h * 0.5));
    let paint = Paint::color(color);
    canvas.fill_path(&path, &paint);
}

/// Draws `icon_play` (`48x48` component `[90]`/`[91]`) or Pause / FastForward / Rewind in `(x, y, 48, 48)`.
pub fn draw_play_mode_icon<R: Renderer>(
    canvas: &mut Canvas<R>,
    x: f32,
    y: f32,
    mode: PlayMode,
    color: Color,
) {
    let paint = Paint::color(color);
    match mode {
        PlayMode::Stop => {}
        PlayMode::Play => {
            // Exact Node [91] inside [90] (48x48): transform (6.2146246, 3.4433433), r = 4.0
            let ox = x + 6.214_624_6;
            let oy = y + 3.443_343_3;
            let pts = [
                (ox + 0.0, oy - 2.381_612_2),
                (ox + 0.0, oy + 43.494_926),
                (ox + 38.583_8, oy + 20.556_657),
            ];
            let path = rounded_polygon_path(&pts, 4.0);
            canvas.fill_path(&path, &paint);
        }
        PlayMode::Pause => {
            // Balanced twin rounded bars matching the 36px optical height of 48x48 icon_play
            let mut path = Path::new();
            path.rounded_rect(x + 8.0, y + 6.0, 10.5, 36.0, 3.0);
            path.rounded_rect(x + 26.5, y + 6.0, 10.5, 36.0, 3.0);
            canvas.fill_path(&path, &paint);
        }
        PlayMode::FastForward => {
            let t1 = rounded_polygon_path(
                &[
                    (x + 4.5, y + 7.5),
                    (x + 4.5, y + 40.5),
                    (x + 25.5, y + 24.0),
                ],
                2.5,
            );
            let t2 = rounded_polygon_path(
                &[
                    (x + 24.0, y + 7.5),
                    (x + 24.0, y + 40.5),
                    (x + 45.0, y + 24.0),
                ],
                2.5,
            );
            canvas.fill_path(&t1, &paint);
            canvas.fill_path(&t2, &paint);
        }
        PlayMode::Rewind => {
            let t1 = rounded_polygon_path(
                &[
                    (x + 24.0, y + 7.5),
                    (x + 24.0, y + 40.5),
                    (x + 3.0, y + 24.0),
                ],
                2.5,
            );
            let t2 = rounded_polygon_path(
                &[
                    (x + 43.5, y + 7.5),
                    (x + 43.5, y + 40.5),
                    (x + 22.5, y + 24.0),
                ],
                2.5,
            );
            canvas.fill_path(&t1, &paint);
            canvas.fill_path(&t2, &paint);
        }
    }
}

/// Draws `icon_play_s` (`32x32` submenu chevron `[326]`/`[325]`).
pub fn draw_chevron_s<R: Renderer>(canvas: &mut Canvas<R>, x: f32, y: f32, color: Color) {
    // Exact Node [325] inside [326] (32x32): transform (4.0, 0.7462484), r = 2.0
    let ox = x + 4.0;
    let oy = y + 0.746_248_4;
    let pts = [
        (ox + 0.0, oy - 1.512_692_1),
        (ox + 0.0, oy + 32.020_195),
        (ox + 28.202_35, oy + 15.253_752),
    ];
    let path = rounded_polygon_path(&pts, 2.0);
    let paint = Paint::color(color);
    canvas.fill_path(&path, &paint);
}

/// Draws left-pointing `icon_play_s` (`32x32` rotated 180°, used in `MenuItem` `Active4` `[1340]`).
pub fn draw_chevron_left_s<R: Renderer>(canvas: &mut Canvas<R>, x: f32, y: f32, color: Color) {
    // Exact 180-degree rotation of [325] around the 32x32 box center (16.0, 16.0)
    let ox = x + 32.0 - 4.0;
    let oy = y + 32.0 - 0.746_248_4;
    let pts = [
        (ox - 0.0, oy + 1.512_692_1),
        (ox - 0.0, oy - 32.020_195),
        (ox - 28.202_35, oy - 15.253_752),
    ];
    let path = rounded_polygon_path(&pts, 2.0);
    let paint = Paint::color(color);
    canvas.fill_path(&path, &paint);
}

/// Draws `toggle` (`64x32` componentSet `[50]`: `[46]` `enabled=False` / `[51]` `enabled=True`).
pub fn draw_toggle<R: Renderer>(
    canvas: &mut Canvas<R>,
    x: f32,
    y: f32,
    enabled: bool,
    ink_color: Color,
    cutout_color: Color,
) {
    if enabled {
        // [51] enabled=True: 64x32 filled pill (r=16) in ink_color, 20x20 circle at (38, 6) in cutout_color
        fill_rounded_rect(canvas, x, y, 64.0, 32.0, 16.0, ink_color);
        let mut knob = Path::new();
        knob.circle(x + 38.0 + 10.0, y + 6.0 + 10.0, 10.0);
        canvas.fill_path(&knob, &Paint::color(cutout_color));
    } else {
        // [46] enabled=False: 64x32 hollow pill ([44], r=16, stroke=4 inside) + 20x20 hollow circle at (7, 6, stroke=4 inside)
        stroke_rounded_rect_inside(canvas, x, y, 64.0, 32.0, 16.0, 4.0, ink_color);
        let mut knob = Path::new();
        // 20x20 circle at (7, 6) with 4px inside stroke => centerline radius = 8.0 at (17, 16)
        knob.circle(x + 7.0 + 10.0, y + 6.0 + 10.0, 8.0);
        let mut paint = Paint::color(ink_color);
        paint.set_line_width(4.0);
        canvas.stroke_path(&knob, &paint);
    }
}

fn draw_charge_bolt_scaled<R: Renderer>(
    canvas: &mut Canvas<R>,
    ox: f32,
    oy: f32,
    scale: f32,
    color: Color,
) {
    const V: [(f32, f32); 13] = [
        (7.948_998, 14.250_425),
        (1.024_976_7, 14.250_425),
        (0.131_554_58, 13.849_91),
        (0.354_910_1, 12.648_365),
        (11.522_686, 0.632_912_7),
        (13.086_175, 0.032_140_1),
        (13.532_886, 1.433_942_9),
        (10.070_875, 9.644_502),
        (17.106_575, 9.644_502),
        (17.999_996, 10.645_79),
        (6.608_865, 23.462_273),
        (5.045_376, 23.939_013),
        (4.486_987, 22.661_242),
    ];

    // Segments: (a, b, ta, tb)
    const SEGS: [(usize, usize, (f32, f32), (f32, f32)); 13] = [
        (0, 1, (0.0, 0.0), (0.0, 0.0)),
        (1, 2, (0.0, 0.0), (0.223_355_52, 0.400_515_08)),
        (
            2,
            3,
            (-0.223_355_52, -0.400_515_08),
            (-0.394_209_83, 0.528_503_18),
        ),
        (3, 4, (0.394_209_83, -0.528_503_18), (0.0, 0.0)),
        (4, 5, (0.0, 0.0), (-0.670_066_54, -0.200_257_54)),
        (
            5,
            6,
            (0.670_066_54, 0.200_257_54),
            (0.223_355_52, -0.600_772_6),
        ),
        (
            6,
            7,
            (-0.223_355_52, 0.600_772_6),
            (-0.050_875_634, -0.095_680_81),
        ),
        (
            7,
            8,
            (0.050_875_634, 0.095_680_81),
            (0.094_785_21, -0.091_194_04),
        ),
        (
            8,
            9,
            (-0.094_785_21, 0.091_194_04),
            (-0.007_454_753_5, -0.938_792_6),
        ),
        (9, 10, (0.007_454_753_5, 0.938_792_6), (0.0, 0.0)),
        (10, 11, (0.0, 0.0), (0.893_422_1, 0.276_482_55)),
        (
            11,
            12,
            (-0.893_422_1, -0.276_482_55),
            (-0.111_677_76, 0.200_257_54),
        ),
        (12, 0, (0.111_677_76, -0.200_257_54), (0.0, 0.0)),
    ];

    let mut path = Path::new();
    path.move_to(ox + V[0].0 * scale, oy + V[0].1 * scale);
    for &(a, b, ta, tb) in &SEGS {
        let va = V[a];
        let vb = V[b];
        if ta.0.abs() < 1e-5 && ta.1.abs() < 1e-5 && tb.0.abs() < 1e-5 && tb.1.abs() < 1e-5 {
            path.line_to(ox + vb.0 * scale, oy + vb.1 * scale);
        } else {
            path.bezier_to(
                ox + (va.0 + ta.0) * scale,
                oy + (va.1 + ta.1) * scale,
                ox + (vb.0 + tb.0) * scale,
                oy + (vb.1 + tb.1) * scale,
                ox + vb.0 * scale,
                oy + vb.1 * scale,
            );
        }
    }
    path.close();
    canvas.fill_path(&path, &Paint::color(color));
}

/// Draws `icon_charge` (`28x28` component `[195]`/`[197]`) using the exact cubic Bezier network.
pub fn draw_charge_bolt<R: Renderer>(canvas: &mut Canvas<R>, x: f32, y: f32, color: Color) {
    // Transform of Vector [197] inside 28x28 icon_charge [195]: (5.0, 2.0)
    draw_charge_bolt_scaled(canvas, x + 5.0, y + 2.0, 1.0, color);
}

/// Draws a crisp padlock icon (`28x36`) when the Hold switch is engaged.
pub fn draw_hold_lock<R: Renderer>(
    canvas: &mut Canvas<R>,
    x: f32,
    y: f32,
    fg: Color,
    bg: Color,
) {
    // Shackle arch
    let mut shackle = Path::new();
    shackle.move_to(x + 7.0, y + 16.0);
    shackle.line_to(x + 7.0, y + 9.0);
    shackle.arc_to(x + 7.0, y + 2.0, x + 14.0, y + 2.0, 7.0);
    shackle.arc_to(x + 21.0, y + 2.0, x + 21.0, y + 9.0, 7.0);
    shackle.line_to(x + 21.0, y + 16.0);
    let mut sp = Paint::color(fg);
    sp.set_line_width(3.5);
    sp.set_line_cap(LineCap::Round);
    sp.set_line_join(LineJoin::Round);
    canvas.stroke_path(&shackle, &sp);

    // Body
    fill_rounded_rect(canvas, x + 2.0, y + 15.0, 24.0, 19.0, 4.0, fg);
    // Keyhole cutout
    let mut kh = Path::new();
    kh.circle(x + 14.0, y + 23.0, 2.5);
    kh.rounded_rect(x + 12.75, y + 24.0, 2.5, 6.0, 1.0);
    canvas.fill_path(&kh, &Paint::color(bg));
}

/// Builds the outer contour of `Battery` shell `[1562]` (`64.0 x 38.4` at `(ox, oy)`).
fn battery_shell_outer_path(ox: f32, oy: f32) -> Path {
    const V: [(f32, f32); 10] = [
        (51.2, 0.0),
        (9.6, 0.0),
        (0.0, 9.6),
        (0.0, 28.8),
        (9.6, 38.4),
        (51.2, 38.4),
        (60.8, 28.8),
        (64.0, 25.6),
        (64.0, 12.8),
        (60.8, 9.6),
    ];
    const SEGS: [(usize, usize, (f32, f32), (f32, f32)); 10] = [
        (0, 1, (0.0, 0.0), (0.0, 0.0)),
        (1, 2, (-5.301_933_3, 0.0), (0.0, -5.301_933_3)),
        (2, 3, (0.0, 0.0), (0.0, 0.0)),
        (3, 4, (0.0, 5.301_935), (-5.301_933_3, 0.0)),
        (4, 5, (0.0, 0.0), (0.0, 0.0)),
        (5, 6, (5.301_935, 0.0), (0.0, 5.301_935)),
        (6, 7, (1.767_309_6, 0.0), (0.0, 1.767_312_6)),
        (7, 8, (0.0, 0.0), (0.0, 0.0)),
        (8, 9, (0.0, -1.767_312_6), (1.767_309_6, 0.0)),
        (9, 0, (0.0, -5.301_933_3), (5.301_935, 0.0)),
    ];

    let mut p = Path::new();
    p.move_to(ox + V[0].0, oy + V[0].1);
    for &(a, b, ta, tb) in &SEGS {
        let va = V[a];
        let vb = V[b];
        if ta.0.abs() < 1e-5 && ta.1.abs() < 1e-5 && tb.0.abs() < 1e-5 && tb.1.abs() < 1e-5 {
            p.line_to(ox + vb.0, oy + vb.1);
        } else {
            p.bezier_to(
                ox + va.0 + ta.0,
                oy + va.1 + ta.1,
                ox + vb.0 + tb.0,
                oy + vb.1 + tb.1,
                ox + vb.0,
                oy + vb.1,
            );
        }
    }
    p.close();
    p
}

/// Draws `Battery` (`64x64` componentSet `[1564]`: `[1560]` Default & `[1565]` Variant2 charging).
pub fn draw_battery<R: Renderer>(
    canvas: &mut Canvas<R>,
    x: f32,
    y: f32,
    percent: u8,
    is_charging: bool,
    fg: Color,
    bg: Color,
) {
    // 1. Shell [1562]: outer body at (x, y + 12.8) + inner cavity cutout (x + 6.4, y + 19.2, 48.0, 25.6, r = 3.2)
    let shell = battery_shell_outer_path(x, y + 12.8);
    canvas.fill_path(&shell, &Paint::color(fg));
    let cav_x = x + 6.4;
    let cav_y = y + 19.2;
    let cav_w = 48.0;
    let cav_h = 25.6;
    fill_rounded_rect(canvas, cav_x, cav_y, cav_w, cav_h, 3.2, bg);

    // 2. Inner fill bar [1561]: (x + 9.6, y + 22.4, 41.6 * pct, 19.2), left corners r=3.2, right flat (r=3.2 at full)
    let ix = x + 9.6;
    let iy = y + 22.4;
    let iw_max = 41.6;
    let ih = 19.2;

    let clamped_pct = percent.min(100);
    let pct = clamped_pct as f32 / 100.0;
    let fill_w = (iw_max * pct).clamp(0.0, iw_max);
    if fill_w > 0.5 {
        let r_left = 3.2_f32.min(fill_w * 0.5);
        let r_right = if clamped_pct >= 98 { r_left } else { 0.0 };
        let mut bar = Path::new();
        bar.rounded_rect_varying(ix, iy, fill_w, ih, r_left, r_right, r_right, r_left);
        canvas.fill_path(&bar, &Paint::color(fg));
    }

    // 3. Variant2 [1565]/[1570]: 12x16 charging bolt [1569] at (x + 25.0, y + 24.4), subtracted from fill bar
    if is_charging {
        let split_x = (ix + fill_w).clamp(cav_x, cav_x + cav_w);
        let filled_span = split_x - cav_x;
        let empty_span = (cav_x + cav_w) - split_x;

        if filled_span > 0.0 {
            canvas.save();
            canvas.intersect_scissor(cav_x, cav_y, filled_span, cav_h);
            draw_charge_bolt_scaled(canvas, x + 25.0, y + 24.4, 2.0 / 3.0, bg);
            canvas.restore();
        }
        if empty_span > 0.0 {
            canvas.save();
            canvas.intersect_scissor(split_x, cav_y, empty_span, cav_h);
            draw_charge_bolt_scaled(canvas, x + 25.0, y + 24.4, 2.0 / 3.0, fg);
            canvas.restore();
        }
    }
}

/// Draws vertical `Scrollbar` (`16xh` component `[312]`): `r=8`, `stroke=4 inside`, `4px` inset `8px` thumb `r=4`.
pub fn draw_scrollbar_v<R: Renderer>(
    canvas: &mut Canvas<R>,
    x: f32,
    y: f32,
    w: f32,
    h: f32,
    total_items: usize,
    start_item: usize,
    visible_items: usize,
    fg: Color,
) {
    stroke_rounded_rect_inside(canvas, x, y, w, h, 8.0, 4.0, fg);
    let inset = 4.0;
    let inner_w = (w - inset * 2.0).max(4.0); // 8.0 when w == 16.0
    let inner_h = (h - inset * 2.0).max(8.0);
    if total_items == 0 {
        return;
    }
    let vis = visible_items.min(total_items) as f32;
    let tot = total_items as f32;
    let thumb_h = (inner_h * (vis / tot)).clamp(24.0_f32.min(inner_h), inner_h);
    let max_start = total_items.saturating_sub(visible_items);
    let scroll_ratio = if max_start > 0 {
        (start_item.min(max_start) as f32) / (max_start as f32)
    } else {
        0.0
    };
    let thumb_y = y + inset + (inner_h - thumb_h) * scroll_ratio;
    fill_rounded_rect(canvas, x + inset, thumb_y, inner_w, thumb_h, 4.0, fg);
}

/// Draws horizontal `Seekbar` track (`wx24` component `[641]`): `r=16` (pill), `stroke=4 inside`, `4px` inset `16px` fill `r=8`.
pub fn draw_seekbar_track_h<R: Renderer>(
    canvas: &mut Canvas<R>,
    x: f32,
    y: f32,
    w: f32,
    h: f32,
    progress: f32,
    fg: Color,
) {
    stroke_rounded_rect_inside(canvas, x, y, w, h, 16.0, 4.0, fg);
    let inset = 4.0;
    let inner_w_max = (w - inset * 2.0).max(0.0);
    let inner_h = (h - inset * 2.0).max(4.0); // 16.0 when h == 24.0
    let p = progress.clamp(0.0, 1.0);
    let fill_w = inner_w_max * p;
    if fill_w >= 1.0 {
        fill_rounded_rect(canvas, x + inset, y + inset, fill_w, inner_h, 8.0, fg);
    }
}

/// Draws `icon_note_16` (`16x16` component `[404]`/`[398]`) at `(x, y)`.
pub fn draw_icon_note_16<R: Renderer>(canvas: &mut Canvas<R>, x: f32, y: f32, color: Color) {
    // Group 1 [398] transform inside 16x16: (1.620755, 1.0)
    let gx = x + 1.620_754_7;
    let gy = y + 1.0;
    let paint = Paint::color(color);

    let mut path = Path::new();
    // Left notehead ellipse [399]: (0.0, 8.9811, w=4.966, h=5.0189)
    path.ellipse(
        gx + 2.483_018_9,
        gy + 8.981_132 + 2.509_434,
        2.483_018_9,
        2.509_434,
    );
    // Left stem [400]: (3.7245, 2.1925, w=1.2679, h=9.2981)
    path.rect(gx + 3.724_528_3, gy + 2.192_452_9, 1.267_924_5, 9.298_113);
    // Right notehead ellipse [401]: (7.7660, 7.3434, w=4.966, h=5.0189)
    path.ellipse(
        gx + 7.766_038 + 2.483_018_9,
        gy + 7.343_396 + 2.509_434,
        2.483_018_9,
        2.509_434,
    );
    // Right stem [402]: (11.4906, 0.6604, w=1.2679, h=9.2981)
    path.move_to(gx + 11.490_566, gy + 0.660_377_4 + 9.298_113);
    path.line_to(gx + 11.490_566, gy + 0.660_377_4 + 0.475_471_7);
    path.line_to(gx + 11.490_566 + 1.267_924_5, gy + 0.660_377_4);
    path.line_to(
        gx + 11.490_566 + 1.267_924_5,
        gy + 0.660_377_4 + 9.298_113,
    );
    path.close();
    // Slanted top beam [403]: transform (3.7245, 0.0)
    let bx = gx + 3.724_528_3;
    let by = gy;
    path.move_to(bx, by + 2.192_452_9);
    path.line_to(bx + 9.007_547, by);
    path.line_to(bx + 9.007_547, by + 3.328_302);
    path.line_to(bx, by + 5.415_094_4);
    path.close();

    canvas.fill_path(&path, &paint);
}

/// Draws `icon_play_16` (`16x16` component `[449]`/`[450]`) at `(x, y)`.
fn draw_icon_play_16<R: Renderer>(canvas: &mut Canvas<R>, x: f32, y: f32, color: Color) {
    let ox = x + 2.159_841;
    let oy = y + 2.039_539_3;
    let pts = [
        (ox + 0.0, oy - 0.634_550_5),
        (ox + 0.0, oy + 12.555_472),
        (ox + 12.873_573, oy + 5.960_460_7),
    ];
    let path = rounded_polygon_path(&pts, 1.0);
    canvas.fill_path(&path, &Paint::color(color));
}

/// Draws `icon_txt_16` (`16x16` component `[474]`/`[476]`): crisp typographic `'A'` glyph.
fn draw_icon_txt_16<R: Renderer>(
    canvas: &mut Canvas<R>,
    x: f32,
    y: f32,
    color: Color,
    bold_fonts: &[FontId],
) {
    let mut tp = Paint::color(color);
    tp.set_font(bold_fonts);
    tp.set_font_size(14.5);
    tp.set_text_align(Align::Center);
    tp.set_text_baseline(Baseline::Alphabetic);
    let _ = canvas.fill_text(x + 8.0, y + 8.0 + 14.5 * 0.355, "A", &tp);
}

/// Draws `icon_filetype` (`32x32` component `[412]`) or `icon_dir` (`32x32` component `[1536]`).
pub fn draw_filetype_icon<R: Renderer>(
    canvas: &mut Canvas<R>,
    x: f32,
    y: f32,
    kind: &FileIconKind,
    ink_color: Color,
    cutout_color: Color,
    bold_fonts: &[FontId],
) {
    match kind {
        FileIconKind::None => {}
        FileIconKind::Folder | FileIconKind::AddFolder => {
            // ui.figcraft [1536] `icon_dir`: 32x28 vector [1533] at (x, y + 2.0)
            let ox = x;
            let oy = y + 2.0;
            const V: [(f32, f32); 11] = [
                (29.714_285, 4.923_084),
                (14.194_286, 4.923_084),
                (11.554_285, 1.009_239),
                (9.702_857, 0.000_008_3),
                (2.285_714_4, 0.000_008_3),
                (0.0, 2.461_546_2),
                (0.0, 25.538_462),
                (2.285_714_4, 28.0),
                (29.714_285, 28.0),
                (32.0, 25.538_462),
                (32.0, 7.384_621_6),
            ];
            const SEGS: [(usize, usize, (f32, f32), (f32, f32)); 11] = [
                (0, 1, (0.0, 0.0), (0.0, 0.0)),
                (1, 2, (0.0, 0.0), (0.0, 0.0)),
                (
                    2,
                    3,
                    (-0.431_614_46, -0.636_096_5),
                    (0.731_552_1, -0.002_044_09),
                ),
                (3, 4, (0.0, 0.0), (0.0, 0.0)),
                (4, 5, (-1.262_365_1, 0.0), (0.0, -1.359_469_7)),
                (5, 6, (0.0, 0.0), (0.0, 0.0)),
                (6, 7, (0.0, 1.359_470_9), (-1.262_365_1, 0.0)),
                (7, 8, (0.0, 0.0), (0.0, 0.0)),
                (8, 9, (1.262_366_2, 0.0), (0.0, 1.359_470_9)),
                (9, 10, (0.0, 0.0), (0.0, 0.0)),
                (10, 0, (0.0, -1.359_469_7), (1.262_366_2, 0.0)),
            ];

            let mut outer = Path::new();
            outer.move_to(ox + V[0].0, oy + V[0].1);
            for &(a, b, ta, tb) in &SEGS {
                let (ax, ay) = V[a];
                let (bx, by) = V[b];
                if ta.0 == 0.0 && ta.1 == 0.0 && tb.0 == 0.0 && tb.1 == 0.0 {
                    outer.line_to(ox + bx, oy + by);
                } else {
                    outer.bezier_to(
                        ox + ax + ta.0,
                        oy + ay + ta.1,
                        ox + bx + tb.0,
                        oy + by + tb.1,
                        ox + bx,
                        oy + by,
                    );
                }
            }
            outer.close();
            canvas.fill_path(&outer, &Paint::color(ink_color));

            // Inner tab cutout quad (v11..v14 of [1533])
            let mut tab_cut = Path::new();
            tab_cut.move_to(ox + 2.285_714_4, oy + 4.923_084);
            tab_cut.line_to(ox + 2.285_714_4, oy + 2.461_546_2);
            tab_cut.line_to(ox + 9.702_857, oy + 2.461_546_2);
            tab_cut.line_to(ox + 11.554_285, oy + 4.923_084);
            tab_cut.close();
            canvas.fill_path(&tab_cut, &Paint::color(cutout_color));

            if *kind == FileIconKind::AddFolder {
                // [1537] Type=Add: 13x13 rounded plus [1541] subtracted at (ox + 16.0, oy + 10.0)
                let px = ox + 16.0;
                let py = oy + 10.0;
                let arm = 4.333_333_5;
                let rad = 1.444_444_4;
                let mut plus_cut = Path::new();
                plus_cut.rounded_rect(px, py + arm, 13.0, arm, rad);
                plus_cut.rounded_rect(px + arm, py, arm, 13.0, rad);
                canvas.fill_path(&plus_cut, &Paint::color(cutout_color));
            }
        }
        FileIconKind::Audio(label) | FileIconKind::Video(label) | FileIconKind::Text(label) => {
            // Outer 32x32 frame: corner_radius = 4.0, stroke = 2.0 inside
            stroke_rounded_rect_inside(canvas, x, y, 32.0, 32.0, 4.0, 2.0, ink_color);

            // Top 16x16 symbol centered horizontally at (x + 8.0, y + 2.0)
            match kind {
                FileIconKind::Audio(_) => draw_icon_note_16(canvas, x + 8.0, y + 2.0, ink_color),
                FileIconKind::Video(_) => draw_icon_play_16(canvas, x + 8.0, y + 2.0, ink_color),
                FileIconKind::Text(_) => {
                    draw_icon_txt_16(canvas, x + 8.0, y + 2.0, ink_color, bold_fonts)
                }
                _ => {}
            }

            // Bottom Frame 18 [484]: (x + 2.0, y + 18.0, 28.0, 12.0) filled in ink_color
            let mut pill = Path::new();
            pill.rounded_rect_varying(x + 2.0, y + 18.0, 28.0, 12.0, 0.0, 0.0, 2.0, 2.0);
            canvas.fill_path(&pill, &Paint::color(ink_color));

            // Label [6]: 8px Bold uppercase centered in (x + 2.0, y + 18.0, 28.0, 12.0) in cutout_color
            let tag = label.trim();
            if !tag.is_empty() {
                let mut tp = Paint::color(cutout_color);
                tp.set_font(bold_fonts);
                let fsz = if tag.len() >= 4 { 7.5 } else { 8.5 };
                tp.set_font_size(fsz);
                tp.set_text_align(Align::Center);
                tp.set_text_baseline(Baseline::Alphabetic);
                let _ = canvas.fill_text(x + 16.0, y + 24.0 + fsz * 0.36, tag, &tp);
            }
        }
    }
}
