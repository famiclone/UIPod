#![allow(clippy::missing_safety_doc)]

pub use uipod_codecs as codecs;
pub use uipod_dj as dj;
pub use uipod_jellyfin as jellyfin;
pub use uipod_lastfm as lastfm;
pub use uipod_listenbrainz as listenbrainz;
pub use uipod_navidrome as navidrome;
pub use uipod_nsf as nsf;
pub use uipod_plugins as plugins;
pub use uipod_track_intel as track_intel;
pub use uipod_ui as ui;

use std::ffi::CStr;
use std::os::raw::c_char;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, AtomicU64, AtomicU8, Ordering};
use std::sync::Mutex;
use std::time::Duration;

use uipod_dj::{
    should_respect_album_gapless, CrossfadeMode, DjConfig, DjTelemetry, DualDeckMixer,
    MaxTempoStretch, MixerPhase, TransitionLength,
};
use uipod_plugin_sdk::{PluginLiveTelemetry, PluginTrackAnalysis};
use uipod_track_intel::{
    audio_tap_clear, audio_tap_push_apu, audio_tap_push_s16, compute_track_key_hash,
    normalize_track_id, publish_live_frame, read_active_analysis, read_live_frame,
    read_synced_apu_telemetry, read_synced_s16_window, AnalyzeRequest, LiveTelemetryEngine,
    TrackIntelCoordinator, TrackKeyInput,
};

static DJ_MIXER: Mutex<Option<DualDeckMixer>> = Mutex::new(None);
static DJ_CFG_MODE: AtomicU8 = AtomicU8::new(0);
static DJ_CFG_KEEP_GAPLESS: AtomicU8 = AtomicU8::new(1);
static DJ_ACTIVE_PHASE: AtomicU8 = AtomicU8::new(0);

// Lock-free 60 FPS DJ Telemetry SeqLock for Now Playing UI
static DJ_TEL_SEQ: AtomicU32 = AtomicU32::new(0);
static DJ_TEL_WORDS: [AtomicU32; 5] = [
    AtomicU32::new(0),
    AtomicU32::new(0),
    AtomicU32::new(0),
    AtomicU32::new(0),
    AtomicU32::new(0),
];

#[inline]
unsafe fn ffi_cstr<'a>(ptr: *const c_char) -> &'a str {
    if ptr.is_null() {
        ""
    } else {
        unsafe { CStr::from_ptr(ptr) }.to_str().unwrap_or("")
    }
}

fn publish_dj_telemetry(tel: DjTelemetry) {
    let seq = DJ_TEL_SEQ.load(Ordering::Relaxed);
    let odd = (seq | 1).wrapping_add(if (seq & 1) != 0 { 2 } else { 0 });
    DJ_TEL_SEQ.store(odd, Ordering::Release);

    let w0 = (tel.active as u32)
        | ((tel.mix_kind as u32) << 8)
        | ((tel.bass_swap_flash as u32) << 16);
    DJ_TEL_WORDS[0].store(w0, Ordering::Relaxed);
    DJ_TEL_WORDS[1].store(tel.progress.to_bits(), Ordering::Relaxed);
    DJ_TEL_WORDS[2].store(tel.bpm_a.to_bits(), Ordering::Relaxed);
    DJ_TEL_WORDS[3].store(tel.bpm_b.to_bits(), Ordering::Relaxed);
    DJ_TEL_WORDS[4].store(tel.current_bpm.to_bits(), Ordering::Relaxed);

    DJ_TEL_SEQ.store(odd.wrapping_add(1), Ordering::Release);
}

fn read_dj_telemetry() -> DjTelemetry {
    for _ in 0..8 {
        let s1 = DJ_TEL_SEQ.load(Ordering::Acquire);
        if (s1 & 1) != 0 {
            std::hint::spin_loop();
            continue;
        }
        let w0 = DJ_TEL_WORDS[0].load(Ordering::Relaxed);
        let w1 = DJ_TEL_WORDS[1].load(Ordering::Relaxed);
        let w2 = DJ_TEL_WORDS[2].load(Ordering::Relaxed);
        let w3 = DJ_TEL_WORDS[3].load(Ordering::Relaxed);
        let w4 = DJ_TEL_WORDS[4].load(Ordering::Relaxed);
        std::sync::atomic::fence(Ordering::Acquire);
        if s1 == DJ_TEL_SEQ.load(Ordering::Relaxed) {
            return DjTelemetry {
                active: (w0 & 0xFF) as u8,
                mix_kind: ((w0 >> 8) & 0xFF) as u8,
                bass_swap_flash: ((w0 >> 16) & 0xFF) as u8,
                progress: f32::from_bits(w1),
                bpm_a: f32::from_bits(w2),
                bpm_b: f32::from_bits(w3),
                current_bpm: f32::from_bits(w4),
            };
        }
    }
    DjTelemetry::default()
}

