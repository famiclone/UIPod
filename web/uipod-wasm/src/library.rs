use uipod_ui::image_decode::decode_image_rgb24;

#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct Track {
    pub id: usize,
    pub title: String,
    pub artist: String,
    pub album: String,
    pub genre: String,
    pub codec: String,
    pub bitdepth: String,
    pub samplerate: String,
    pub duration_ms: u32,
    pub is_user_file: bool,
    pub art_rgb24: Option<Vec<u8>>, // 256 * 256 * 3
}

/// Resizes an arbitrary decoded RGB24 image into `256x256` RGB24 using center-crop + bilinear filter.
pub fn resize_rgb24_to_256(src_w: u32, src_h: u32, src: &[u8]) -> Vec<u8> {
    let mut out = vec![0u8; 256 * 256 * 3];
    if src_w == 0 || src_h == 0 || src.len() < (src_w as usize) * (src_h as usize) * 3 {
        return out;
    }
    let side = src_w.min(src_h) as f32;
    let ox = (src_w as f32 - side) * 0.5;
    let oy = (src_h as f32 - side) * 0.5;

    for y in 0..256usize {
        let fy = oy + ((y as f32 + 0.5) / 256.0) * side - 0.5;
        let y0 = (fy.floor() as i32).clamp(0, src_h as i32 - 1) as usize;
        let y1 = (y0 + 1).min(src_h as usize - 1);
        let ty = (fy - y0 as f32).clamp(0.0, 1.0);
        for x in 0..256usize {
            let fx = ox + ((x as f32 + 0.5) / 256.0) * side - 0.5;
            let x0 = (fx.floor() as i32).clamp(0, src_w as i32 - 1) as usize;
            let x1 = (x0 + 1).min(src_w as usize - 1);
            let tx = (fx - x0 as f32).clamp(0.0, 1.0);

            let i00 = (y0 * src_w as usize + x0) * 3;
            let i10 = (y0 * src_w as usize + x1) * 3;
            let i01 = (y1 * src_w as usize + x0) * 3;
            let i11 = (y1 * src_w as usize + x1) * 3;

            let dst = (y * 256 + x) * 3;
            for c in 0..3 {
                let top = src[i00 + c] as f32 * (1.0 - tx) + src[i10 + c] as f32 * tx;
                let bot = src[i01 + c] as f32 * (1.0 - tx) + src[i11 + c] as f32 * tx;
                out[dst + c] = (top * (1.0 - ty) + bot * ty).round().clamp(0.0, 255.0) as u8;
            }
        }
    }
    out
}

pub fn decode_cover_256(bytes: &[u8]) -> Option<Vec<u8>> {
    let (w, h, rgb) = decode_image_rgb24(bytes)?;
    Some(resize_rgb24_to_256(w, h, &rgb))
}

