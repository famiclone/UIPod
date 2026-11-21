//! Lock-free runtime bridge, `RustAudioDecoder` adapter, and C/JNI FFI for `uipod-nsf`.
#![allow(clippy::missing_safety_doc)]

use std::collections::VecDeque;
use std::ffi::CStr;
use std::io::Read;
use std::os::raw::c_char;
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU16, AtomicU32, Ordering};
use std::sync::Mutex;

use uipod_nsf::{
    format_subtrack_uri, inspect_nsf_header_fast, scan_nsf_container, ApuFrameTelemetry,
    ChannelDescriptor, ChannelState, ChipFlags, NsfContainerMeta, NsfEngine, SubtrackUri,
    TrackKind,
};

use crate::{copy_str_to_c_buf, cstr_to_str, RustAudioDecoder, UipodRsTrackMeta};

pub const NSF_SAMPLE_RATE: u32 = 44_100;
const CHIP_FLAGS_INACTIVE: u16 = 0xFFFF;
const TELEMETRY_WORDS: usize = 25; // 4 + 8 * 12 = 100 bytes

// ============================================================================
// Global Lock-Free Runtime Controls & 60 FPS Telemetry SeqLock
// ============================================================================

static NSF_CHANNEL_MUTE_MASK: AtomicU16 = AtomicU16::new(0);
static NSF_ENDLESS_LOOP: AtomicBool = AtomicBool::new(false);
static NSF_ACTIVE_CHIP_FLAGS: AtomicU16 = AtomicU16::new(CHIP_FLAGS_INACTIVE);
static NSF_TEMPO_RATIO_BITS: AtomicU32 = AtomicU32::new(0x3F80_0000); // 1.0f32

/// SeqLock sequence counter: even = consistent snapshot, odd = writer in progress.
static TELEMETRY_SEQ: AtomicU32 = AtomicU32::new(0);
static TELEMETRY_DATA: [AtomicU32; TELEMETRY_WORDS] = [
    AtomicU32::new(0),
    AtomicU32::new(0),
    AtomicU32::new(0),
    AtomicU32::new(0),
    AtomicU32::new(0),
    AtomicU32::new(0),
    AtomicU32::new(0),
    AtomicU32::new(0),
    AtomicU32::new(0),
    AtomicU32::new(0),
    AtomicU32::new(0),
    AtomicU32::new(0),
    AtomicU32::new(0),
    AtomicU32::new(0),
    AtomicU32::new(0),
    AtomicU32::new(0),
    AtomicU32::new(0),
    AtomicU32::new(0),
    AtomicU32::new(0),
    AtomicU32::new(0),
    AtomicU32::new(0),
    AtomicU32::new(0),
    AtomicU32::new(0),
    AtomicU32::new(0),
    AtomicU32::new(0),
];

/// Canonical base file path of the most recently loaded `.nsf`/`.nsfe` container.
/// Preserves `NSF_CHANNEL_MUTE_MASK` across subtracks of the same game while resetting
/// to `0` when switching to a different `.nsf` container, a non-NSF codec, or stopping.
static LAST_NSF_CONTAINER: Mutex<Option<String>> = Mutex::new(None);

const SCAN_CACHE_CAP: usize = 64;

/// LRU cache of fully scanned `NsfContainerMeta` entries (`(normalized_path, byte_len, meta)`).
static SCAN_CACHE: Mutex<VecDeque<(String, usize, NsfContainerMeta)>> =
    Mutex::new(VecDeque::new());

#[inline]
pub fn get_nsf_channel_mute_mask() -> u16 {
    NSF_CHANNEL_MUTE_MASK.load(Ordering::Relaxed)
}

pub fn set_nsf_channel_mute_mask(mask: u16) {
    NSF_CHANNEL_MUTE_MASK.store(mask, Ordering::Relaxed);
    // Reflect mute change immediately in published telemetry snapshot even while paused
    let mut snap = get_nsf_apu_telemetry();
    if snap.channel_count > 0 {
        snap.mute_mask = mask;
        for i in 0..8 {
            let muted = (mask & (1 << i)) != 0;
            snap.channels[i].muted = u8::from(muted);
            if muted {
                snap.channels[i].volume = 0.0;
            }
        }
        publish_nsf_apu_telemetry(&snap);
    }
}

