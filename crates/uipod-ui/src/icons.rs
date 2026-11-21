//! Vector primitives and composite UI icons faithfully implementing `assets/icons/ui.figcraft`.

use femtovg::{Align, Baseline, Canvas, Color, FontId, LineCap, Paint, Path, Renderer};

use crate::{FileIconKind, HeaderSourceIcon, PlayMode};

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

/// Draws `icon_arrow_s` (`32x32` submenu chevron `[1738]`/`[1737]`).
pub fn draw_arrow_s<R: Renderer>(canvas: &mut Canvas<R>, x: f32, y: f32, color: Color) {
    let ox = x + 6.055_625;
    let oy = y + 0.999_941_4;
    const V: [(f32, f32); 10] = [
        (0.569_767_6, 0.911_383_5),
        (4.088_759, 0.569_767_6),
        (18.977_438, 12.823_519),
        (19.888_632, 14.729_569),
        (19.014_591, 16.652_94),
        (4.125_911, 29.399_187),
        (0.600_954_5, 29.125_912),
        (0.874_231, 25.600_954),
        (13.500_52, 14.791_546),
        (0.911_383_5, 4.430_374_6),
    ];
    const SEGS: [(usize, usize, (f32, f32), (f32, f32)); 10] = [
        (
            0,
            1,
            (0.877_407_9, -1.066_077_2),
            (-1.066_077_2, -0.877_407_9),
        ),
        (1, 2, (0.0, 0.0), (0.0, 0.0)),
        (
            2,
            3,
            (0.570_453_4, 0.469_497_26),
            (-0.007_167_53, -0.738_778_3),
        ),
        (
            3,
            4,
            (0.007_167_53, 0.738_778_3),
            (0.561_236_86, -0.480_476_77),
        ),
        (4, 5, (0.0, 0.0), (0.0, 0.0)),
        (
            5,
            6,
            (-1.048_853_2, 0.897_926_7),
            (0.897_926_7, 1.048_853_2),
        ),
        (
            6,
            7,
            (-0.897_926_7, -1.048_853_2),
            (-1.048_853_2, 0.897_926_7),
        ),
        (7, 8, (0.0, 0.0), (0.0, 0.0)),
        (8, 9, (0.0, 0.0), (0.0, 0.0)),
        (
            9,
            0,
            (-1.066_077_2, -0.877_407_9),
            (-0.877_407_9, 1.066_077_2),
        ),
    ];

    let mut path = Path::new();
    path.move_to(ox + V[0].0, oy + V[0].1);
    for &(a, b, ta, tb) in &SEGS {
        let va = V[a];
        let vb = V[b];
        if ta.0.abs() < 1e-5 && ta.1.abs() < 1e-5 && tb.0.abs() < 1e-5 && tb.1.abs() < 1e-5 {
            path.line_to(ox + vb.0, oy + vb.1);
        } else {
            path.bezier_to(
                ox + va.0 + ta.0,
                oy + va.1 + ta.1,
                ox + vb.0 + tb.0,
                oy + vb.1 + tb.1,
                ox + vb.0,
                oy + vb.1,
            );
        }
    }
    path.close();
    canvas.fill_path(&path, &Paint::color(color));
}

/// Draws `icon_play_s` (`32x32` cycle arrow `[326]`/`[325]`).
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

/// Draws `icon_lock_64` (`[2968]`/`[2970]`: `30.476x40.0` at `(x + 16.7619, y + 12.0)`) when Hold is engaged.
pub fn draw_hold_lock<R: Renderer>(
    canvas: &mut Canvas<R>,
    x: f32,
    y: f32,
    fg: Color,
    bg: Color,
) {
    let ox = x + 16.761_906;
    let oy = y + 12.0;

    // Outer silhouette: rounded 30.476x26.667 body + 19.048-wide arched shackle (r = 9.5238)
    let shackle_outer_r = 9.523_81;
    let mut shackle_outer = Path::new();
    shackle_outer.rounded_rect(ox + 5.714_285, oy, 19.047_62, 22.0, shackle_outer_r);
    canvas.fill_path(&shackle_outer, &Paint::color(fg));

    fill_rounded_rect(
        canvas,
        ox,
        oy + 13.333_333,
        30.476_19,
        26.666_666,
        3.809_523_8,
        fg,
    );

    // Inner shackle cutout ([2970] loop 2: x=9.5238..20.9524, y=3.8095..13.3333, r=5.7143)
    let mut shackle_cut = Path::new();
    shackle_cut.rounded_rect_varying(
        ox + 9.523_809,
        oy + 3.809_524,
        11.428_571,
        9.523_809,
        5.714_285_4,
        5.714_285_4,
        0.0,
        0.0,
    );
    canvas.fill_path(&shackle_cut, &Paint::color(bg));

    // Circular keyhole cutout ([2970] loop 1: cx=15.2381, cy=26.6667, r=3.8095)
    let mut keyhole = Path::new();
    keyhole.circle(ox + 15.238_094, oy + 26.666_666, 3.809_523_8);
    canvas.fill_path(&keyhole, &Paint::color(bg));
}

