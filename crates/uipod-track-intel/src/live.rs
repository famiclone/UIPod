//! Lock-free Tier-2 Real-Time Audio Telemetry (`live.rs`):
//! - Wait-free `16,384`-frame stereo SPSC PCM tap (`64 KB` BSS, zero `Mutex`, zero heap allocs).
//! - Latency-compensated `32`-slot `.nsf` APU ring matching speaker / Bluetooth A2DP output delay.
//! - Atomic `SeqLock` publication of [`TrackAnalysisRecord`] (`512 B`) and [`TrackLiveFrame`] (`592 B`).
//! - Zero-allocation `1024`-point radix-2 Hann FFT, `64`-bin log spectrum + peak hold, `256`-sample
//!   gain-normalized scope waveform, normalized `kick_impulse` / `snare_impulse`, and monotonic
//!   bar/beat phase grid anchored to `TrackAnalysisRecord`.

use std::f32::consts::{PI, TAU};
use std::sync::atomic::{fence, AtomicU32, AtomicU64, Ordering};

use uipod_nsf::ApuFrameTelemetry;
use uipod_plugin_sdk::{
    beat_flag, PluginApuTelemetry, PluginChannelState, PluginLiveTelemetry,
    TRACK_ANALYSIS_RECORD_BYTES,
};

use crate::analyzer::{fold_bpm_60_200, DEFAULT_BPM};
use crate::db::{decode_record_bytes, encode_record_bytes, TrackAnalysisRecord};

pub type TrackLiveFrame = PluginLiveTelemetry;

const SPSC_FRAMES: usize = 16_384; // ~371 ms @ 44.1 kHz (covers ALSA + AAudio + Bluetooth A2DP)
const SPSC_MASK: usize = SPSC_FRAMES - 1;
const DISPLAY_LEAD_US: u32 = 18_000; // ~18 ms scanout lead

const APU_RING_SLOTS: usize = 32;
const APU_RING_MASK: usize = APU_RING_SLOTS - 1;
const APU_WORDS_PER_SLOT: usize = 27; // frame_pos(2) + header(1) + 8 * 3(24)

const ANALYSIS_WORDS: usize = TRACK_ANALYSIS_RECORD_BYTES / 4; // 128 u32 words = 512 bytes
const LIVE_WORDS: usize = core::mem::size_of::<TrackLiveFrame>() / 4; // 148 u32 words = 592 bytes

const FFT_SIZE: usize = 1024;
const FFT_BINS: usize = FFT_SIZE / 2;

#[allow(clippy::declare_interior_mutable_const)]
const ZERO_U32: AtomicU32 = AtomicU32::new(0);

// ---- Wait-Free SPSC Audio Sink Ring ----
static SPSC_STEREO_RING: [AtomicU32; SPSC_FRAMES] = [ZERO_U32; SPSC_FRAMES];
static SPSC_WRITE_POS: AtomicU64 = AtomicU64::new(0);
static SPSC_SAMPLE_RATE: AtomicU32 = AtomicU32::new(44_100);
static SPSC_DELAY_US: AtomicU32 = AtomicU32::new(25_000);
static SPSC_STAMP_MS: AtomicU64 = AtomicU64::new(0);

// ---- Wait-Free Delay-Tagged .nsf APU Ring ----
static APU_RING_WRITE: AtomicU32 = AtomicU32::new(0);
static APU_RING_SEQ: [AtomicU32; APU_RING_SLOTS] = [ZERO_U32; APU_RING_SLOTS];
static APU_RING_DATA: [AtomicU32; APU_RING_SLOTS * APU_WORDS_PER_SLOT] =
    [ZERO_U32; APU_RING_SLOTS * APU_WORDS_PER_SLOT];

// ---- Lock-Free SeqLock Slots for Active Analysis & Live Frame ----
static ANALYSIS_SEQ: AtomicU32 = AtomicU32::new(0);
static ANALYSIS_DATA: [AtomicU32; ANALYSIS_WORDS] = [ZERO_U32; ANALYSIS_WORDS];

static LIVE_SEQ: AtomicU32 = AtomicU32::new(0);
static LIVE_DATA: [AtomicU32; LIVE_WORDS] = [ZERO_U32; LIVE_WORDS];