#[inline]
pub fn get_nsf_active_chip_flags() -> Option<ChipFlags> {
    let raw = NSF_ACTIVE_CHIP_FLAGS.load(Ordering::Relaxed);
    if raw == CHIP_FLAGS_INACTIVE {
        None
    } else {
        Some(ChipFlags((raw & 0x3F) as u8))
    }
}

#[inline]
pub fn set_nsf_endless_loop(enabled: bool) {
    NSF_ENDLESS_LOOP.store(enabled, Ordering::Relaxed);
}

#[inline]
pub fn get_nsf_endless_loop() -> bool {
    NSF_ENDLESS_LOOP.load(Ordering::Relaxed)
}

#[inline]
pub fn set_nsf_tempo_ratio(ratio: f32) {
    let clamped = if ratio.is_finite() {
        ratio.clamp(0.5, 2.0)
    } else {
        1.0
    };
    NSF_TEMPO_RATIO_BITS.store(clamped.to_bits(), Ordering::Relaxed);
}

#[inline]
pub fn get_nsf_tempo_ratio() -> f32 {
    f32::from_bits(NSF_TEMPO_RATIO_BITS.load(Ordering::Relaxed))
}

/// Publish a new 60 Hz `ApuFrameTelemetry` frame lock-free.
pub fn publish_nsf_apu_telemetry(tel: &ApuFrameTelemetry) {
    let seq = TELEMETRY_SEQ.load(Ordering::Relaxed);
    let odd = (seq | 1).wrapping_add(if (seq & 1) != 0 { 2 } else { 0 });
    TELEMETRY_SEQ.store(odd, Ordering::Release);

    let w0 = (tel.channel_count as u32)
        | ((tel.chip_flags as u32) << 8)
        | ((tel.mute_mask as u32) << 16);
    TELEMETRY_DATA[0].store(w0, Ordering::Relaxed);

    for (i, ch) in tel.channels.iter().enumerate() {
        let base = 1 + i * 3;
        let flags = (ch.active as u32)
            | ((ch.muted as u32) << 8)
            | ((ch.onset_trigger as u32) << 16)
            | ((ch.duty_or_mode as u32) << 24);
        TELEMETRY_DATA[base].store(flags, Ordering::Relaxed);
        TELEMETRY_DATA[base + 1].store(ch.volume.to_bits(), Ordering::Relaxed);
        TELEMETRY_DATA[base + 2].store(ch.freq_hz.to_bits(), Ordering::Relaxed);
    }

    TELEMETRY_SEQ.store(odd.wrapping_add(1), Ordering::Release);
}

/// Lock-free 60 FPS reader for the latest `ApuFrameTelemetry` snapshot.
pub fn get_nsf_apu_telemetry() -> ApuFrameTelemetry {
    for _ in 0..8 {
        let s1 = TELEMETRY_SEQ.load(Ordering::Acquire);
        if (s1 & 1) != 0 {
            std::hint::spin_loop();
            continue;
        }

        let mut words = [0u32; TELEMETRY_WORDS];
        for (dst, src) in words.iter_mut().zip(TELEMETRY_DATA.iter()) {
            *dst = src.load(Ordering::Relaxed);
        }

        std::sync::atomic::fence(Ordering::Acquire);
        let s2 = TELEMETRY_SEQ.load(Ordering::Relaxed);
        if s1 == s2 {
            let w0 = words[0];
            let mut channels = [ChannelState::default(); 8];
            for (i, ch) in channels.iter_mut().enumerate() {
                let base = 1 + i * 3;
                let flags = words[base];
                *ch = ChannelState {
                    active: (flags & 0xFF) as u8,
                    muted: ((flags >> 8) & 0xFF) as u8,
                    onset_trigger: ((flags >> 16) & 0xFF) as u8,
                    duty_or_mode: ((flags >> 24) & 0xFF) as u8,
                    volume: f32::from_bits(words[base + 1]),
                    freq_hz: f32::from_bits(words[base + 2]),
                };
            }
            return ApuFrameTelemetry {
                channel_count: (w0 & 0xFF) as u8,
                chip_flags: ((w0 >> 8) & 0xFF) as u8,
                mute_mask: ((w0 >> 16) & 0xFFFF) as u16,
                channels,
            };
        }
    }
    ApuFrameTelemetry::default()
}