fn with_mixer<R>(f: impl FnOnce(&mut DualDeckMixer) -> R, default_val: R) -> R {
    if let Ok(mut guard) = DJ_MIXER.lock() {
        let mixer = guard.get_or_insert_with(|| DualDeckMixer::new(44_100));
        let out = f(mixer);
        DJ_ACTIVE_PHASE.store(mixer.phase() as u8, Ordering::Release);
        out
    } else {
        default_val
    }
}

#[no_mangle]
pub extern "C" fn uipod_rs_dj_set_config(
    mode: u8,
    length: u8,
    max_stretch: u8,
    keep_album_gapless: i32,
    quick_cut_on_next: i32,
) {
    let cfg = DjConfig {
        mode: CrossfadeMode::from_u8(mode),
        length: TransitionLength::from_u8(length),
        max_stretch: MaxTempoStretch::from_u8(max_stretch),
        keep_album_gapless: keep_album_gapless != 0,
        quick_cut_on_next: quick_cut_on_next != 0,
    };
    DJ_CFG_MODE.store(cfg.mode as u8, Ordering::Relaxed);
    DJ_CFG_KEEP_GAPLESS.store(u8::from(cfg.keep_album_gapless), Ordering::Relaxed);
    with_mixer(
        |m| {
            m.set_config(cfg);
            if cfg.mode == CrossfadeMode::Off && m.phase() == MixerPhase::Idle {
                uipod_codecs::set_nsf_tempo_ratio(1.0);
                publish_dj_telemetry(DjTelemetry::default());
            }
        },
        (),
    );
}

#[no_mangle]
pub extern "C" fn uipod_rs_dj_get_phase() -> i32 {
    DJ_ACTIVE_PHASE.load(Ordering::Acquire) as i32
}

#[no_mangle]
pub extern "C" fn uipod_rs_dj_reset() {
    uipod_codecs::set_nsf_tempo_ratio(1.0);
    publish_dj_telemetry(DjTelemetry::default());
    with_mixer(|m| m.reset(), ());
}

#[no_mangle]
pub extern "C" fn uipod_rs_dj_flush_to_gapless_drain() {
    uipod_codecs::set_nsf_tempo_ratio(1.0);
    publish_dj_telemetry(DjTelemetry::default());
    with_mixer(|m| m.flush_to_gapless_drain(), ());
}

#[no_mangle]
pub extern "C" fn uipod_rs_dj_set_track_timing(
    deck_a_start_ms: u32,
    deck_a_duration_ms: u32,
    deck_b_duration_ms: u32,
) {
    with_mixer(
        |m| m.set_track_timing(deck_a_start_ms, deck_a_duration_ms, deck_b_duration_ms),
        (),
    );
}

#[no_mangle]
pub unsafe extern "C" fn uipod_rs_dj_should_gapless_album(
    shuffle_active: i32,
    album_a: *const c_char,
    track_a: i32,
    album_b: *const c_char,
    track_b: i32,
) -> i32 {
    let keep = DJ_CFG_KEEP_GAPLESS.load(Ordering::Relaxed) != 0;
    let aa = unsafe { ffi_cstr(album_a) };
    let ab = unsafe { ffi_cstr(album_b) };
    i32::from(should_respect_album_gapless(
        keep,
        shuffle_active != 0,
        aa,
        track_a,
        ab,
        track_b,
    ))
}

#[no_mangle]
pub unsafe extern "C" fn uipod_rs_dj_begin_deck_a(
    sample_rate: u32,
    album_a: *const c_char,
    track_a: i32,
    shuffle_active: i32,
    manual_quick_cut: i32,
) {
    let aa = unsafe { ffi_cstr(album_a) };
    uipod_codecs::set_nsf_tempo_ratio(1.0);
    publish_dj_telemetry(DjTelemetry::default());
    with_mixer(
        |m| {
            m.begin_deck_a_capture(
                sample_rate,
                aa,
                track_a,
                shuffle_active != 0,
                manual_quick_cut != 0,
            );
        },
        (),
    );
}