/// Push pre-volume interleaved `i16` stereo frames from the real-time audio thread.
/// Completely wait-free: zero locks, zero allocations, zero syscalls.
pub fn audio_tap_push_s16(
    stereo_s16: &[i16],
    sample_rate: u32,
    output_delay_us: u32,
    now_ms: u64,
) {
    let frames = stereo_s16.len() / 2;
    if frames == 0 {
        return;
    }
    if sample_rate >= 8_000 {
        SPSC_SAMPLE_RATE.store(sample_rate, Ordering::Relaxed);
    }
    SPSC_DELAY_US.store(output_delay_us, Ordering::Relaxed);
    SPSC_STAMP_MS.store(now_ms, Ordering::Relaxed);

    let mut w = SPSC_WRITE_POS.load(Ordering::Relaxed);
    for i in 0..frames {
        let l = stereo_s16[i * 2] as u16 as u32;
        let r = stereo_s16[i * 2 + 1] as u16 as u32;
        let packed = l | (r << 16);
        SPSC_STEREO_RING[(w as usize) & SPSC_MASK].store(packed, Ordering::Relaxed);
        w = w.wrapping_add(1);
    }
    SPSC_WRITE_POS.store(w, Ordering::Release);
}

/// Reset the SPSC audio tap (e.g. on stop).
pub fn audio_tap_clear() {
    SPSC_DELAY_US.store(0, Ordering::Relaxed);
}

/// Push a hardware `.nsf` APU telemetry snapshot tagged with the current SPSC write frame
/// so visualizers & rhythm games read APU channel states aligned to speaker output latency.
pub fn audio_tap_push_apu(tel: &ApuFrameTelemetry) {
    let slot_idx = (APU_RING_WRITE.load(Ordering::Relaxed) as usize) & APU_RING_MASK;
    let seq = APU_RING_SEQ[slot_idx].load(Ordering::Relaxed);
    let odd = (seq | 1).wrapping_add(if (seq & 1) != 0 { 2 } else { 0 });
    APU_RING_SEQ[slot_idx].store(odd, Ordering::Release);

    let w_pos = SPSC_WRITE_POS.load(Ordering::Relaxed);
    let base = slot_idx * APU_WORDS_PER_SLOT;
    APU_RING_DATA[base].store((w_pos & 0xFFFF_FFFF) as u32, Ordering::Relaxed);
    APU_RING_DATA[base + 1].store((w_pos >> 32) as u32, Ordering::Relaxed);

    let hdr = (tel.channel_count as u32)
        | ((tel.chip_flags as u32) << 8)
        | ((tel.mute_mask as u32) << 16);
    APU_RING_DATA[base + 2].store(hdr, Ordering::Relaxed);

    for (c, ch) in tel.channels.iter().enumerate() {
        let off = base + 3 + c * 3;
        let flags = (ch.active as u32)
            | ((ch.muted as u32) << 8)
            | ((ch.onset_trigger as u32) << 16)
            | ((ch.duty_or_mode as u32) << 24);
        APU_RING_DATA[off].store(flags, Ordering::Relaxed);
        APU_RING_DATA[off + 1].store(ch.volume.to_bits(), Ordering::Relaxed);
        APU_RING_DATA[off + 2].store(ch.freq_hz.to_bits(), Ordering::Relaxed);
    }

    APU_RING_SEQ[slot_idx].store(odd.wrapping_add(1), Ordering::Release);
    APU_RING_WRITE.fetch_add(1, Ordering::Release);
}

/// Interpolate live speaker / Bluetooth A2DP output delay in microseconds at `now_ms`.
#[inline]
pub fn interpolated_output_delay_us(now_ms: u64) -> u32 {
    let base_us = SPSC_DELAY_US.load(Ordering::Relaxed);
    let stamp = SPSC_STAMP_MS.load(Ordering::Relaxed);
    if stamp == 0 || now_ms <= stamp {
        return base_us;
    }
    let elapsed_us = ((now_ms - stamp).min(250) as u32) * 1_000;
    base_us.saturating_sub(elapsed_us)
}

