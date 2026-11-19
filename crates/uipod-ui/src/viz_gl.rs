//! OpenGL ES 3.0 two-pass 4-shade audio-reactive visualizer engine (`VizEngine`).
//!
//! - **Pass 1 (Offscreen Ping-Pong Feedback FBO at `256x192` `4x4` or `512x384` `2x2`)**:
//!   Renders continuous luminance scenes with Milkdrop-style UV warp feedback, trails,
//!   live stereo oscilloscopes, and 256-band log FFT geometry.
//! - **Pass 2 (Fullscreen `1024x768` 4-Shade Bayer LCD Post-Process)**:
//!   Samples Pass 1 at crisp macro-pixel coordinates, applies exact `4x4` ordered Bayer
//!   dithering, and maps every pixel strictly onto the active UIPod theme's 4 shades
//!   (`bg -> fg1/fg2 -> fg`), fading cleanly into `theme.bg` during transitions.

#[cfg(not(target_arch = "wasm32"))]
use std::ffi::c_void;

use glow::HasContext;

use crate::viz_dsp::{VizAudioFrame, VizDsp, AUDIO_TEX_H, AUDIO_TEX_W};
use crate::ThemeColors;

/// Metadata for a built-in 4-shade audio-reactive preset.
#[derive(Debug, Clone, Copy)]
pub struct VizPresetInfo {
    pub id: &'static str,
    pub name_en: &'static str,
    pub name_uk: &'static str,
}

pub const VIZ_PRESETS: &[VizPresetInfo] = &[
    VizPresetInfo {
        id: "lcd_plasma",
        name_en: "LCD Plasma",
        name_uk: "LCD Плазма",
    },
    VizPresetInfo {
        id: "sonic_vortex",
        name_en: "Sonic Vortex",
        name_uk: "Звуковий вихор",
    },
    VizPresetInfo {
        id: "phosphor_scope",
        name_en: "Phosphor Scope",
        name_uk: "Осцилограф",
    },
    VizPresetInfo {
        id: "spectrum_matrix",
        name_en: "Spectrum Matrix",
        name_uk: "Спектр-матриця",
    },
    VizPresetInfo {
        id: "cyber_horizon",
        name_en: "Cyber Horizon",
        name_uk: "Кібер-горизонт",
    },
    VizPresetInfo {
        id: "liquid_metal",
        name_en: "Liquid Metal",
        name_uk: "Рідкий метал",
    },
    VizPresetInfo {
        id: "chladni_plate",
        name_en: "Chladni Plate",
        name_uk: "Хладні-резонанс",
    },
    VizPresetInfo {
        id: "star_warp",
        name_en: "Star Warp",
        name_uk: "Зоряний стрибок",
    },
    VizPresetInfo {
        id: "vu_meter",
        name_en: "Analog VU Meter",
        name_uk: "Стерео VU-метр",
    },
    VizPresetInfo {
        id: "cassette_deck",
        name_en: "Cassette Deck",
        name_uk: "Касетна дека",
    },
];

#[inline]
fn unpack_rgb_f32_and_luma(rgb: u32) -> (i32, [f32; 3]) {
    let r = ((rgb >> 16) & 0xFF) as i32;
    let g = ((rgb >> 8) & 0xFF) as i32;
    let b = (rgb & 0xFF) as i32;
    let luma = 54 * r + 183 * g + 19 * b;
    (
        luma,
        [
            (r as f32) / 255.0,
            (g as f32) / 255.0,
            (b as f32) / 255.0,
        ],
    )
}

/// Orders the 4 theme colors from `theme.bg_rgb` (`slot 0`, intensity `0.0`) toward
/// `theme.fg_rgb` (`slot 3`, intensity `1.0`), sorting `fg1_rgb` and `fg2_rgb` monotonically
/// between them. Works identically on classic light iPod Mini LCD themes and dark themes.
pub fn order_palette_bg_to_fg(theme: ThemeColors) -> [[f32; 3]; 4] {
    let (l_bg, c_bg) = unpack_rgb_f32_and_luma(theme.bg_rgb);
    let (l_fg, c_fg) = unpack_rgb_f32_and_luma(theme.fg_rgb);
    let (l_1, c_1) = unpack_rgb_f32_and_luma(theme.fg1_rgb);
    let (l_2, c_2) = unpack_rgb_f32_and_luma(theme.fg2_rgb);

    // Distance from `bg` along the luminance direction toward `fg`
    let dir = if l_fg >= l_bg { 1 } else { -1 };
    let d1 = (l_1 - l_bg) * dir;
    let d2 = (l_2 - l_bg) * dir;

    if d1 <= d2 {
        [c_bg, c_1, c_2, c_fg]
    } else {
        [c_bg, c_2, c_1, c_fg]
    }
}

struct PingPongPair {
    w: i32,
    h: i32,
    tex: [glow::Texture; 2],
    fbo: [glow::Framebuffer; 2],
    front: usize,
}

struct SceneProgram {
    prog: glow::Program,
    u_prev: Option<glow::UniformLocation>,
    u_audio: Option<glow::UniformLocation>,
    u_res: Option<glow::UniformLocation>,
    u_time: Option<glow::UniformLocation>,
    u_bands: Option<glow::UniformLocation>,
    u_att: Option<glow::UniformLocation>,
    u_beat: Option<glow::UniformLocation>,
    u_vu: Option<glow::UniformLocation>,
    u_transport: Option<glow::UniformLocation>,
}

struct PresentProgram {
    prog: glow::Program,
    u_scene: Option<glow::UniformLocation>,
    u_grid_size: Option<glow::UniformLocation>,
    u_fade: Option<glow::UniformLocation>,
    u_pal0: Option<glow::UniformLocation>,
    u_pal1: Option<glow::UniformLocation>,
    u_pal2: Option<glow::UniformLocation>,
    u_pal3: Option<glow::UniformLocation>,
}

pub struct VizEngine {
    gl: glow::Context,
    dsp: VizDsp,
    vao: Option<glow::VertexArray>,
    vbo: glow::Buffer,
    audio_tex: glow::Texture,
    fbo_coarse: PingPongPair, // 256x192 (4x4 macro-pixels on 1024x768)
    fbo_fine: PingPongPair,   // 512x384 (2x2 fine pixels on 1024x768)
    scenes: Vec<SceneProgram>,
    custom_scene: Option<SceneProgram>,
    use_custom_scene: bool,
    present: PresentProgram,
    active_preset: usize,
    fine_grain: bool,
    needs_clear: bool,
    reel_ang_l: f32,
    reel_ang_r: f32,
}

const SHARED_VS: &str = r#"
attribute vec2 a_pos;
attribute vec2 a_uv;
varying vec2 v_uv;
void main() {
    v_uv = a_uv;
    gl_Position = vec4(a_pos, 0.0, 1.0);
}
"#;

const SCENE_FS_HEADER: &str = r#"
#ifdef GL_ES
#ifdef GL_FRAGMENT_PRECISION_HIGH
precision highp float;
#else
precision mediump float;
#endif
#endif
varying vec2 v_uv;
uniform sampler2D u_prev;
uniform sampler2D u_audio;
uniform vec2 u_res;
uniform vec2 u_time;      // x = wall_time, y = music_time
uniform vec4 u_bands;     // x = sub_bass, y = bass, z = mid, w = treb
uniform vec4 u_att;       // x = bass_att, y = mid_att, z = treb_att, w = stereo_width
uniform vec4 u_beat;      // x = beat_pulse, y = beat_dir, z = beat_phase, w = reset_flag
uniform vec4 u_vu;        // x = vu_l, y = vu_r, z = peak_l, w = peak_r
uniform vec4 u_transport; // x = track_progress, y = reel_ang_l, z = reel_ang_r, w = transport_speed

const float PI = 3.14159265;
const float TAU = 6.28318530;

vec2 aspect_p(vec2 uv) {
    vec2 p = uv * 2.0 - 1.0;
    p.x *= u_res.x / u_res.y;
    return p;
}

float wave_mono(float x) {
    return texture2D(u_audio, vec2(clamp(x, 0.0, 1.0), 0.25)).b * 2.0 - 1.0;
}

vec2 wave_lr(float x) {
    vec2 lr = texture2D(u_audio, vec2(clamp(x, 0.0, 1.0), 0.25)).rg;
    return lr * 2.0 - 1.0;
}

vec3 spec_sample(float x) {
    return texture2D(u_audio, vec2(clamp(x, 0.0, 1.0), 0.75)).rgb;
}

vec3 prev_scene(vec2 uv) {
    return texture2D(u_prev, clamp(uv, 0.002, 0.998)).rgb * (1.0 - u_beat.w);
}

mat2 rot2(float a) {
    float c = cos(a);
    float s = sin(a);
    return mat2(c, -s, s, c);
}
"#;