#[no_mangle]
pub unsafe extern "C" fn uipod_rs_dj_push_deck_a_frames(
    stereo_s16: *const i16,
    frames: usize,
) {
    if stereo_s16.is_null() || frames == 0 {
        return;
    }
    let src = unsafe { std::slice::from_raw_parts(stereo_s16, frames * 2) };
    with_mixer(|m| m.push_deck_a_frames(src), ());
}

#[no_mangle]
pub unsafe extern "C" fn uipod_rs_dj_pop_spill_s16(
    out_stereo_s16: *mut i16,
    max_frames: usize,
    out_elapsed_ms: *mut u32,
) -> usize {
    if out_stereo_s16.is_null() || max_frames == 0 {
        return 0;
    }
    let dst = unsafe { std::slice::from_raw_parts_mut(out_stereo_s16, max_frames * 2) };
    let (popped, el_ms) = with_mixer(|m| m.pop_spill_s16(dst), (0, 0));
    if !out_elapsed_ms.is_null() {
        unsafe {
            *out_elapsed_ms = el_ms;
        }
    }
    popped
}

#[no_mangle]
pub unsafe extern "C" fn uipod_rs_dj_push_deck_a(
    stereo_s16: *const i16,
    frames: usize,
    spill_out: *mut i16,
    spill_cap_frames: usize,
) -> usize {
    if stereo_s16.is_null() || frames == 0 {
        return 0;
    }
    let src = unsafe { std::slice::from_raw_parts(stereo_s16, frames * 2) };
    with_mixer(|m| m.push_deck_a_frames(src), ());
    if spill_out.is_null() || spill_cap_frames == 0 {
        return 0;
    }
    let dst = unsafe { std::slice::from_raw_parts_mut(spill_out, spill_cap_frames * 2) };
    let (popped, _) = with_mixer(|m| m.pop_spill_s16(dst), (0, 0));
    popped
}

#[no_mangle]
pub unsafe extern "C" fn uipod_rs_dj_finish_deck_a_begin_deck_b(
    album_b: *const c_char,
    track_b: i32,
    deck_b_is_nsf: i32,
) {
    let ab = unsafe { ffi_cstr(album_b) };
    with_mixer(
        |m| m.finish_deck_a_and_begin_deck_b(ab, track_b, deck_b_is_nsf != 0),
        (),
    );
}

#[no_mangle]
pub unsafe extern "C" fn uipod_rs_dj_push_deck_b(
    stereo_s16: *const i16,
    frames: usize,
    is_eof: i32,
) -> i32 {
    let src: &[i16] = if stereo_s16.is_null() || frames == 0 {
        &[]
    } else {
        unsafe { std::slice::from_raw_parts(stereo_s16, frames * 2) }
    };
    i32::from(with_mixer(|m| m.push_deck_b_frames(src, is_eof != 0), false))
}

#[no_mangle]
pub unsafe extern "C" fn uipod_rs_dj_render_s16(
    out_stereo_s16: *mut i16,
    max_frames: usize,
    out_midpoint_crossed: *mut i32,
) -> usize {
    unsafe {
        uipod_rs_dj_render_s16_timed(
            out_stereo_s16,
            max_frames,
            out_midpoint_crossed,
            std::ptr::null_mut(),
        )
    }
}

#[no_mangle]
pub unsafe extern "C" fn uipod_rs_dj_render_s16_timed(
    out_stereo_s16: *mut i16,
    max_frames: usize,
    out_midpoint_crossed: *mut i32,
    out_elapsed_ms: *mut u32,
) -> usize {
    if out_stereo_s16.is_null() || max_frames == 0 {
        return 0;
    }
    let dst = unsafe { std::slice::from_raw_parts_mut(out_stereo_s16, max_frames * 2) };
    let (written, mid, el_ms, tel, nsf_tempo) = with_mixer(
        |m| {
            let (w, crossed, chunk_el) = m.render_s16_timed(dst);
            let t = m.telemetry();
            let nsf_r = if m.phase() == MixerPhase::Mixing {
                m.recommended_nsf_tempo_b()
            } else {
                1.0
            };
            (w, crossed, chunk_el, t, nsf_r)
        },
        (0, false, 0, DjTelemetry::default(), 1.0),
    );

    uipod_codecs::set_nsf_tempo_ratio(nsf_tempo);
    publish_dj_telemetry(tel);

    if !out_midpoint_crossed.is_null() {
        unsafe {
            *out_midpoint_crossed = i32::from(mid);
        }
    }
    if !out_elapsed_ms.is_null() {
        unsafe {
            *out_elapsed_ms = el_ms;
        }
    }
    written
}

