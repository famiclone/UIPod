//! Pure-Rust image decoder (`JPEG`, `PNG`, `GIF`, `BMP`), proportional aspect-preserving
//! downscaler, and 4x4 Bayer ordered 4-shade iPod Mini theme ditherer.

use std::io::Cursor;

use png::{ColorType, Transformations};
use zune_jpeg::zune_core::bytestream::ZCursor;
use zune_jpeg::zune_core::colorspace::ColorSpace;
use zune_jpeg::zune_core::options::DecoderOptions;
use zune_jpeg::JpegDecoder;

const MAX_SOURCE_DIM: usize = 6144;

/// Decoded tightly-packed `RGB24` image (`width * height * 3`).
#[derive(Debug, Clone)]
pub struct DecodedRgbImage {
    pub width: u32,
    pub height: u32,
    pub rgb: Vec<u8>,
}

/// Decodes `JPEG`, `PNG`, `GIF` (`GIF87a`/`GIF89a`), or `BMP` (`1/4/8/24/32-bit`) into `RGB24`,
/// compositing alpha transparency cleanly onto `bg_rgb` (`0xRRGGBB`).
pub fn decode_illustration_rgb24(bytes: &[u8], bg_rgb: u32) -> Option<DecodedRgbImage> {
    if bytes.len() < 10 {
        return None;
    }
    let bg = [
        ((bg_rgb >> 16) & 0xFF) as u8,
        ((bg_rgb >> 8) & 0xFF) as u8,
        (bg_rgb & 0xFF) as u8,
    ];

    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        decode_png(bytes, bg)
    } else if bytes.starts_with(&[0xFF, 0xD8, 0xFF]) {
        decode_jpeg(bytes)
    } else if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        decode_gif(bytes, bg)
    } else if bytes.starts_with(b"BM") {
        decode_bmp(bytes, bg)
    } else {
        decode_jpeg(bytes)
            .or_else(|| decode_png(bytes, bg))
            .or_else(|| decode_gif(bytes, bg))
            .or_else(|| decode_bmp(bytes, bg))
    }
}

/// Fast header probe returning `(width, height)` without decoding pixel data so layout & pagination
/// stay instantaneous even on books with dozens of illustrations!
pub fn probe_image_dimensions(bytes: &[u8]) -> Option<(u32, u32)> {
    if bytes.len() < 10 {
        return None;
    }
    // PNG IHDR
    if bytes.len() >= 24 && bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        let w = u32::from_be_bytes(bytes[16..20].try_into().ok()?);
        let h = u32::from_be_bytes(bytes[20..24].try_into().ok()?);
        if w > 0 && h > 0 && (w as usize) <= MAX_SOURCE_DIM && (h as usize) <= MAX_SOURCE_DIM {
            return Some((w, h));
        }
    }
    // GIF Logical Screen Descriptor
    if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        let w = u16::from_le_bytes([bytes[6], bytes[7]]) as u32;
        let h = u16::from_le_bytes([bytes[8], bytes[9]]) as u32;
        if w > 0 && h > 0 {
            return Some((w, h));
        }
    }
    // BMP DIB header
    if bytes.len() >= 26 && bytes.starts_with(b"BM") {
        let w = i32::from_le_bytes(bytes[18..22].try_into().ok()?);
        let h = i32::from_le_bytes(bytes[22..26].try_into().ok()?).unsigned_abs();
        if w > 0 && h > 0 && (w as usize) <= MAX_SOURCE_DIM && (h as usize) <= MAX_SOURCE_DIM {
            return Some((w as u32, h));
        }
    }
    // JPEG SOF marker scan
    if bytes.starts_with(&[0xFF, 0xD8]) {
        let mut opts = DecoderOptions::default()
            .set_max_width(MAX_SOURCE_DIM)
            .set_max_height(MAX_SOURCE_DIM);
        let _ = &mut opts;
        let mut dec = JpegDecoder::new_with_options(ZCursor::new(bytes), opts);
        if dec.decode_headers().is_ok() {
            if let Some((w, h)) = dec.dimensions() {
                if w > 0 && h > 0 {
                    return Some((w as u32, h as u32));
                }
            }
        }
    }
    None
}