/// Draws `icon_fx_64` (`[3187]`: `40x40` `r=8` badge at `(x+12, y+12)` in `fg` with `[3195]` `FX` cutout in `bg`).
pub fn draw_fx_64<R: Renderer>(
    canvas: &mut Canvas<R>,
    x: f32,
    y: f32,
    fg: Color,
    bg: Color,
) {
    fill_rounded_rect(canvas, x + 12.0, y + 12.0, 40.0, 40.0, 8.0, fg);

    let ox = x + 12.0 + 5.926_156;
    let oy = y + 12.0 + 11.6;
    const F_PTS: [(f32, f32); 10] = [
        (0.0, 16.8),
        (0.0, 0.0),
        (10.836, 0.0),
        (10.836, 3.24),
        (3.552, 3.24),
        (3.552, 7.176),
        (10.092, 7.176),
        (10.092, 10.296),
        (3.552, 10.296),
        (3.552, 16.8),
    ];
    const X_PTS: [(f32, f32); 12] = [
        (12.559_688, 16.8),
        (17.839_687, 8.22),
        (12.799_687, 0.0),
        (17.071_688, 0.0),
        (20.395_687, 5.544),
        (23.863_688, 0.0),
        (28.015_688, 0.0),
        (22.807_688, 8.292),
        (28.147_688, 16.8),
        (23.839_687, 16.8),
        (20.239_687, 10.908),
        (16.747_688, 16.8),
    ];

    let mut cut = Path::new();
    cut.move_to(ox + F_PTS[0].0, oy + F_PTS[0].1);
    for &(px, py) in &F_PTS[1..] {
        cut.line_to(ox + px, oy + py);
    }
    cut.close();

    cut.move_to(ox + X_PTS[0].0, oy + X_PTS[0].1);
    for &(px, py) in &X_PTS[1..] {
        cut.line_to(ox + px, oy + py);
    }
    cut.close();

    canvas.fill_path(&cut, &Paint::color(bg));
}

fn append_speaker_cone_and_box(path: &mut Path, ox: f32, oy: f32) {
    // Exact loops 0 & 1 of `icon_speaker_32` ([1599] & [1602])
    const V: [(f32, f32); 18] = [
        (17.997_786, 1.029_413),
        (17.997_786, 24.956_47),
        (17.509_096, 25.853_859),
        (16.384_233, 25.783_867),
        (8.188_992, 19.409_65),
        (7.999_016, 19.015_951),
        (7.999_016, 6.977_431),
        (8.192_742, 6.583_73),
        (16.387_983, 0.209_513_96),
        (17.637_83, 0.229_511_48),
        (5.499_323_4, 6.997_429),
        (1.999_754, 6.997_429),
        (0.0, 8.997_183),
        (0.0, 16.996_199),
        (1.999_754, 18.995_953),
        (5.499_323_4, 18.995_953),
        (5.999_262, 18.496_014),
        (5.999_262, 7.497_367_4),
    ];
    const CONE_SEGS: [(usize, usize, (f32, f32), (f32, f32)); 10] = [
        (0, 1, (0.0, 0.0), (0.0, 0.0)),
        (1, 2, (0.006_189, 0.364_24), (0.309_328, -0.192_42)),
        (2, 3, (-0.353_638, 0.210_341), (0.324_837, 0.252_557)),
        (3, 4, (0.0, 0.0), (0.0, 0.0)),
        (4, 5, (-0.120_385, -0.095_129), (-0.000_444, 0.153_433)),
        (5, 6, (0.0, 0.0), (0.0, 0.0)),
        (6, 7, (0.000_459, -0.154_074), (-0.121_793, 0.094_37)),
        (7, 8, (0.0, 0.0), (0.0, 0.0)),
        (8, 9, (0.369_952, -0.286_668), (-0.360_589, -0.298_357)),
        (9, 0, (0.232_782, 0.199_977), (0.004_711, -0.306_849)),
    ];
    const BOX_SEGS: [(usize, usize, (f32, f32), (f32, f32)); 8] = [
        (10, 11, (0.0, 0.0), (0.0, 0.0)),
        (11, 12, (-1.104_434, 0.0), (0.0, -1.104_434)),
        (12, 13, (0.0, 0.0), (0.0, 0.0)),
        (13, 14, (0.0, 1.104_435), (-1.104_434, 0.0)),
        (14, 15, (0.0, 0.0), (0.0, 0.0)),
        (15, 16, (0.276_109, 0.0), (0.0, 0.276_108)),
        (16, 17, (0.0, 0.0), (0.0, 0.0)),
        (17, 10, (0.0, -0.276_108), (0.276_109, 0.0)),
    ];

    for loop_segs in [&CONE_SEGS[..], &BOX_SEGS[..]] {
        let start = V[loop_segs[0].0];
        path.move_to(ox + start.0, oy + start.1);
        for &(a, b, ta, tb) in loop_segs {
            let va = V[a];
            let vb = V[b];
            if ta.0.abs() < 1e-5 && ta.1.abs() < 1e-5 && tb.0.abs() < 1e-5 && tb.1.abs() < 1e-5 {
                path.line_to(ox + vb.0, oy + vb.1);
            } else {
                path.bezier_to(
                    ox + va.0 + ta.0,
                    oy + va.1 + ta.1,
                    ox + vb.0 + tb.0,
                    oy + vb.1 + tb.1,
                    ox + vb.0,
                    oy + vb.1,
                );
            }
        }
        path.close();
    }
}