#[no_mangle]
pub unsafe extern "C" fn uipod_rs_dj_get_telemetry(out_tel: *mut DjTelemetry) -> i32 {
    if out_tel.is_null() {
        return 0;
    }
    let tel = read_dj_telemetry();
    unsafe {
        *out_tel = tel;
    }
    i32::from(tel.active != 0)
}

#[no_mangle]
pub unsafe extern "C" fn uipod_rs_dj_get_telemetry_synced(
    out_tel: *mut DjTelemetry,
    downstream_frames: usize,
) -> i32 {
    if out_tel.is_null() {
        return 0;
    }
    let tel = if let Ok(guard) = DJ_MIXER.try_lock() {
        if let Some(m) = guard.as_ref() {
            let synced = m.playhead_at_delay_frames(downstream_frames).telemetry;
            publish_dj_telemetry(synced);
            synced
        } else {
            DjTelemetry::default()
        }
    } else {
        read_dj_telemetry()
    };
    unsafe {
        *out_tel = tel;
    }
    i32::from(tel.active != 0)
}

#[no_mangle]
pub extern "C" fn uipod_rs_dj_set_bpm_hints(
    bpm_a: f32,
    conf_a: f32,
    bpm_b: f32,
    conf_b: f32,
) {
    let ha = if bpm_a > 0.0 && conf_a > 0.0 {
        Some((bpm_a, conf_a))
    } else {
        None
    };
    let hb = if bpm_b > 0.0 && conf_b > 0.0 {
        Some((bpm_b, conf_b))
    } else {
        None
    };
    with_mixer(|m| m.set_deck_bpm_hints(ha, hb), ());
}

// ============================================================================
// Unified Track Intelligence C FFI (`uipod_rs_intel_*`)
// ============================================================================

pub type CIntelPcmFallbackFn =
    unsafe extern "C" fn(out_stereo: *mut i16, max_frames: usize, out_sr: *mut u32) -> usize;

static INTEL_PCM_CB: Mutex<Option<CIntelPcmFallbackFn>> = Mutex::new(None);
static LIVE_TELEMETRY_ENG: Mutex<Option<LiveTelemetryEngine>> = Mutex::new(None);
static LAST_LIVE_TICK_MS: AtomicU64 = AtomicU64::new(0);

fn invoke_c_pcm_fallback(out_stereo: &mut [i16]) -> (usize, u32) {
    let cb_opt = INTEL_PCM_CB.lock().ok().and_then(|g| *g);
    let Some(cb) = cb_opt else {
        return (0, 44_100);
    };
    let max_frames = out_stereo.len() / 2;
    let mut sr = 44_100u32;
    let got = unsafe { cb(out_stereo.as_mut_ptr(), max_frames, &mut sr) };
    (got.min(max_frames), sr.max(8_000))
}

#[allow(clippy::too_many_arguments)]
fn build_analyze_request(
    raw_path: &str,
    file_size: u64,
    mtime_sec: i64,
    cue_subtrack: i32,
    slice_start_ms: u32,
    duration_ms: u32,
    sample_rate: u32,
    title: &str,
    artist: &str,
    album: &str,
    genre: &str,
    high_priority: bool,
) -> AnalyzeRequest {
    let cue_opt = if (0..=255).contains(&cue_subtrack) {
        Some(cue_subtrack as u16)
    } else {
        None
    };
    let norm = normalize_track_id(raw_path, cue_opt);
    let eff_size = if file_size > 0 {
        file_size
    } else {
        duration_ms as u64
    };
    let key_hash = if norm.canonical_source_id.is_empty() {
        0
    } else {
        compute_track_key_hash(&TrackKeyInput {
            canonical_source_id: &norm.canonical_source_id,
            file_size: eff_size,
            mtime_sec,
            subtrack_index: norm.subtrack_index,
        })
    };

    AnalyzeRequest {
        key_hash,
        fs_path: norm.fs_path,
        subtrack_index: norm.subtrack_index,
        slice_start_ms,
        duration_ms,
        sample_rate_hint: sample_rate.max(44_100),
        title: title.to_string(),
        artist: artist.to_string(),
        album: album.to_string(),
        genre: genre.to_string(),
        tag_tbpm: None,
        high_priority,
    }
}