/// Read up to `out_stereo.len() / 2` latency-compensated stereo `i16` frames from the SPSC ring.
/// Returns `(frames_copied, sample_rate, speaker_frame_pos)`.
pub fn read_synced_s16_window(out_stereo: &mut [i16], now_ms: u64) -> (usize, u32, u64) {
    let sr = SPSC_SAMPLE_RATE.load(Ordering::Relaxed).max(8_000);
    let max_frames = (out_stereo.len() / 2).min(SPSC_FRAMES / 2);
    if max_frames == 0 {
        return (0, sr, 0);
    }

    let w = SPSC_WRITE_POS.load(Ordering::Acquire);
    if w == 0 {
        return (0, sr, 0);
    }

    let raw_dly_us = interpolated_output_delay_us(now_ms);
    let eff_dly_us = raw_dly_us.saturating_sub(DISPLAY_LEAD_US);
    let max_delay = (SPSC_FRAMES - max_frames - 512) as u64;
    let delay_frames = (((eff_dly_us as u64) * (sr as u64)) / 1_000_000).min(max_delay);
    if w <= delay_frames {
        return (0, sr, 0);
    }

    let speaker_frame = w - delay_frames;
    let end = (speaker_frame + (max_frames as u64) / 2).min(w);
    let n = (max_frames as u64).min(end) as usize;
    let start = end - (n as u64);

    for i in 0..n {
        let idx = ((start + i as u64) as usize) & SPSC_MASK;
        let packed = SPSC_STEREO_RING[idx].load(Ordering::Relaxed);
        out_stereo[i * 2] = (packed & 0xFFFF) as u16 as i16;
        out_stereo[i * 2 + 1] = ((packed >> 16) & 0xFFFF) as u16 as i16;
    }

    (n, sr, speaker_frame)
}

fn read_apu_slot(slot_idx: usize) -> Option<(u64, PluginApuTelemetry)> {
    for _ in 0..4 {
        let s1 = APU_RING_SEQ[slot_idx].load(Ordering::Acquire);
        if s1 == 0 || (s1 & 1) != 0 {
            return None;
        }
        let base = slot_idx * APU_WORDS_PER_SLOT;
        let mut words = [0u32; APU_WORDS_PER_SLOT];
        for i in 0..APU_WORDS_PER_SLOT {
            words[i] = APU_RING_DATA[base + i].load(Ordering::Relaxed);
        }
        fence(Ordering::Acquire);
        if s1 == APU_RING_SEQ[slot_idx].load(Ordering::Relaxed) {
            let fpos = (words[0] as u64) | ((words[1] as u64) << 32);
            let hdr = words[2];
            let channel_count = (hdr & 0xFF) as u8;
            if channel_count == 0 {
                return None;
            }
            let mut channels = [PluginChannelState::default(); 8];
            for (c, ch) in channels.iter_mut().enumerate() {
                let off = 3 + c * 3;
                let flg = words[off];
                *ch = PluginChannelState {
                    active: (flg & 0xFF) as u8,
                    muted: ((flg >> 8) & 0xFF) as u8,
                    onset_trigger: ((flg >> 16) & 0xFF) as u8,
                    duty_or_mode: ((flg >> 24) & 0xFF) as u8,
                    volume: f32::from_bits(words[off + 1]),
                    freq_hz: f32::from_bits(words[off + 2]),
                };
            }
            return Some((
                fpos,
                PluginApuTelemetry {
                    has_apu_telemetry: 1,
                    channel_count,
                    chip_flags: ((hdr >> 8) & 0xFF) as u8,
                    reserved: 0,
                    mute_mask: ((hdr >> 16) & 0xFFFF) as u16,
                    pad: 0,
                    channels,
                },
            ));
        }
    }
    None
}

/// Read the `.nsf` APU telemetry snapshot closest to `speaker_frame`.
pub fn read_synced_apu_telemetry(speaker_frame: u64) -> Option<PluginApuTelemetry> {
    let w = APU_RING_WRITE.load(Ordering::Acquire);
    if w == 0 {
        return None;
    }
    let mut best: Option<(u64, PluginApuTelemetry)> = None;
    let count = (w as usize).min(APU_RING_SLOTS);
    for back in 0..count {
        let slot = (w.wrapping_sub(1 + back as u32) as usize) & APU_RING_MASK;
        if let Some((fpos, tel)) = read_apu_slot(slot) {
            let dist = speaker_frame.abs_diff(fpos);
            if best.as_ref().map(|(bd, _)| dist < *bd).unwrap_or(true) {
                best = Some((dist, tel));
            }
        }
    }
    best.and_then(|(dist, tel)| if dist <= 32_768 { Some(tel) } else { None })
}