/// Draws `icon_speaker_32` (`Property 1=Silent` `[1601]`/`[1602]`, `32x32`).
pub fn draw_speaker_silent_32<R: Renderer>(canvas: &mut Canvas<R>, x: f32, y: f32, color: Color) {
    let mut path = Path::new();
    append_speaker_cone_and_box(&mut path, x + 7.001_032, y + 3.002_808);
    canvas.fill_path(&path, &Paint::color(color));
}

/// Draws `icon_speaker_32` (`Property 1=Default` `[1597]`/`[1599]`, `32x32` with 2 acoustic waves).
pub fn draw_speaker_loud_32<R: Renderer>(canvas: &mut Canvas<R>, x: f32, y: f32, color: Color) {
    let ox = x + 2.0;
    let oy = y + 3.002_808;
    let mut path = Path::new();
    append_speaker_cone_and_box(&mut path, ox, oy);

    // Exact wave loops 2 & 3 from `[1599]`
    const W: [(f32, f32); 18] = [
        (21.343_624, 9.602_108),
        (21.005_79, 10.289_613),
        (21.253_635, 11.014_435),
        (21.253_635, 14.978_947),
        (21.043_798, 15.965_848),
        (21.800_613, 16.633_106),
        (22.753_45, 16.301_285),
        (22.753_45, 9.692_097),
        (22.065_346, 9.353_639),
        (21.339_876, 9.602_108),
        (26.454_247, 6.331_260_7),
        (25.493_563, 5.991_954_3),
        (24.734_112, 6.671_127_3),
        (24.964_43, 7.663_597),
        (24.964_43, 18.329_784),
        (24.734_112, 19.322_254),
        (25.493_563, 20.001_427),
        (26.454_247, 19.662_12),
    ];
    const WAVE1: [(usize, usize, (f32, f32), (f32, f32)); 10] = [
        (0, 1, (-0.199_412, 0.175_235), (0.016_88, -0.264_929)),
        (1, 2, (-0.016_88, 0.264_929), (-0.175_559, -0.199_127)),
        (2, 3, (0.997_747, 1.133_178), (0.997_747, -1.133_177)),
        (3, 4, (-0.242_964, 0.266_971), (-0.113_328, -0.342_727)),
        (4, 5, (0.113_328, 0.342_727), (-0.354_224, -0.069_499)),
        (5, 6, (0.354_225, 0.069_499), (-0.234_43, 0.274_497)),
        (6, 7, (1.663_888, -1.888_895), (1.663_888, 1.888_894)),
        (7, 8, (-0.175_271, -0.199_786), (0.265_234, 0.016_885)),
        (8, 9, (-0.265_236, -0.016_885), (0.199_19, -0.175_947)),
        (9, 0, (0.0, 0.0), (0.0, 0.0)),
    ];
    const WAVE2: [(usize, usize, (f32, f32), (f32, f32)); 8] = [
        (10, 11, (-0.234_268, -0.280_006), (0.358_158, -0.070_765)),
        (11, 12, (-0.358_158, 0.070_765), (0.110_174, -0.348_062)),
        (12, 13, (-0.110_174, 0.348_062), (-0.252_204, -0.263_967)),
        (13, 14, (2.716_365, 3.036_561), (2.716_365, -3.036_562)),
        (14, 15, (-0.252_204, 0.263_967), (-0.110_174, -0.348_062)),
        (15, 16, (0.110_174, 0.348_062), (-0.358_158, -0.070_765)),
        (16, 17, (0.358_158, 0.070_765), (-0.234_268, 0.280_006)),
        (17, 10, (3.394_339, -3.795_414), (3.394_339, 3.795_413)),
    ];

    for loop_segs in [&WAVE1[..], &WAVE2[..]] {
        let start = W[loop_segs[0].0];
        path.move_to(ox + start.0, oy + start.1);
        for &(a, b, ta, tb) in loop_segs {
            let va = W[a];
            let vb = W[b];
            if ta.0.abs() < 1e-5 && ta.1.abs() < 1e-5 && tb.0.abs() < 1e-5 && tb.1.abs() < 1e-5 {
                path.line_to(ox + vb.0, oy + vb.1);
            } else {
                path.bezier_to(
                    ox + va.0 + ta.0,
                    oy + va.1 + ta.1,
                    ox + vb.0 + tb.0,
                    oy + vb.1 + tb.1,
                    ox + vb.0,
                    oy + vb.1,
                );
            }
        }
        path.close();
    }
    canvas.fill_path(&path, &Paint::color(color));
}