// 0. LCD Plasma — Audio-reactive interference waves + radial bass shockwaves
const FS_LCD_PLASMA: &str = r#"
void main() {
    vec2 p = aspect_p(v_uv);
    float mt = u_time.y * 1.35;
    float bass = u_bands.y;
    float mid = u_bands.z;
    float treb = u_bands.w;
    float pulse = u_beat.x;

    vec2 c1 = vec2(sin(mt * 0.63) * 0.55, cos(mt * 0.47) * 0.42);
    vec2 c2 = vec2(cos(mt * 0.51) * -0.60, sin(mt * 0.79) * 0.45);
    float d1 = length(p - c1);
    float d2 = length(p - c2);
    float r0 = length(p);

    float freq = 5.5 + mid * 2.4;
    float w1 = sin(p.x * freq + mt * 1.6 + sin(p.y * 3.5 - mt));
    float w2 = sin(p.y * (freq * 0.92) - mt * 1.3 + cos(p.x * 4.0 + mt * 0.8));
    float w3 = sin((p.x + p.y * u_beat.y) * (4.2 + treb * 2.2) + mt * 1.9);
    float rings = sin(d1 * (11.0 + bass * 5.0) - mt * 3.4)
                + sin(d2 * (9.5 + mid * 3.5) - mt * 2.7);

    // Live circular waveform ripple at center
    float ang = atan(p.y, p.x) / TAU + 0.5;
    float wv = wave_mono(ang) * (0.14 + 0.12 * bass);
    float ring_halo = exp(-abs(r0 - (0.44 + 0.10 * pulse + wv)) * 12.0) * (0.45 + 0.55 * bass);

    float plasma = (w1 + w2 + w3 + rings) / 5.0; // -1..1
    float luma = clamp(plasma * (0.48 + 0.14 * bass) + 0.50 + ring_halo * 0.42 + pulse * 0.10, 0.0, 1.0);
    gl_FragColor = vec4(vec3(luma), 1.0);
}
"#;

// 1. Sonic Vortex — Classic Milkdrop ping-pong UV zoom/swirl warp + circular wave + sparks
const FS_SONIC_VORTEX: &str = r#"
float hash21(vec2 p) {
    p = fract(p * vec2(123.34, 456.21));
    p += dot(p, p + 45.32);
    return fract(p.x * p.y);
}

void main() {
    vec2 p = aspect_p(v_uv);
    float r = length(p);
    float ang = atan(p.y, p.x);

    // Audio-driven Milkdrop warp field
    float zoom = 1.018 + 0.042 * u_bands.y + 0.035 * u_beat.x;
    float swirl = (0.022 + 0.048 * u_bands.z) * u_beat.y * (1.15 - 0.55 * smoothstep(0.0, 1.4, r));
    vec2 pw = rot2(swirl) * (p / zoom);
    pw += 0.008 * u_bands.w * vec2(
        sin(p.y * 9.0 + u_time.y * 3.0),
        cos(p.x * 9.0 - u_time.y * 2.7)
    );

    vec2 prev_uv = vec2(pw.x * (u_res.y / u_res.x), pw.y) * 0.5 + 0.5;
    float trail = prev_scene(prev_uv).r * (0.915 + 0.03 * clamp(u_att.x, 0.0, 1.5));

    // Symmetrical mirrored circular waveform in center
    float u = abs(ang / PI);
    float wv = wave_mono(u);
    float target_r = 0.28 + 0.10 * u_bands.x + 0.06 * u_beat.x + wv * (0.13 + 0.08 * u_bands.z);
    float thickness = 0.016 + 0.012 * u_beat.x;
    float wave_ring = smoothstep(thickness * 2.2, 0.0, abs(r - target_r));

    // Radial spectrum spokes on strong mids/highs
    float spoke_u = fract(ang / TAU + 0.5 + u_time.y * 0.03 * u_beat.y);
    float sp = spec_sample(abs(spoke_u * 2.0 - 1.0)).r;
    float spokes = smoothstep(0.04, 0.0, abs(r - (0.12 + sp * 0.32))) * sp * 0.38;

    // Treble star-sparks injected near center and advected outward by warp!
    vec2 cell = floor((p + 2.0) * 28.0 + floor(u_time.x * 15.0));
    float rnd = hash21(cell);
    float spark = step(0.994 - 0.006 * clamp(u_bands.w, 0.0, 2.0), rnd) * smoothstep(0.75, 0.05, r) * 0.85;

    float luma = clamp(max(trail, wave_ring * 0.96) + spokes + spark, 0.0, 1.0);
    gl_FragColor = vec4(vec3(luma), 1.0);
}
"#;

// 2. Phosphor Scope — Dual-beam stereo CRT oscilloscope + XY Lissajous + phosphor persistence
const FS_PHOSPHOR_SCOPE: &str = r#"
void main() {
    vec2 p = aspect_p(v_uv);

    // Subtle phosphor trail decay with gentle upward heat drift
    vec3 old = prev_scene(v_uv + vec2(0.0, -0.0025));
    float phosphor = old.r * 0.80;

    // Horizontal stereo oscilloscope beams (top/bottom split on stereo width, merged on mono)
    vec2 lr = wave_lr(v_uv.x);
    float sep = 0.16 * clamp(u_att.w, 0.0, 1.2);
    float y_l = lr.x * 0.44 + sep;
    float y_r = lr.y * 0.44 - sep;
    float d_l = abs(p.y - y_l);
    float d_r = abs(p.y - y_r);
    float beam_w = 0.018 + 0.010 * u_beat.x;
    float beam = smoothstep(beam_w * 2.6, 0.0, d_l) * 0.82
               + smoothstep(beam_w * 2.6, 0.0, d_r) * 0.82;

    // Central XY Stereo Lissajous orbital trace (32 sample segments)
    float liss = 0.0;
    for (int i = 0; i < 32; i++) {
        float fi = (float(i) + 0.5) / 32.0;
        vec2 s = wave_lr(fi) * 0.58;
        // Rotate 45 deg so mono is vertical and stereo width expands horizontally
        vec2 lp = vec2((s.x - s.y) * 0.85, (s.x + s.y) * 0.62);
        float d = length(p - lp);
        liss = max(liss, smoothstep(0.055, 0.008, d));
    }

    // Subtle CRT scope crosshair & border graticule ticks
    float gx = smoothstep(0.008, 0.0, abs(p.y)) * step(0.55, fract(v_uv.x * 24.0));
    float gy = smoothstep(0.008, 0.0, abs(p.x)) * step(0.55, fract(v_uv.y * 18.0));
    float grat = (gx + gy) * (0.24 + 0.22 * u_beat.x);

    float luma = clamp(max(phosphor, max(beam, liss * 0.92)) + grat, 0.0, 1.0);
    gl_FragColor = vec4(vec3(luma), 1.0);
}
"#;

// 3. Spectrum Matrix — 32-band segmented LED FFT analyzer + peak-hold caps + rippled reflection
const FS_SPECTRUM_MATRIX: &str = r#"
void main() {
    float horizon = 0.24;
    float cols = 32.0;
    float col_idx = floor(v_uv.x * cols);
    float col_f = fract(v_uv.x * cols);
    float bar_mask = smoothstep(0.06, 0.14, col_f) * smoothstep(0.94, 0.86, col_f);

    float freq_u = (col_idx + 0.5) / cols;
    vec3 sp = spec_sample(freq_u);
    float h = sp.r * 0.68;
    float pk = sp.g * 0.68;

    float luma = 0.0;
    if (v_uv.y >= horizon) {
        float y = v_uv.y - horizon;
        // Segmented LED cells vertically
        float seg_f = fract(y * 32.0);
        float seg_mask = smoothstep(0.08, 0.20, seg_f);
        float lit = step(y, h) * bar_mask * seg_mask;

        // Crisp peak-hold floating cap
        float cap = smoothstep(0.018, 0.004, abs(y - pk)) * bar_mask * step(0.02, pk);

        // Background ambient column glow + top live waveform ribbon
        float halo = exp(-max(y - h, 0.0) * 9.0) * sp.r * 0.30 * (0.6 + 0.6 * u_beat.x);
        float wv = wave_mono(v_uv.x) * 0.06;
        float top_wave = smoothstep(0.016, 0.003, abs(v_uv.y - (0.90 + wv))) * 0.65;

        luma = clamp(lit * (0.62 + 0.38 * (y / 0.68)) + cap + halo + top_wave, 0.0, 1.0);
    } else {
        // Liquid water reflection below horizon
        float depth = (horizon - v_uv.y) / horizon; // 0 at horizon -> 1 at bottom
        float ripple = sin(depth * 28.0 - u_time.y * 5.5 + v_uv.x * 12.0) * (0.008 + 0.012 * u_bands.y) * depth;
        float rx = clamp(v_uv.x + ripple, 0.0, 1.0);
        float r_col = floor(rx * cols);
        float r_cf = fract(rx * cols);
        float r_mask = smoothstep(0.06, 0.16, r_cf) * smoothstep(0.94, 0.84, r_cf);
        float rh = spec_sample((r_col + 0.5) / cols).r * 0.68;
        float ry = depth * 0.48;
        float refl = step(ry, rh) * r_mask * (1.0 - depth * 0.72) * 0.58;
        float horizon_line = smoothstep(0.015, 0.0, abs(v_uv.y - horizon)) * (0.55 + 0.40 * u_beat.x);
        luma = clamp(refl + horizon_line, 0.0, 1.0);
    }

    gl_FragColor = vec4(vec3(luma), 1.0);
}
"#;