fn decode_jpeg(bytes: &[u8]) -> Option<DecodedRgbImage> {
    let opts = DecoderOptions::default()
        .jpeg_set_out_colorspace(ColorSpace::RGB)
        .set_max_width(MAX_SOURCE_DIM)
        .set_max_height(MAX_SOURCE_DIM);
    let mut dec = JpegDecoder::new_with_options(ZCursor::new(bytes), opts);
    let rgb = dec.decode().ok()?;
    let (w, h) = dec.dimensions()?;
    if w == 0 || h == 0 || rgb.len() < w.checked_mul(h)?.checked_mul(3)? {
        return None;
    }
    Some(DecodedRgbImage {
        width: w as u32,
        height: h as u32,
        rgb,
    })
}

fn decode_png(bytes: &[u8], bg: [u8; 3]) -> Option<DecodedRgbImage> {
    let mut decoder = png::Decoder::new(Cursor::new(bytes));
    decoder.set_transformations(Transformations::EXPAND | Transformations::STRIP_16);
    let mut reader = decoder.read_info().ok()?;
    let info = reader.info();
    let w = info.width as usize;
    let h = info.height as usize;
    if w == 0 || h == 0 || w > MAX_SOURCE_DIM || h > MAX_SOURCE_DIM {
        return None;
    }

    let mut raw = vec![0u8; reader.output_buffer_size()];
    let frame = reader.next_frame(&mut raw).ok()?;
    let data = &raw[..frame.buffer_size()];
    let npx = w.checked_mul(h)?;

    let rgb = match frame.color_type {
        ColorType::Rgb => {
            if data.len() < npx * 3 {
                return None;
            }
            raw.truncate(npx * 3);
            raw
        }
        ColorType::Rgba => {
            if data.len() < npx * 4 {
                return None;
            }
            let mut out = Vec::with_capacity(npx * 3);
            for px in data[..npx * 4].chunks_exact(4) {
                let a = px[3] as u32;
                let ia = 255 - a;
                out.push(((px[0] as u32 * a + bg[0] as u32 * ia + 127) / 255) as u8);
                out.push(((px[1] as u32 * a + bg[1] as u32 * ia + 127) / 255) as u8);
                out.push(((px[2] as u32 * a + bg[2] as u32 * ia + 127) / 255) as u8);
            }
            out
        }
        ColorType::Grayscale => {
            if data.len() < npx {
                return None;
            }
            let mut out = Vec::with_capacity(npx * 3);
            for &g in &data[..npx] {
                out.extend_from_slice(&[g, g, g]);
            }
            out
        }
        ColorType::GrayscaleAlpha => {
            if data.len() < npx * 2 {
                return None;
            }
            let mut out = Vec::with_capacity(npx * 3);
            for ga in data[..npx * 2].chunks_exact(2) {
                let a = ga[1] as u32;
                let ia = 255 - a;
                let g = ga[0] as u32;
                out.push(((g * a + bg[0] as u32 * ia + 127) / 255) as u8);
                out.push(((g * a + bg[1] as u32 * ia + 127) / 255) as u8);
                out.push(((g * a + bg[2] as u32 * ia + 127) / 255) as u8);
            }
            out
        }
        ColorType::Indexed => return None,
    };

    Some(DecodedRgbImage {
        width: w as u32,
        height: h as u32,
        rgb,
    })
}

