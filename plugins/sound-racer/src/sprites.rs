//! Pre-rasterized multi-scale `evenodd` car sprite bank for `Sound Racer`.
//!
//! Embeds `assets/icons/racer.svg` (center lane) and `assets/icons/racerl.svg` (left lane,
//! horizontally mirrored for right lane) at compile time, rasterizes supersampled alpha masks
//! once on first use, and caches theme-tinted `RGBA8` mipmaps for zero-allocation 60 FPS rendering.

use std::sync::OnceLock;

const RACER_CENTER_SVG: &str = include_str!("../../../assets/icons/racer.svg");
const RACER_LEFT_SVG: &str = include_str!("../../../assets/icons/racerl.svg");

/// Negative corner radius sentinel instructing `UiEngine::plugin_draw_rgba_image`
/// to sample the pre-filtered mipmap with `GL_LINEAR` rather than `GL_NEAREST`.
pub const LINEAR_SPRITE_RADIUS: f32 = -1.0;

/// Number of discrete perspective mip widths from horizon (`14px`) to past-camera (`140px`).
pub const MIP_COUNT: usize = 12;

/// Mip index matching `CAR_BASE_W` (`126px`) at `z = 0.0` (`scale = 1.0`).
#[cfg(test)]
pub const PLAYER_MIP_IDX: usize = 10;

/// Discrete pixel widths pre-rasterized once at init.
pub const MIP_WIDTHS: [u32; MIP_COUNT] = [14, 20, 28, 36, 46, 56, 68, 80, 94, 108, 126, 140];

/// Canonical foreground lane car width at `z = 0.0` (`scale = 1.0`) for side lanes (`racerl.svg`).
pub const CAR_BASE_W: f32 = 126.0;

/// Scale factor applied to the center-lane rear-view sprite (`racer.svg`) relative to
/// `racerl.svg` on the left/right lanes so visual chassis/roof mass matches across lanes.
pub const CENTER_CAR_SCALE: f32 = 0.85;

/// Normalized Y coordinate (`0.0..=1.0`) where the rear tires contact the road in both SVGs
/// (`718.6 / 740 = 0.9711` in `racer.svg`, `470.0 / 484 = 0.9711` in `racerl.svg`).
pub const TIRE_CONTACT_Y_RATIO: f32 = 0.971;

/// Returns the per-lane sprite scale (`1.0` on left/right `racerl.svg`, `0.85` on center `racer.svg`).
#[inline]
pub const fn lane_car_scale(lane: usize) -> f32 {
    if lane == 1 {
        CENTER_CAR_SCALE
    } else {
        1.0
    }
}

/// Computes `(dst_x, dst_y, dst_w, dst_h)` for a player or traffic car in `lane` (`0 = Left`,
/// `1 = Center`, `2 = Right`), anchoring the rear tires onto the exact same ground line at `ground_y`.
#[inline]
#[allow(clippy::too_many_arguments)]
pub fn car_draw_rect(
    sprites: &CarSpriteCache,
    lane: usize,
    cx: f32,
    ground_y: f32,
    persp_scale: f32,
    tire_offset_y: f32,
    jump_px: f32,
    min_w: f32,
) -> (f32, f32, f32, f32) {
    let l_scale = lane_car_scale(lane);
    let dst_w = (CAR_BASE_W * persp_scale * l_scale).max(min_w * l_scale);
    let dst_h = dst_w * sprites.aspect_h_over_w(lane);
    let tire_y = ground_y + tire_offset_y * persp_scale - jump_px;
    let dst_x = cx - dst_w * 0.5;
    let dst_y = tire_y - dst_h * TIRE_CONTACT_Y_RATIO;
    (dst_x, dst_y, dst_w, dst_h)
}

const BEZIER_STEPS: usize = 16;
const SUPERSAMPLE_Y: u32 = 8;

#[derive(Clone, Copy)]
struct DirectedEdge {
    y_min: f32,
    y_max: f32,
    x_at_ymin: f32,
    dx_dy: f32,
}

struct AlphaMip {
    width: u32,
    height: u32,
    alpha: Vec<u8>,
}

struct LaneMaskSet {
    aspect_h_over_w: f32,
    mips: [AlphaMip; MIP_COUNT],
}