// 4. Cyber Horizon — 3D synthwave audio canyon grid + waveform-ringed pulsing sun
const FS_CYBER_HORIZON: &str = r#"
void main() {
    vec2 p = aspect_p(v_uv);
    float hor = 0.06 + 0.03 * sin(u_time.x * 0.6);
    float luma = 0.0;

    if (p.y > hor) {
        // Sky: pulsing dithered sun with vintage horizontal scan cutouts + live waveform corona
        vec2 sc = vec2(0.0, hor + 0.38);
        vec2 dp = p - sc;
        float r = length(dp);
        float sun_r = 0.30 + 0.045 * u_bands.x + 0.035 * u_beat.x;
        float bands = step(0.28, fract((p.y - hor) * 18.0 + u_time.x * 0.25)) + step(0.06, p.y - sc.y);
        float sun = smoothstep(sun_r + 0.012, sun_r - 0.012, r) * clamp(bands, 0.0, 1.0);

        float ang = abs(atan(dp.y, dp.x) / PI);
        float wv = wave_mono(ang) * (0.06 + 0.05 * u_bands.z);
        float corona = smoothstep(0.024, 0.004, abs(r - (sun_r + 0.045 + wv))) * 0.90;
        float glow = exp(-max(r - sun_r, 0.0) * 5.5) * (0.35 + 0.28 * u_bands.y);

        // Distant FFT mountain silhouette along the horizon
        float mu = clamp(abs(p.x) / 1.35, 0.0, 1.0);
        float mh = spec_sample(mu * 0.75).r * (0.26 + 0.10 * u_bands.y) * smoothstep(0.08, 0.38, abs(p.x));
        float mtn_top = hor + mh;
        float mtn_edge = smoothstep(0.018, 0.003, abs(p.y - mtn_top)) * step(hor, mtn_top - 0.005);
        float mtn_fill = step(p.y, mtn_top);

        luma = mix(clamp(sun * 0.92 + corona + glow, 0.0, 1.0), 0.22, mtn_fill * 0.85) + mtn_edge * 0.95;
    } else {
        // Perspective 3D canyon floor deformed by FFT spectrum
        float dy = max(hor - p.y, 0.004);
        float z = 0.42 / dy;
        float x3 = p.x * z;
        float scroll = u_time.y * 3.8;

        // Raise canyon walls using live spectrum along X
        float wall_u = clamp((abs(x3) - 0.6) / 4.5, 0.0, 1.0);
        float elev = spec_sample(wall_u).r * smoothstep(0.5, 2.2, abs(x3)) * 0.55;
        float z_mod = z + scroll - elev * 1.4;

        float gx = abs(fract(x3 * 0.65 + 0.5) - 0.5);
        float gz = abs(fract(z_mod * 0.55) - 0.5);
        float lw = 0.035 + dy * 0.05;
        float line_x = smoothstep(lw * 1.8, 0.0, gx);
        float line_z = smoothstep(lw * 2.2, 0.0, gz);
        float fog = exp(-z * 0.13);

        luma = (max(line_x, line_z) * (0.72 + 0.28 * u_beat.x) + elev * 0.45) * fog;
        luma += smoothstep(0.02, 0.0, dy) * (0.75 + 0.25 * u_beat.x);
    }

    gl_FragColor = vec4(vec3(clamp(luma, 0.0, 1.0)), 1.0);
}
"#;

// 5. Liquid Metal — 7 audio-reactive ferrofluid metaballs + viscous ripple feedback
const FS_LIQUID_METAL: &str = r#"
void main() {
    vec2 p = aspect_p(v_uv);
    float mt = u_time.y * 1.15;

    // Gentle outward ripple advection in feedback buffer
    vec2 dir = normalize(p + vec2(0.0001));
    float rip = sin(length(p) * 18.0 - mt * 5.0) * 0.004 * (0.4 + u_bands.y);
    float bg_trail = prev_scene(v_uv - dir * (0.006 + rip)).r * 0.78;

    float field = 0.0;
    for (int i = 0; i < 7; i++) {
        float fi = float(i);
        float sp_v = spec_sample((fi + 0.5) / 8.0).r;
        float a = mt * (0.55 + 0.14 * fi) * ((mod(fi, 2.0) < 0.5) ? 1.0 : -1.0) + fi * 0.95;
        float orbit = (0.22 + 0.07 * fi) * (0.75 + 0.32 * u_bands.z) * (1.0 - 0.22 * u_beat.x);
        vec2 center = vec2(cos(a) * 1.18, sin(a * 1.25)) * orbit;
        float rad = 0.10 + 0.055 * sp_v + ((i == 0) ? 0.065 * u_bands.x : 0.0);
        float d2 = dot(p - center, p - center) + 0.006;
        field += (rad * rad) / d2;
    }

    // Specular metallic banding + crisp liquid contour rim
    float blob = smoothstep(0.88, 1.18, field);
    float rim = smoothstep(0.82, 0.98, field) * (1.0 - smoothstep(0.98, 1.16, field));
    float spec_hi = smoothstep(1.55, 2.45, field);
    float metal = blob * (0.42 + 0.35 * sin(field * 3.2 - mt * 2.0)) + rim * 0.85 + spec_hi * 0.55;

    float luma = clamp(max(bg_trail * 0.68, metal) + u_beat.x * rim * 0.25, 0.0, 1.0);
    gl_FragColor = vec4(vec3(luma), 1.0);
}
"#;

// 6. Chladni Plate — Standing-wave acoustic cymatics morphing resonant modes with the music
const FS_CHLADNI_PLATE: &str = r#"
void main() {
    vec2 p = aspect_p(v_uv) * 0.92;
    float r = length(p);

    // Morph modal harmonic numbers smoothly with music_time and beat_phase
    float t = u_time.y * 0.28 + u_beat.z * 0.35;
    float n = 2.0 + 3.0 * (0.5 + 0.5 * sin(t * 0.9));
    float m = 3.0 + 4.0 * (0.5 + 0.5 * cos(t * 0.7 + 1.1));

    // Audio vibration warps plate coordinates slightly with live waveform & bass
    float wv = wave_mono(clamp(r * 0.65, 0.0, 1.0)) * (0.045 + 0.045 * u_bands.y);
    vec2 q = rot2(u_time.x * 0.06 * u_beat.y) * (p * (1.0 + wv));

    float a = 1.0 + 0.45 * sin(u_time.y * 0.8) + 0.35 * u_bands.y;
    float b = -1.0 + 0.45 * cos(u_time.y * 0.65) - 0.35 * u_bands.z;

    float ch1 = a * sin(PI * n * q.x) * sin(PI * m * q.y)
              + b * sin(PI * m * q.x) * sin(PI * n * q.y);

    // Nodal sand lines form where |ch1| -> 0; sharpen width on treble & beats
    float nodal_w = 0.38 + 0.26 * u_bands.x + 0.18 * u_beat.x;
    float sand = 1.0 - smoothstep(0.0, nodal_w, abs(ch1));

    // Subtle persistence trail + circular acoustic plate boundary
    float trail = prev_scene(v_uv).r * 0.65;
    float plate_vignette = smoothstep(1.52, 0.85, r);
    float luma = clamp(max(trail, sand * (0.78 + 0.22 * u_bands.w)) * plate_vignette, 0.0, 1.0);
    gl_FragColor = vec4(vec3(luma), 1.0);
}
"#;

// 7. Star Warp — Radial zoom-blur tunnel + multi-layer starfield + bass shockwave ring
const FS_STAR_WARP: &str = r#"
float hash21(vec2 p) {
    p = fract(p * vec2(234.34, 435.345));
    p += dot(p, p + 34.23);
    return fract(p.x * p.y);
}

void main() {
    vec2 p = aspect_p(v_uv);
    vec2 center = vec2(sin(u_time.x * 0.42) * 0.18, cos(u_time.x * 0.31) * 0.14);
    vec2 dp = p - center;

    // Radial zoom-blur trail feedback (stars streak outward on bass hits!)
    float warp_pull = 0.968 - 0.032 * clamp(u_bands.y, 0.0, 2.0) - 0.025 * u_beat.x;
    vec2 prev_p = center + rot2(0.010 * u_beat.y * u_bands.z) * (dp * warp_pull);
    vec2 prev_uv = vec2(prev_p.x * (u_res.y / u_res.x), prev_p.y) * 0.5 + 0.5;
    float trail = prev_scene(prev_uv).r * (0.84 + 0.08 * clamp(u_bands.y, 0.0, 1.5));

    // 4 depth-looping starfield planes flying toward viewer
    float stars = 0.0;
    float speed = u_time.y * 0.65;
    for (int i = 0; i < 4; i++) {
        float fi = float(i);
        float z = fract(fi * 0.25 - speed);
        float scale = mix(14.0, 1.4, 1.0 - z);
        vec2 uv_s = dp * scale + vec2(fi * 17.3, fi * 9.1);
        vec2 id = floor(uv_s);
        vec2 gv = fract(uv_s) - 0.5;
        vec2 jitter = vec2(hash21(id) - 0.5, hash21(id + vec2(19.7, 7.3)) - 0.5) * 0.68;
        float d = length(gv - jitter);
        float fade_z = smoothstep(0.0, 0.18, 1.0 - z) * smoothstep(1.0, 0.72, 1.0 - z);
        float star = smoothstep(0.11, 0.015, d) * fade_z;
        stars = max(stars, star);
    }

    // Expanding bass waveform shockwave ring
    float r = length(dp);
    float ang = abs(atan(dp.y, dp.x) / PI);
    float wv = wave_mono(ang) * 0.05;
    float ring_r = fract(u_time.y * 0.55) * 1.25;
    float shock = smoothstep(0.035, 0.005, abs(r - (ring_r + wv)))
                * (1.0 - ring_r / 1.35) * (0.35 + 0.65 * u_bands.x);

    float luma = clamp(max(trail, stars * 0.98) + shock, 0.0, 1.0);
    gl_FragColor = vec4(vec3(luma), 1.0);
}
"#;

// 8. Analog VU Meter — Classic twin rectangular stereo galvanometer dials (L / R) + PEAK lamps
const FS_VU_METER: &str = r#"
float sd_rbox(vec2 p, vec2 b, float r) {
    vec2 q = abs(p) - b + vec2(r);
    return length(max(q, 0.0)) + min(max(q.x, q.y), 0.0) - r;
}

float sd_seg(vec2 p, vec2 a, vec2 b) {
    vec2 pa = p - a;
    vec2 ba = b - a;
    float h = clamp(dot(pa, ba) / max(dot(ba, ba), 0.00001), 0.0, 1.0);
    return length(pa - ba * h);
}