fn decode_bmp(bytes: &[u8], bg: [u8; 3]) -> Option<DecodedRgbImage> {
    if bytes.len() < 54 {
        return None;
    }
    let data_off = u32::from_le_bytes(bytes[10..14].try_into().ok()?) as usize;
    let dib_size = u32::from_le_bytes(bytes[14..18].try_into().ok()?) as usize;
    if dib_size < 40 {
        return None;
    }
    let w_i32 = i32::from_le_bytes(bytes[18..22].try_into().ok()?);
    let h_i32 = i32::from_le_bytes(bytes[22..26].try_into().ok()?);
    let bpp = u16::from_le_bytes(bytes[28..30].try_into().ok()?);
    let comp = u32::from_le_bytes(bytes[30..34].try_into().ok()?);
    if w_i32 <= 0 || h_i32 == 0 || (comp != 0 && comp != 3) {
        return None;
    }
    let top_down = h_i32 < 0;
    let w = w_i32 as usize;
    let h = h_i32.unsigned_abs() as usize;
    if w > MAX_SOURCE_DIM || h > MAX_SOURCE_DIM {
        return None;
    }

    let mut out = vec![0u8; w.checked_mul(h)?.checked_mul(3)?];
    match bpp {
        24 | 32 => {
            let bytes_pp = (bpp / 8) as usize;
            let row_stride = (w.checked_mul(bytes_pp)? + 3) & !3;
            if bytes.len() < data_off.checked_add(row_stride.checked_mul(h)?)? {
                return None;
            }
            for dy in 0..h {
                let sy = if top_down { dy } else { h - 1 - dy };
                let srow = &bytes[data_off + sy * row_stride..];
                let drow = &mut out[dy * w * 3..(dy + 1) * w * 3];
                for x in 0..w {
                    let b = srow[x * bytes_pp];
                    let g = srow[x * bytes_pp + 1];
                    let r = srow[x * bytes_pp + 2];
                    if bytes_pp == 4 {
                        let a = srow[x * 4 + 3] as u32;
                        let ia = 255 - a;
                        drow[x * 3] = ((r as u32 * a + bg[0] as u32 * ia + 127) / 255) as u8;
                        drow[x * 3 + 1] = ((g as u32 * a + bg[1] as u32 * ia + 127) / 255) as u8;
                        drow[x * 3 + 2] = ((b as u32 * a + bg[2] as u32 * ia + 127) / 255) as u8;
                    } else {
                        drow[x * 3] = r;
                        drow[x * 3 + 1] = g;
                        drow[x * 3 + 2] = b;
                    }
                }
            }
        }
        1 | 4 | 8 => {
            let pal_off = 14 + dib_size;
            let max_colors = 1usize << bpp;
            let row_bits = w.checked_mul(bpp as usize)?;
            let row_stride = ((row_bits + 31) / 32) * 4;
            if bytes.len() < data_off.checked_add(row_stride.checked_mul(h)?)? {
                return None;
            }
            let mut palette = [[0u8; 3]; 256];
            let avail_pal = data_off.saturating_sub(pal_off) / 4;
            for (i, slot) in palette
                .iter_mut()
                .enumerate()
                .take(max_colors.min(avail_pal))
            {
                let p = pal_off + i * 4;
                *slot = [bytes[p + 2], bytes[p + 1], bytes[p]];
            }
            for dy in 0..h {
                let sy = if top_down { dy } else { h - 1 - dy };
                let srow = &bytes[data_off + sy * row_stride..];
                let drow = &mut out[dy * w * 3..(dy + 1) * w * 3];
                for x in 0..w {
                    let idx = match bpp {
                        8 => srow[x] as usize,
                        4 => {
                            let byte = srow[x / 2];
                            if x & 1 == 0 {
                                (byte >> 4) as usize
                            } else {
                                (byte & 0x0F) as usize
                            }
                        }
                        _ => {
                            let byte = srow[x / 8];
                            ((byte >> (7 - (x & 7))) & 1) as usize
                        }
                    };
                    let c = palette[idx];
                    drow[x * 3] = c[0];
                    drow[x * 3 + 1] = c[1];
                    drow[x * 3 + 2] = c[2];
                }
            }
        }
        _ => return None,
    }
    Some(DecodedRgbImage {
        width: w as u32,
        height: h as u32,
        rgb: out,
    })
}