struct CarMaskBank {
    lanes: [LaneMaskSet; 3], // 0 = Left (`racerl`), 1 = Center (`racer`), 2 = Right (flipped `racerl`)
}

static MASK_BANK: OnceLock<CarMaskBank> = OnceLock::new();

fn mask_bank() -> &'static CarMaskBank {
    MASK_BANK.get_or_init(build_mask_bank)
}

pub struct TintedMip {
    pub tex_key: u64,
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

/// Per-session cache of theme-tinted `RGBA8` sprites (`Foreground` player + `Secondary 2` traffic).
pub struct CarSpriteCache {
    fg_rgb: u32,
    fg2_rgb: u32,
    initialized: bool,
    player_lanes: [TintedMip; 3],
    obstacle_mips: [[TintedMip; MIP_COUNT]; 3],
}

impl Default for CarSpriteCache {
    fn default() -> Self {
        Self::new()
    }
}

impl CarSpriteCache {
    pub fn new() -> Self {
        let empty_mip = || TintedMip {
            tex_key: 0,
            width: 0,
            height: 0,
            rgba: Vec::new(),
        };
        Self {
            fg_rgb: 0,
            fg2_rgb: 0,
            initialized: false,
            player_lanes: std::array::from_fn(|_| empty_mip()),
            obstacle_mips: std::array::from_fn(|_| std::array::from_fn(|_| empty_mip())),
        }
    }

    /// Re-tints cached sprites only when the active theme's `fg` or `fg2` color changes (`O(1)` no-op otherwise).
    pub fn ensure_tinted(&mut self, fg_rgb: u32, fg2_rgb: u32) {
        if self.initialized && self.fg_rgb == fg_rgb && self.fg2_rgb == fg2_rgb {
            return;
        }
        let bank = mask_bank();
        self.fg_rgb = fg_rgb;
        self.fg2_rgb = fg2_rgb;
        self.initialized = true;

        for lane in 0..3 {
            let p_mip_idx = select_mip_idx(CAR_BASE_W * lane_car_scale(lane));
            let pm = &bank.lanes[lane].mips[p_mip_idx];
            self.player_lanes[lane] = tint_alpha_mip(pm, fg_rgb, lane as u8, p_mip_idx as u8, 1);
            for mip_idx in 0..MIP_COUNT {
                let om = &bank.lanes[lane].mips[mip_idx];
                self.obstacle_mips[lane][mip_idx] =
                    tint_alpha_mip(om, fg2_rgb, lane as u8, mip_idx as u8, 2);
            }
        }
    }

    #[inline]
    pub fn aspect_h_over_w(&self, lane: usize) -> f32 {
        mask_bank().lanes[lane.min(2)].aspect_h_over_w
    }

    #[inline]
    pub fn player_sprite(&self, lane: usize) -> &TintedMip {
        &self.player_lanes[lane.min(2)]
    }

