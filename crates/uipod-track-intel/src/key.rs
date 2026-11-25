//! Canonical track/subtrack identification and fast 64-bit `XXH3`-style hashing.
//!
//! Ensures multi-song `.nsf`/`.nsfe` (`Game.nsf#3`) and `.cue` subtracks receive isolated
//! cache entries, platform mount roots (`/mnt/SDCARD/`, `/storage/emulated/0/`) normalize
//! deterministically, and modified files auto-invalidate via `(file_size, mtime_sec)`.

use uipod_nsf::parse_subtrack_uri;

/// Sentinel `subtrack_index` indicating a standalone single-track file or stream.
pub const SUBTRACK_NONE: u16 = 0xFFFF;

/// Canonical identity tuple hashed into a 64-bit `TrackAnalysisRecord::key_hash`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TrackKeyInput<'a> {
    pub canonical_source_id: &'a str,
    pub file_size: u64,
    pub mtime_sec: i64,
    pub subtrack_index: u16,
}

/// Parsed URI + subtrack result extracted from a raw path or virtual URI.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NormalizedTrackId {
    /// Canonical source ID with platform SD-card prefix normalized to `sd:/` and `#idx` stripped.
    pub canonical_source_id: String,
    /// Raw filesystem base path without `#idx` fragment (suitable for `stat()` / `File::open`).
    pub fs_path: String,
    /// `0..255` for `.nsf`/`.nsfe`/`.cue` subtracks, or [`SUBTRACK_NONE`] (`0xFFFF`).
    pub subtrack_index: u16,
}

/// Normalize a raw track path or URI (plus an optional explicit `.cue` subtrack index)
/// into its canonical source ID, filesystem path, and `subtrack_index`.
pub fn normalize_track_id(raw_path: &str, cue_subtrack: Option<u16>) -> NormalizedTrackId {
    let trimmed = raw_path.trim();
    let parsed_nsf = parse_subtrack_uri(trimmed);
    let fs_path = parsed_nsf.file_path.to_string();
    let subtrack_index = cue_subtrack
        .or(parsed_nsf.subtrack_index)
        .unwrap_or(SUBTRACK_NONE);

    let canonical_source_id = canonicalize_mount_prefix(&fs_path);
    NormalizedTrackId {
        canonical_source_id,
        fs_path,
        subtrack_index,
    }
}

fn canonicalize_mount_prefix(path: &str) -> String {
    let p = path.trim();
    for prefix in ["/mnt/SDCARD/", "/storage/emulated/0/"] {
        if let Some(rest) = p.strip_prefix(prefix) {
            let mut out = String::with_capacity(4 + rest.len());
            out.push_str("sd:/");
            out.push_str(rest);
            return out;
        }
    }
    p.to_string()
}

#[inline]
fn avalanche64(mut x: u64) -> u64 {
    x ^= x >> 33;
    x = x.wrapping_mul(0xFF51_AFD7_ED55_8CCD);
    x ^= x >> 33;
    x = x.wrapping_mul(0xC4CE_B9FE_1A85_EC53);
    x ^= x >> 33;
    x
}

/// Fast 64-bit `XXH3` / `FNV-1a` hybrid hash over `(canonical_source_id, file_size, mtime_sec, subtrack_index)`.
/// Never returns `0` (`0` is reserved as the empty/uninitialized sentinel).
pub fn compute_track_key_hash(input: &TrackKeyInput<'_>) -> u64 {
    const PRIME1: u64 = 0x9E37_79B1_85EB_CA87;
    const PRIME2: u64 = 0xC2B2_AE3D_27D4_EB4F;
    const PRIME3: u64 = 0x1656_67B1_9E37_79F9;

    let mut acc = 0xCBF2_9CE4_8422_2325_u64 ^ PRIME1;
    let bytes = input.canonical_source_id.as_bytes();
    let mut i = 0usize;
    while i + 8 <= bytes.len() {
        let mut buf = [0u8; 8];
        buf.copy_from_slice(&bytes[i..i + 8]);
        let lane = u64::from_le_bytes(buf);
        acc ^= lane.wrapping_mul(PRIME2);
        acc = acc.rotate_left(31).wrapping_mul(PRIME1);
        i += 8;
    }
    while i < bytes.len() {
        acc ^= (bytes[i] as u64).wrapping_mul(PRIME3);
        acc = acc.rotate_left(11).wrapping_mul(PRIME1);
        i += 1;
    }

    acc ^= (bytes.len() as u64).wrapping_mul(PRIME3);
    acc = avalanche64(acc ^ input.file_size.wrapping_mul(PRIME2));
    acc = avalanche64(acc ^ (input.mtime_sec as u64).wrapping_mul(PRIME3));
    acc = avalanche64(acc ^ ((input.subtrack_index as u64) | 0xA5A5_0000).wrapping_mul(PRIME1));

    if acc == 0 {
        1
    } else {
        acc
    }
}