/// Pure-Rust `GIF87a` / `GIF89a` first-frame decoder (handling global/local palettes, transparency,
/// interlaced pass ordering, and variable-width LZW decompression).
fn decode_gif(bytes: &[u8], bg: [u8; 3]) -> Option<DecodedRgbImage> {
    if bytes.len() < 13 {
        return None;
    }
    let screen_w = u16::from_le_bytes([bytes[6], bytes[7]]) as usize;
    let screen_h = u16::from_le_bytes([bytes[8], bytes[9]]) as usize;
    if screen_w == 0 || screen_h == 0 || screen_w > MAX_SOURCE_DIM || screen_h > MAX_SOURCE_DIM {
        return None;
    }
    let packed = bytes[10];
    let has_gct = (packed & 0x80) != 0;
    let gct_len = if has_gct {
        1usize << ((packed & 0x07) + 1)
    } else {
        0
    };

    let mut pos = 13usize;
    let mut global_pal = [[bg[0], bg[1], bg[2]]; 256];
    if has_gct {
        let need = gct_len * 3;
        if pos + need > bytes.len() {
            return None;
        }
        for (i, rgb) in bytes[pos..pos + need].chunks_exact(3).enumerate() {
            global_pal[i] = [rgb[0], rgb[1], rgb[2]];
        }
        pos += need;
    }

    let mut transparent_idx: Option<u8> = None;
    let mut canvas = vec![0u8; screen_w.checked_mul(screen_h)?.checked_mul(3)?];
    for px in canvas.chunks_exact_mut(3) {
        px.copy_from_slice(&bg);
    }

    while pos < bytes.len() {
        let block = bytes[pos];
        pos += 1;
        match block {
            0x3B => break, // Trailer
            0x21 => {
                // Extension block
                if pos >= bytes.len() {
                    break;
                }
                let ext_label = bytes[pos];
                pos += 1;
                if ext_label == 0xF9 && pos + 5 <= bytes.len() && bytes[pos] == 4 {
                    let gce_packed = bytes[pos + 1];
                    let trans = bytes[pos + 4];
                    if (gce_packed & 0x01) != 0 {
                        transparent_idx = Some(trans);
                    }
                }
                skip_gif_subblocks(bytes, &mut pos);
            }
            0x2C => {
                // Image Descriptor
                if pos + 9 > bytes.len() {
                    return None;
                }
                let left = u16::from_le_bytes([bytes[pos], bytes[pos + 1]]) as usize;
                let top = u16::from_le_bytes([bytes[pos + 2], bytes[pos + 3]]) as usize;
                let iw = u16::from_le_bytes([bytes[pos + 4], bytes[pos + 5]]) as usize;
                let ih = u16::from_le_bytes([bytes[pos + 6], bytes[pos + 7]]) as usize;
                let ipacked = bytes[pos + 8];
                pos += 9;
                if iw == 0 || ih == 0 || iw > MAX_SOURCE_DIM || ih > MAX_SOURCE_DIM {
                    return None;
                }

                let mut pal = global_pal;
                if (ipacked & 0x80) != 0 {
                    let lct_len = 1usize << ((ipacked & 0x07) + 1);
                    let need = lct_len * 3;
                    if pos + need > bytes.len() {
                        return None;
                    }
                    for (i, rgb) in bytes[pos..pos + need].chunks_exact(3).enumerate() {
                        pal[i] = [rgb[0], rgb[1], rgb[2]];
                    }
                    pos += need;
                }

                if pos >= bytes.len() {
                    return None;
                }
                let min_code_size = bytes[pos] as usize;
                pos += 1;
                if !(2..=8).contains(&min_code_size) {
                    return None;
                }

                let mut lzw_stream = Vec::new();
                while pos < bytes.len() {
                    let sub_len = bytes[pos] as usize;
                    pos += 1;
                    if sub_len == 0 {
                        break;
                    }
                    if pos + sub_len > bytes.len() {
                        break;
                    }
                    lzw_stream.extend_from_slice(&bytes[pos..pos + sub_len]);
                    pos += sub_len;
                }

                let indices = gif_lzw_decode(&lzw_stream, min_code_size, iw.checked_mul(ih)?)?;
                let interlaced = (ipacked & 0x40) != 0;
                let mut row_map = Vec::with_capacity(ih);
                if interlaced {
                    for &(start, step) in &[(0usize, 8usize), (4, 8), (2, 4), (1, 2)] {
                        let mut y = start;
                        while y < ih {
                            row_map.push(y);
                            y += step;
                        }
                    }
                } else {
                    row_map.extend(0..ih);
                }

                for (src_row_idx, &local_y) in row_map.iter().enumerate() {
                    let dy = top + local_y;
                    if dy >= screen_h {
                        continue;
                    }
                    let row_start = src_row_idx * iw;
                    if row_start >= indices.len() {
                        break;
                    }
                    let row_end = (row_start + iw).min(indices.len());
                    for (local_x, &ci) in indices[row_start..row_end].iter().enumerate() {
                        let dx = left + local_x;
                        if dx >= screen_w || Some(ci) == transparent_idx {
                            continue;
                        }
                        let off = (dy * screen_w + dx) * 3;
                        canvas[off..off + 3].copy_from_slice(&pal[ci as usize]);
                    }
                }
                return Some(DecodedRgbImage {
                    width: screen_w as u32,
                    height: screen_h as u32,
                    rgb: canvas,
                });
            }
            _ => break,
        }
    }
    None
}