    #[inline]
    pub fn obstacle_sprite(&self, lane: usize, dst_w: f32) -> &TintedMip {
        let idx = select_mip_idx(dst_w);
        &self.obstacle_mips[lane.min(2)][idx]
    }
}

#[inline]
pub fn select_mip_idx(dst_w: f32) -> usize {
    let target = dst_w.ceil().max(1.0) as u32;
    for (i, &mw) in MIP_WIDTHS.iter().enumerate() {
        if mw >= target {
            return i;
        }
    }
    MIP_COUNT - 1
}

fn tint_alpha_mip(src: &AlphaMip, rgb: u32, lane: u8, mip_idx: u8, role: u8) -> TintedMip {
    let r = ((rgb >> 16) & 0xFF) as u8;
    let g = ((rgb >> 8) & 0xFF) as u8;
    let b = (rgb & 0xFF) as u8;

    // Store constant `(r, g, b)` across all texels (including `alpha == 0`) so `GL_LINEAR`
    // interpolation never bleeds dark RGB halos into anti-aliased silhouette or window edges.
    let mut rgba = Vec::with_capacity(src.alpha.len() * 4);
    for &a in &src.alpha {
        rgba.push(r);
        rgba.push(g);
        rgba.push(b);
        rgba.push(a);
    }

    // Collision-free 64-bit GPU texture cache key:
    // bits 63..48 = 0x5352 ("SR"), bits 47..40 = role (1=player, 2=obstacle),
    // bits 39..32 = lane (0=left, 1=center, 2=right), bits 31..24 = mip_idx, bits 23..0 = rgb.
    let tex_key = 0x5352_0000_0000_0000_u64
        | ((role as u64) << 40)
        | ((lane as u64) << 32)
        | ((mip_idx as u64) << 24)
        | ((rgb as u64) & 0x00FF_FFFF);

    TintedMip {
        tex_key,
        width: src.width,
        height: src.height,
        rgba,
    }
}

fn build_mask_bank() -> CarMaskBank {
    let (c_vb, c_edges) = parse_svg_to_normalized_edges(RACER_CENTER_SVG);
    let (l_vb, l_edges) = parse_svg_to_normalized_edges(RACER_LEFT_SVG);

    let c_aspect = c_vb.3 / c_vb.2;
    let l_aspect = l_vb.3 / l_vb.2;

    let center_mips: [AlphaMip; MIP_COUNT] = std::array::from_fn(|i| {
        let w = MIP_WIDTHS[i];
        let h = ((w as f32) * c_aspect).round().max(1.0) as u32;
        rasterize_evenodd_mip(&c_edges, w, h)
    });

    let left_mips: [AlphaMip; MIP_COUNT] = std::array::from_fn(|i| {
        let w = MIP_WIDTHS[i];
        let h = ((w as f32) * l_aspect).round().max(1.0) as u32;
        rasterize_evenodd_mip(&l_edges, w, h)
    });

    let right_mips: [AlphaMip; MIP_COUNT] = std::array::from_fn(|i| flip_mip_horizontal(&left_mips[i]));

    CarMaskBank {
        lanes: [
            LaneMaskSet {
                aspect_h_over_w: l_aspect,
                mips: left_mips,
            },
            LaneMaskSet {
                aspect_h_over_w: c_aspect,
                mips: center_mips,
            },
            LaneMaskSet {
                aspect_h_over_w: l_aspect,
                mips: right_mips,
            },
        ],
    }
}

fn flip_mip_horizontal(src: &AlphaMip) -> AlphaMip {
    let w = src.width as usize;
    let h = src.height as usize;
    let mut out = vec![0u8; w * h];
    for y in 0..h {
        let row = y * w;
        for x in 0..w {
            out[row + x] = src.alpha[row + (w - 1 - x)];
        }
    }
    AlphaMip {
        width: src.width,
        height: src.height,
        alpha: out,
    }
}

fn rasterize_evenodd_mip(edges: &[DirectedEdge], width: u32, height: u32) -> AlphaMip {
    let w = width as usize;
    let h = height as usize;
    let wf = width as f32;
    let total_sub_y = (height * SUPERSAMPLE_Y) as f32;
    let inv_sub_y = 255.0 / (SUPERSAMPLE_Y as f32);

    let mut alpha = vec![0u8; w * h];
    let mut row_cov = vec![0.0f32; w];
    let mut xs = Vec::with_capacity(32);

    for py in 0..h {
        row_cov.fill(0.0);
        for sy in 0..SUPERSAMPLE_Y {
            let y_norm = ((py as u32 * SUPERSAMPLE_Y + sy) as f32 + 0.5) / total_sub_y;
            xs.clear();
            for e in edges {
                if y_norm >= e.y_min && y_norm < e.y_max {
                    let x_norm = e.x_at_ymin + (y_norm - e.y_min) * e.dx_dy;
                    xs.push(x_norm * wf);
                }
            }
            xs.sort_unstable_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));

            let pairs = xs.len() / 2;
            for p in 0..pairs {
                let xl = xs[p * 2].clamp(0.0, wf);
                let xr = xs[p * 2 + 1].clamp(0.0, wf);
                if xr <= xl {
                    continue;
                }
                let x0 = (xl.floor() as usize).min(w);
                let x1 = (xr.ceil() as usize).min(w);
                for (px, slot) in row_cov.iter_mut().enumerate().take(x1).skip(x0) {
                    let pxf = px as f32;
                    let cov = (xr.min(pxf + 1.0) - xl.max(pxf)).max(0.0);
                    *slot += cov;
                }
            }
        }