float draw_glyph_vu(vec2 p) {
    // Crisp vector "V U" centered at origin
    float d_v1 = sd_seg(p, vec2(-0.086, 0.033), vec2(-0.050, -0.033));
    float d_v2 = sd_seg(p, vec2(-0.050, -0.033), vec2(-0.014, 0.033));
    float d_u1 = sd_seg(p, vec2(0.018, 0.033), vec2(0.018, -0.020));
    float d_u2 = sd_seg(p, vec2(0.080, 0.033), vec2(0.080, -0.020));
    float d_u3 = sd_seg(p, vec2(0.018, -0.025), vec2(0.080, -0.025));
    float d = min(min(d_v1, d_v2), min(d_u1, min(d_u2, d_u3)));
    return smoothstep(0.012, 0.004, d);
}

float draw_chan_glyph(vec2 p, float is_right) {
    if (is_right < 0.5) {
        // 'L'
        float d1 = sd_seg(p, vec2(-0.024, 0.036), vec2(-0.024, -0.036));
        float d2 = sd_seg(p, vec2(-0.024, -0.036), vec2(0.026, -0.036));
        return smoothstep(0.013, 0.004, min(d1, d2));
    } else {
        // 'R'
        float d1 = sd_seg(p, vec2(-0.026, 0.036), vec2(-0.026, -0.036));
        float d2 = sd_seg(p, vec2(-0.026, 0.036), vec2(0.020, 0.036));
        float d3 = sd_seg(p, vec2(0.020, 0.036), vec2(0.020, 0.002));
        float d4 = sd_seg(p, vec2(-0.026, 0.002), vec2(0.020, 0.002));
        float d5 = sd_seg(p, vec2(-0.002, 0.002), vec2(0.026, -0.036));
        float d = min(min(d1, d2), min(d3, min(d4, d5)));
        return smoothstep(0.013, 0.004, d);
    }
}

float render_single_vu(vec2 p, float cx, float vu_val, float pk_val, float is_right) {
    // Wide landscape rectangular window centered vertically (y = 0.0), 1.5x shorter than square
    vec2 wc = vec2(cx, 0.0);
    vec2 half_sz = vec2(0.585, 0.363);
    vec2 q = p - wc;
    float d_win = sd_rbox(q, half_sz, 0.05);

    if (d_win > 0.0) {
        // Recessed bezel frame outside the glass dial
        float rim = smoothstep(0.038, 0.004, d_win) * 0.92;
        return rim;
    }

    // Inside analog glass dial window: warm backlit radial lamp glow + recessed top/side shadow
    float inner_bevel = smoothstep(-0.042, 0.0, d_win) * (0.42 + 0.25 * clamp(q.y + 0.22, 0.0, 1.0));
    float lamp_dist = length((q - vec2(0.0, -0.10)) * vec2(0.80, 1.35));
    float lamp = (1.0 - smoothstep(0.08, 0.68, lamp_dist)) * (0.17 + 0.09 * pk_val);
    float luma = max(lamp, inner_bevel);

    // Panoramic galvanometer pivot sits below the lower window frame
    vec2 piv = vec2(cx, -0.66);
    vec2 rel = p - piv;
    float rad = length(rel);
    float ang = atan(rel.x, rel.y); // 0 = vertical 12 o'clock
    float ang_max = 0.56;
    float u_arc = (ang + ang_max) / (2.0 * ang_max); // 0.0 (-20 dB) .. 1.0 (+3 dB)

    if (u_arc >= -0.01 && u_arc <= 1.01) {
        // Main baseline arc (r = 0.84) + inner % modulation sub-arc (r = 0.77 up to 0 VU = 0.72)
        float main_arc = smoothstep(0.010, 0.003, abs(rad - 0.84)) * 0.80;
        float sub_arc = smoothstep(0.008, 0.002, abs(rad - 0.77)) * step(u_arc, 0.725) * 0.48;
        luma = max(luma, max(main_arc, sub_arc));

        // Classic thick +3 dB overload band (0.72 .. 1.00, r = 0.845 .. 0.885)
        if (u_arc >= 0.72 && u_arc <= 1.005) {
            float red_band = smoothstep(0.022, 0.013, abs(rad - 0.864)) * 0.96;
            luma = max(luma, red_band);
        }

        // 13 graduated radial dB ticks along the panoramic scale arc
        float tick_idx = floor(u_arc * 12.0 + 0.5);
        float tick_u = tick_idx / 12.0;
        float is_major = (mod(tick_idx, 2.0) < 0.5 || abs(tick_idx - 9.0) < 0.5) ? 1.0 : 0.0;
        float tick_len = mix(0.038, 0.068, is_major);
        float tick_ang = mix(-ang_max, ang_max, clamp(tick_u, 0.0, 1.0));
        float arc_dist = abs(ang - tick_ang) * rad;
        float r_lo = 0.84;
        float r_hi = 0.84 + tick_len;
        if (rad >= r_lo - 0.005 && rad <= r_hi) {
            float tw = mix(0.007, 0.012, is_major);
            float tk = smoothstep(tw, tw * 0.25, arc_dist) * mix(0.68, 0.95, is_major);
            luma = max(luma, tk);
        }
    }

    // Center "VU" legend & lower-left L / R channel badge
    float vu_lbl = draw_glyph_vu(p - vec2(cx, -0.075)) * 0.72;
    vec2 ch_pos = p - vec2(cx - 0.455, -0.235);
    float ch_box = smoothstep(0.010, 0.003, abs(sd_rbox(ch_pos, vec2(0.060, 0.056), 0.016))) * 0.65;
    float ch_lbl = draw_chan_glyph(ch_pos, is_right) * 0.92;
    luma = max(luma, max(vu_lbl, max(ch_box, ch_lbl)));

    // Upper-right PEAK overload lamp jewel
    vec2 pk_pos = p - vec2(cx + 0.465, 0.245);
    float pk_r = length(pk_pos);
    float pk_ring = smoothstep(0.011, 0.003, abs(pk_r - 0.046)) * 0.82;
    float pk_core = smoothstep(0.038, 0.013, pk_r) * clamp(0.18 + pk_val * 0.95, 0.0, 1.0);
    float pk_halo = exp(-pk_r * 18.0) * pk_val * 0.55;
    luma = clamp(max(luma, max(pk_ring, pk_core)) + pk_halo, 0.0, 1.0);

    // Physical needle + dial cast shadow
    float n_ang = mix(-ang_max, ang_max, clamp(vu_val, 0.0, 1.03));
    vec2 n_dir = vec2(sin(n_ang), cos(n_ang));
    vec2 n_a = piv + n_dir * 0.385;
    vec2 n_b = piv + n_dir * 0.912;

    // Soft offset shadow cast onto the backlit scale card
    float d_shad = sd_seg(p - vec2(0.018, -0.022), n_a, n_b);
    luma = max(luma, smoothstep(0.018, 0.005, d_shad) * 0.36);

    // Tapered high-contrast galvanometer needle
    float proj = clamp(dot(p - n_a, n_dir) / 0.527, 0.0, 1.0);
    float nw = mix(0.015, 0.007, proj);
    float d_needle = sd_seg(p, n_a, n_b);
    float needle = smoothstep(nw, nw * 0.25, d_needle);

    // Shallow bottom movement coil dome & concentric trim ring
    float cap_body = smoothstep(0.385, 0.372, rad) * 0.88;
    float cap_rim = smoothstep(0.013, 0.004, abs(rad - 0.385)) * 1.0;
    float cap_inner = smoothstep(0.009, 0.003, abs(rad - 0.340)) * 0.65;

    return clamp(max(luma, max(needle, max(cap_body, max(cap_rim, cap_inner)))), 0.0, 1.0);
}

void main() {
    vec2 p = aspect_p(v_uv);

    // Brushed metallic stereo faceplate chassis
    float brush = 0.20 + 0.04 * sin(p.y * 110.0) + 0.02 * cos(p.x * 18.0);
    float luma = brush;

    // Render vertically centered Left (x < 0) and Right (x > 0) rectangular VU housings
    if (abs(p.y) < 0.42) {
        if (p.x < 0.0) {
            float m_l = render_single_vu(p, -0.635, u_vu.x, u_vu.z, 0.0);
            if (sd_rbox(p - vec2(-0.635, 0.0), vec2(0.623, 0.401), 0.088) < 0.0) {
                luma = m_l;
            }
        } else {
            float m_r = render_single_vu(p, 0.635, u_vu.y, u_vu.w, 1.0);
            if (sd_rbox(p - vec2(0.635, 0.0), vec2(0.623, 0.401), 0.088) < 0.0) {
                luma = m_r;
            }
        }
    }

    // Subtle needle motion persistence
    float prev_m = prev_scene(v_uv).r;
    luma = clamp(max(luma, prev_m * 0.28), 0.0, 1.0);
    gl_FragColor = vec4(vec3(luma), 1.0);
}
"#;

// 9. Cassette Deck — Tape-deck viewing window with spinning 6-spline reels + 28-segment LED bridge
const FS_CASSETTE: &str = r#"
float sd_rbox(vec2 p, vec2 b, float r) {
    vec2 q = abs(p) - b + vec2(r);
    return length(max(q, 0.0)) + min(max(q.x, q.y), 0.0) - r;
}

float sd_seg(vec2 p, vec2 a, vec2 b) {
    vec2 pa = p - a;
    vec2 ba = b - a;
    float h = clamp(dot(pa, ba) / max(dot(ba, ba), 0.00001), 0.0, 1.0);
    return length(pa - ba * h);
}