fn skip_gif_subblocks(bytes: &[u8], pos: &mut usize) {
    while *pos < bytes.len() {
        let len = bytes[*pos] as usize;
        *pos += 1;
        if len == 0 {
            break;
        }
        *pos = (*pos + len).min(bytes.len());
    }
}

fn gif_lzw_decode(data: &[u8], min_code_size: usize, expected_pixels: usize) -> Option<Vec<u8>> {
    let clear_code = 1u16 << min_code_size;
    let end_code = clear_code + 1;

    let mut prefix = vec![0u16; 4096];
    let mut suffix = vec![0u8; 4096];
    let mut stack = vec![0u8; 4096];
    for i in 0..clear_code {
        suffix[i as usize] = i as u8;
    }

    let mut code_size = min_code_size + 1;
    let mut next_code = end_code + 1;
    let mut prev_code: Option<u16> = None;
    let mut first_byte = 0u8;

    let mut bit_buf = 0u32;
    let mut bit_cnt = 0usize;
    let mut byte_idx = 0usize;
    let mut out = Vec::with_capacity(expected_pixels.min(256 * 1024));

    while out.len() < expected_pixels {
        while bit_cnt < code_size && byte_idx < data.len() {
            bit_buf |= (data[byte_idx] as u32) << bit_cnt;
            bit_cnt += 8;
            byte_idx += 1;
        }
        if bit_cnt < code_size {
            break;
        }
        let mask = (1u32 << code_size) - 1;
        let code = (bit_buf & mask) as u16;
        bit_buf >>= code_size;
        bit_cnt -= code_size;

        if code == clear_code {
            code_size = min_code_size + 1;
            next_code = end_code + 1;
            prev_code = None;
            continue;
        }
        if code == end_code {
            break;
        }

        let mut sp = 0usize;
        let cur_code = if code < next_code {
            code
        } else if code == next_code {
            let p = prev_code?;
            stack[sp] = first_byte;
            sp += 1;
            p
        } else {
            break;
        };

        let mut c = cur_code;
        while c >= clear_code && sp < 4095 {
            stack[sp] = suffix[c as usize];
            sp += 1;
            c = prefix[c as usize];
        }
        first_byte = suffix[c as usize];
        stack[sp] = first_byte;
        sp += 1;

        while sp > 0 && out.len() < expected_pixels {
            sp -= 1;
            out.push(stack[sp]);
        }

        if let Some(p) = prev_code {
            if (next_code as usize) < 4096 {
                prefix[next_code as usize] = p;
                suffix[next_code as usize] = first_byte;
                next_code += 1;
                if next_code == (1u16 << code_size) && code_size < 12 {
                    code_size += 1;
                }
            }
        }
        prev_code = Some(code);
    }

    if out.is_empty() {
        None
    } else {
        Some(out)
    }
}