/// Apply container-transition mute-mask hygiene when loading an `.nsf`/`.nsfe` container:
/// - Same container path -> preserves `NSF_CHANNEL_MUTE_MASK`.
/// - Different container path -> resets `NSF_CHANNEL_MUTE_MASK` to `0`.
pub fn notify_nsf_container_loaded(container_file_path: &str, chip_flags: ChipFlags) -> u16 {
    let norm = normalize_container_key(container_file_path);
    let mask = if let Ok(mut guard) = LAST_NSF_CONTAINER.lock() {
        let same = guard.as_deref() == Some(norm.as_str()) && !norm.is_empty();
        if !same {
            NSF_CHANNEL_MUTE_MASK.store(0, Ordering::Relaxed);
            *guard = Some(norm);
            0
        } else {
            NSF_CHANNEL_MUTE_MASK.load(Ordering::Relaxed)
        }
    } else {
        NSF_CHANNEL_MUTE_MASK.load(Ordering::Relaxed)
    };
    NSF_ACTIVE_CHIP_FLAGS.store(chip_flags.0 as u16, Ordering::Relaxed);
    mask
}

/// Called when a single decoder instance closes (may be followed immediately by the next
/// subtrack of the same `.nsf`, so `LAST_NSF_CONTAINER` and `NSF_CHANNEL_MUTE_MASK` are kept).
pub fn notify_nsf_decoder_closed() {
    NSF_ACTIVE_CHIP_FLAGS.store(CHIP_FLAGS_INACTIVE, Ordering::Relaxed);
    set_nsf_tempo_ratio(1.0);
    publish_nsf_apu_telemetry(&ApuFrameTelemetry::default());
}

/// Reset the entire NSF runtime session (called when switching to a non-NSF track or when
/// audio playback stops cleanly). Resets `channel_mute_mask` to `0` and clears active flags.
pub fn reset_nsf_runtime_session() {
    NSF_CHANNEL_MUTE_MASK.store(0, Ordering::Relaxed);
    NSF_ACTIVE_CHIP_FLAGS.store(CHIP_FLAGS_INACTIVE, Ordering::Relaxed);
    set_nsf_tempo_ratio(1.0);
    if let Ok(mut guard) = LAST_NSF_CONTAINER.lock() {
        *guard = None;
    }
    publish_nsf_apu_telemetry(&ApuFrameTelemetry::default());
}

fn normalize_container_key(path: &str) -> String {
    SubtrackUri::parse(path.trim()).file_path.to_string()
}

fn lookup_scan_cache(base_path: &str, expected_len: Option<usize>) -> Option<NsfContainerMeta> {
    let key = normalize_container_key(base_path);
    if key.is_empty() {
        return None;
    }
    let mut guard = SCAN_CACHE.lock().ok()?;
    let pos = guard.iter().position(|(cached_path, cached_len, _)| {
        *cached_path == key && expected_len.is_none_or(|len| *cached_len == len)
    })?;
    let entry = guard.remove(pos)?;
    let meta = entry.2.clone();
    guard.push_front(entry);
    Some(meta)
}

fn insert_scan_cache(base_path: &str, byte_len: usize, meta: &NsfContainerMeta) {
    let key = normalize_container_key(base_path);
    if key.is_empty() {
        return;
    }
    if let Ok(mut guard) = SCAN_CACHE.lock() {
        if let Some(pos) = guard.iter().position(|(p, _, _)| *p == key) {
            guard.remove(pos);
        }
        if guard.len() >= SCAN_CACHE_CAP {
            guard.pop_back();
        }
        guard.push_front((key, byte_len, meta.clone()));
    }
}

/// Read only the minimal bytes needed for fast zero-emulation header inspection (`128` bytes
/// for standard `NESM\x1A` v1 `.nsf` files, or full file for NSF2 with chunks / `NSFE`).
fn read_nsf_header_bytes_from_disk(file_path: &str) -> std::io::Result<Vec<u8>> {
    let mut file = std::fs::File::open(file_path)?;
    let mut hdr = [0u8; 128];
    let mut got = 0usize;
    while got < hdr.len() {
        let n = file.read(&mut hdr[got..])?;
        if n == 0 {
            break;
        }
        got += n;
    }
    if got >= 128 && &hdr[0..5] == b"NESM\x1A" {
        let has_nsf2_chunks =
            hdr[0x05] >= 2 && (hdr[0x7D] != 0 || hdr[0x7E] != 0 || hdr[0x7F] != 0);
        if !has_nsf2_chunks {
            return Ok(hdr.to_vec());
        }
    }
    let mut out = Vec::from(&hdr[..got]);
    file.read_to_end(&mut out)?;
    Ok(out)
}

