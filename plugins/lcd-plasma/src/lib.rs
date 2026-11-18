//! 60 FPS direct-framebuffer 4-shade Bayer-dithered `LCD Plasma` demoscene plugin
//! (inspired by Rockbox's classic `plasma` demo, quantized to UIPod's 4 theme shades).

use std::path::Path;

use uipod_plugin_sdk::{
    btn, uipod_declare_plugin, HostContext, PluginCategory, PluginKind, SCREEN_HEIGHT, SCREEN_WIDTH,
};

const BAYER4: [[i32; 4]; 4] = [
    [0, 8, 2, 10],
    [12, 4, 14, 6],
    [3, 11, 1, 9],
    [15, 7, 13, 5],
];

#[inline]
fn unpack_bgr_with_luma(rgb: u32) -> (u32, [u8; 3]) {
    let r = ((rgb >> 16) & 0xFF) as u8;
    let g = ((rgb >> 8) & 0xFF) as u8;
    let b = (rgb & 0xFF) as u8;
    let luma = 54 * (r as u32) + 183 * (g as u32) + 19 * (b as u32);
    (luma, [b, g, r])
}

fn run_lcd_plasma(ctx: &mut HostContext, _file: Option<&Path>) -> i32 {
    // Precompute fast 256-entry integer sine table (-127..=127)
    let mut sin_lut = [0i32; 256];
    for (i, slot) in sin_lut.iter_mut().enumerate() {
        let rad = (i as f64) * std::f64::consts::TAU / 256.0;
        *slot = (rad.sin() * 127.0).round() as i32;
    }

    let mut phase: usize = 0;
    let mut preset: usize = 0;
    let mut paused = false;

    loop {
        let t0 = ctx.ticks_ms();
        let inp = ctx.poll_input(0);
        if inp.should_exit || inp.pressed(btn::B) {
            return 0;
        }
        if inp.pressed(btn::A | btn::START) {
            paused = !paused;
        }
        if inp.pressed(btn::LEFT) {
            preset = (preset + 2) % 3;
        } else if inp.pressed(btn::RIGHT) {
            preset = (preset + 1) % 3;
        }

        let theme = ctx.theme();
        let mut shades = [
            unpack_bgr_with_luma(theme.bg),
            unpack_bgr_with_luma(theme.fg1),
            unpack_bgr_with_luma(theme.fg2),
            unpack_bgr_with_luma(theme.fg),
        ];
        shades.sort_by_key(|&(l, _)| l);
        let pal = [shades[0].1, shades[1].1, shades[2].1, shades[3].1];

        if let Some(fb) = ctx.framebuffer_mut() {
            // Render at 4x4 crisp LCD macro-pixels (256x192 grid -> 1024x768)
            let gw = SCREEN_WIDTH / 4; // 256
            let gh = SCREEN_HEIGHT / 4; // 192
            let p1 = phase & 255;
            let p2 = (phase * 2) & 255;
            let p3 = (phase * 3) & 255;

            for gy in 0..gh {
                let b_row = &BAYER4[gy & 3];
                let sy1 = sin_lut[(gy * 2 + p1) & 255];
                let sy2 = sin_lut[(gy * 3 + p2) & 255];
                let py0 = gy * 4;

                for gx in 0..gw {
                    let v = match preset {
                        0 => {
                            let sx1 = sin_lut[(gx * 2 + p2) & 255];
                            let sxy = sin_lut[((gx + gy) * 2 + p3) & 255];
                            sx1 + sy1 + sy2 + sxy
                        }
                        1 => {
                            let dx = (gx as i32) - 128;
                            let dy = (gy as i32) - 96;
                            let dist = (((dx * dx + dy * dy) as f32).sqrt() * 4.0) as usize;
                            sin_lut[(dist.wrapping_sub(p3)) & 255] * 2
                                + sin_lut[(gx * 3 + p1) & 255]
                                + sy1
                        }
                        _ => {
                            sin_lut[(gx * 4 + p1) & 255]
                                + sin_lut[(gy * 4 + p2) & 255]
                                + sin_lut[((gx ^ gy) * 2 + p3) & 255] * 2
                        }
                    };

                    // Normalize roughly -508..+508 -> 0..768, apply 4x4 Bayer dither -> 0..=3
                    let norm = (v + 512).clamp(0, 1024);
                    let dither = b_row[gx & 3] * 12 - 90;
                    let idx = ((norm * 3 + dither) / 1024).clamp(0, 3) as usize;
                    let bgr = pal[idx];
                    let bgr4 = [
                        bgr[0], bgr[1], bgr[2], bgr[0], bgr[1], bgr[2], bgr[0], bgr[1], bgr[2],
                        bgr[0], bgr[1], bgr[2],
                    ];

                    let x_byte = gx * 12;
                    for dy in 0..4 {
                        let row_off = (py0 + dy) * SCREEN_WIDTH * 3 + x_byte;
                        fb[row_off..row_off + 12].copy_from_slice(&bgr4);
                    }
                }
            }
            ctx.present_framebuffer();
        }

        if !paused {
            phase = phase.wrapping_add(2);
        }

        let el = ctx.ticks_ms().saturating_sub(t0);
        if el < 16 {
            ctx.sleep_ms((16 - el) as u32);
        } else {
            ctx.sleep_ms(0);
        }
    }
}

uipod_declare_plugin! {
    id: "lcd_plasma",
    name: "LCD Plasma",
    name_uk: "LCD Плазма",
    author: "UIPod",
    version: "0.1.0",
    category: PluginCategory::Demo,
    kind: PluginKind::Utility,
    extensions: "",
    entry: run_lcd_plasma,
}