/// Computes proportional `(display_w, display_h)` fitting inside `(max_w, max_h)`.
/// Small inline diagrams/icons are upscaled gently up to at most `2.0x` so tiny equations stay legible
/// without blowing up decorative bullets to full-column width.
pub fn fit_illustration_box(src_w: u32, src_h: u32, max_w: f32, max_h: f32) -> (u32, u32) {
    let sw = (src_w.max(1)) as f32;
    let sh = (src_h.max(1)) as f32;
    let scale_fit = (max_w / sw).min(max_h / sh);
    let scale = scale_fit.min(2.0).max(0.01);
    let dw = (sw * scale).round().max(8.0) as u32;
    let dh = (sh * scale).round().max(8.0) as u32;
    (dw, dh)
}

/// Resamples `src` (`RGB24`) to `(dst_w, dst_h)` using box-averaging on downscale (eliminating
/// moire before dithering!) or bilinear interpolation on upscale, then applies UIPod's **4-shade
/// 4x4 Bayer ordered dithering** mapped to `(bg_rgb, fg1_rgb, fg2_rgb, fg_rgb)` sorted darkest -> lightest.
pub fn rasterize_4shade_rgba(
    src: &DecodedRgbImage,
    dst_w: u32,
    dst_h: u32,
    bg_rgb: u32,
    fg_rgb: u32,
    fg1_rgb: u32,
    fg2_rgb: u32,
) -> Vec<u8> {
    const BAYER4: [[u8; 4]; 4] = [
        [0, 8, 2, 10],
        [12, 4, 14, 6],
        [3, 11, 1, 9],
        [15, 7, 13, 5],
    ];

    let w = dst_w.max(1) as usize;
    let h = dst_h.max(1) as usize;
    let sw = src.width.max(1) as usize;
    let sh = src.height.max(1) as usize;

    // 1. Resample directly to grayscale luma `[0..=255]`
    let mut luma_buf = vec![0u8; w * h];
    for dy in 0..h {
        let y0f = (dy as f32) * (sh as f32) / (h as f32);
        let y1f = ((dy + 1) as f32) * (sh as f32) / (h as f32);
        let sy0 = (y0f.floor() as usize).min(sh - 1);
        let sy1 = (y1f.ceil() as usize).clamp(sy0 + 1, sh);

        for dx in 0..w {
            let x0f = (dx as f32) * (sw as f32) / (w as f32);
            let x1f = ((dx + 1) as f32) * (sw as f32) / (w as f32);
            let sx0 = (x0f.floor() as usize).min(sw - 1);
            let sx1 = (x1f.ceil() as usize).clamp(sx0 + 1, sw);

            let mut acc_l = 0u32;
            let mut cnt = 0u32;
            for sy in sy0..sy1 {
                let row_off = sy * sw * 3;
                for sx in sx0..sx1 {
                    let p = row_off + sx * 3;
                    let r = src.rgb[p] as u32;
                    let g = src.rgb[p + 1] as u32;
                    let b = src.rgb[p + 2] as u32;
                    acc_l += (54 * r + 183 * g + 19 * b + 128) >> 8;
                    cnt += 1;
                }
            }
            luma_buf[dy * w + dx] = (acc_l / cnt.max(1)) as u8;
        }
    }

    // 2. Sort theme palette darkest -> lightest (`Shade 0..3`)
    let mut shades = [
        rgb_to_luma_rgba(bg_rgb),
        rgb_to_luma_rgba(fg1_rgb),
        rgb_to_luma_rgba(fg2_rgb),
        rgb_to_luma_rgba(fg_rgb),
    ];
    shades.sort_by_key(|&(l, _)| l);
    let pal = [shades[0].1, shades[1].1, shades[2].1, shades[3].1];

    // 3. 2nd–98th percentile auto-levels (preserving crisp white/black line art if already high contrast)
    let npx = luma_buf.len();
    let mut hist = [0u32; 256];
    for &l in &luma_buf {
        hist[l as usize] += 1;
    }
    let clip = (npx / 50) as u32;
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
    if hi - lo < 56 {
        lo = 0;
        hi = 255;
    }
    let inv_span = 1.0 / ((hi - lo).max(1) as f32);

    // 4. 4x4 Bayer ordered dithering at exact destination screen texel coordinates
    let mut out_rgba = vec![0u8; npx * 4];
    for y in 0..h {
        let brow = &BAYER4[y & 3];
        for x in 0..w {
            let i = y * w + x;
            let raw_l = luma_buf[i] as i32;
            let norm = ((raw_l - lo) as f32 * inv_span).clamp(0.0, 1.0);
            let g_l = norm.powf(1.08);
            let tone = if g_l <= 0.09 {
                0
            } else if g_l >= 0.91 {
                3
            } else {
                let bval = (brow[x & 3] as f32 / 16.0) - 0.5 + (1.0 / 32.0);
                (g_l * 3.0 + bval * 0.48).round().clamp(0.0, 3.0) as usize
            };
            out_rgba[i * 4..i * 4 + 4].copy_from_slice(&pal[tone]);
        }
    }
    out_rgba
}

