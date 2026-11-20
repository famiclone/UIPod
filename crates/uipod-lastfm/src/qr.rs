//! Pure-Rust ISO/IEC 18004 QR Code encoder (Byte mode, ECC Level L, Versions 1..=8).
//! Supports URLs up to 192 bytes (Last.fm auth URLs are ~93 bytes -> Version 5, 37x37).

#[derive(Debug, Clone)]
pub struct QrMatrix {
    pub size: usize,
    pub modules: Vec<u8>, // 1 = dark, 0 = light, row-major `size * size`
}

struct VersionSpec {
    version: usize,
    total_data_codewords: usize,
    ec_codewords_per_block: usize,
    num_blocks: usize,
    align_centers: &'static [usize],
    version_bits: u32, // 18-bit BCH(18,6) for version >= 7
}

const SPECS_L: [VersionSpec; 8] = [
    VersionSpec { version: 1, total_data_codewords: 19,  ec_codewords_per_block: 7,  num_blocks: 1, align_centers: &[],            version_bits: 0 },
    VersionSpec { version: 2, total_data_codewords: 34,  ec_codewords_per_block: 10, num_blocks: 1, align_centers: &[6, 18],       version_bits: 0 },
    VersionSpec { version: 3, total_data_codewords: 55,  ec_codewords_per_block: 15, num_blocks: 1, align_centers: &[6, 22],       version_bits: 0 },
    VersionSpec { version: 4, total_data_codewords: 80,  ec_codewords_per_block: 20, num_blocks: 1, align_centers: &[6, 26],       version_bits: 0 },
    VersionSpec { version: 5, total_data_codewords: 108, ec_codewords_per_block: 26, num_blocks: 1, align_centers: &[6, 30],       version_bits: 0 },
    VersionSpec { version: 6, total_data_codewords: 136, ec_codewords_per_block: 18, num_blocks: 2, align_centers: &[6, 34],       version_bits: 0 },
    VersionSpec { version: 7, total_data_codewords: 156, ec_codewords_per_block: 20, num_blocks: 2, align_centers: &[6, 22, 38],   version_bits: 0x07C94 },
    VersionSpec { version: 8, total_data_codewords: 194, ec_codewords_per_block: 24, num_blocks: 2, align_centers: &[6, 24, 42],   version_bits: 0x085BC },
];

fn gf_mul(mut x: u8, mut y: u8) -> u8 {
    let mut z: u8 = 0;
    for _ in 0..8 {
        if (y & 1) != 0 {
            z ^= x;
        }
        let hi = x & 0x80;
        x <<= 1;
        if hi != 0 {
            x ^= 0x1D;
        }
        y >>= 1;
    }
    z
}

fn rs_divisor(degree: usize) -> Vec<u8> {
    let mut result = vec![0u8; degree];
    result[degree - 1] = 1;
    let mut root: u8 = 1;
    for _ in 0..degree {
        for j in 0..degree {
            result[j] = gf_mul(result[j], root);
            if j + 1 < degree {
                result[j] ^= result[j + 1];
            }
        }
        root = gf_mul(root, 0x02);
    }
    result
}

fn rs_remainder(data: &[u8], divisor: &[u8]) -> Vec<u8> {
    let mut result = vec![0u8; divisor.len()];
    for &b in data {
        let factor = b ^ result[0];
        result.remove(0);
        result.push(0);
        for (r, &d) in result.iter_mut().zip(divisor.iter()) {
            *r ^= gf_mul(d, factor);
        }
    }
    result
}

fn push_bits(buf: &mut Vec<u8>, val: u32, len: usize) {
    for i in (0..len).rev() {
        buf.push(((val >> i) & 1) as u8);
    }
}

fn format_info_bits_level_l(mask: u8) -> u16 {
    // ECC Level L = 0b01
    let data: u16 = (0b01 << 3) | ((mask as u16) & 0x07);
    let mut rem = data << 10;
    for i in (0..5).rev() {
        if ((rem >> (i + 10)) & 1) != 0 {
            rem ^= 0x537 << i;
        }
    }
    ((data << 10) | rem) ^ 0x5412
}

fn mask_bit(mask: u8, x: usize, y: usize) -> bool {
    match mask {
        0 => (y + x) % 2 == 0,
        1 => y % 2 == 0,
        2 => x % 3 == 0,
        3 => (y + x) % 3 == 0,
        4 => ((y / 2) + (x / 3)) % 2 == 0,
        5 => ((y * x) % 2) + ((y * x) % 3) == 0,
        6 => (((y * x) % 2) + ((y * x) % 3)) % 2 == 0,
        _ => (((y + x) % 2) + ((y * x) % 3)) % 2 == 0,
    }
}

fn set_func(modules: &mut [u8], is_func: &mut [bool], size: usize, x: usize, y: usize, dark: bool) {
    if x < size && y < size {
        let idx = y * size + x;
        modules[idx] = if dark { 1 } else { 0 };
        is_func[idx] = true;
    }
}