pub(crate) fn scan_nsf_cached(base_path: &str, bytes: &[u8]) -> Result<NsfContainerMeta, &'static str> {
    if let Some(meta) = lookup_scan_cache(base_path, Some(bytes.len())) {
        return Ok(meta);
    }

    let meta = scan_nsf_container(bytes).map_err(|_| "Invalid NSF/NSFe file")?;
    insert_scan_cache(base_path, bytes.len(), &meta);
    Ok(meta)
}

/// Expand a bare `.nsf`/`.nsfe` container into canonical `file.nsf#<idx>` URIs for all
/// `TrackKind::Music` subtracks (falling back to `starting_subtrack` if none are Music).
pub fn expand_nsf_music_uris(container_path: &str, bytes: &[u8]) -> Result<Vec<String>, &'static str> {
    let base = SubtrackUri::parse(container_path).file_path;
    let meta = scan_nsf_cached(base, bytes)?;
    let mut uris: Vec<String> = meta
        .subtracks
        .iter()
        .filter(|s| s.kind == TrackKind::Music)
        .map(|s| format_subtrack_uri(base, s.index))
        .collect();
    if uris.is_empty() && meta.total_subtracks > 0 {
        uris.push(format_subtrack_uri(base, meta.starting_subtrack));
    }
    Ok(uris)
}

// ============================================================================
// `RustAudioDecoder` Implementation for `NsfEngine`
// ============================================================================

pub struct NsfDecoder {
    engine: NsfEngine,
}

impl NsfDecoder {
    pub fn open(track_path: &str, bytes: &[u8]) -> Result<Self, &'static str> {
        Self::open_with_rate(track_path, bytes, NSF_SAMPLE_RATE)
    }

    pub fn open_with_rate(
        track_path: &str,
        bytes: &[u8],
        sample_rate: u32,
    ) -> Result<Self, &'static str> {
        let uri = SubtrackUri::parse(track_path);
        // Never run a full multi-subtrack 6502 scan on decoder open! Reuse `SCAN_CACHE` if
        // already cached in RAM; otherwise `NsfEngine::load_with_meta(..., None)` initializes
        // in < 0.2 ms from the header/NSFE chunks and detects loops live during playback.
        let pre_meta = lookup_scan_cache(uri.file_path, Some(bytes.len()));
        let mut engine = NsfEngine::load_with_meta(bytes, sample_rate, pre_meta.as_ref())
            .map_err(|_| "Failed to initialize NsfEngine")?;

        let chip_flags = engine.container_meta().chip_flags;
        let mask = notify_nsf_container_loaded(uri.file_path, chip_flags);

        let target_idx = match uri.subtrack_index {
            Some(idx) if idx < engine.container_meta().total_subtracks => idx,
            Some(_) => return Err("NSF subtrack index out of range"),
            None if pre_meta.is_some() => engine
                .container_meta()
                .subtracks
                .iter()
                .find(|s| s.kind == TrackKind::Music)
                .map(|s| s.index)
                .unwrap_or_else(|| engine.container_meta().starting_subtrack),
            None => engine.first_music_subtrack(),
        };

        engine.set_channel_mute_mask(mask);
        engine.set_endless_loop(get_nsf_endless_loop());
        engine
            .select_subtrack(target_idx)
            .map_err(|_| "Failed to select NSF subtrack")?;
        publish_nsf_apu_telemetry(&engine.telemetry());

        Ok(Self { engine })
    }

    #[inline]
    pub fn engine(&self) -> &NsfEngine {
        &self.engine
    }

    #[inline]
    pub fn engine_mut(&mut self) -> &mut NsfEngine {
        &mut self.engine
    }
}

impl Drop for NsfDecoder {
    fn drop(&mut self) {
        notify_nsf_decoder_closed();
    }
}

impl RustAudioDecoder for NsfDecoder {
    fn sample_rate(&self) -> u32 {
        self.engine.sample_rate()
    }

    fn duration_ms(&self) -> u32 {
        self.engine.total_duration_ms()
    }

    fn elapsed_ms(&self) -> u32 {
        self.engine.elapsed_ms()
    }

    fn seek_ms(&mut self, target_ms: u32) {
        self.engine.set_channel_mute_mask(get_nsf_channel_mute_mask());
        self.engine.set_endless_loop(get_nsf_endless_loop());
        self.engine.seek_ms(target_ms);
        publish_nsf_apu_telemetry(&self.engine.telemetry());
    }