#[no_mangle]
pub unsafe extern "C" fn uipod_rs_intel_init(
    db_path: *const c_char,
    pcm_fallback: Option<CIntelPcmFallbackFn>,
) {
    if let Ok(mut g) = INTEL_PCM_CB.lock() {
        *g = pcm_fallback;
    }
    let path_str = unsafe { ffi_cstr(db_path) };
    if !path_str.is_empty() {
        TrackIntelCoordinator::global().init(
            PathBuf::from(path_str),
            Some(invoke_c_pcm_fallback),
        );
    }
}

#[no_mangle]
pub unsafe extern "C" fn uipod_rs_intel_compute_key(
    raw_path: *const c_char,
    file_size: u64,
    mtime_sec: i64,
    cue_subtrack: i32,
    duration_ms: u32,
) -> u64 {
    let p = unsafe { ffi_cstr(raw_path) };
    if p.is_empty() {
        return 0;
    }
    let cue_opt = if (0..=255).contains(&cue_subtrack) {
        Some(cue_subtrack as u16)
    } else {
        None
    };
    let norm = normalize_track_id(p, cue_opt);
    let eff_size = if file_size > 0 {
        file_size
    } else {
        duration_ms as u64
    };
    compute_track_key_hash(&TrackKeyInput {
        canonical_source_id: &norm.canonical_source_id,
        file_size: eff_size,
        mtime_sec,
        subtrack_index: norm.subtrack_index,
    })
}

#[no_mangle]
pub unsafe extern "C" fn uipod_rs_intel_on_track_change(
    raw_path: *const c_char,
    file_size: u64,
    mtime_sec: i64,
    cue_subtrack: i32,
    slice_start_ms: u32,
    duration_ms: u32,
    sample_rate: u32,
    title: *const c_char,
    artist: *const c_char,
    album: *const c_char,
    genre: *const c_char,
) -> i32 {
    let req = build_analyze_request(
        unsafe { ffi_cstr(raw_path) },
        file_size,
        mtime_sec,
        cue_subtrack,
        slice_start_ms,
        duration_ms,
        sample_rate,
        unsafe { ffi_cstr(title) },
        unsafe { ffi_cstr(artist) },
        unsafe { ffi_cstr(album) },
        unsafe { ffi_cstr(genre) },
        false,
    );
    i32::from(TrackIntelCoordinator::global().on_track_start(req))
}

#[no_mangle]
pub unsafe extern "C" fn uipod_rs_intel_prefetch_track(
    raw_path: *const c_char,
    file_size: u64,
    mtime_sec: i64,
    cue_subtrack: i32,
    slice_start_ms: u32,
    duration_ms: u32,
    sample_rate: u32,
    title: *const c_char,
    artist: *const c_char,
    album: *const c_char,
    genre: *const c_char,
) {
    let req = build_analyze_request(
        unsafe { ffi_cstr(raw_path) },
        file_size,
        mtime_sec,
        cue_subtrack,
        slice_start_ms,
        duration_ms,
        sample_rate,
        unsafe { ffi_cstr(title) },
        unsafe { ffi_cstr(artist) },
        unsafe { ffi_cstr(album) },
        unsafe { ffi_cstr(genre) },
        false,
    );
    TrackIntelCoordinator::global().prefetch_track(req);
}

#[no_mangle]
pub extern "C" fn uipod_rs_intel_is_ready(key_hash: u64) -> i32 {
    i32::from(TrackIntelCoordinator::global().is_ready(key_hash))
}

#[no_mangle]
pub unsafe extern "C" fn uipod_rs_intel_ensure_current(
    raw_path: *const c_char,
    file_size: u64,
    mtime_sec: i64,
    cue_subtrack: i32,
    slice_start_ms: u32,
    duration_ms: u32,
    sample_rate: u32,
    title: *const c_char,
    artist: *const c_char,
    album: *const c_char,
    genre: *const c_char,
    timeout_ms: u32,
) -> i32 {
    let req = build_analyze_request(
        unsafe { ffi_cstr(raw_path) },
        file_size,
        mtime_sec,
        cue_subtrack,
        slice_start_ms,
        duration_ms,
        sample_rate,
        unsafe { ffi_cstr(title) },
        unsafe { ffi_cstr(artist) },
        unsafe { ffi_cstr(album) },
        unsafe { ffi_cstr(genre) },
        true,
    );
    i32::from(
        TrackIntelCoordinator::global()
            .ensure_analyzed(req, Duration::from_millis(timeout_ms as u64))
            .is_some(),
    )
}