#[inline]
fn rgb_to_luma_rgba(rgb: u32) -> (u32, [u8; 4]) {
    let r = ((rgb >> 16) & 0xFF) as u8;
    let g = ((rgb >> 8) & 0xFF) as u8;
    let b = (rgb & 0xFF) as u8;
    let luma = 54u32 * (r as u32) + 183u32 * (g as u32) + 19u32 * (b as u32);
    (luma, [r, g, b, 255])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_gif_and_dithers_strictly_to_4_theme_shades() {
        // Minimal 2x2 GIF89a (red, white, black, blue)
        let gif: &[u8] = &[
            b'G', b'I', b'F', b'8', b'9', b'a', 2, 0, 2, 0, 0x81, 0, 0, // LSD (4-color GCT)
            0, 0, 0, 255, 0, 0, 0, 0, 255, 255, 255, 255, // GCT
            0x2C, 0, 0, 0, 0, 2, 0, 2, 0, 0, // Image descriptor
            2, // LZW min code size = 2 (clear=4, end=5)
            3, 0x84, 0x39, 0x05, // LZW codes: 4 (clear), 0, 1, 2, 3, 5 (end) -> 0b00000100_...
            0, 0x3B,
        ];
        let dec = decode_illustration_rgb24(gif, 0xD6D8D5).expect("GIF decode failed");
        assert_eq!((dec.width, dec.height), (2, 2));

        let rgba = rasterize_4shade_rgba(&dec, 8, 8, 0xD6D8D5, 0x3C3637, 0xA8A8A8, 0x595959);
        assert_eq!(rgba.len(), 8 * 8 * 4);
        let allowed: [[u8; 4]; 4] = [
            [0xD6, 0xD8, 0xD5, 255],
            [0x3C, 0x36, 0x37, 255],
            [0xA8, 0xA8, 0xA8, 255],
            [0x59, 0x59, 0x59, 255],
        ];
        for px in rgba.chunks_exact(4) {
            assert!(allowed.iter().any(|a| a == px));
        }
    }
}