/// Generates crisp, high-contrast procedural 256x256 RGB24 cover artwork tailored to look awesome
/// both in full 24-bit color AND through UIPod's 4-shade Bayer dithering pipeline!
fn generate_procedural_cover(style: usize) -> Vec<u8> {
    let mut rgb = vec![0u8; 256 * 256 * 3];
    for y in 0..256usize {
        let ny = (y as f32 / 255.0) * 2.0 - 1.0;
        for x in 0..256usize {
            let nx = (x as f32 / 255.0) * 2.0 - 1.0;
            let r = (nx * nx + ny * ny).sqrt();
            let ang = ny.atan2(nx);

            let (cr, cg, cb) = match style {
                0 => {
                    // Midnight Tape Loop: warm cassette / concentric vinyl rings + glowing horizon
                    let sun = (1.0 - ((nx * nx + (ny + 0.18).powi(2)).sqrt() / 0.55)).clamp(0.0, 1.0);
                    let stripe = if ny > -0.15 && ((y / 7) % 2 == 0) { 0.35 } else { 1.0 };
                    let ring = (((r * 16.0).sin() * 0.5 + 0.5).powf(2.5)) * 0.25;
                    let sky = (0.14 + 0.55 * (1.0 - y as f32 / 256.0)).clamp(0.0, 1.0);
                    let v = (sky + sun * 0.85 * stripe + ring).clamp(0.0, 1.0);
                    (
                        (v * 245.0).clamp(0.0, 255.0),
                        ((v * v) * 165.0 + 25.0).clamp(0.0, 255.0),
                        (65.0 + (1.0 - v) * 95.0).clamp(0.0, 255.0),
                    )
                }
                1 => {
                    // Phosphor Horizon: synthwave perspective grid + radiant cyan/magenta sphere
                    let sphere = ((0.52 - r) * 8.0).clamp(0.0, 1.0);
                    let wave = ((nx * 9.0 + (ny * 5.0).sin() * 1.4).sin() * 0.5 + 0.5) * 0.45;
                    let grid = if ny > 0.15 {
                        let z = 1.0 / (ny - 0.08);
                        let gx = ((nx * z * 2.2).fract().abs() < 0.09) as u8 as f32;
                        let gz = ((z * 1.6).fract().abs() < 0.12) as u8 as f32;
                        (gx + gz).min(1.0)
                    } else {
                        0.0
                    };
                    let l = (0.12 + sphere * 0.78 + grid * 0.65 + wave * (1.0 - sphere) * 0.35).clamp(0.0, 1.0);
                    (
                        (45.0 + l * 205.0).clamp(0.0, 255.0),
                        (30.0 + sphere * 195.0 + grid * 150.0).clamp(0.0, 255.0),
                        (95.0 + l * 155.0).clamp(0.0, 255.0),
                    )
                }
                2 => {
                    // 4-Shade Rain: minimalist Swiss geometric Bauhaus monolith + wave arcs
                    let d_circle = ((nx - 0.12).powi(2) + (ny + 0.12).powi(2)).sqrt();
                    let circle = (d_circle < 0.54) as u8 as f32;
                    let bar = ((nx > -0.68 && nx < -0.22 && ny > -0.65 && ny < 0.72) as u8) as f32;
                    let ripples = ((d_circle * 24.0).sin() * 0.5 + 0.5) * 0.35;
                    let diag = if (nx + ny) > 0.35 { 0.25 } else { 0.88 };
                    let shade = (diag * (1.0 - circle * 0.72) - bar * 0.55 + ripples * circle).clamp(0.06, 0.96);
                    (
                        shade * 222.0,
                        shade * 234.0,
                        shade * 228.0,
                    )
                }
                3 => {
                    // Pixel Velocity: bold op-art spiral / radial starburst
                    let rays = ((ang * 8.0 + r * 6.0).sin() * 0.5 + 0.5).powf(1.5);
                    let rings = (r * 14.0).cos() * 0.5 + 0.5;
                    let core = (r < 0.24) as u8 as f32;
                    let v = (rays * 0.62 + rings * 0.38 + core * 0.5).clamp(0.0, 1.0);
                    (
                        (25.0 + v * 230.0).clamp(0.0, 255.0),
                        (180.0 * rings + 45.0 * v).clamp(0.0, 255.0),
                        (40.0 + (1.0 - rays) * 185.0).clamp(0.0, 255.0),
                    )
                }
                _ => {
                    // Velvet Cassette: retro dual-spool tape silhouette + warm amber glow
                    let spool_l = ((nx + 0.42).powi(2) + ny.powi(2)).sqrt();
                    let spool_r = ((nx - 0.42).powi(2) + ny.powi(2)).sqrt();
                    let hub = ((spool_l < 0.14 || spool_r < 0.14) as u8) as f32;
                    let tape_l = ((spool_l > 0.16 && spool_l < 0.36) as u8) as f32;
                    let tape_r = ((spool_r > 0.16 && spool_r < 0.29) as u8) as f32;
                    let shell = ((nx.abs() < 0.84 && ny.abs() < 0.56) as u8) as f32;
                    let window = ((nx.abs() < 0.66 && ny.abs() < 0.24) as u8) as f32;
                    let mut l = 0.86 - r * 0.28;
                    if shell > 0.5 {
                        l = 0.22 + (0.56 - ny) * 0.14;
                    }
                    if window > 0.5 {
                        l = 0.78;
                    }
                    if tape_l > 0.5 || tape_r > 0.5 {
                        l = 0.18 + ((spool_l.min(spool_r) * 42.0).sin() * 0.08);
                    }
                    if hub > 0.5 {
                        l = 0.94;
                    }
                    let l = l.clamp(0.0, 1.0);
                    (
                        (l * 242.0).clamp(0.0, 255.0),
                        (l * 198.0 + 14.0).clamp(0.0, 255.0),
                        (l * 124.0 + 18.0).clamp(0.0, 255.0),
                    )
                }
            };

            let i = (y * 256 + x) * 3;
            rgb[i] = cr as u8;
            rgb[i + 1] = cg as u8;
            rgb[i + 2] = cb as u8;
        }
    }
    rgb
}

pub fn built_in_demo_tracks() -> Vec<Track> {
    vec![
        Track {
            id: 0,
            title: "AquaBIOS 2000".to_string(),
            artist: "Cyberia 99".to_string(),
            album: "Y2K System Disc".to_string(),
            genre: "Y2K Ambient".to_string(),
            codec: "MP3".to_string(),
            bitdepth: "".to_string(),
            samplerate: "44.1kHz".to_string(),
            duration_ms: 23_220,
            is_user_file: false,
            art_rgb24: Some(generate_procedural_cover(1)),
        },
        Track {
            id: 1,
            title: "Chrome Breakbeat (Y2K Mix)".to_string(),
            artist: "NetRunner 2000".to_string(),
            album: "Liquid Millennium".to_string(),
            genre: "Atmospheric DnB".to_string(),
            codec: "MP3".to_string(),
            bitdepth: "".to_string(),
            samplerate: "44.1kHz".to_string(),
            duration_ms: 23_325,
            is_user_file: false,
            art_rgb24: Some(generate_procedural_cover(3)),
        },
        Track {
            id: 2,
            title: "No Other Possibility (2000)".to_string(),
            artist: "Vector 99".to_string(),
            album: "Hypertext OS".to_string(),
            genre: "Y2K Trance".to_string(),
            codec: "MP3".to_string(),
            bitdepth: "".to_string(),
            samplerate: "44.1kHz".to_string(),
            duration_ms: 52_723,
            is_user_file: false,
            art_rgb24: Some(generate_procedural_cover(2)),
        },
        Track {
            id: 3,
            title: "distortedspaceship".to_string(),
            artist: "subplaid".to_string(),
            album: "Deep Space (2010)".to_string(),
            genre: "Jungle".to_string(),
            codec: "MP3".to_string(),
            bitdepth: "".to_string(),
            samplerate: "44.1kHz".to_string(),
            duration_ms: 65_907,
            is_user_file: false,
            art_rgb24: Some(generate_procedural_cover(0)),
        },
    ]
}