/// Draws `VolumeBarPopup` (`[3214]`: `527x72`, `r=16`, `padding=16`, `bg` fill + `4px` inside `fg2` stroke).
pub fn draw_volume_popup<R: Renderer>(
    canvas: &mut Canvas<R>,
    x: f32,
    y: f32,
    level: f32,
    bg: Color,
    fg: Color,
    fg1: Color,
    fg2: Color,
) {
    use crate::layout::volume_popup_spec as vp;

    let clamped = level.clamp(0.0, 1.0);
    fill_rounded_rect(canvas, x, y, vp::WIDTH, vp::HEIGHT, vp::CORNER_RADIUS, bg);
    stroke_rounded_rect_inside(
        canvas,
        x,
        y,
        vp::WIDTH,
        vp::HEIGHT,
        vp::CORNER_RADIUS,
        vp::STROKE_WIDTH,
        fg2,
    );

    let bar_x = x + vp::INNER_INSET;
    let bar_y = y + vp::INNER_INSET;
    draw_speaker_silent_32(canvas, bar_x, bar_y, fg2);

    let track_x = bar_x + vp::TRACK_OFFSET_X;
    let track_y = bar_y + vp::TRACK_OFFSET_Y;
    stroke_rounded_rect_inside(
        canvas,
        track_x,
        track_y,
        vp::TRACK_WIDTH,
        vp::TRACK_HEIGHT,
        vp::TRACK_HEIGHT * 0.5,
        vp::STROKE_WIDTH,
        fg1,
    );
    if clamped > 0.001 {
        let max_fill = vp::TRACK_WIDTH - vp::STROKE_WIDTH * 2.0;
        let fill_h = vp::TRACK_HEIGHT - vp::STROKE_WIDTH * 2.0;
        let fill_w = (max_fill * clamped).clamp(fill_h, max_fill);
        fill_rounded_rect(
            canvas,
            track_x + vp::STROKE_WIDTH,
            track_y + vp::STROKE_WIDTH,
            fill_w,
            fill_h,
            fill_h * 0.5,
            fg,
        );
    }

    draw_speaker_loud_32(canvas, bar_x + vp::LOUD_ICON_OFFSET_X, bar_y, fg2);
}

