//! Memory-safe pure-Rust album-art decoder (`JPEG` via NEON-accelerated `zune-jpeg`,
//! `PNG` via `png`, and uncompressed `24/32-bit BMP`), replacing `stb_image` from `libprojectM`.

use std::io::Cursor;

use png::{ColorType, Transformations};
use zune_jpeg::zune_core::bytestream::ZCursor;
use zune_jpeg::zune_core::colorspace::ColorSpace;
use zune_jpeg::zune_core::options::DecoderOptions;
use zune_jpeg::JpegDecoder;

const MAX_DIM: usize = 8192;

/// Decodes in-memory `JPEG`, `PNG`, or `BMP` bytes into tightly-packed `RGB24` (`width * height * 3`).
pub fn decode_image_rgb24(bytes: &[u8]) -> Option<(u32, u32, Vec<u8>)> {
    if bytes.len() < 16 {
        return None;
    }

    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        decode_png_rgb24(bytes)
    } else if bytes.starts_with(&[0xFF, 0xD8, 0xFF]) {
        decode_jpeg_rgb24(bytes)
    } else if bytes.starts_with(b"BM") {
        decode_bmp_rgb24(bytes)
    } else {
        // Fallback: try JPEG first, then PNG (covers slightly non-standard headers)
        decode_jpeg_rgb24(bytes).or_else(|| decode_png_rgb24(bytes))
    }
}

fn decode_jpeg_rgb24(bytes: &[u8]) -> Option<(u32, u32, Vec<u8>)> {
    let opts = DecoderOptions::default()
        .jpeg_set_out_colorspace(ColorSpace::RGB)
        .set_max_width(MAX_DIM)
        .set_max_height(MAX_DIM);
    let mut dec = JpegDecoder::new_with_options(ZCursor::new(bytes), opts);
    let pixels = dec.decode().ok()?;
    let (w, h) = dec.dimensions()?;
    if w == 0 || h == 0 || pixels.len() < w.checked_mul(h)?.checked_mul(3)? {
        return None;
    }
    Some((w as u32, h as u32, pixels))
}

fn decode_png_rgb24(bytes: &[u8]) -> Option<(u32, u32, Vec<u8>)> {
    let mut decoder = png::Decoder::new(Cursor::new(bytes));
    decoder.set_transformations(Transformations::EXPAND | Transformations::STRIP_16);
    let mut reader = decoder.read_info().ok()?;
    let info = reader.info();
    let w = info.width as usize;
    let h = info.height as usize;
    if w == 0 || h == 0 || w > MAX_DIM || h > MAX_DIM {
        return None;
    }

    let buf_size = reader.output_buffer_size();
    let mut raw = vec![0u8; buf_size];
    let frame = reader.next_frame(&mut raw).ok()?;
    let data = &raw[..frame.buffer_size()];
    let npx = w.checked_mul(h)?;

    match frame.color_type {
        ColorType::Rgb => {
            if data.len() < npx * 3 {
                return None;
            }
            raw.truncate(npx * 3);
            Some((w as u32, h as u32, raw))
        }
        ColorType::Rgba => {
            if data.len() < npx * 4 {
                return None;
            }
            let mut rgb = Vec::with_capacity(npx * 3);
            for px in data[..npx * 4].chunks_exact(4) {
                let a = px[3] as u32;
                rgb.push(((px[0] as u32 * a + 127) / 255) as u8);
                rgb.push(((px[1] as u32 * a + 127) / 255) as u8);
                rgb.push(((px[2] as u32 * a + 127) / 255) as u8);
            }
            Some((w as u32, h as u32, rgb))
        }
        ColorType::Grayscale => {
            if data.len() < npx {
                return None;
            }
            let mut rgb = Vec::with_capacity(npx * 3);
            for &g in &data[..npx] {
                rgb.extend_from_slice(&[g, g, g]);
            }
            Some((w as u32, h as u32, rgb))
        }
        ColorType::GrayscaleAlpha => {
            if data.len() < npx * 2 {
                return None;
            }
            let mut rgb = Vec::with_capacity(npx * 3);
            for ga in data[..npx * 2].chunks_exact(2) {
                let v = ((ga[0] as u32 * ga[1] as u32 + 127) / 255) as u8;
                rgb.extend_from_slice(&[v, v, v]);
            }
            Some((w as u32, h as u32, rgb))
        }
        ColorType::Indexed => None,
    }
}

fn decode_bmp_rgb24(bytes: &[u8]) -> Option<(u32, u32, Vec<u8>)> {
    if bytes.len() < 54 {
        return None;
    }
    let data_off = u32::from_le_bytes(bytes[10..14].try_into().ok()?) as usize;
    let dib_size = u32::from_le_bytes(bytes[14..18].try_into().ok()?);
    if dib_size < 40 {
        return None;
    }
    let w_i32 = i32::from_le_bytes(bytes[18..22].try_into().ok()?);
    let h_i32 = i32::from_le_bytes(bytes[22..26].try_into().ok()?);
    let bpp = u16::from_le_bytes(bytes[28..30].try_into().ok()?);
    let comp = u32::from_le_bytes(bytes[30..34].try_into().ok()?);
    if w_i32 <= 0 || h_i32 == 0 || (comp != 0 && comp != 3) || (bpp != 24 && bpp != 32) {
        return None;
    }

    let top_down = h_i32 < 0;
    let w = w_i32 as usize;
    let h = h_i32.unsigned_abs() as usize;
    if w > MAX_DIM || h > MAX_DIM {
        return None;
    }

    let bytes_pp = (bpp / 8) as usize;
    let row_stride = (w.checked_mul(bytes_pp)? + 3) & !3;
    let needed = data_off.checked_add(row_stride.checked_mul(h)?)?;
    if bytes.len() < needed {
        return None;
    }

    let mut out = vec![0u8; w * h * 3];
    for dst_y in 0..h {
        let src_y = if top_down { dst_y } else { h - 1 - dst_y };
        let src_row = &bytes[data_off + src_y * row_stride..];
        let dst_row = &mut out[dst_y * w * 3..(dst_y + 1) * w * 3];
        for x in 0..w {
            let b = src_row[x * bytes_pp];
            let g = src_row[x * bytes_pp + 1];
            let r = src_row[x * bytes_pp + 2];
            dst_row[x * 3] = r;
            dst_row[x * 3 + 1] = g;
            dst_row[x * 3 + 2] = b;
        }
    }
    Some((w as u32, h as u32, out))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_png_rgb_roundtrip() {
        let mut png_bytes = Vec::new();
        {
            let mut enc = png::Encoder::new(&mut png_bytes, 4, 4);
            enc.set_color(ColorType::Rgb);
            enc.set_depth(png::BitDepth::Eight);
            let mut wr = enc.write_header().unwrap();
            let mut raw = [0u8; 4 * 4 * 3];
            for (i, slot) in raw.iter_mut().enumerate() {
                *slot = (i * 5) as u8;
            }
            wr.write_image_data(&raw).unwrap();
        }
        let (w, h, out) = decode_image_rgb24(&png_bytes).expect("PNG decode failed");
        assert_eq!((w, h), (4, 4));
        assert_eq!(out.len(), 48);
        assert_eq!(out[0], 0);
        assert_eq!(out[1], 5);
        assert_eq!(out[2], 10);
    }
}