fn draw_finder(modules: &mut [u8], is_func: &mut [bool], size: usize, cx: isize, cy: isize) {
    for dy in -4..=4isize {
        for dx in -4..=4isize {
            let x = cx + dx;
            let y = cy + dy;
            if x >= 0 && x < size as isize && y >= 0 && y < size as isize {
                let dist = dx.abs().max(dy.abs());
                let dark = dist != 2 && dist != 4;
                set_func(modules, is_func, size, x as usize, y as usize, dark);
            }
        }
    }
}

fn draw_alignment(modules: &mut [u8], is_func: &mut [bool], size: usize, cx: usize, cy: usize) {
    for dy in -2..=2isize {
        for dx in -2..=2isize {
            let x = (cx as isize + dx) as usize;
            let y = (cy as isize + dy) as usize;
            let dist = dx.abs().max(dy.abs());
            set_func(modules, is_func, size, x, y, dist != 1);
        }
    }
}

fn draw_format_bits(modules: &mut [u8], is_func: &mut [bool], size: usize, mask: u8) {
    let bits = format_info_level_l(mask);
    // Copy 1: top-left around finder
    let tl_coords: [(usize, usize); 15] = [
        (8, 0), (8, 1), (8, 2), (8, 3), (8, 4), (8, 5), (8, 7),
        (8, 8), (7, 8), (5, 8), (4, 8), (3, 8), (2, 8), (1, 8), (0, 8),
    ];
    for (bit_idx, &(x, y)) in tl_coords.iter().enumerate() {
        set_func(modules, is_func, size, x, y, ((bits >> bit_idx) & 1) != 0);
    }

    // Copy 2: top-right horizontal (bits 0..=7) + bottom-left vertical (bits 8..=14)
    for bit_idx in 0..=7usize {
        let x = size - 1 - bit_idx;
        set_func(modules, is_func, size, x, 8, ((bits >> bit_idx) & 1) != 0);
    }
    for bit_idx in 8..=14usize {
        let y = size - 15 + bit_idx;
        set_func(modules, is_func, size, 8, y, ((bits >> bit_idx) & 1) != 0);
    }
}

fn format_info_level_l(mask: u8) -> u16 {
    format_info_bits_level_l(mask)
}

fn penalty_score(modules: &[u8], size: usize) -> u32 {
    let mut score = 0u32;
    // N1: adjacent modules in row/col
    for y in 0..size {
        let mut run_color = modules[y * size];
        let mut run_len = 1usize;
        for x in 1..size {
            let c = modules[y * size + x];
            if c == run_color {
                run_len += 1;
            } else {
                if run_len >= 5 {
                    score += 3 + (run_len as u32 - 5);
                }
                run_color = c;
                run_len = 1;
            }
        }
        if run_len >= 5 {
            score += 3 + (run_len as u32 - 5);
        }
    }
    for x in 0..size {
        let mut run_color = modules[x];
        let mut run_len = 1usize;
        for y in 1..size {
            let c = modules[y * size + x];
            if c == run_color {
                run_len += 1;
            } else {
                if run_len >= 5 {
                    score += 3 + (run_len as u32 - 5);
                }
                run_color = c;
                run_len = 1;
            }
        }
        if run_len >= 5 {
            score += 3 + (run_len as u32 - 5);
        }
    }
    // N2: 2x2 blocks
    for y in 0..size - 1 {
        for x in 0..size - 1 {
            let c = modules[y * size + x];
            if c == modules[y * size + x + 1]
                && c == modules[(y + 1) * size + x]
                && c == modules[(y + 1) * size + x + 1]
            {
                score += 3;
            }
        }
    }
    score
}