/// Draws `icon_arrow_64` (`[1590]`/`[1589]`: `40x40` download arrow rotated 180° at `(x+52, y+52)`).
pub fn draw_arrow_down_64<R: Renderer>(canvas: &mut Canvas<R>, x: f32, y: f32, color: Color) {
    const V: [(f32, f32); 17] = [
        (10.220_049, 37.564_68),
        (10.220_049, 25.388_123),
        (1.845_965_2, 25.388_123),
        (0.195_599_12, 24.353_115),
        (0.440_097_66, 22.404_865),
        (18.105_135, 0.913_241_8),
        (20.0, 0.0),
        (21.894_865, 0.913_241_8),
        (39.559_902, 22.404_865),
        (39.804_4, 24.353_115),
        (38.154_034, 25.388_123),
        (29.779_951, 25.388_123),
        (29.779_951, 37.564_68),
        (29.075_794, 39.301_056),
        (27.334_963, 39.999_992),
        (12.665_037, 39.999_992),
        (10.924_206, 39.298_622),
    ];
    const SEGS: [(usize, usize, (f32, f32), (f32, f32)); 17] = [
        (0, 1, (0.0, 0.0), (0.0, 0.0)),
        (1, 2, (0.0, 0.0), (0.0, 0.0)),
        (2, 3, (-0.774_246, 0.0), (0.325_998, 0.690_005)),
        (3, 4, (-0.325_998, -0.690_005), (-0.488_997, 0.608_828)),
        (4, 5, (0.0, 0.0), (0.0, 0.0)),
        (5, 6, (0.488_996, -0.608_828), (-0.774_247, 0.0)),
        (6, 7, (0.774_247, 0.0), (-0.488_996, -0.608_828)),
        (7, 8, (0.0, 0.0), (0.0, 0.0)),
        (8, 9, (0.488_996, 0.608_828), (0.325_997, -0.690_005)),
        (9, 10, (-0.326_0, 0.690_005), (0.774_247, 0.0)),
        (10, 11, (0.0, 0.0), (0.0, 0.0)),
        (11, 12, (0.0, 0.0), (0.0, 0.0)),
        (12, 13, (0.0, 0.690_004), (0.469_438, -0.467_58)),
        (13, 14, (-0.469_438, 0.467_58), (0.691_116, 0.001_623)),
        (14, 15, (0.0, 0.0), (0.0, 0.0)),
        (15, 16, (-0.692_747, 0.0), (0.467_807, 0.467_58)),
        (16, 0, (-0.467_807, -0.467_58), (0.001_631, 0.688_381)),
    ];

    // Transform in [1589]: [-1, 0, 0, -1, 52, 52]
    let tx = |px: f32| x + 52.0 - px;
    let ty = |py: f32| y + 52.0 - py;

    let mut path = Path::new();
    path.move_to(tx(V[0].0), ty(V[0].1));
    for &(a, b, ta, tb) in &SEGS {
        let va = V[a];
        let vb = V[b];
        if ta.0.abs() < 1e-5 && ta.1.abs() < 1e-5 && tb.0.abs() < 1e-5 && tb.1.abs() < 1e-5 {
            path.line_to(tx(vb.0), ty(vb.1));
        } else {
            path.bezier_to(
                tx(va.0 + ta.0),
                ty(va.1 + ta.1),
                tx(vb.0 + tb.0),
                ty(vb.1 + tb.1),
                tx(vb.0),
                ty(vb.1),
            );
        }
    }
    path.close();
    canvas.fill_path(&path, &Paint::color(color));
}