#[no_mangle]
pub extern "C" fn uipod_rs_intel_clear_active() {
    TrackIntelCoordinator::global().clear_active();
    audio_tap_clear();
}

#[no_mangle]
pub unsafe extern "C" fn uipod_rs_intel_lookup_bpm(
    key_hash: u64,
    out_bpm: *mut f32,
    out_conf: *mut f32,
) -> i32 {
    if let Some(rec) = TrackIntelCoordinator::global().lookup(key_hash) {
        if !out_bpm.is_null() {
            unsafe { *out_bpm = rec.bpm };
        }
        if !out_conf.is_null() {
            unsafe { *out_conf = rec.bpm_confidence };
        }
        1
    } else {
        0
    }
}

/// Wait-free audio sink tap called directly from the ALSA / Android SDL writer thread.
/// Never locks a `Mutex`, never allocates, never blocks.
#[no_mangle]
pub unsafe extern "C" fn uipod_rs_intel_audio_tap_push(
    stereo_s16: *const i16,
    frames: usize,
    sample_rate: u32,
    output_delay_us: u32,
    now_ms: u64,
) {
    if stereo_s16.is_null() || frames == 0 {
        return;
    }
    let slice = unsafe { std::slice::from_raw_parts(stereo_s16, frames * 2) };
    audio_tap_push_s16(slice, sample_rate, output_delay_us, now_ms);
    if uipod_codecs::get_nsf_active_chip_flags().is_some() {
        let tel = uipod_codecs::get_nsf_apu_telemetry();
        audio_tap_push_apu(&tel);
    }
}

/// Non-blocking 60–120 Hz live telemetry stepper (`try_lock` guarded so concurrent UI/Viz callers
/// never block each other, publishing `TrackLiveFrame` via atomic `SeqLock`).
#[no_mangle]
pub extern "C" fn uipod_rs_intel_step_live(
    status: u8,
    track_key_hash: u64,
    playhead_ms: u32,
    duration_ms: u32,
    output_latency_ms: f32,
    now_ms: u64,
) {
    let prev_ms = LAST_LIVE_TICK_MS.load(Ordering::Relaxed);
    if prev_ms != 0 && now_ms > prev_ms && (now_ms - prev_ms) < 7 {
        return;
    }
    let Ok(mut guard) = LIVE_TELEMETRY_ENG.try_lock() else {
        return;
    };
    let dt_sec = if prev_ms == 0 || now_ms <= prev_ms {
        1.0 / 60.0
    } else {
        (((now_ms - prev_ms).min(100)) as f32) * 0.001
    };
    LAST_LIVE_TICK_MS.store(now_ms, Ordering::Relaxed);

    let eng = guard.get_or_insert_with(LiveTelemetryEngine::new);
    let mut pcm_win = [0i16; 1024 * 2];
    let (got_frames, sr, spk_frame) = if status == 1 {
        read_synced_s16_window(&mut pcm_win, now_ms)
    } else {
        (0, 44_100, 0)
    };
    let apu_opt = if status == 1 && uipod_codecs::get_nsf_active_chip_flags().is_some() {
        read_synced_apu_telemetry(spk_frame)
    } else {
        None
    };
    let active_rec = read_active_analysis();
    let frame = eng.step(
        status,
        track_key_hash,
        playhead_ms,
        duration_ms,
        output_latency_ms,
        dt_sec,
        &pcm_win[..got_frames * 2],
        sr,
        apu_opt,
        active_rec.as_ref(),
    );
    publish_live_frame(&frame);
}

#[no_mangle]
pub unsafe extern "C" fn uipod_rs_intel_get_active_analysis(
    out: *mut PluginTrackAnalysis,
) -> bool {
    if out.is_null() {
        return false;
    }
    if let Some(rec) = read_active_analysis() {
        unsafe {
            *out = rec;
        }
        true
    } else {
        false
    }
}

#[no_mangle]
pub unsafe extern "C" fn uipod_rs_intel_get_live_telemetry(
    out: *mut PluginLiveTelemetry,
) -> bool {
    if out.is_null() {
        return false;
    }
    unsafe {
        *out = read_live_frame();
    }
    true
}