pub fn encode_qr_bytes(data: &[u8]) -> Option<QrMatrix> {
    if data.is_empty() {
        return None;
    }
    // Byte mode overhead in Versions 1..=8 is 4 (mode) + 8 (len) bits = 12 bits -> 2 bytes
    let spec = SPECS_L
        .iter()
        .find(|s| data.len() + 2 <= s.total_data_codewords)?;

    let cap_bits = spec.total_data_codewords * 8;
    let mut bits = Vec::with_capacity(cap_bits);
    push_bits(&mut bits, 0b0100, 4); // Byte mode
    push_bits(&mut bits, data.len() as u32, 8);
    for &b in data {
        push_bits(&mut bits, b as u32, 8);
    }
    let term = (cap_bits - bits.len()).min(4);
    for _ in 0..term {
        bits.push(0);
    }
    while bits.len() % 8 != 0 {
        bits.push(0);
    }

    let mut data_cw = Vec::with_capacity(spec.total_data_codewords);
    for chunk in bits.chunks_exact(8) {
        let mut b = 0u8;
        for &bit in chunk {
            b = (b << 1) | bit;
        }
        data_cw.push(b);
    }
    let pads = [0xECu8, 0x11u8];
    let mut pi = 0usize;
    while data_cw.len() < spec.total_data_codewords {
        data_cw.push(pads[pi & 1]);
        pi += 1;
    }

    // Split into `num_blocks` equal blocks (for V1..=8 Level L all blocks have equal length!)
    let block_data_len = spec.total_data_codewords / spec.num_blocks;
    let divisor = rs_divisor(spec.ec_codewords_per_block);
    let mut data_blocks = Vec::with_capacity(spec.num_blocks);
    let mut ec_blocks = Vec::with_capacity(spec.num_blocks);
    for b_idx in 0..spec.num_blocks {
        let slice = &data_cw[b_idx * block_data_len..(b_idx + 1) * block_data_len];
        data_blocks.push(slice);
        ec_blocks.push(rs_remainder(slice, &divisor));
    }

    let mut all_cw = Vec::with_capacity(
        spec.total_data_codewords + spec.num_blocks * spec.ec_codewords_per_block,
    );
    for i in 0..block_data_len {
        for b in &data_blocks {
            all_cw.push(b[i]);
        }
    }
    for i in 0..spec.ec_codewords_per_block {
        for e in &ec_blocks {
            all_cw.push(e[i]);
        }
    }

    let size = spec.version * 4 + 17;
    let mut base_modules = vec![0u8; size * size];
    let mut is_func = vec![false; size * size];

    // Timing patterns
    for i in 0..size {
        set_func(&mut base_modules, &mut is_func, size, 6, i, i % 2 == 0);
        set_func(&mut base_modules, &mut is_func, size, i, 6, i % 2 == 0);
    }

    // Finder patterns (3 corners)
    draw_finder(&mut base_modules, &mut is_func, size, 3, 3);
    draw_finder(&mut base_modules, &mut is_func, size, (size - 4) as isize, 3);
    draw_finder(&mut base_modules, &mut is_func, size, 3, (size - 4) as isize);

    // Alignment patterns
    let n_align = spec.align_centers.len();
    for (iy, &cy) in spec.align_centers.iter().enumerate() {
        for (ix, &cx) in spec.align_centers.iter().enumerate() {
            let is_finder_corner = (ix == 0 && iy == 0)
                || (ix == 0 && iy + 1 == n_align)
                || (ix + 1 == n_align && iy == 0);
            if !is_finder_corner {
                draw_alignment(&mut base_modules, &mut is_func, size, cx, cy);
            }
        }
    }

    // Dark module & reserve format bits
    set_func(&mut base_modules, &mut is_func, size, 8, 4 * spec.version + 9, true);
    draw_format_bits(&mut base_modules, &mut is_func, size, 0);

    // Version info (>= 7)
    if spec.version >= 7 {
        for i in 0..18usize {
            let bit = ((spec.version_bits >> i) & 1) != 0;
            let a = size - 11 + (i % 3);
            let b = i / 3;
            set_func(&mut base_modules, &mut is_func, size, a, b, bit);
            set_func(&mut base_modules, &mut is_func, size, b, a, bit);
        }
    }

    // Zigzag placement of data + EC bits
    let mut bit_idx = 0usize;
    let total_bits = all_cw.len() * 8;
    let mut right = (size - 1) as isize;
    let mut upward = true;
    while right >= 1 {
        if right == 6 {
            right -= 1;
        }
        for vert in 0..size {
            let y = if upward { size - 1 - vert } else { vert };
            for j in 0..2isize {
                let x = (right - j) as usize;
                let idx = y * size + x;
                if !is_func[idx] {
                    let dark = if bit_idx < total_bits {
                        ((all_cw[bit_idx / 8] >> (7 - (bit_idx % 8))) & 1) != 0
                    } else {
                        false
                    };
                    base_modules[idx] = if dark { 1 } else { 0 };
                    bit_idx += 1;
                }
            }
        }
        upward = !upward;
        right -= 2;
    }

    // Evaluate all 8 masks and pick lowest penalty
    let mut best_mask = 0u8;
    let mut best_score = u32::MAX;
    let mut best_modules = base_modules.clone();

    for mask in 0..8u8 {
        let mut m = base_modules.clone();
        for y in 0..size {
            for x in 0..size {
                let idx = y * size + x;
                if !is_func[idx] && mask_bit(mask, x, y) {
                    m[idx] ^= 1;
                }
            }
        }
        let mut dummy_func = is_func.clone();
        draw_format_bits(&mut m, &mut dummy_func, size, mask);
        let sc = penalty_score(&m, size);
        if sc < best_score {
            best_score = sc;
            best_mask = mask;
            best_modules = m;
        }
    }
    let _ = best_mask;

    Some(QrMatrix {
        size,
        modules: best_modules,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_format_bch_level_l_mask0() {
        // ISO/IEC 18004 Table C.1: data 01_000 (Level L, mask 0) -> masked format info 0x77C4
        assert_eq!(format_info_bits_level_l(0), 0x77C4);
        assert_eq!(format_info_bits_level_l(1), 0x72F3);
    }

    #[test]
    fn test_encode_lastfm_auth_url() {
        let url = b"https://www.last.fm/api/auth/?api_key=0123456789abcdef0123456789abcdef&token=fedcba9876543210fedcba9876543210";
        let qr = encode_qr_bytes(url).expect("QR should encode 109-byte URL");
        assert_eq!(qr.size, 41); // Version 6 (109 + 2 = 111 <= 136)
        assert_eq!(qr.modules.len(), 41 * 41);
        // Top-left finder center (3,3) must be dark (1)
        assert_eq!(qr.modules[3 * 41 + 3], 1);
    }
}