float draw_chan_mini(vec2 p, float is_right) {
    if (is_right < 0.5) {
        float d1 = sd_seg(p, vec2(-0.020, 0.032), vec2(-0.020, -0.032));
        float d2 = sd_seg(p, vec2(-0.020, -0.032), vec2(0.022, -0.032));
        return smoothstep(0.011, 0.003, min(d1, d2));
    } else {
        float d1 = sd_seg(p, vec2(-0.022, 0.032), vec2(-0.022, -0.032));
        float d2 = sd_seg(p, vec2(-0.022, 0.032), vec2(0.016, 0.032));
        float d3 = sd_seg(p, vec2(0.016, 0.032), vec2(0.016, 0.002));
        float d4 = sd_seg(p, vec2(-0.022, 0.002), vec2(0.016, 0.002));
        float d5 = sd_seg(p, vec2(-0.002, 0.002), vec2(0.022, -0.032));
        float d = min(min(d1, d2), min(d3, min(d4, d5)));
        return smoothstep(0.011, 0.003, d);
    }
}

// Renders one cassette reel (magnetic tape pancake + 3-slot hub collar + 6-spline teeth + spindle)
// Returns vec2(luma, alpha_mask)
vec2 render_reel(vec2 p, vec2 center, float r_pack, float reel_ang) {
    vec2 rel = p - center;
    float r = length(rel);
    if (r > r_pack + 0.010) {
        return vec2(0.0, 0.0);
    }

    float a = atan(rel.x, rel.y) + reel_ang;

    // 1. Wound magnetic oxide tape pancake (0.208 .. r_pack)
    if (r > 0.208) {
        float rings = 0.04 * sin(r * 190.0);
        float sheen = 0.12 * pow(0.5 + 0.5 * cos(2.0 * a - 0.65), 2.0);
        float rim = smoothstep(0.010, 0.002, abs(r - r_pack)) * 0.45;
        float pack_luma = clamp(0.24 + rings + sheen + rim, 0.0, 1.0);
        float pack_mask = smoothstep(r_pack + 0.008, r_pack - 0.004, r);
        return vec2(pack_luma, pack_mask);
    }

    // 2. Bright hub wheel collar (0.146 .. 0.208) with 3 radial tape-clamp notches
    if (r >= 0.146) {
        float hub = 0.86;
        // 3 radial clamp slots every 120 deg so wheel rotation is crisp and unmistakable
        float slot_ang = cos(3.0 * a);
        if (r > 0.158 && r < 0.197 && slot_ang > 0.78) {
            hub = 0.16;
        }
        float outer_rim = smoothstep(0.008, 0.002, abs(r - 0.206)) * 0.98;
        float inner_rim = smoothstep(0.008, 0.002, abs(r - 0.148)) * 0.98;
        return vec2(max(hub, max(outer_rim, inner_rim)), 1.0);
    }

    // 3. Drive bore with 6 classic inward-pointing cassette splines / teeth (0.062 .. 0.146)
    if (r >= 0.062) {
        float bore = 0.06;
        float tooth = smoothstep(0.32, 0.52, cos(6.0 * a))
                    * smoothstep(0.095, 0.103, r);
        return vec2(mix(bore, 0.96, tooth), 1.0);
    }

    // 4. Deck drive motor spindle & 3-lobe drive dog in the center (< 0.062)
    float spindle = smoothstep(0.060, 0.050, r) * 0.54;
    float dog = smoothstep(0.42, 0.65, cos(3.0 * a + 1.05)) * step(0.022, r) * 0.78;
    float pin = smoothstep(0.022, 0.015, r) * 0.12;
    float c_luma = (r < 0.022) ? pin : max(spindle, dog);
    return vec2(c_luma, 1.0);
}

void main() {
    vec2 p = aspect_p(v_uv);

    // Brushed anodized tape-deck faceplate chassis
    float brush = 0.18 + 0.035 * sin(p.y * 108.0) + 0.018 * cos(p.x * 16.0);
    float luma = brush;

    // ---- Top / Center Tape-Deck Cassette Door Viewing Window ----------------
    vec2 wc = vec2(0.0, 0.23);
    vec2 w_half = vec2(1.16, 0.55);
    float d_door = sd_rbox(p - wc, w_half, 0.055);

    if (d_door < 0.038) {
        if (d_door > 0.0) {
            // Recessed metallic deck window bezel
            luma = max(luma, smoothstep(0.038, 0.004, d_door) * 0.90);
        } else {
            // Inside cassette compartment behind deck door glass
            luma = 0.06;

            // Moulded Compact Cassette shell body
            float d_shell = sd_rbox(p - wc, vec2(1.09, 0.49), 0.045);
            if (d_shell < 0.0) {
                luma = 0.27 + 0.02 * sin(p.x * 90.0);
                float shell_rim = smoothstep(-0.012, 0.0, d_shell) * 0.68;
                luma = max(luma, shell_rim);

                // 4 classic cassette corner assembly screws
                vec2 sc_p = abs(p - wc) - vec2(1.00, 0.41);
                float sc_r = length(sc_p);
                if (sc_r < 0.030) {
                    luma = (sc_r < 0.018) ? 0.10 : 0.68;
                }

                // Retro cassette top/side label sticker with classic header stripes
                float d_lbl = sd_rbox(p - vec2(0.0, 0.275), vec2(0.96, 0.375), 0.030);
                if (d_lbl < 0.0) {
                    luma = 0.56;
                    if (p.y > 0.555 && p.y < 0.590) {
                        luma = 0.88;
                    } else if (p.y > 0.515 && p.y < 0.538) {
                        luma = 0.24;
                    }
                    float lbl_edge = smoothstep(-0.010, 0.0, d_lbl) * 0.28;
                    luma = mix(luma, 0.22, lbl_edge);
                }

                // Bottom trapezoidal head-shield ridge & capstan/guide holes
                float trap_w = 0.72 - (p.y - (-0.26)) * 0.32;
                if (p.y < -0.105 && abs(p.x) < trap_w) {
                    luma = 0.35;
                    if (abs(p.y - (-0.105)) < 0.010 || abs(abs(p.x) - trap_w) < 0.010) {
                        luma = 0.68;
                    }
                    float h1 = length(vec2(abs(p.x) - 0.46, p.y - (-0.195)));
                    float h2 = length(vec2(abs(p.x) - 0.23, p.y - (-0.205)));
                    if (h1 < 0.032) {
                        luma = (h1 < 0.022) ? 0.05 : 0.76;
                    }
                    if (h2 < 0.022) {
                        luma = (h2 < 0.014) ? 0.05 : 0.68;
                    }
                }

                // ---- Central Transparent Cassette Window (Cutout showing spinning wheels) ----
                float d_clear = sd_rbox(p - vec2(0.0, 0.24), vec2(0.83, 0.275), 0.125);
                if (d_clear < 0.0) {
                    // Dimly backlit deck well behind clear window (pulses subtly with bass)
                    luma = 0.06 + 0.06 * u_bands.y;

                    // Area-conserving supply (Left) & take-up (Right) magnetic tape radii
                    float prog = clamp(u_transport.x, 0.0, 1.0);
                    float r_l = sqrt(mix(0.430 * 0.430, 0.212 * 0.212, prog));
                    float r_r = sqrt(mix(0.212 * 0.212, 0.430 * 0.430, prog));

                    vec2 c_l = vec2(-0.48, 0.24);
                    vec2 c_r = vec2(0.48, 0.24);

                    // Tensioned magnetic tape ribbon peeling off outer edges toward guide rollers
                    float vib = wave_mono(clamp(p.x * 0.5 + 0.5, 0.0, 1.0)) * 0.006 * u_bands.y;
                    float d_tl = sd_seg(p, c_l + vec2(-r_l + 0.004, 0.02), vec2(-0.78 + vib, -0.04));
                    float d_tr = sd_seg(p, c_r + vec2(r_r - 0.004, 0.02), vec2(0.78 + vib, -0.04));
                    float tape_line = smoothstep(0.012, 0.003, min(d_tl, d_tr)) * 0.52;
                    luma = max(luma, tape_line);

                    // Spin Left & Right reels
                    if (p.x < 0.0) {
                        vec2 rl = render_reel(p, c_l, r_l, u_transport.y);
                        luma = mix(luma, rl.x, rl.y);
                    } else {
                        vec2 rr = render_reel(p, c_r, r_r, u_transport.z);
                        luma = mix(luma, rr.x, rr.y);
                    }

                    // Center remaining-tape graduation scale between the two hubs
                    vec2 g_pos = p - vec2(0.0, 0.24);
                    if (abs(g_pos.x) < 0.165 && abs(g_pos.y) < 0.115) {
                        float g_box = smoothstep(0.009, 0.002,
                            abs(sd_rbox(g_pos, vec2(0.155, 0.100), 0.015))) * 0.46;
                        luma = max(luma, g_box);
                        if (abs(g_pos.x) <= 0.125) {
                            float base_ln = smoothstep(0.007, 0.002, abs(g_pos.y)) * 0.68;
                            float t_idx = floor((g_pos.x + 0.12) / 0.06 + 0.5);
                            float t_x = -0.12 + clamp(t_idx, 0.0, 4.0) * 0.06;
                            float is_mid = (abs(t_idx - 2.0) < 0.4 || abs(t_idx) < 0.4 || abs(t_idx - 4.0) < 0.4)
                                         ? 1.0 : 0.0;
                            float t_h = mix(0.024, 0.042, is_mid);
                            float tick = smoothstep(0.008, 0.002, abs(g_pos.x - t_x))
                                       * step(abs(g_pos.y), t_h) * 0.85;
                            luma = max(luma, max(base_ln, tick));
                        }
                    }

                    // Beveled clear window rim
                    float clear_rim = smoothstep(-0.015, 0.0, d_clear) * 0.82;
                    luma = max(luma, clear_rim);
                }
            }

            // Subtle diagonal deck-door glass reflection sheen + inner window shadow
            float glass_sheen = smoothstep(0.10, 0.0, abs((p.x * 0.42 + (p.y - 0.23)) - 0.16)) * 0.09;
            float door_shadow = smoothstep(-0.035, 0.0, d_door) * 0.18;
            luma = clamp(luma + glass_sheen - door_shadow * 0.35, 0.0, 1.0);
        }
    }

    // ---- Bottom Recessed 28-Segment Stereo LED Peak-Level Indicator ---------
    vec2 br_c = vec2(0.0, -0.67);
    vec2 br_half = vec2(1.16, 0.165);
    float d_br = sd_rbox(p - br_c, br_half, 0.04);
    if (d_br < 0.028) {
        if (d_br > 0.0) {
            luma = max(luma, smoothstep(0.028, 0.004, d_br) * 0.88);
        } else {
            luma = 0.04;

            // L & R channel labels on the left of the LED bridge
            float lbl_l = draw_chan_mini(p - vec2(-1.065, -0.612), 0.0) * 0.90;
            float lbl_r = draw_chan_mini(p - vec2(-1.065, -0.728), 1.0) * 0.90;
            luma = max(luma, max(lbl_l, lbl_r));

            float bx = (p.x - (-0.98)) / 2.06; // 0.0 .. 1.0 across 28 LED segments
            if (bx >= 0.0 && bx <= 1.0) {
                float segs = 28.0;
                float s_idx = floor(bx * segs);
                float s_f = fract(bx * segs);
                float seg_x = smoothstep(0.10, 0.22, s_f) * smoothstep(0.90, 0.78, s_f);
                float seg_u = (s_idx + 0.5) / segs;

                float dy_l = abs(p.y - (-0.612));
                float dy_r = abs(p.y - (-0.728));
                float row_l = smoothstep(0.036, 0.024, dy_l);
                float row_r = smoothstep(0.036, 0.024, dy_r);

                float bright = (seg_u >= 0.72) ? 1.0 : 0.74;
                float unlit = 0.15;

                // Instantaneous stereo VU bar + single-segment Peak-Hold dot
                float pk_idx_l = floor(clamp(u_vu.z, 0.0, 0.999) * segs);
                float pk_idx_r = floor(clamp(u_vu.w, 0.0, 0.999) * segs);
                float is_pk_l = (u_vu.z > 0.04 && abs(s_idx - pk_idx_l) < 0.4) ? 1.0 : 0.0;
                float is_pk_r = (u_vu.w > 0.04 && abs(s_idx - pk_idx_r) < 0.4) ? 1.0 : 0.0;

                float lit_l = max(step(seg_u, u_vu.x), is_pk_l);
                float lit_r = max(step(seg_u, u_vu.y), is_pk_r);
                float top_l = max(bright, is_pk_l);
                float top_r = max(bright, is_pk_r);

                luma += seg_x * row_l * mix(unlit, top_l, lit_l);
                luma += seg_x * row_r * mix(unlit, top_r, lit_r);

                // Subtle dB graduation dots between L and R LED rows
                if (abs(p.y - (-0.67)) < 0.007 &&
                    (mod(s_idx, 4.0) < 0.4 || abs(s_idx - 20.0) < 0.4)) {
                    luma += seg_x * 0.45;
                }
            }
        }
    }

    // Crisp 1x spokes, with realistic motion-blur trail during fast-forward / rewind
    if (abs(u_transport.w) > 2.0) {
        float prev_m = prev_scene(v_uv).r;
        luma = mix(luma, max(luma, prev_m), 0.25);
    }

    gl_FragColor = vec4(vec3(clamp(luma, 0.0, 1.0)), 1.0);
}
"#;