        let dst_row = py * w;
        for px in 0..w {
            alpha[dst_row + px] = (row_cov[px] * inv_sub_y).round().clamp(0.0, 255.0) as u8;
        }
    }

    AlphaMip {
        width,
        height,
        alpha,
    }
}

/* ---- Minimal Zero-Dependency SVG `viewBox` + `<path d>` Flattener ------- */

#[derive(Clone, Copy)]
enum PathTok {
    Cmd(char),
    Num(f32),
}

fn parse_svg_to_normalized_edges(svg: &str) -> ((f32, f32, f32, f32), Vec<DirectedEdge>) {
    let vb = extract_attr(svg, "viewBox")
        .and_then(parse_four_floats)
        .unwrap_or((0.0, 0.0, 1000.0, 740.0));
    let (min_x, min_y, vb_w, vb_h) = (vb.0, vb.1, vb.2.max(1.0), vb.3.max(1.0));

    let mut raw_segs = Vec::with_capacity(1024);
    let mut rest = svg;
    while let Some(p_idx) = rest.find("<path") {
        let after = &rest[p_idx + 5..];
        let end_idx = after.find('>').unwrap_or(after.len());
        let tag_body = &after[..end_idx];
        if let Some(d_str) = extract_attr(tag_body, "d") {
            flatten_svg_path(d_str, &mut raw_segs);
        }
        rest = &after[end_idx..];
    }

    let inv_w = 1.0 / vb_w;
    let inv_h = 1.0 / vb_h;
    let mut edges = Vec::with_capacity(raw_segs.len());
    for (x0, y0, x1, y1) in raw_segs {
        let nx0 = (x0 - min_x) * inv_w;
        let ny0 = (y0 - min_y) * inv_h;
        let nx1 = (x1 - min_x) * inv_w;
        let ny1 = (y1 - min_y) * inv_h;
        let dy = ny1 - ny0;
        if dy.abs() < 1e-7 {
            continue;
        }
        if ny0 < ny1 {
            edges.push(DirectedEdge {
                y_min: ny0,
                y_max: ny1,
                x_at_ymin: nx0,
                dx_dy: (nx1 - nx0) / dy,
            });
        } else {
            edges.push(DirectedEdge {
                y_min: ny1,
                y_max: ny0,
                x_at_ymin: nx1,
                dx_dy: (nx0 - nx1) / (-dy),
            });
        }
    }
    edges.sort_by(|a, b| a.y_min.partial_cmp(&b.y_min).unwrap_or(std::cmp::Ordering::Equal));
    ((min_x, min_y, vb_w, vb_h), edges)
}

fn extract_attr<'a>(src: &'a str, attr: &str) -> Option<&'a str> {
    let mut search = src;
    while let Some(pos) = search.find(attr) {
        let prev_ok = pos == 0
            || search.as_bytes()[pos - 1].is_ascii_whitespace()
            || search.as_bytes()[pos - 1] == b'<';
        let after = &search[pos + attr.len()..];
        let trimmed = after.trim_start();
        if prev_ok && trimmed.starts_with('=') {
            let val_part = trimmed[1..].trim_start();
            let quote = val_part.as_bytes().first().copied()?;
            if quote == b'"' || quote == b'\'' {
                let inner = &val_part[1..];
                let close = inner.find(quote as char)?;
                return Some(&inner[..close]);
            }
        }
        search = &search[pos + attr.len()..];
    }
    None
}

fn parse_four_floats(s: &str) -> Option<(f32, f32, f32, f32)> {
    let mut it = s
        .split(|c: char| c.is_ascii_whitespace() || c == ',')
        .filter(|p| !p.is_empty())
        .filter_map(|p| p.parse::<f32>().ok());
    Some((it.next()?, it.next()?, it.next()?, it.next()?))
}