/// Publish (or clear) the active [`TrackAnalysisRecord`] via atomic `SeqLock`.
pub fn publish_active_analysis(rec: Option<&TrackAnalysisRecord>) {
    let bytes = match rec {
        Some(r) => encode_record_bytes(r),
        None => [0u8; TRACK_ANALYSIS_RECORD_BYTES],
    };
    let seq = ANALYSIS_SEQ.load(Ordering::Relaxed);
    let odd = (seq | 1).wrapping_add(if (seq & 1) != 0 { 2 } else { 0 });
    ANALYSIS_SEQ.store(odd, Ordering::Release);

    for i in 0..ANALYSIS_WORDS {
        let off = i * 4;
        let w = u32::from_le_bytes([bytes[off], bytes[off + 1], bytes[off + 2], bytes[off + 3]]);
        ANALYSIS_DATA[i].store(w, Ordering::Relaxed);
    }

    ANALYSIS_SEQ.store(odd.wrapping_add(1), Ordering::Release);
}

/// Read the active [`TrackAnalysisRecord`] lock-free (`None` if cleared or unpopulated).
pub fn read_active_analysis() -> Option<TrackAnalysisRecord> {
    for _ in 0..8 {
        let s1 = ANALYSIS_SEQ.load(Ordering::Acquire);
        if s1 == 0 || (s1 & 1) != 0 {
            core::hint::spin_loop();
            continue;
        }
        let mut bytes = [0u8; TRACK_ANALYSIS_RECORD_BYTES];
        for i in 0..ANALYSIS_WORDS {
            let w = ANALYSIS_DATA[i].load(Ordering::Relaxed).to_le_bytes();
            let off = i * 4;
            bytes[off..off + 4].copy_from_slice(&w);
        }
        fence(Ordering::Acquire);
        if s1 == ANALYSIS_SEQ.load(Ordering::Relaxed) {
            return decode_record_bytes(&bytes);
        }
    }
    None
}

/// Publish a 60+ FPS [`TrackLiveFrame`] lock-free via atomic `SeqLock`.
pub fn publish_live_frame(frame: &TrackLiveFrame) {
    let seq = LIVE_SEQ.load(Ordering::Relaxed);
    let odd = (seq | 1).wrapping_add(if (seq & 1) != 0 { 2 } else { 0 });
    LIVE_SEQ.store(odd, Ordering::Release);

    // SAFETY: `TrackLiveFrame` (`PluginLiveTelemetry`) is `#[repr(C)]`, `Copy`, 8-byte aligned,
    // and its size (`592` bytes) is an exact multiple of 4 (`148 * 4`), with zero uninitialized padding.
    let src_words: &[u32; LIVE_WORDS] = unsafe { &*(frame as *const TrackLiveFrame as *const [u32; LIVE_WORDS]) };
    for i in 0..LIVE_WORDS {
        LIVE_DATA[i].store(src_words[i], Ordering::Relaxed);
    }

    LIVE_SEQ.store(odd.wrapping_add(1), Ordering::Release);
}

/// Read the latest [`TrackLiveFrame`] lock-free (`< 200 ns`, zero mutex contention).
pub fn read_live_frame() -> TrackLiveFrame {
    for _ in 0..8 {
        let s1 = LIVE_SEQ.load(Ordering::Acquire);
        if s1 == 0 {
            return TrackLiveFrame::default();
        }
        if (s1 & 1) != 0 {
            core::hint::spin_loop();
            continue;
        }
        let mut words = [0u64; LIVE_WORDS / 2];
        let u32_dst: &mut [u32; LIVE_WORDS] =
            unsafe { &mut *(&mut words as *mut [u64; LIVE_WORDS / 2] as *mut [u32; LIVE_WORDS]) };
        for i in 0..LIVE_WORDS {
            u32_dst[i] = LIVE_DATA[i].load(Ordering::Relaxed);
        }
        fence(Ordering::Acquire);
        if s1 == LIVE_SEQ.load(Ordering::Relaxed) {
            return unsafe { core::ptr::read(&words as *const _ as *const TrackLiveFrame) };
        }
    }
    TrackLiveFrame::default()
}

/// Stateful zero-allocation real-time DSP & beat-grid tracker producing [`TrackLiveFrame`]s.
pub struct LiveTelemetryEngine {
    hann: [f32; FFT_SIZE],
    bit_rev: [u16; FFT_SIZE],
    twiddle_re: [f32; FFT_BINS],
    twiddle_im: [f32; FFT_BINS],
    fft_re: [f32; FFT_SIZE],
    fft_im: [f32; FFT_SIZE],
    mag: [f32; FFT_BINS],

    spec_smooth: [f32; 64],
    spec_peak: [f32; 64],
    bands_smooth: [f32; 5],
    wave_rms_avg: f32,

    vu_l: f32,
    vu_r: f32,
    vu_peak_l: f32,
    vu_peak_r: f32,