    fn seek_step_ms(&mut self, target_ms: u32, max_step_ms: u32) -> bool {
        self.engine.set_channel_mute_mask(get_nsf_channel_mute_mask());
        self.engine.set_endless_loop(get_nsf_endless_loop());
        let done = self.engine.seek_step_ms(target_ms, max_step_ms);
        if done {
            publish_nsf_apu_telemetry(&self.engine.telemetry());
        }
        done
    }

    fn render_stereo_s16(&mut self, out_interleaved: &mut [i16], loop_forever: bool) -> usize {
        let live_mask = get_nsf_channel_mute_mask();
        if self.engine.channel_mute_mask() != live_mask {
            self.engine.set_channel_mute_mask(live_mask);
        }
        let live_endless = get_nsf_endless_loop() || loop_forever;
        if self.engine.endless_loop() != live_endless {
            self.engine.set_endless_loop(live_endless);
        }
        self.engine.set_tempo_ratio(get_nsf_tempo_ratio());

        let frames = self.engine.render_stereo_s16(out_interleaved);
        publish_nsf_apu_telemetry(&self.engine.telemetry());
        frames
    }
}

// ============================================================================
// Formatting Helper for Subtrack Display Titles
// ============================================================================

pub fn format_nsf_subtrack_title(meta: &NsfContainerMeta, subtrack_idx: u16, base_path: &str) -> String {
    if let Some(sub) = meta.subtracks.get(subtrack_idx as usize) {
        if let Some(ref t) = sub.title {
            let trimmed = t.trim();
            if !trimmed.is_empty() {
                return trimmed.to_string();
            }
        }
    }
    let game = if !meta.game_title.trim().is_empty() {
        meta.game_title.trim()
    } else {
        Path::new(base_path)
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("NSF")
    };
    if meta.total_subtracks <= 1 {
        game.to_string()
    } else {
        format!("{game} - Track {:02}", subtrack_idx + 1)
    }
}

// ============================================================================
// C FFI Exports for Rockbox / Linux / Android (`uipod_rs_codecs.h`)
// ============================================================================