fn tokenize_path(d: &str) -> Vec<PathTok> {
    let bytes = d.as_bytes();
    let n = bytes.len();
    let mut i = 0usize;
    let mut out = Vec::with_capacity(512);

    while i < n {
        let b = bytes[i];
        if b.is_ascii_whitespace() || b == b',' {
            i += 1;
            continue;
        }
        if b.is_ascii_alphabetic() {
            out.push(PathTok::Cmd(b as char));
            i += 1;
            continue;
        }
        let start = i;
        if b == b'+' || b == b'-' {
            i += 1;
        }
        let mut seen_dot = false;
        while i < n {
            let c = bytes[i];
            if c.is_ascii_digit() {
                i += 1;
            } else if c == b'.' && !seen_dot {
                seen_dot = true;
                i += 1;
            } else if (c == b'e' || c == b'E') && i > start {
                i += 1;
                if i < n && (bytes[i] == b'+' || bytes[i] == b'-') {
                    i += 1;
                }
            } else {
                break;
            }
        }
        if i > start {
            if let Ok(val) = d[start..i].parse::<f32>() {
                out.push(PathTok::Num(val));
            }
        } else {
            i += 1;
        }
    }
    out
}

fn flatten_svg_path(d: &str, segs: &mut Vec<(f32, f32, f32, f32)>) {
    let toks = tokenize_path(d);
    let mut idx = 0usize;
    let mut cmd = 'M';
    let mut cx = 0.0f32;
    let mut cy = 0.0f32;
    let mut sx = 0.0f32;
    let mut sy = 0.0f32;
    let mut subpath_open = false;

    let next_num = |idx: &mut usize| -> Option<f32> {
        match toks.get(*idx).copied() {
            Some(PathTok::Num(v)) => {
                *idx += 1;
                Some(v)
            }
            _ => None,
        }
    };

    while idx < toks.len() {
        if let PathTok::Cmd(c) = toks[idx] {
            cmd = c;
            idx += 1;
            if matches!(cmd, 'Z' | 'z') {
                if subpath_open && ((cx - sx).abs() > 1e-5 || (cy - sy).abs() > 1e-5) {
                    segs.push((cx, cy, sx, sy));
                }
                cx = sx;
                cy = sy;
                subpath_open = false;
                continue;
            }
        }

        match cmd {
            'M' | 'm' => {
                if subpath_open && ((cx - sx).abs() > 1e-5 || (cy - sy).abs() > 1e-5) {
                    segs.push((cx, cy, sx, sy));
                }
                let Some(mut x) = next_num(&mut idx) else { break };
                let Some(mut y) = next_num(&mut idx) else { break };
                if cmd == 'm' {
                    x += cx;
                    y += cy;
                }
                cx = x;
                cy = y;
                sx = x;
                sy = y;
                subpath_open = true;
                cmd = if cmd == 'm' { 'l' } else { 'L' };
            }
            'L' | 'l' => {
                let Some(mut x) = next_num(&mut idx) else { break };
                let Some(mut y) = next_num(&mut idx) else { break };
                if cmd == 'l' {
                    x += cx;
                    y += cy;
                }
                segs.push((cx, cy, x, y));
                cx = x;
                cy = y;
            }
            'H' | 'h' => {
                let Some(mut x) = next_num(&mut idx) else { break };
                if cmd == 'h' {
                    x += cx;
                }
                segs.push((cx, cy, x, cy));
                cx = x;
            }
            'V' | 'v' => {
                let Some(mut y) = next_num(&mut idx) else { break };
                if cmd == 'v' {
                    y += cy;
                }
                segs.push((cx, cy, cx, y));
                cy = y;
            }
            'C' | 'c' => {
                let Some(mut x1) = next_num(&mut idx) else { break };
                let Some(mut y1) = next_num(&mut idx) else { break };
                let Some(mut x2) = next_num(&mut idx) else { break };
                let Some(mut y2) = next_num(&mut idx) else { break };
                let Some(mut x3) = next_num(&mut idx) else { break };
                let Some(mut y3) = next_num(&mut idx) else { break };
                if cmd == 'c' {
                    x1 += cx;
                    y1 += cy;
                    x2 += cx;
                    y2 += cy;
                    x3 += cx;
                    y3 += cy;
                }
                let mut px = cx;
                let mut py = cy;
                for step in 1..=BEZIER_STEPS {
                    let t = (step as f32) / (BEZIER_STEPS as f32);
                    let mt = 1.0 - t;
                    let nx = mt * mt * mt * cx
                        + 3.0 * mt * mt * t * x1
                        + 3.0 * mt * t * t * x2
                        + t * t * t * x3;
                    let ny = mt * mt * mt * cy
                        + 3.0 * mt * mt * t * y1
                        + 3.0 * mt * t * t * y2
                        + t * t * t * y3;
                    segs.push((px, py, nx, ny));
                    px = nx;
                    py = ny;
                }
                cx = x3;
                cy = y3;
            }
            'Q' | 'q' => {
                let Some(mut x1) = next_num(&mut idx) else { break };
                let Some(mut y1) = next_num(&mut idx) else { break };
                let Some(mut x2) = next_num(&mut idx) else { break };
                let Some(mut y2) = next_num(&mut idx) else { break };
                if cmd == 'q' {
                    x1 += cx;
                    y1 += cy;
                    x2 += cx;
                    y2 += cy;
                }
                let mut px = cx;
                let mut py = cy;
                for step in 1..=12 {
                    let t = (step as f32) / 12.0;
                    let mt = 1.0 - t;
                    let nx = mt * mt * cx + 2.0 * mt * t * x1 + t * t * x2;
                    let ny = mt * mt * cy + 2.0 * mt * t * y1 + t * t * y2;
                    segs.push((px, py, nx, ny));
                    px = nx;
                    py = ny;
                }
                cx = x2;
                cy = y2;
            }
            _ => {
                idx += 1;
            }
        }
    }

    if subpath_open && ((cx - sx).abs() > 1e-5 || (cy - sy).abs() > 1e-5) {
        segs.push((cx, cy, sx, sy));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn svg_masks_preserve_transparent_cutouts_and_mirror_symmetry() {
        let bank = mask_bank();

        // 1. Center lane (`racer.svg`, 126 x 93)
        let center = &bank.lanes[1].mips[PLAYER_MIP_IDX];
        assert_eq!(center.width, 126);
        assert_eq!(center.height, 93);
        let sample_c = |u: f32, v: f32| -> u8 {
            let x = ((u * center.width as f32) as usize).min(center.width as usize - 1);
            let y = ((v * center.height as f32) as usize).min(center.height as usize - 1);
            center.alpha[y * center.width as usize + x]
        };
        // Solid car trunk/body between rear window and license plate
        assert_eq!(sample_c(0.50, 0.32), 255);
        // Rear windshield cutout -> transparent (`alpha == 0`)
        assert_eq!(sample_c(0.50, 0.15), 0);
        // License plate cutout -> transparent (`alpha == 0`)
        assert_eq!(sample_c(0.50, 0.46), 0);
        // Underbody road gap between rear tires -> transparent (`alpha == 0`)
        assert_eq!(sample_c(0.50, 0.90), 0);
        // Left and right rear tires -> opaque (`alpha == 255`)
        assert_eq!(sample_c(0.10, 0.90), 255);
        assert_eq!(sample_c(0.90, 0.90), 255);

        // 2. Left lane (`racerl.svg`) & Right lane (horizontally mirrored `racerl.svg`)
        let left = &bank.lanes[0].mips[PLAYER_MIP_IDX];
        let right = &bank.lanes[2].mips[PLAYER_MIP_IDX];
        assert_eq!(left.width, 126);
        assert_eq!(left.width, right.width);
        assert_eq!(left.height, right.height);

        let w = left.width as usize;
        let h = left.height as usize;
        // Solid body center
        assert_eq!(left.alpha[(h * 35 / 100) * w + (w * 45 / 100)], 255);
        // Rear window cutout (`(315, 87)` in `656x484`) -> transparent
        assert_eq!(left.alpha[(h * 18 / 100) * w + (w * 48 / 100)], 0);
        // License plate cutout (`(275, 242)` in `656x484`) -> transparent
        assert_eq!(left.alpha[(h * 50 / 100) * w + (w * 42 / 100)], 0);

        // Exact horizontal mirror `x' = w - 1 - x`
        for y in 0..h {
            for x in 0..w {
                assert_eq!(left.alpha[y * w + x], right.alpha[y * w + (w - 1 - x)]);
            }
        }

        // 3. Tinting strictly applies Foreground (`fg`) to player & Secondary 2 (`fg2`) to obstacles
        let mut cache = CarSpriteCache::new();
        cache.ensure_tinted(0x3C3637, 0x595959);
        let p_spr = cache.player_sprite(1);
        assert_eq!(&p_spr.rgba[0..3], &[0x3C, 0x36, 0x37]);
        let o_spr = cache.obstacle_sprite(0, 48.0);
        assert_eq!(&o_spr.rgba[0..3], &[0x59, 0x59, 0x59]);
    }

    #[test]
    fn lane_sprite_keys_are_distinct_and_center_scales_085x_on_same_tire_ground() {
        let mut cache = CarSpriteCache::new();
        // Test even when `fg == fg2` to guarantee zero `role` or `lane` bitmask collisions!
        cache.ensure_tinted(0x2B2B2B, 0x2B2B2B);

        let p0 = cache.player_sprite(0);
        let p1 = cache.player_sprite(1);
        let p2 = cache.player_sprite(2);
        assert_ne!(p0.tex_key, p1.tex_key, "Left and Center player tex_key must never collide");
        assert_ne!(p1.tex_key, p2.tex_key);
        assert_ne!(p0.tex_key, p2.tex_key);

        // Verify left lane (`p0`) is asymmetric 3/4 `racerl.svg`, NOT symmetric `racer.svg`
        let p0_w = p0.width as usize;
        let p0_h = p0.height as usize;
        let mut asym_diffs = 0usize;
        for y in 0..p0_h {
            for x in 0..(p0_w / 2) {
                let al = p0.rgba[(y * p0_w + x) * 4 + 3];
                let ar = p0.rgba[(y * p0_w + (p0_w - 1 - x)) * 4 + 3];
                if al != ar {
                    asym_diffs += 1;
                }
            }
        }
        assert!(asym_diffs > 400, "Left lane sprite must be 3/4 perspective `racerl.svg`");

        // Verify obstacle `tex_key`s are distinct across all 3 lanes & distinct from player keys
        for &test_w in &[20.0_f32, 48.0, 96.0, 126.0] {
            let o0 = cache.obstacle_sprite(0, test_w);
            let o1 = cache.obstacle_sprite(1, test_w * CENTER_CAR_SCALE);
            let o2 = cache.obstacle_sprite(2, test_w);
            assert_ne!(o0.tex_key, o1.tex_key);
            assert_ne!(o1.tex_key, o2.tex_key);
            assert_ne!(o0.tex_key, o2.tex_key);
            assert_ne!(o0.tex_key, p0.tex_key);
            assert_ne!(o1.tex_key, p1.tex_key);
        }

        // Verify Center (`0.85x`) vs Left/Right (`1.0x`) dimensions & identical tire ground Y
        for &persp_scale in &[1.0_f32, 0.65, 0.30] {
            let (_, dy0, dw0, dh0) =
                car_draw_rect(&cache, 0, 200.0, 520.0, persp_scale, 14.0, 0.0, 12.0);
            let (_, dy1, dw1, dh1) =
                car_draw_rect(&cache, 1, 382.0, 520.0, persp_scale, 14.0, 0.0, 12.0);
            let (_, dy2, dw2, dh2) =
                car_draw_rect(&cache, 2, 564.0, 520.0, persp_scale, 14.0, 0.0, 12.0);

            assert!((dw0 - dw2).abs() < 1e-4);
            assert!((dh0 - dh2).abs() < 1e-4);
            assert!(((dw1 / dw0) - CENTER_CAR_SCALE).abs() < 1e-4);

            let tire_y0 = dy0 + dh0 * TIRE_CONTACT_Y_RATIO;
            let tire_y1 = dy1 + dh1 * TIRE_CONTACT_Y_RATIO;
            let tire_y2 = dy2 + dh2 * TIRE_CONTACT_Y_RATIO;
            assert!((tire_y0 - tire_y1).abs() < 1e-3);
            assert!((tire_y0 - tire_y2).abs() < 1e-3);
        }
    }
}