// Pass 2: Fullscreen 4-Shade Ordered Bayer LCD Quantizer
const PRESENT_FS: &str = r#"
#ifdef GL_ES
#ifdef GL_FRAGMENT_PRECISION_HIGH
precision highp float;
#else
precision mediump float;
#endif
#endif
varying vec2 v_uv;
uniform sampler2D u_scene;
uniform vec2 u_grid_size;
uniform float u_fade;
uniform vec3 u_pal0;
uniform vec3 u_pal1;
uniform vec3 u_pal2;
uniform vec3 u_pal3;

float bayer2(vec2 p) {
    return 2.0 * abs(p.x - p.y) + p.y;
}

float bayer4_norm(vec2 cell) {
    vec2 c0 = floor(mod(cell, 2.0));
    vec2 c1 = floor(mod(cell * 0.5, 2.0));
    float b = 4.0 * bayer2(c0) + bayer2(c1);
    return (b + 0.5) / 16.0 - 0.5;
}

void main() {
    vec2 cell = floor(v_uv * u_grid_size);
    vec2 center_uv = (cell + 0.5) / u_grid_size;
    float raw_luma = texture2D(u_scene, center_uv).r;
    float val = clamp(raw_luma * u_fade, 0.0, 1.0);

    float dither = bayer4_norm(cell);
    float q = floor(clamp(val * 3.0 + dither * 0.92 + 0.5, 0.0, 3.0));

    vec3 col = u_pal0;
    if (q > 2.5) {
        col = u_pal3;
    } else if (q > 1.5) {
        col = u_pal2;
    } else if (q > 0.5) {
        col = u_pal1;
    }

    gl_FragColor = vec4(col, 1.0);
}
"#;

const PRESET_BODIES: [&str; 10] = [
    FS_LCD_PLASMA,
    FS_SONIC_VORTEX,
    FS_PHOSPHOR_SCOPE,
    FS_SPECTRUM_MATRIX,
    FS_CYBER_HORIZON,
    FS_LIQUID_METAL,
    FS_CHLADNI_PLATE,
    FS_STAR_WARP,
    FS_VU_METER,
    FS_CASSETTE,
];