/// Draws `64x64` `Header -> Group 3` source icon (`[1594]`, `[1591]`, `[1800]`, `[1804]`).
pub fn draw_header_source_icon<R: Renderer>(
    canvas: &mut Canvas<R>,
    x: f32,
    y: f32,
    kind: HeaderSourceIcon,
    fg: Color,
    bg: Color,
) {
    match kind {
        HeaderSourceIcon::None => {}
        HeaderSourceIcon::MicroSd => {
            // [1594]/[1596] icon_microsd_64: 32.533x40.667 at (15.733, 11.667)
            let ox = x + 15.733_334;
            let oy = y + 11.666_667;
            let mut body = Path::new();
            body.move_to(ox + 8.133_333, oy + 0.0);
            body.bezier_to(
                ox + 8.133_333 - 2.245_958,
                oy + 0.0,
                ox + 4.066_666_6,
                oy + 4.066_666_6 - 2.245_958,
                ox + 4.066_666_6,
                oy + 4.066_666_6,
            );
            body.line_to(ox + 4.066_666_6, oy + 18.3);
            body.line_to(ox + 0.0, oy + 22.366_667);
            body.line_to(ox + 0.0, oy + 36.6);
            body.bezier_to(
                ox + 0.0,
                oy + 36.6 + 2.245_96,
                ox + 4.066_666_6 - 2.245_958,
                oy + 40.666_668,
                ox + 4.066_666_6,
                oy + 40.666_668,
            );
            body.line_to(ox + 28.466_667, oy + 40.666_668);
            body.bezier_to(
                ox + 28.466_667 + 2.245_96,
                oy + 40.666_668,
                ox + 32.533_333,
                oy + 36.6 + 2.245_96,
                ox + 32.533_333,
                oy + 36.6,
            );
            body.line_to(ox + 32.533_333, oy + 4.066_666_6);
            body.bezier_to(
                ox + 32.533_333,
                oy + 4.066_666_6 - 2.245_958,
                ox + 28.466_667 + 2.245_96,
                oy + 0.0,
                ox + 28.466_667,
                oy + 0.0,
            );
            body.close();
            canvas.fill_path(&body, &Paint::color(fg));

            let mut pins = Path::new();
            pins.rect(ox + 10.166_667, oy + 4.066_666_6, 4.066_666_6, 8.133_333);
            pins.rect(ox + 16.266_666, oy + 4.066_666_6, 4.066_666_6, 8.133_333);
            pins.rect(ox + 22.366_667, oy + 4.066_666_6, 4.066_666_6, 8.133_333);
            canvas.fill_path(&pins, &Paint::color(bg));
        }
        HeaderSourceIcon::Navidrome => {
            // [1591]/[1593] icon_navidrome_64: 40x40 vinyl disc at (12.446, 13.391 -> center y=32)
            let cx = x + 32.0;
            let cy = y + 32.0;
            let mut disc = Path::new();
            disc.circle(cx, cy, 20.0);
            canvas.fill_path(&disc, &Paint::color(fg));

            // Center label ring + spindle cutout matching [1593] loops
            let mut cut_ring = Path::new();
            cut_ring.circle(cx, cy, 7.64);
            canvas.fill_path(&cut_ring, &Paint::color(bg));

            let mut inner_ring = Path::new();
            inner_ring.circle(cx, cy, 6.07);
            canvas.fill_path(&inner_ring, &Paint::color(fg));

            let mut label_hole = Path::new();
            label_hole.circle(cx, cy, 2.2);
            canvas.fill_path(&label_hole, &Paint::color(bg));

            let mut spindle = Path::new();
            spindle.circle(cx, cy, 0.75);
            canvas.fill_path(&spindle, &Paint::color(fg));

            // Curved groove highlights (top-left & bottom-right arcs in bg)
            let mut grooves = Paint::color(bg);
            grooves.set_line_width(1.6);
            grooves.set_line_cap(LineCap::Round);
            for &r in &[11.5_f32, 15.2_f32] {
                let mut g1 = Path::new();
                g1.arc(
                    cx,
                    cy,
                    r,
                    std::f32::consts::PI,
                    std::f32::consts::PI * 1.5,
                    femtovg::Solidity::Solid,
                );
                canvas.stroke_path(&g1, &grooves);

                let mut g2 = Path::new();
                g2.arc(
                    cx,
                    cy,
                    r,
                    0.0,
                    std::f32::consts::PI * 0.5,
                    femtovg::Solidity::Solid,
                );
                canvas.stroke_path(&g2, &grooves);
            }
        }
        HeaderSourceIcon::Jellyfin => {
            // [1800]/[1799] icon_jellyfin_64: exact 3 concentric rounded Bezier triangles at (12.0, 11.667)
            let ox = x + 11.999_262;
            let oy = y + 11.666_667;

            let mut outer = Path::new();
            outer.move_to(ox + 20.000_738, oy + 0.003_333_4);
            outer.bezier_to(
                ox + 20.000_738 - 5.29,
                oy + 0.003_333_4,
                ox + 0.267_406_67 - 2.596_666_3,
                oy + 36.110_535 - 5.215_077,
                ox + 0.267_406_67,
                oy + 36.110_535,
            );
            outer.bezier_to(
                ox + 0.267_406_67 + 2.6,
                oy + 36.110_535 + 5.215_077,
                ox + 39.737_4 - 2.57,
                oy + 36.110_535 + 5.156_743,
                ox + 39.737_4,
                oy + 36.110_535,
            );
            outer.bezier_to(
                ox + 39.737_4 + 2.570_002_6,
                oy + 36.110_535 - 5.156_743,
                ox + 20.000_738 + 5.295,
                oy + 0.0,
                ox + 20.000_738,
                oy + 0.0,
            );
            outer.close();
            canvas.fill_path(&outer, &Paint::color(fg));

            let mut mid = Path::new();
            mid.move_to(ox + 32.934_07, oy + 31.585_47);
            mid.bezier_to(
                ox + 32.934_07 - 1.679_998_9,
                oy + 31.585_47 + 3.380_05,
                ox + 7.077_405_5 + 1.701_667_1,
                oy + 31.585_47 + 3.416_716,
                ox + 7.077_405_5,
                oy + 31.585_47,
            );
            mid.bezier_to(
                ox + 7.077_405_5 - 1.703_332_9,
                oy + 31.585_47 - 3.418_385,
                ox + 20.005_737 - 3.471_666_6,
                oy + 7.925_118,
                ox + 20.005_737,
                oy + 7.925_118,
            );
            mid.bezier_to(
                ox + 20.005_737 + 3.468_331_8,
                oy + 7.925_118,
                ox + 32.937_4 + 1.685_002_4,
                oy + 31.585_47 - 3.383_384_7,
                ox + 32.937_4,
                oy + 31.585_47,
            );
            mid.close();
            canvas.fill_path(&mid, &Paint::color(bg));

            let mut inner = Path::new();
            inner.move_to(ox + 20.000_738, oy + 15.330_228);
            inner.bezier_to(
                ox + 20.000_738 - 1.756_666_3,
                oy + 15.330_228,
                ox + 13.450_738 - 0.859_999_54,
                oy + 27.312_07 - 1.731_691_6,
                ox + 13.450_738,
                oy + 27.312_07,
            );
            inner.bezier_to(
                ox + 13.450_738 + 0.863_334_24,
                oy + 27.312_07 + 1.733_360_6,
                ox + 26.550_737 - 0.853_333_35,
                oy + 27.312_07 + 1.711_692_8,
                ox + 26.550_737,
                oy + 27.312_07,
            );
            inner.bezier_to(
                ox + 26.550_737 + 0.851_666,
                oy + 27.312_07 - 1.711_691_2,
                ox + 20.000_738 + 1.759_999_4,
                oy + 15.328_56,
                ox + 20.000_738,
                oy + 15.328_56,
            );
            inner.close();
            canvas.fill_path(&inner, &Paint::color(fg));
        }
    }
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
    shell_color: Color,
    bar_color: Color,
    bg: Color,
) {
    // 1. Shell [1562]/[1567] in fg2 (`shell_color`): outer body at (x, y + 12.8) + inner cavity cutout in bg
    let shell = battery_shell_outer_path(x, y + 12.8);
    canvas.fill_path(&shell, &Paint::color(shell_color));
    let cav_x = x + 6.4;
    let cav_y = y + 19.2;
    let cav_w = 48.0;
    let cav_h = 25.6;
    fill_rounded_rect(canvas, cav_x, cav_y, cav_w, cav_h, 3.2, bg);

    // 2. Inner fill bar [1561]/[1566] in fg1 (`bar_color`): (x + 9.6, y + 22.4, 41.6 * pct, 19.2)
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
        canvas.fill_path(&bar, &Paint::color(bar_color));
    }

    // 3. Variant2 [1565]/[1570]: 12x16 charging bolt [1569] at (x + 25.0, y + 24.4)
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
            draw_charge_bolt_scaled(canvas, x + 25.0, y + 24.4, 2.0 / 3.0, shell_color);
            canvas.restore();
        }
    }
}