    // Real-time kick/snare impulse filters
    hp40_s: f32,
    lp150_s: f32,
    hp160_s: f32,
    lp2200_s: f32,
    prev_k_log: f32,
    prev_s_log: f32,
    kick_imp_smooth: f32,
    snare_imp_smooth: f32,

    // Monotonic beat/bar clock state
    last_track_hash: u64,
    monotonic_beats: f64,
    clock_initialized: bool,
    generation: u32,
}

impl Default for LiveTelemetryEngine {
    fn default() -> Self {
        Self::new()
    }
}

impl LiveTelemetryEngine {
    pub fn new() -> Self {
        let mut hann = [0.0f32; FFT_SIZE];
        for (i, w) in hann.iter_mut().enumerate() {
            *w = 0.5 * (1.0 - (TAU * (i as f32) / (FFT_SIZE as f32)).cos());
        }
        let bits = FFT_SIZE.trailing_zeros();
        let mut bit_rev = [0u16; FFT_SIZE];
        for (i, slot) in bit_rev.iter_mut().enumerate() {
            *slot = ((i as u32).reverse_bits() >> (32 - bits)) as u16;
        }
        let mut twiddle_re = [0.0f32; FFT_BINS];
        let mut twiddle_im = [0.0f32; FFT_BINS];
        for k in 0..FFT_BINS {
            let angle = -TAU * (k as f32) / (FFT_SIZE as f32);
            twiddle_re[k] = angle.cos();
            twiddle_im[k] = angle.sin();
        }

        Self {
            hann,
            bit_rev,
            twiddle_re,
            twiddle_im,
            fft_re: [0.0; FFT_SIZE],
            fft_im: [0.0; FFT_SIZE],
            mag: [0.0; FFT_BINS],
            spec_smooth: [0.0; 64],
            spec_peak: [0.0; 64],
            bands_smooth: [0.0; 5],
            wave_rms_avg: 0.12,
            vu_l: 0.0,
            vu_r: 0.0,
            vu_peak_l: 0.0,
            vu_peak_r: 0.0,
            hp40_s: 0.0,
            lp150_s: 0.0,
            hp160_s: 0.0,
            lp2200_s: 0.0,
            prev_k_log: 0.0,
            prev_s_log: 0.0,
            kick_imp_smooth: 0.0,
            snare_imp_smooth: 0.0,
            last_track_hash: 0,
            monotonic_beats: 0.0,
            clock_initialized: false,
            generation: 0,
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub fn step(
        &mut self,
        status: u8,
        track_key_hash: u64,
        playhead_ms: u32,
        duration_ms: u32,
        output_latency_ms: f32,
        dt_sec: f32,
        pcm_stereo: &[i16],
        sample_rate: u32,
        apu_opt: Option<PluginApuTelemetry>,
        analysis_opt: Option<&TrackAnalysisRecord>,
    ) -> TrackLiveFrame {
        let dt = dt_sec.clamp(1.0 / 240.0, 0.10);
        self.generation = self.generation.wrapping_add(1);
        if self.generation == 0 {
            self.generation = 1;
        }

        if track_key_hash != self.last_track_hash {
            self.last_track_hash = track_key_hash;
            self.clock_initialized = false;
            self.prev_k_log = 0.0;
            self.prev_s_log = 0.0;
        }

        let matched_rec = analysis_opt.filter(|r| r.is_valid() && r.key_hash == track_key_hash);
        let active_bpm = if status == 1 {
            matched_rec
                .map(|r| fold_bpm_60_200(r.bpm))
                .unwrap_or(DEFAULT_BPM)
        } else {
            DEFAULT_BPM
        };
        let bpm_conf = matched_rec.map(|r| r.bpm_confidence).unwrap_or(0.0);
        let structural_energy = matched_rec
            .map(|r| r.contour_at_ms(playhead_ms))
            .unwrap_or(0.5);

        // Advance monotonic beat/bar phase clock
        let prev_beats = self.monotonic_beats;
        let mut seek_jumped = false;
        if status == 1 && matched_rec.is_some() {
            let rec = matched_rec.unwrap();
            let offset_ms = rec.first_beat_offset_ms as f64;
            let target_beats =
                ((playhead_ms as f64) - offset_ms) * (active_bpm as f64 / 60_000.0);
            if !self.clock_initialized {
                self.monotonic_beats = target_beats.max(0.0);
                self.clock_initialized = true;
                seek_jumped = true;
            } else {
                let err = target_beats - self.monotonic_beats;
                if err.abs() > 2.5 {
                    // User seeked or track restarted
                    self.monotonic_beats = target_beats.max(0.0);
                    seek_jumped = true;
                } else {
                    // Smooth frequency-steered PLL slew (never steps backward!)
                    let trim = (err * 2.8).clamp(-0.18, 0.22);
                    let step_b = ((dt as f64) * (active_bpm as f64) / 60.0) * (1.0 + trim);
                    self.monotonic_beats += step_b.max(0.0);
                }
            }
        } else {
            let step_b = (dt as f64) * (active_bpm as f64) / 60.0;
            if !self.clock_initialized {
                self.clock_initialized = true;
                seek_jumped = true;
            }
            self.monotonic_beats += step_b;
        }

        let mut beat_flags = 0u8;
        if !seek_jumped {
            let pb = prev_beats.floor() as i64;
            let cb = self.monotonic_beats.floor() as i64;
            let ph = (prev_beats * 2.0).floor() as i64;
            let ch = (self.monotonic_beats * 2.0).floor() as i64;
            let pbar = prev_beats.div_euclid(4.0) as i64;
            let cbar = self.monotonic_beats.div_euclid(4.0) as i64;
            if cb > pb {
                beat_flags |= beat_flag::FIRE_BEAT;
                if (cb.rem_euclid(2)) == 0 {
                    beat_flags |= beat_flag::FIRE_STRONG_BEAT;
                }
            }
            if cbar > pbar {
                beat_flags |= beat_flag::FIRE_BAR;
            }
            if ch > ph {
                beat_flags |= beat_flag::FIRE_HALF_BEAT;
            }
        }

        let beat_floor = self.monotonic_beats.floor();
        let beat_index = beat_floor.max(0.0) as u64;
        let bar_beat = (beat_floor as i64).rem_euclid(4) as u8;
        let beat_phase = ((self.monotonic_beats - beat_floor) as f32).clamp(0.0, 0.999_99);
        let bar_phase = (bar_beat as f32) + beat_phase;

        // Audio spectrum, waveform, VU, and normalized impulses
        let frames = (pcm_stereo.len() / 2).min(FFT_SIZE);
        let sr = sample_rate.clamp(8_000, 192_000) as f32;
        let mut waveform_256 = [0i8; 256];
        let apu = apu_opt.unwrap_or_default();

        if status != 1 || frames < 32 {
            let rel = (-dt * 7.0).exp();
            self.vu_l *= rel;
            self.vu_r *= rel;
            self.vu_peak_l *= rel;
            self.vu_peak_r *= rel;
            self.kick_imp_smooth *= rel;
            self.snare_imp_smooth *= rel;
            for b in &mut self.bands_smooth {
                *b *= rel;
            }
            for i in 0..64 {
                self.spec_smooth[i] *= rel;
                self.spec_peak[i] = (self.spec_peak[i] - dt * 0.45).max(self.spec_smooth[i]);
            }
        } else {
            let inv_i16 = 1.0 / 32768.0f32;
            let mut mono_buf = [0.0f32; FFT_SIZE];
            let pad = FFT_SIZE - frames;
            let mut sq_l = 0.0f32;
            let mut sq_r = 0.0f32;
            let mut sq_m = 0.0f32;
            let mut pk_l = 0.0f32;
            let mut pk_r = 0.0f32;
            let mut dc_sum = 0.0f32;

            let a_hp40 = (1.0 - (-2.0 * PI * 40.0 / sr).exp()).clamp(0.001, 0.35);
            let a_lp150 = (1.0 - (-2.0 * PI * 150.0 / sr).exp()).clamp(0.003, 0.55);
            let a_hp160 = (1.0 - (-2.0 * PI * 160.0 / sr).exp()).clamp(0.003, 0.55);
            let a_lp2200 = (1.0 - (-2.0 * PI * 2200.0 / sr).exp()).clamp(0.02, 0.92);
            let mut k_sq = 0.0f32;
            let mut s_sq = 0.0f32;

            for i in 0..frames {
                let l = (pcm_stereo[i * 2] as f32) * inv_i16;
                let r = (pcm_stereo[i * 2 + 1] as f32) * inv_i16;
                let m = 0.5 * (l + r);
                mono_buf[pad + i] = m;
                dc_sum += m;
                sq_m += m * m;
                sq_l += l * l;
                sq_r += r * r;
                pk_l = pk_l.max(l.abs());
                pk_r = pk_r.max(r.abs());

                self.hp40_s += a_hp40 * (m - self.hp40_s);
                let kh = m - self.hp40_s;
                self.lp150_s += a_lp150 * (kh - self.lp150_s);
                k_sq += self.lp150_s * self.lp150_s;

                self.hp160_s += a_hp160 * (m - self.hp160_s);
                let sh = m - self.hp160_s;
                self.lp2200_s += a_lp2200 * (sh - self.lp2200_s);
                s_sq += self.lp2200_s * self.lp2200_s;
            }

            let inv_f = 1.0 / (frames as f32);
            let dc = dc_sum * inv_f;
            let rms_m = (sq_m * inv_f).sqrt();
            let rms_l = (sq_l * inv_f).sqrt();
            let rms_r = (sq_r * inv_f).sqrt();

            // Auto-gain from cached `avg_rms` (if available) or rolling program RMS
            let ref_rms = matched_rec
                .map(|r| r.avg_rms.max(0.035))
                .unwrap_or_else(|| {
                    if rms_m > 0.002 {
                        self.wave_rms_avg += (rms_m - self.wave_rms_avg) * (1.0 - (-dt * 1.8).exp());
                    }
                    self.wave_rms_avg.max(0.035)
                });
            let gain = (0.30 / ref_rms).clamp(0.60, 4.2);

            let vu_alpha = 1.0 - (-dt * 14.0).exp();
            self.vu_l += ((rms_l * gain).clamp(0.0, 1.0) - self.vu_l) * vu_alpha;
            self.vu_r += ((rms_r * gain).clamp(0.0, 1.0) - self.vu_r) * vu_alpha;
            let pk_decay = (-dt * 3.5).exp();
            self.vu_peak_l = (self.vu_peak_l * pk_decay).max((pk_l * gain * 0.85).clamp(0.0, 1.0));
            self.vu_peak_r = (self.vu_peak_r * pk_decay).max((pk_r * gain * 0.85).clamp(0.0, 1.0));

            // Normalized Kick & Snare impulses against cached track spectral baselines
            let k_log = (1.0 + 45.0 * (k_sq * inv_f).sqrt()).ln();
            let s_log = (1.0 + 45.0 * (s_sq * inv_f).sqrt()).ln();
            let raw_kf = (k_log - self.prev_k_log).max(0.0);
            let raw_sf = (s_log - self.prev_s_log).max(0.0);
            self.prev_k_log = k_log;
            self.prev_s_log = s_log;

            let k_base = matched_rec.map(|r| r.kick_baseline.max(0.02)).unwrap_or(0.12);
            let s_base = matched_rec.map(|r| r.snare_baseline.max(0.02)).unwrap_or(0.12);
            let apu_bass = apu.bass_trigger();
            let apu_perc = apu.percussion_trigger();

            let inst_kick = ((raw_kf / (k_base * 1.35)).clamp(0.0, 1.0)).max(apu_bass * 0.85);
            let inst_snare = ((raw_sf / (s_base * 1.35)).clamp(0.0, 1.0)).max(apu_perc * 0.85);
            let imp_rel = (-dt * 12.0).exp();
            self.kick_imp_smooth = (self.kick_imp_smooth * imp_rel).max(inst_kick);
            self.snare_imp_smooth = (self.snare_imp_smooth * imp_rel).max(inst_snare);

            // 256-sample scope waveform
            for (x, slot) in waveform_256.iter_mut().enumerate() {
                let idx = pad + (x * frames) / 256;
                let v = ((mono_buf[idx] - dc) * gain).clamp(-1.0, 1.0);
                *slot = (v * 127.0).round() as i8;
            }

            // 1024-pt Hann radix-2 FFT
            for i in 0..FFT_SIZE {
                let rev = self.bit_rev[i] as usize;
                let s = if rev >= pad { mono_buf[rev] - dc } else { 0.0 };
                self.fft_re[i] = s * self.hann[rev];
                self.fft_im[i] = 0.0;
            }
            self.run_fft_in_place();

            let inv_norm = (2.0 / (FFT_SIZE as f32)) * gain;
            for k in 0..FFT_BINS {
                let re = self.fft_re[k];
                let im = self.fft_im[k];
                self.mag[k] = (re * re + im * im).sqrt() * inv_norm;
            }

            // 5-band live energies & 64 log-spaced spectrum bins (32 Hz .. 16 kHz)
            let hz_per_bin = sr / (FFT_SIZE as f32);
            let edges_hz = [25.0f32, 85.0, 260.0, 1250.0, 3600.0, 16_000.0];
            for b in 0..5 {
                let k0 = ((edges_hz[b] / hz_per_bin).floor() as usize).clamp(1, FFT_BINS - 1);
                let k1 = ((edges_hz[b + 1] / hz_per_bin).ceil() as usize).clamp(k0 + 1, FFT_BINS);
                let mut sum = 0.0f32;
                for k in k0..k1 {
                    sum += self.mag[k];
                }
                let mean = (sum / ((k1 - k0) as f32)) * (2.5 + (b as f32) * 1.4);
                let val = (mean * 2.2).sqrt().clamp(0.0, 1.0);
                let atk = if val > self.bands_smooth[b] { 0.48 } else { 0.18 };
                self.bands_smooth[b] += (val - self.bands_smooth[b]) * atk;
            }

            let min_hz = 32.0f32;
            let max_hz = (sr * 0.45).min(16_500.0).max(min_hz * 2.0);
            let log_ratio = (max_hz / min_hz).ln();
            for i in 0..64 {
                let t0 = (i as f32) / 64.0;
                let t1 = ((i + 1) as f32) / 64.0;
                let f0 = min_hz * (log_ratio * t0).exp();
                let f1 = min_hz * (log_ratio * t1).exp();
                let b0 = ((f0 / hz_per_bin).floor() as usize).clamp(1, FFT_BINS - 1);
                let b1 = ((f1 / hz_per_bin).ceil() as usize).clamp(b0 + 1, FFT_BINS);
                let mut pk = 0.0f32;
                for k in b0..b1 {
                    pk = pk.max(self.mag[k]);
                }
                // Pink-noise tilt (+3 dB/oct) so highs match bass visually
                let tilt = 1.4 + 2.8 * t0;
                let level = (pk * tilt * 2.4).sqrt().clamp(0.0, 1.0);
                let alpha = if level > self.spec_smooth[i] { 0.55 } else { 0.22 };
                self.spec_smooth[i] += (level - self.spec_smooth[i]) * alpha;
                self.spec_peak[i] = (self.spec_peak[i] - dt * 0.42).max(self.spec_smooth[i]);
            }
        }

        let mut spectrum_64 = [0u8; 64];
        let mut spectrum_peak_64 = [0u8; 64];
        for i in 0..64 {
            spectrum_64[i] = (self.spec_smooth[i].clamp(0.0, 1.0) * 255.0).round() as u8;
            spectrum_peak_64[i] = (self.spec_peak[i].clamp(0.0, 1.0) * 255.0).round() as u8;
        }

        TrackLiveFrame {
            track_key_hash,
            beat_index,
            seq_generation: self.generation,
            status,
            has_analysis: u8::from(matched_rec.is_some()),
            bar_beat,
            beat_flags,
            playhead_ms,
            duration_ms,
            sample_rate,
            output_latency_ms,
            bpm: active_bpm,
            bpm_confidence: bpm_conf,
            beat_phase,
            bar_phase,
            kick_impulse: self.kick_imp_smooth,
            snare_impulse: self.snare_imp_smooth,
            structural_energy,
            vu_l: self.vu_l,
            vu_r: self.vu_r,
            vu_peak_l: self.vu_peak_l,
            vu_peak_r: self.vu_peak_r,
            bands_5: self.bands_smooth,
            spectrum_64,
            spectrum_peak_64,
            waveform_256,
            apu,
        }
    }

    fn run_fft_in_place(&mut self) {
        let mut len = 2usize;
        while len <= FFT_SIZE {
            let half = len / 2;
            let step = FFT_SIZE / len;
            let mut base = 0usize;
            while base < FFT_SIZE {
                for j in 0..half {
                    let tw = j * step;
                    let wr = self.twiddle_re[tw];
                    let wi = self.twiddle_im[tw];
                    let u_idx = base + j;
                    let v_idx = u_idx + half;
                    let vr = self.fft_re[v_idx] * wr - self.fft_im[v_idx] * wi;
                    let vi = self.fft_re[v_idx] * wi + self.fft_im[v_idx] * wr;
                    let ur = self.fft_re[u_idx];
                    let ui = self.fft_im[u_idx];
                    self.fft_re[u_idx] = ur + vr;
                    self.fft_im[u_idx] = ui + vi;
                    self.fft_re[v_idx] = ur - vr;
                    self.fft_im[v_idx] = ui - vi;
                }
                base += len;
            }
            len <<= 1;
        }
    }
}