impl VizEngine {
    /// Returns the GLSL fragment shader header (uniforms & helper functions) shared by all presets.
    pub fn scene_fs_header() -> &'static str {
        SCENE_FS_HEADER
    }

    /// Returns the GLSL fragment shader body for preset `idx`.
    pub fn preset_shader_source(idx: usize) -> &'static str {
        PRESET_BODIES[idx % PRESET_BODIES.len()]
    }

    /// Initializes all GLSL shaders, Ping-Pong FBOs (`256x192` + `512x384`), and DSP tables.
    ///
    /// # Safety
    /// A valid OpenGL ES 2.0/3.0 context must be current on the calling thread.
    #[cfg(not(target_arch = "wasm32"))]
    pub unsafe fn new<F>(mut get_proc_address: F) -> Result<Self, String>
    where
        F: FnMut(&std::ffi::CStr) -> *const c_void,
    {
        let gl = glow::Context::from_loader_function_cstr(|s| get_proc_address(s));
        Self::init_from_gl(gl)
    }

    /// Initializes all GLSL shaders, Ping-Pong FBOs, and DSP tables on a WebGL2 context.
    ///
    /// # Safety
    /// `gl_ctx` must be a valid active WebGL2 rendering context.
    #[cfg(target_arch = "wasm32")]
    pub unsafe fn new_from_webgl2(gl_ctx: web_sys::WebGl2RenderingContext) -> Result<Self, String> {
        let gl = glow::Context::from_webgl2_context(gl_ctx);
        Self::init_from_gl(gl)
    }

    unsafe fn init_from_gl(gl: glow::Context) -> Result<Self, String> {

        let vao = gl.create_vertex_array().ok();
        if let Some(v) = vao {
            gl.bind_vertex_array(Some(v));
        }

        #[rustfmt::skip]
        let quad_verts: [f32; 16] = [
            -1.0, -1.0, 0.0, 0.0,
             1.0, -1.0, 1.0, 0.0,
            -1.0,  1.0, 0.0, 1.0,
             1.0,  1.0, 1.0, 1.0,
        ];
        let vbo = gl
            .create_buffer()
            .map_err(|e| format!("viz VBO create failed: {e}"))?;
        gl.bind_buffer(glow::ARRAY_BUFFER, Some(vbo));
        let vert_bytes = std::slice::from_raw_parts(
            quad_verts.as_ptr() as *const u8,
            std::mem::size_of_val(&quad_verts),
        );
        gl.buffer_data_u8_slice(glow::ARRAY_BUFFER, vert_bytes, glow::STATIC_DRAW);

        let dsp = VizDsp::new();
        let audio_tex = gl
            .create_texture()
            .map_err(|e| format!("viz audio texture failed: {e}"))?;
        gl.bind_texture(glow::TEXTURE_2D, Some(audio_tex));
        gl.tex_parameter_i32(glow::TEXTURE_2D, glow::TEXTURE_MIN_FILTER, glow::LINEAR as i32);
        gl.tex_parameter_i32(glow::TEXTURE_2D, glow::TEXTURE_MAG_FILTER, glow::LINEAR as i32);
        gl.tex_parameter_i32(glow::TEXTURE_2D, glow::TEXTURE_WRAP_S, glow::CLAMP_TO_EDGE as i32);
        gl.tex_parameter_i32(glow::TEXTURE_2D, glow::TEXTURE_WRAP_T, glow::CLAMP_TO_EDGE as i32);
        gl.tex_image_2d(
            glow::TEXTURE_2D,
            0,
            glow::RGBA as i32,
            AUDIO_TEX_W as i32,
            AUDIO_TEX_H as i32,
            0,
            glow::RGBA,
            glow::UNSIGNED_BYTE,
            Some(&dsp.audio_tex),
        );

        let fbo_coarse = Self::create_ping_pong(&gl, 256, 192)?;
        let fbo_fine = Self::create_ping_pong(&gl, 512, 384)?;

        let vs = Self::compile_shader(&gl, glow::VERTEX_SHADER, SHARED_VS, "shared_vs")?;

        let mut scenes = Vec::with_capacity(PRESET_BODIES.len());
        for (idx, body) in PRESET_BODIES.iter().enumerate() {
            let full_fs = format!("{SCENE_FS_HEADER}\n{body}");
            let label = VIZ_PRESETS.get(idx).map(|p| p.id).unwrap_or("scene");
            let fs = Self::compile_shader(&gl, glow::FRAGMENT_SHADER, &full_fs, label)?;
            let prog = Self::link_program(&gl, vs, fs, label)?;
            gl.delete_shader(fs);

            let u_prev = gl.get_uniform_location(prog, "u_prev");
            let u_audio = gl.get_uniform_location(prog, "u_audio");
            let u_res = gl.get_uniform_location(prog, "u_res");
            let u_time = gl.get_uniform_location(prog, "u_time");
            let u_bands = gl.get_uniform_location(prog, "u_bands");
            let u_att = gl.get_uniform_location(prog, "u_att");
            let u_beat = gl.get_uniform_location(prog, "u_beat");
            let u_vu = gl.get_uniform_location(prog, "u_vu");
            let u_transport = gl.get_uniform_location(prog, "u_transport");
            scenes.push(SceneProgram {
                prog,
                u_prev,
                u_audio,
                u_res,
                u_time,
                u_bands,
                u_att,
                u_beat,
                u_vu,
                u_transport,
            });
        }

        let present_fs = Self::compile_shader(&gl, glow::FRAGMENT_SHADER, PRESENT_FS, "present_fs")?;
        let present_prog = Self::link_program(&gl, vs, present_fs, "present_prog")?;
        gl.delete_shader(present_fs);
        gl.delete_shader(vs);

        let present = PresentProgram {
            prog: present_prog,
            u_scene: gl.get_uniform_location(present_prog, "u_scene"),
            u_grid_size: gl.get_uniform_location(present_prog, "u_grid_size"),
            u_fade: gl.get_uniform_location(present_prog, "u_fade"),
            u_pal0: gl.get_uniform_location(present_prog, "u_pal0"),
            u_pal1: gl.get_uniform_location(present_prog, "u_pal1"),
            u_pal2: gl.get_uniform_location(present_prog, "u_pal2"),
            u_pal3: gl.get_uniform_location(present_prog, "u_pal3"),
        };

        let engine = Self {
            gl,
            dsp,
            vao,
            vbo,
            audio_tex,
            fbo_coarse,
            fbo_fine,
            scenes,
            custom_scene: None,
            use_custom_scene: false,
            present,
            active_preset: 0,
            fine_grain: false,
            needs_clear: true,
            reel_ang_l: 0.0,
            reel_ang_r: 0.9,
        };
        engine.restore_gl_state();
        Ok(engine)
    }

    /// Compiles and activates a custom GLSL fragment shader body (appended to `SCENE_FS_HEADER`).
    ///
    /// # Safety
    /// Must be called on the thread owning the active OpenGL ES / WebGL2 context.
    pub unsafe fn compile_custom_shader(&mut self, body_glsl: &str) -> Result<(), String> {
        let vs = Self::compile_shader(&self.gl, glow::VERTEX_SHADER, SHARED_VS, "custom_vs")?;
        let full_fs = format!("{SCENE_FS_HEADER}\n{body_glsl}");
        let fs = match Self::compile_shader(&self.gl, glow::FRAGMENT_SHADER, &full_fs, "custom_fs")
        {
            Ok(s) => s,
            Err(e) => {
                self.gl.delete_shader(vs);
                return Err(e);
            }
        };
        let prog = match Self::link_program(&self.gl, vs, fs, "custom_prog") {
            Ok(p) => p,
            Err(e) => {
                self.gl.delete_shader(vs);
                self.gl.delete_shader(fs);
                return Err(e);
            }
        };
        self.gl.delete_shader(vs);
        self.gl.delete_shader(fs);

        if let Some(old) = self.custom_scene.take() {
            self.gl.delete_program(old.prog);
        }

        let sc = SceneProgram {
            prog,
            u_prev: self.gl.get_uniform_location(prog, "u_prev"),
            u_audio: self.gl.get_uniform_location(prog, "u_audio"),
            u_res: self.gl.get_uniform_location(prog, "u_res"),
            u_time: self.gl.get_uniform_location(prog, "u_time"),
            u_bands: self.gl.get_uniform_location(prog, "u_bands"),
            u_att: self.gl.get_uniform_location(prog, "u_att"),
            u_beat: self.gl.get_uniform_location(prog, "u_beat"),
            u_vu: self.gl.get_uniform_location(prog, "u_vu"),
            u_transport: self.gl.get_uniform_location(prog, "u_transport"),
        };
        self.custom_scene = Some(sc);
        self.use_custom_scene = true;
        self.needs_clear = true;
        self.restore_gl_state();
        Ok(())
    }

    pub fn set_use_custom_shader(&mut self, enabled: bool) {
        self.use_custom_scene = enabled && self.custom_scene.is_some();
        self.needs_clear = true;
    }

    pub fn set_preset(&mut self, idx: usize) {
        let clamped = idx % self.scenes.len().max(1);
        if clamped != self.active_preset {
            self.active_preset = clamped;
        }
        self.use_custom_scene = false;
        self.needs_clear = true;
        self.dsp.soft_reset_peaks();
    }

    pub fn preset(&self) -> usize {
        self.active_preset
    }

    pub fn is_fine_grain(&self) -> bool {
        self.fine_grain
    }

    pub fn set_fine_grain(&mut self, fine: bool) {
        if self.fine_grain != fine {
            self.fine_grain = fine;
            self.needs_clear = true;
        }
    }

    pub fn toggle_grain(&mut self) -> bool {
        self.fine_grain = !self.fine_grain;
        self.needs_clear = true;
        self.fine_grain
    }

    /// Updates audio analysis and renders one complete frame into the window's default framebuffer.
    ///
    /// # Safety
    /// Must be called on the thread currently owning the SDL GLES context.
    pub unsafe fn render_frame(
        &mut self,
        pcm_interleaved: &[i16],
        sample_rate: u32,
        dt: f32,
        win_w: i32,
        win_h: i32,
        theme: ThemeColors,
        fade: f32,
        track_progress: f32,
        transport_speed: f32,
    ) {
        let af: VizAudioFrame = self.dsp.update(pcm_interleaved, sample_rate, dt);

        // Constant linear tape speed (4.76 cm/s analog feel): nearly-empty reel spins faster
        let prog = track_progress.clamp(0.0, 1.0);
        let r_l = (0.430f32 * 0.430 * (1.0 - prog) + 0.212 * 0.212 * prog).sqrt();
        let r_r = (0.212f32 * 0.212 * (1.0 - prog) + 0.430 * 0.430 * prog).sqrt();
        let v_lin = 0.36f32 * transport_speed;
        self.reel_ang_l = (self.reel_ang_l + dt * (v_lin / r_l)).rem_euclid(std::f32::consts::TAU);
        self.reel_ang_r = (self.reel_ang_r + dt * (v_lin / r_r)).rem_euclid(std::f32::consts::TAU);

        if let Some(v) = self.vao {
            self.gl.bind_vertex_array(Some(v));
        }
        self.gl.disable(glow::DEPTH_TEST);
        self.gl.disable(glow::STENCIL_TEST);
        self.gl.disable(glow::SCISSOR_TEST);
        self.gl.disable(glow::CULL_FACE);
        self.gl.disable(glow::BLEND);
        self.gl.pixel_store_i32(glow::UNPACK_ALIGNMENT, 4);

        // Upload 256x2 RGBA8 audio waveform + log-spectrum texture (2 KB)
        self.gl.active_texture(glow::TEXTURE1);
        self.gl.bind_texture(glow::TEXTURE_2D, Some(self.audio_tex));
        self.gl.tex_sub_image_2d(
            glow::TEXTURE_2D,
            0,
            0,
            0,
            AUDIO_TEX_W as i32,
            AUDIO_TEX_H as i32,
            glow::RGBA,
            glow::UNSIGNED_BYTE,
            glow::PixelUnpackData::Slice(&self.dsp.audio_tex),
        );

        let pp = if self.fine_grain {
            &mut self.fbo_fine
        } else {
            &mut self.fbo_coarse
        };

        let reset_flag = if self.needs_clear {
            for &fbo in &pp.fbo {
                self.gl.bind_framebuffer(glow::FRAMEBUFFER, Some(fbo));
                self.gl.viewport(0, 0, pp.w, pp.h);
                self.gl.clear_color(0.0, 0.0, 0.0, 1.0);
                self.gl.clear(glow::COLOR_BUFFER_BIT);
            }
            self.needs_clear = false;
            1.0f32
        } else {
            0.0f32
        };

        let prev_idx = pp.front;
        let curr_idx = 1 - prev_idx;
        pp.front = curr_idx;

        let grid_w = pp.w;
        let grid_h = pp.h;
        let prev_tex = pp.tex[prev_idx];
        let curr_fbo = pp.fbo[curr_idx];
        let curr_tex = pp.tex[curr_idx];

        // Bind fullscreen quad attributes (pos.xy, uv.xy)
        self.gl.bind_buffer(glow::ARRAY_BUFFER, Some(self.vbo));
        let stride = (4 * std::mem::size_of::<f32>()) as i32;
        self.gl.enable_vertex_attrib_array(0);
        self.gl
            .vertex_attrib_pointer_f32(0, 2, glow::FLOAT, false, stride, 0);
        self.gl.enable_vertex_attrib_array(1);
        self.gl.vertex_attrib_pointer_f32(
            1,
            2,
            glow::FLOAT,
            false,
            stride,
            (2 * std::mem::size_of::<f32>()) as i32,
        );

        // Pass 1: Render audio-reactive scene into `curr_fbo` reading `prev_tex` + `audio_tex`
        let sc = if self.use_custom_scene {
            self.custom_scene
                .as_ref()
                .unwrap_or(&self.scenes[self.active_preset % self.scenes.len()])
        } else {
            &self.scenes[self.active_preset % self.scenes.len()]
        };
        self.gl.bind_framebuffer(glow::FRAMEBUFFER, Some(curr_fbo));
        self.gl.viewport(0, 0, grid_w, grid_h);
        self.gl.use_program(Some(sc.prog));

        self.gl.active_texture(glow::TEXTURE0);
        self.gl.bind_texture(glow::TEXTURE_2D, Some(prev_tex));
        self.gl.uniform_1_i32(sc.u_prev.as_ref(), 0);
        self.gl.uniform_1_i32(sc.u_audio.as_ref(), 1);
        self.gl
            .uniform_2_f32(sc.u_res.as_ref(), grid_w as f32, grid_h as f32);
        self.gl
            .uniform_2_f32(sc.u_time.as_ref(), af.time, af.music_time);
        self.gl.uniform_4_f32(
            sc.u_bands.as_ref(),
            af.sub_bass,
            af.bass,
            af.mid,
            af.treb,
        );
        self.gl.uniform_4_f32(
            sc.u_att.as_ref(),
            af.bass_att,
            af.mid_att,
            af.treb_att,
            af.stereo_width,
        );
        self.gl.uniform_4_f32(
            sc.u_beat.as_ref(),
            af.beat_pulse,
            af.beat_dir,
            af.beat_phase,
            reset_flag,
        );
        self.gl.uniform_4_f32(
            sc.u_vu.as_ref(),
            af.vu_l,
            af.vu_r,
            af.vu_peak_l,
            af.vu_peak_r,
        );
        self.gl.uniform_4_f32(
            sc.u_transport.as_ref(),
            prog,
            self.reel_ang_l,
            self.reel_ang_r,
            transport_speed,
        );
        self.gl.draw_arrays(glow::TRIANGLE_STRIP, 0, 4);

        // Pass 2: Fullscreen 4-Shade Ordered Bayer LCD Quantizer into window FBO 0
        let pal = order_palette_bg_to_fg(theme);
        self.gl.bind_framebuffer(glow::FRAMEBUFFER, None);
        self.gl.viewport(0, 0, win_w.max(1), win_h.max(1));
        self.gl
            .clear_color(pal[0][0], pal[0][1], pal[0][2], 1.0);
        self.gl.clear(glow::COLOR_BUFFER_BIT);

        self.gl.use_program(Some(self.present.prog));
        self.gl.active_texture(glow::TEXTURE0);
        self.gl.bind_texture(glow::TEXTURE_2D, Some(curr_tex));
        self.gl.uniform_1_i32(self.present.u_scene.as_ref(), 0);
        self.gl.uniform_2_f32(
            self.present.u_grid_size.as_ref(),
            grid_w as f32,
            grid_h as f32,
        );
        self.gl
            .uniform_1_f32(self.present.u_fade.as_ref(), fade.clamp(0.0, 1.0));
        self.gl
            .uniform_3_f32_slice(self.present.u_pal0.as_ref(), &pal[0]);
        self.gl
            .uniform_3_f32_slice(self.present.u_pal1.as_ref(), &pal[1]);
        self.gl
            .uniform_3_f32_slice(self.present.u_pal2.as_ref(), &pal[2]);
        self.gl
            .uniform_3_f32_slice(self.present.u_pal3.as_ref(), &pal[3]);
        self.gl.draw_arrays(glow::TRIANGLE_STRIP, 0, 4);

        self.gl.disable_vertex_attrib_array(0);
        self.gl.disable_vertex_attrib_array(1);
        self.restore_gl_state();
    }

    unsafe fn create_ping_pong(
        gl: &glow::Context,
        w: i32,
        h: i32,
    ) -> Result<PingPongPair, String> {
        let mut tex_arr = [None, None];
        let mut fbo_arr = [None, None];

        for i in 0..2 {
            let t = gl
                .create_texture()
                .map_err(|e| format!("viz FBO tex failed: {e}"))?;
            gl.bind_texture(glow::TEXTURE_2D, Some(t));
            gl.tex_image_2d(
                glow::TEXTURE_2D,
                0,
                glow::RGBA as i32,
                w,
                h,
                0,
                glow::RGBA,
                glow::UNSIGNED_BYTE,
                None,
            );
            // GL_LINEAR allows smooth Milkdrop UV warp advection in Pass 1;
            // Pass 2 samples exact `(floor(uv * grid) + 0.5) / grid` texel centers for crisp LCD cells!
            gl.tex_parameter_i32(glow::TEXTURE_2D, glow::TEXTURE_MIN_FILTER, glow::LINEAR as i32);
            gl.tex_parameter_i32(glow::TEXTURE_2D, glow::TEXTURE_MAG_FILTER, glow::LINEAR as i32);
            gl.tex_parameter_i32(glow::TEXTURE_2D, glow::TEXTURE_WRAP_S, glow::CLAMP_TO_EDGE as i32);
            gl.tex_parameter_i32(glow::TEXTURE_2D, glow::TEXTURE_WRAP_T, glow::CLAMP_TO_EDGE as i32);

            let f = gl
                .create_framebuffer()
                .map_err(|e| format!("viz FBO create failed: {e}"))?;
            gl.bind_framebuffer(glow::FRAMEBUFFER, Some(f));
            gl.framebuffer_texture_2d(
                glow::FRAMEBUFFER,
                glow::COLOR_ATTACHMENT0,
                glow::TEXTURE_2D,
                Some(t),
                0,
            );
            if gl.check_framebuffer_status(glow::FRAMEBUFFER) != glow::FRAMEBUFFER_COMPLETE {
                gl.bind_framebuffer(glow::FRAMEBUFFER, None);
                return Err(format!("viz PingPong FBO ({w}x{h}) incomplete"));
            }
            gl.clear_color(0.0, 0.0, 0.0, 1.0);
            gl.clear(glow::COLOR_BUFFER_BIT);

            tex_arr[i] = Some(t);
            fbo_arr[i] = Some(f);
        }
        gl.bind_framebuffer(glow::FRAMEBUFFER, None);

        Ok(PingPongPair {
            w,
            h,
            tex: [tex_arr[0].unwrap(), tex_arr[1].unwrap()],
            fbo: [fbo_arr[0].unwrap(), fbo_arr[1].unwrap()],
            front: 0,
        })
    }

    unsafe fn compile_shader(
        gl: &glow::Context,
        kind: u32,
        src: &str,
        label: &str,
    ) -> Result<glow::Shader, String> {
        let s = gl
            .create_shader(kind)
            .map_err(|e| format!("{label}: create_shader failed: {e}"))?;
        gl.shader_source(s, src);
        gl.compile_shader(s);
        if !gl.get_shader_compile_status(s) {
            let log = gl.get_shader_info_log(s);
            gl.delete_shader(s);
            return Err(format!("{label} compile failed: {log}"));
        }
        Ok(s)
    }

    unsafe fn link_program(
        gl: &glow::Context,
        vs: glow::Shader,
        fs: glow::Shader,
        label: &str,
    ) -> Result<glow::Program, String> {
        let prog = gl
            .create_program()
            .map_err(|e| format!("{label}: create_program failed: {e}"))?;
        gl.attach_shader(prog, vs);
        gl.attach_shader(prog, fs);
        gl.bind_attrib_location(prog, 0, "a_pos");
        gl.bind_attrib_location(prog, 1, "a_uv");
        gl.link_program(prog);
        if !gl.get_program_link_status(prog) {
            let log = gl.get_program_info_log(prog);
            gl.delete_program(prog);
            return Err(format!("{label} link failed: {log}"));
        }
        Ok(prog)
    }

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
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preset_registry_matches_shader_bodies() {
        assert_eq!(VIZ_PRESETS.len(), PRESET_BODIES.len());
        for p in VIZ_PRESETS {
            assert!(!p.id.is_empty());
            assert!(!p.name_en.is_empty());
            assert!(!p.name_uk.is_empty());
        }
    }

    #[test]
    fn palette_ordering_starts_at_bg_and_ends_at_fg_for_light_and_dark_themes() {
        // Light default theme: bg (#D6D8D5) -> fg1 (#A8A8A8) -> fg2 (#595959) -> fg (#3C3637)
        let light = ThemeColors::default();
        let lp = order_palette_bg_to_fg(light);
        assert_eq!(lp[0], unpack_rgb_f32_and_luma(light.bg_rgb).1);
        assert_eq!(lp[1], unpack_rgb_f32_and_luma(light.fg1_rgb).1);
        assert_eq!(lp[2], unpack_rgb_f32_and_luma(light.fg2_rgb).1);
        assert_eq!(lp[3], unpack_rgb_f32_and_luma(light.fg_rgb).1);

        // Classic Dark theme: bg (#1E1B1C) -> fg1 (#555253) -> fg2 (#AEAFAD) -> fg (#D6D8D5)
        let dark = ThemeColors::classic_dark();
        let dp = order_palette_bg_to_fg(dark);
        assert_eq!(dp[0], unpack_rgb_f32_and_luma(dark.bg_rgb).1);
        assert_eq!(dp[1], unpack_rgb_f32_and_luma(dark.fg1_rgb).1);
        assert_eq!(dp[2], unpack_rgb_f32_and_luma(dark.fg2_rgb).1);
        assert_eq!(dp[3], unpack_rgb_f32_and_luma(dark.fg_rgb).1);
    }
}