/// Draws vertical `Scrollbar` (`16xh` component `[312]`): `r=8`, `stroke=4 inside` (`fg1`), `4px` inset `8px` thumb `r=4` (`fg`).
pub fn draw_scrollbar_v<R: Renderer>(
    canvas: &mut Canvas<R>,
    x: f32,
    y: f32,
    w: f32,
    h: f32,
    total_items: usize,
    start_item: usize,
    visible_items: usize,
    border_color: Color,
    thumb_color: Color,
) {
    stroke_rounded_rect_inside(canvas, x, y, w, h, 8.0, 4.0, border_color);
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
    fill_rounded_rect(canvas, x + inset, thumb_y, inner_w, thumb_h, 4.0, thumb_color);
}

/// Draws horizontal `Seekbar` track (`wx24` component `[641]`): `r=16` (pill), `stroke=4 inside` (`fg1`), `4px` inset `16px` fill `r=8` (`fg2`).
pub fn draw_seekbar_track_h<R: Renderer>(
    canvas: &mut Canvas<R>,
    x: f32,
    y: f32,
    w: f32,
    h: f32,
    progress: f32,
    border_color: Color,
    fill_color: Color,
) {
    stroke_rounded_rect_inside(canvas, x, y, w, h, 16.0, 4.0, border_color);
    let inset = 4.0;
    let inner_w_max = (w - inset * 2.0).max(0.0);
    let inner_h = (h - inset * 2.0).max(4.0); // 16.0 when h == 24.0
    let p = progress.clamp(0.0, 1.0);
    let fill_w = inner_w_max * p;
    if fill_w >= 1.0 {
        fill_rounded_rect(canvas, x + inset, y + inset, fill_w, inner_h, 8.0, fill_color);
    }
}

/// Draws horizontal `Seekbar` track with an optional 4-shade chiptune `loop_point_ms` marker tick.
#[allow(clippy::too_many_arguments)]
pub fn draw_seekbar_track_h_with_loop<R: Renderer>(
    canvas: &mut Canvas<R>,
    x: f32,
    y: f32,
    w: f32,
    h: f32,
    progress: f32,
    loop_progress: Option<f32>,
    border_color: Color,
    fill_color: Color,
    tick_color: Color,
) {
    draw_seekbar_track_h(canvas, x, y, w, h, progress, border_color, fill_color);
    if let Some(lp) = loop_progress {
        let lp_clamped = lp.clamp(0.02, 0.98);
        let inset = 4.0;
        let inner_w_max = (w - inset * 2.0).max(0.0);
        let tick_cx = (x + inset + inner_w_max * lp_clamped).round();
        let tw = crate::layout::nsf_now_playing_spec::LOOP_TICK_W;
        let over = crate::layout::nsf_now_playing_spec::LOOP_TICK_OVERHANG_Y;
        let cap_w = crate::layout::nsf_now_playing_spec::LOOP_TICK_CAP_W;
        let cap_h = crate::layout::nsf_now_playing_spec::LOOP_TICK_CAP_H;

        let mut p = Path::new();
        p.rect(tick_cx - tw * 0.5, y - over, tw, h + over * 2.0);
        p.rect(tick_cx - cap_w * 0.5, y - over, cap_w, cap_h);
        p.rect(
            tick_cx - cap_w * 0.5,
            y + h + over - cap_h,
            cap_w,
            cap_h,
        );
        canvas.fill_path(&p, &Paint::color(tick_color));
    }
}