#[repr(C)]
#[derive(Clone, Copy)]
pub struct UipodRsNsfSubtrackInfo {
    pub subtrack_index: u16,
    pub track_kind: u8,
    pub chip_flags: u8,
    pub duration_ms: u32,
    pub fade_ms: u32,
    pub loop_point_ms: i32,
    pub title: [u8; 256],
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct UipodRsNsfContainerHeader {
    pub game_title: [u8; 256],
    pub artist: [u8; 256],
    pub copyright: [u8; 256],
    pub total_subtracks: u16,
    pub starting_subtrack: u16,
    pub chip_flags: u8,
    pub is_pal: u8,
    pub payload_crc32: u32,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct UipodRsNsfChannelDesc {
    pub bit_index: u8,
    pub short_code: [u8; 8],
    pub display_name: [u8; 32],
}

#[no_mangle]
pub extern "C" fn uipod_rs_nsf_get_channel_mute_mask() -> u16 {
    get_nsf_channel_mute_mask()
}

#[no_mangle]
pub extern "C" fn uipod_rs_nsf_set_channel_mute_mask(mask: u16) {
    set_nsf_channel_mute_mask(mask);
}

/// Returns active `ChipFlags` bitmask (`0..63`) when an `.nsf`/`.nsfe` track is loaded,
/// or `-1` when no NSF track is active.
#[no_mangle]
pub extern "C" fn uipod_rs_nsf_get_active_chip_flags() -> i32 {
    match get_nsf_active_chip_flags() {
        Some(flags) => flags.0 as i32,
        None => -1,
    }
}

#[no_mangle]
pub extern "C" fn uipod_rs_nsf_get_endless_loop() -> i32 {
    i32::from(get_nsf_endless_loop())
}

#[no_mangle]
pub extern "C" fn uipod_rs_nsf_set_endless_loop(enabled: i32) {
    set_nsf_endless_loop(enabled != 0);
}

#[no_mangle]
pub extern "C" fn uipod_rs_nsf_reset_session() {
    reset_nsf_runtime_session();
}

/// Copy lock-free 60 FPS `ApuFrameTelemetry` into `out_telemetry`.
/// Returns `1` when an NSF track is active (`channel_count > 0`), `0` otherwise.
#[no_mangle]
pub unsafe extern "C" fn uipod_rs_nsf_get_telemetry(out_telemetry: *mut ApuFrameTelemetry) -> i32 {
    if out_telemetry.is_null() {
        return 0;
    }
    let tel = get_nsf_apu_telemetry();
    unsafe {
        *out_telemetry = tel;
    }
    i32::from(tel.channel_count > 0)
}

/// Populate `out_descs` with the channel descriptors for `chip_flags` (`0..63`).
/// Returns the total number of descriptors written.
#[no_mangle]
pub unsafe extern "C" fn uipod_rs_nsf_get_channel_descriptors(
    chip_flags: u8,
    out_descs: *mut UipodRsNsfChannelDesc,
    max_descs: usize,
) -> usize {
    let descs: &[ChannelDescriptor] = ChipFlags(chip_flags & 0x3F).channel_descriptors();
    if out_descs.is_null() || max_descs == 0 {
        return descs.len();
    }
    let count = descs.len().min(max_descs);
    let dst = unsafe { std::slice::from_raw_parts_mut(out_descs, count) };
    for (slot, d) in dst.iter_mut().zip(descs.iter().take(count)) {
        slot.bit_index = d.bit_index;
        copy_str_to_c_buf(Some(d.short_code), &mut slot.short_code);
        copy_str_to_c_buf(Some(d.display_name), &mut slot.display_name);
    }
    count
}

/// Parse metadata for a single `.nsf` / `.nsfe` path or virtual subtrack URI (`Game.nsf#3`).
///
/// Uses `SCAN_CACHE` if the container was already scanned, or `inspect_nsf_header_fast` (~5 µs,
/// zero 6502 CPU emulation!) so Rockbox `buffering_thread` and `playlist_viewer` never stall.
#[no_mangle]
pub unsafe extern "C" fn uipod_rs_nsf_parse_metadata(
    data: *const u8,
    data_len: usize,
    track_path: *const c_char,
    out_meta: *mut UipodRsTrackMeta,
) -> i32 {
    if out_meta.is_null() {
        return 0;
    }
    let res = std::panic::catch_unwind(|| {
        let path_str = unsafe { cstr_to_str(track_path) };
        let uri = SubtrackUri::parse(path_str);

        let container = if let Some(cached) = lookup_scan_cache(uri.file_path, None) {
            cached
        } else {
            let owned_bytes: Vec<u8>;
            let slice: &[u8] = if !data.is_null() && data_len >= 8 {
                unsafe { std::slice::from_raw_parts(data, data_len) }
            } else if !uri.file_path.is_empty() {
                let Ok(v) = read_nsf_header_bytes_from_disk(uri.file_path) else {
                    return 0;
                };
                owned_bytes = v;
                &owned_bytes
            } else {
                return 0;
            };
            let Ok(fast) = inspect_nsf_header_fast(slice) else {
                return 0;
            };
            fast
        };

        let sub_idx = match uri.subtrack_index {
            Some(i) if i < container.total_subtracks => i,
            Some(_) => return 0,
            None => container
                .subtracks
                .iter()
                .find(|s| s.kind == TrackKind::Music)
                .map(|s| s.index)
                .unwrap_or(container.starting_subtrack),
        };

        let title = format_nsf_subtrack_title(&container, sub_idx, uri.file_path);
        let album = if !container.game_title.trim().is_empty() {
            container.game_title.trim().to_string()
        } else {
            Path::new(uri.file_path)
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("NSF")
                .to_string()
        };
        let dur_ms = container
            .subtracks
            .get(sub_idx as usize)
            .map(|s| s.duration_ms.saturating_add(s.fade_ms))
            .unwrap_or(124_000);

        let meta = unsafe { &mut *out_meta };
        copy_str_to_c_buf(Some(&title), &mut meta.title);
        copy_str_to_c_buf(
            if container.artist.trim().is_empty() {
                None
            } else {
                Some(container.artist.trim())
            },
            &mut meta.artist,
        );
        copy_str_to_c_buf(Some(&album), &mut meta.album);
        copy_str_to_c_buf(Some("Chiptune"), &mut meta.genre);
        copy_str_to_c_buf(
            if container.copyright.trim().is_empty() {
                None
            } else {
                Some(container.copyright.trim())
            },
            &mut meta.composer,
        );
        copy_str_to_c_buf(
            Some(container.chip_flags.primary_badge_label()),
            &mut meta.comment,
        );
        meta.year = extract_leading_year(&container.copyright).unwrap_or(0);
        meta.track_num = (sub_idx as i32) + 1;
        meta.duration_ms = dur_ms;
        meta.sample_rate = NSF_SAMPLE_RATE;
        meta.bitrate_kbps = 1411;
        1
    });
    res.unwrap_or(0)
}

fn write_container_to_ffi(
    container: &NsfContainerMeta,
    base_path: &str,
    out_header: *mut UipodRsNsfContainerHeader,
    out_subtracks: *mut UipodRsNsfSubtrackInfo,
    max_subtracks: usize,
) -> usize {
    if !out_header.is_null() {
        let hdr = unsafe { &mut *out_header };
        let album_fallback = if !container.game_title.trim().is_empty() {
            container.game_title.trim().to_string()
        } else {
            Path::new(base_path)
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("NSF")
                .to_string()
        };
        copy_str_to_c_buf(Some(&album_fallback), &mut hdr.game_title);
        copy_str_to_c_buf(Some(container.artist.trim()), &mut hdr.artist);
        copy_str_to_c_buf(Some(container.copyright.trim()), &mut hdr.copyright);
        hdr.total_subtracks = container.total_subtracks;
        hdr.starting_subtrack = container.starting_subtrack;
        hdr.chip_flags = container.chip_flags.0;
        hdr.is_pal = u8::from(container.is_pal);
        hdr.payload_crc32 = container.payload_crc32;
    }

    if !out_subtracks.is_null() && max_subtracks > 0 {
        let n = container.subtracks.len().min(max_subtracks);
        let dst = unsafe { std::slice::from_raw_parts_mut(out_subtracks, n) };
        for (slot, sub) in dst.iter_mut().zip(container.subtracks.iter().take(n)) {
            let disp_title = format_nsf_subtrack_title(container, sub.index, base_path);
            slot.subtrack_index = sub.index;
            slot.track_kind = sub.kind as u8;
            slot.chip_flags = container.chip_flags.0;
            slot.duration_ms = sub.duration_ms.saturating_add(sub.fade_ms);
            slot.fade_ms = sub.fade_ms;
            slot.loop_point_ms = sub.loop_point_ms.map(|v| v as i32).unwrap_or(-1);
            copy_str_to_c_buf(Some(&disp_title), &mut slot.title);
        }
    }

    container.total_subtracks as usize
}

/// Fast zero-emulation header/chunk inspection for unindexed File Browser `[N ♪]` badges and
/// bulk unindexed directory walks (1,000+ `.nsf` files). Reads only 128 bytes for standard
/// `NESM\x1A` v1 `.nsf` files and parses `NSFE` chunks without running 6502 CPU frames.
#[no_mangle]
pub unsafe extern "C" fn uipod_rs_nsf_inspect_header_fast(
    file_path: *const c_char,
    data: *const u8,
    data_len: usize,
    out_header: *mut UipodRsNsfContainerHeader,
    out_subtracks: *mut UipodRsNsfSubtrackInfo,
    max_subtracks: usize,
) -> usize {
    let res = std::panic::catch_unwind(|| {
        let path_str = unsafe { cstr_to_str(file_path) };
        let base_path = SubtrackUri::parse(path_str).file_path;

        if let Some(cached) = lookup_scan_cache(base_path, None) {
            return write_container_to_ffi(&cached, base_path, out_header, out_subtracks, max_subtracks);
        }

        let owned: Vec<u8>;
        let bytes: &[u8] = if !data.is_null() && data_len >= 8 {
            unsafe { std::slice::from_raw_parts(data, data_len) }
        } else if !base_path.is_empty() {
            let Ok(v) = read_nsf_header_bytes_from_disk(base_path) else {
                return 0;
            };
            owned = v;
            &owned
        } else {
            return 0;
        };

        let Ok(container) = inspect_nsf_header_fast(bytes) else {
            return 0;
        };
        write_container_to_ffi(&container, base_path, out_header, out_subtracks, max_subtracks)
    });
    res.unwrap_or(0)
}

/// Scan an `.nsf` or `.nsfe` file on disk (or in memory if `data` is non-null) and return all
/// subtrack metadata records in one batch for SQLite indexing and queue expansion.
///
/// Returns total subtrack count (`> 0`) on success, or `0` on error.
#[no_mangle]
pub unsafe extern "C" fn uipod_rs_nsf_scan_container(
    file_path: *const c_char,
    data: *const u8,
    data_len: usize,
    out_header: *mut UipodRsNsfContainerHeader,
    out_subtracks: *mut UipodRsNsfSubtrackInfo,
    max_subtracks: usize,
) -> usize {
    let res = std::panic::catch_unwind(|| {
        let path_str = unsafe { cstr_to_str(file_path) };
        let base_path = SubtrackUri::parse(path_str).file_path;

        let owned: Vec<u8>;
        let bytes: &[u8] = if !data.is_null() && data_len >= 8 {
            unsafe { std::slice::from_raw_parts(data, data_len) }
        } else if !base_path.is_empty() {
            let Ok(v) = std::fs::read(base_path) else {
                return 0;
            };
            owned = v;
            &owned
        } else {
            return 0;
        };

        let Ok(container) = scan_nsf_cached(base_path, bytes) else {
            return 0;
        };

        write_container_to_ffi(&container, base_path, out_header, out_subtracks, max_subtracks)
    });
    res.unwrap_or(0)
}

/// Parse a virtual subtrack URI from C (`"/Music/Contra.nsf#3"`): writes the stripped physical
/// file path into `out_file_path` (when non-null) and returns the 0-based `subtrack_index`,
/// or `-1` if `uri` has no `.nsf#<digits>` / `.nsfe#<digits>` fragment.
#[no_mangle]
pub unsafe extern "C" fn uipod_rs_nsf_parse_uri(
    uri: *const c_char,
    out_file_path: *mut c_char,
    out_cap: usize,
) -> i32 {
    if uri.is_null() {
        return -1;
    }
    let s = unsafe { CStr::from_ptr(uri) }.to_str().unwrap_or("");
    let parsed = SubtrackUri::parse(s);
    if !out_file_path.is_null() && out_cap > 0 {
        let dst = unsafe { std::slice::from_raw_parts_mut(out_file_path as *mut u8, out_cap) };
        copy_str_to_c_buf(Some(parsed.file_path), dst);
    }
    match parsed.subtrack_index {
        Some(idx) => idx as i32,
        None => -1,
    }
}

fn extract_leading_year(copyright: &str) -> Option<i32> {
    let bytes = copyright.as_bytes();
    if bytes.len() < 4 {
        return None;
    }
    for w in bytes.windows(4) {
        if w.iter().all(|b| b.is_ascii_digit()) {
            if let Ok(yr) = std::str::from_utf8(w).ok()?.parse::<i32>() {
                if (1980..=2099).contains(&yr) {
                    return Some(yr);
                }
            }
        }
    }
    None
}

// ============================================================================
// Android JNI Exports (`dev.famiclone.uipod.UIPodActivity`)
// ============================================================================

#[cfg(target_os = "android")]
mod android_jni {
    use super::*;
    use std::ffi::c_void;

    #[no_mangle]
    pub extern "C" fn Java_dev_famiclone_uipod_UIPodActivity_nativeGetNsfChannelMuteMask(
        _env: *mut c_void,
        _cls: *mut c_void,
    ) -> i32 {
        get_nsf_channel_mute_mask() as i32
    }

    #[no_mangle]
    pub extern "C" fn Java_dev_famiclone_uipod_UIPodActivity_nativeSetNsfChannelMuteMask(
        _env: *mut c_void,
        _cls: *mut c_void,
        mask: i32,
    ) {
        set_nsf_channel_mute_mask((mask & 0xFFFF) as u16);
    }

    #[no_mangle]
    pub extern "C" fn Java_dev_famiclone_uipod_UIPodActivity_nativeGetNsfActiveChipFlags(
        _env: *mut c_void,
        _cls: *mut c_void,
    ) -> i32 {
        match get_nsf_active_chip_flags() {
            Some(flags) => flags.0 as i32,
            None => -1,
        }
    }

    #[no_mangle]
    pub extern "C" fn Java_dev_famiclone_uipod_UIPodActivity_nativeGetNsfEndlessLoop(
        _env: *mut c_void,
        _cls: *mut c_void,
    ) -> u8 {
        u8::from(get_nsf_endless_loop())
    }

    #[no_mangle]
    pub extern "C" fn Java_dev_famiclone_uipod_UIPodActivity_nativeSetNsfEndlessLoop(
        _env: *mut c_void,
        _cls: *mut c_void,
        enabled: u8,
    ) {
        set_nsf_endless_loop(enabled != 0);
    }
}