/// Draws a crisp 4-shade `[∞]` Endless Loop indicator badge at `(x, y, w, h)`.
pub fn draw_infinity_badge<R: Renderer>(
    canvas: &mut Canvas<R>,
    x: f32,
    y: f32,
    w: f32,
    h: f32,
    border_color: Color,
    ink_color: Color,
) {
    use crate::layout::nsf_now_playing_spec as spec;
    stroke_rounded_rect_inside(
        canvas,
        x,
        y,
        w,
        h,
        spec::ENDLESS_BADGE_RADIUS,
        spec::ENDLESS_BADGE_STROKE,
        border_color,
    );

    let cx = x + w * 0.5;
    let cy = y + h * 0.5;
    let dx = spec::INFINITY_LOBE_DX;
    let r = spec::INFINITY_LOBE_R;
    let mut inf = Path::new();
    // Smooth figure-eight lemniscate built from two tangent cubic loops meeting at `(cx, cy)`
    inf.move_to(cx, cy);
    inf.bezier_to(cx + dx * 0.65, cy - r * 1.45, cx + dx + r, cy - r * 1.35, cx + dx + r, cy);
    inf.bezier_to(cx + dx + r, cy + r * 1.35, cx + dx * 0.65, cy + r * 1.45, cx, cy);
    inf.bezier_to(cx - dx * 0.65, cy - r * 1.45, cx - dx - r, cy - r * 1.35, cx - dx - r, cy);
    inf.bezier_to(cx - dx - r, cy + r * 1.35, cx - dx * 0.65, cy + r * 1.45, cx, cy);
    let mut p = Paint::color(ink_color);
    p.set_line_width(spec::INFINITY_STROKE_W);
    p.set_line_cap(LineCap::Round);
    canvas.stroke_path(&inf, &p);
}

/// Draws `icon_note_16` (`[404]`/`[398]`) uniformly scaled to `(size x size)` at `(x, y)`.
pub fn draw_icon_note_scaled<R: Renderer>(
    canvas: &mut Canvas<R>,
    x: f32,
    y: f32,
    size: f32,
    color: Color,
) {
    let s = (size / 16.0).max(0.25);
    let gx = x + 1.620_754_7 * s;
    let gy = y + 1.0 * s;
    let paint = Paint::color(color);

    let mut path = Path::new();
    path.ellipse(
        gx + 2.483_018_9 * s,
        gy + (8.981_132 + 2.509_434) * s,
        2.483_018_9 * s,
        2.509_434 * s,
    );
    path.rect(
        gx + 3.724_528_3 * s,
        gy + 2.192_452_9 * s,
        1.267_924_5 * s,
        9.298_113 * s,
    );
    path.ellipse(
        gx + (7.766_038 + 2.483_018_9) * s,
        gy + (7.343_396 + 2.509_434) * s,
        2.483_018_9 * s,
        2.509_434 * s,
    );
    path.move_to(gx + 11.490_566 * s, gy + (0.660_377_4 + 9.298_113) * s);
    path.line_to(gx + 11.490_566 * s, gy + (0.660_377_4 + 0.475_471_7) * s);
    path.line_to(gx + (11.490_566 + 1.267_924_5) * s, gy + 0.660_377_4 * s);
    path.line_to(
        gx + (11.490_566 + 1.267_924_5) * s,
        gy + (0.660_377_4 + 9.298_113) * s,
    );
    path.close();
    let bx = gx + 3.724_528_3 * s;
    let by = gy;
    path.move_to(bx, by + 2.192_452_9 * s);
    path.line_to(bx + 9.007_547 * s, by);
    path.line_to(bx + 9.007_547 * s, by + 3.328_302 * s);
    path.line_to(bx, by + 5.415_094_4 * s);
    path.close();

    canvas.fill_path(&path, &paint);
}

/// Draws `icon_note_16` (`16x16` component `[404]`/`[398]`) at `(x, y)`.
pub fn draw_icon_note_16<R: Renderer>(canvas: &mut Canvas<R>, x: f32, y: f32, color: Color) {
    draw_icon_note_scaled(canvas, x, y, 16.0, color);
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
