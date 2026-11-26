//! Manual Dual-Deck DJ Engine (`ManualDjEngine`) for `.uipod` plugins (`ABI v11+`).
//!
//! Architecture:
//! - **Lock-Free Real-Time Audio Core (`ManualDjCore`)**:
//!   Runs strictly on the hardware audio thread (`pull_source_frames` / `fill_period` / WebAudio)
//!   in `128`-frame sub-blocks with zero heap allocations and zero file I/O locks:
//!   - Per-deck WSOLA (`MasterTempo`, `0.75x..1.25x` ± `4%` bend) & Hermite (`Vinyl`) resampling
//!   - Seamless RAM-pinned Beat Loops (`4`, `2`, `1`, `1/2` beats) with `2 ms` equal-power seam
//!   - `< 3 ms` raised-cosine anti-click micro-ramp on `CueReturn`, `CueStutter`, & quantized `Sync`
//!   - Per-deck resonant bipolar Cytomic TPT SVF Macro Filter (`LPF 120 Hz` .. `0` .. `HPF 3.5 kHz`)
//!   - Equal-power Crossfader + symmetric `180 Hz` Bass Swap crossover
//! - **Per-Deck Hybrid RAM Window (`DeckSharedRam`)**:
//!   Combines a pinned `CueWindow` (`~8s` around Cue for instant zero-seek stutter) + sliding
//!   `ChunkCache` (`8192`-frame stereo chunks refilled asynchronously on `uipod-dj-refill`) +
//!   `5 ms` (`200 Hz`) pre-extracted `(peak_u8, kick_u8)` envelope strip for `< 1 µs` 24-bar
//!   4-beat discrete waveform telemetry.

use std::f32::consts::{FRAC_1_SQRT_2, FRAC_PI_2, PI};
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicU32, AtomicU64, AtomicU8, Ordering};
use std::sync::{Arc, Mutex};

use uipod_plugin_sdk::{
    DjCueAction, DjDeckId, DjLoopLength, DjTempoMode, PluginDjDeckTelemetry, PluginDjTelemetry,
};

use crate::filter::OnePoleSmoother;
use crate::planner::fold_bpm_to_reference;
use crate::resampler::VinylResampler;
use crate::wsola::WsolaStretcher;

const SUB_BLOCK_FRAMES: usize = 128;
/// `8192` stereo frames per sliding RAM chunk (`~185.7 ms` @ `44.1 kHz`, `32 KiB`).
pub const RAM_CHUNK_FRAMES: usize = 8192;
/// Up to `96` resident chunks per deck (`~17.8s` sliding window = `3.0 MiB` per deck).
const MAX_RESIDENT_CHUNKS: usize = 96;
/// Pinned Cue window duration (`-0.5s .. +7.5s` = `8.0s` = `~1.4 MiB` per deck).
#[cfg(not(target_arch = "wasm32"))]
const CUE_PRE_ROLL_MS: u32 = 500;
#[cfg(not(target_arch = "wasm32"))]
const CUE_POST_ROLL_MS: u32 = 7_500;
/// Envelope bucket hop (`~5 ms` = `200 Hz` resolution).
const ENV_BUCKET_MS: u32 = 5;

#[inline]
fn write_fixed_cstr(dst: &mut [u8], s: &str) {
    dst.fill(0);
    let trimmed = s.trim();
    if trimmed.is_empty() || dst.len() < 2 {
        return;
    }
    let max = dst.len() - 1;
    let bytes = trimmed.as_bytes();
    let mut len = bytes.len().min(max);
    while len > 0 && !trimmed.is_char_boundary(len) {
        len -= 1;
    }
    dst[..len].copy_from_slice(&bytes[..len]);
    dst[len] = 0;
}

/// Resonant Bipolar Cytomic TPT SVF Macro Filter (`-1.0 .. +1.0`):
/// - `[-1.0, -0.05)`: Resonant `LPF` sweeping down to `120 Hz`
/// - `[-0.05, +0.05]`: True flat bypass (`FILTER: 0`)
/// - `(+0.05, +1.0]`: Resonant `HPF` sweeping up to `3.5 kHz`
#[derive(Clone, Debug)]
pub struct DjMacroFilter {
    sample_rate: u32,
    val_smoother: OnePoleSmoother,
    wet_smoother: OnePoleSmoother,
    ic1eq_l: f32,
    ic2eq_l: f32,
    ic1eq_r: f32,
    ic2eq_r: f32,
    a1: f32,
    a2: f32,
    a3: f32,
    k_damp: f32,
    is_hpf: bool,
    coeff_div: u8,
}

impl DjMacroFilter {
    pub const DEADZONE: f32 = 0.05;
    pub const LPF_MIN_HZ: f32 = 120.0;
    pub const LPF_MAX_HZ: f32 = 18_000.0;
    pub const HPF_MIN_HZ: f32 = 25.0;
    pub const HPF_MAX_HZ: f32 = 3_500.0;
    /// Subtle musical DJ resonance (`Q = 1.32` -> `k = 1/Q = 0.7576`).
    const K_RESONANT: f32 = 0.7576;

    pub fn new(sample_rate: u32) -> Self {
        let sr = sample_rate.max(8_000);
        let mut f = Self {
            sample_rate: sr,
            val_smoother: OnePoleSmoother::new(0.0, sr, 0.008),
            wet_smoother: OnePoleSmoother::new(0.0, sr, 0.006),
            ic1eq_l: 0.0,
            ic2eq_l: 0.0,
            ic1eq_r: 0.0,
            ic2eq_r: 0.0,
            a1: 0.0,
            a2: 0.0,
            a3: 0.0,
            k_damp: Self::K_RESONANT,
            is_hpf: false,
            coeff_div: 0,
        };
        f.update_coeffs(0.0);
        f
    }

    pub fn reset(&mut self, sample_rate: u32) {
        let sr = sample_rate.max(8_000);
        self.sample_rate = sr;
        self.val_smoother.set_sample_rate(sr, 0.008);
        self.wet_smoother.set_sample_rate(sr, 0.006);
        self.val_smoother.reset(0.0);
        self.wet_smoother.reset(0.0);
        self.ic1eq_l = 0.0;
        self.ic2eq_l = 0.0;
        self.ic1eq_r = 0.0;
        self.ic2eq_r = 0.0;
        self.coeff_div = 0;
        self.update_coeffs(0.0);
    }

    #[inline]
    pub fn set_target(&mut self, value: f32) {
        let v = value.clamp(-1.0, 1.0);
        self.val_smoother.set_target(v);
        if v.abs() <= Self::DEADZONE {
            self.wet_smoother.set_target(0.0);
        } else {
            self.wet_smoother.set_target(1.0);
        }
    }

    #[inline]
    pub fn target_value(&self) -> f32 {
        let v = self.val_smoother.current();
        if v.abs() <= Self::DEADZONE {
            0.0
        } else {
            v
        }
    }

    #[inline]
    fn update_coeffs(&mut self, val: f32) {
        let fs = self.sample_rate as f32;
        let nyq_guard = (fs * 0.44).max(400.0);
        let fc = if val < -Self::DEADZONE {
            self.is_hpf = false;
            let u = ((-val - Self::DEADZONE) / (1.0 - Self::DEADZONE)).clamp(0.0, 1.0);
            let max_hz = Self::LPF_MAX_HZ.min(nyq_guard);
            max_hz * (Self::LPF_MIN_HZ / max_hz).powf(u)
        } else if val > Self::DEADZONE {
            self.is_hpf = true;
            let u = ((val - Self::DEADZONE) / (1.0 - Self::DEADZONE)).clamp(0.0, 1.0);
            let max_hz = Self::HPF_MAX_HZ.min(nyq_guard);
            Self::HPF_MIN_HZ * (max_hz / Self::HPF_MIN_HZ).powf(u)
        } else if self.is_hpf {
            Self::HPF_MIN_HZ
        } else {
            Self::LPF_MAX_HZ.min(nyq_guard)
        };

        let g = (PI * fc.clamp(15.0, nyq_guard) / fs).tan();
        let k = Self::K_RESONANT;
        let a1 = 1.0 / (1.0 + g * (g + k));
        let a2 = g * a1;
        let a3 = g * a2;
        self.a1 = a1;
        self.a2 = a2;
        self.a3 = a3;
        self.k_damp = k;
    }

    #[inline]
    pub fn process_frame(&mut self, l: f32, r: f32) -> (f32, f32) {
        let val = self.val_smoother.next_sample();
        let wet = self.wet_smoother.next_sample();

        // Pure zero-distortion flat bypass once settled inside deadzone
        if wet <= 1e-5 && val.abs() <= Self::DEADZONE {
            self.ic1eq_l *= 0.95;
            self.ic2eq_l *= 0.95;
            self.ic1eq_r *= 0.95;
            self.ic2eq_r *= 0.95;
            return (l, r);
        }

        if self.coeff_div == 0 {
            self.update_coeffs(val);
            self.coeff_div = 15;
        } else {
            self.coeff_div -= 1;
        }

        let a1 = self.a1;
        let a2 = self.a2;
        let a3 = self.a3;
        let k = self.k_damp;

        let v3_l = l - self.ic2eq_l;
        let v1_l = a1 * self.ic1eq_l + a2 * v3_l;
        let v2_l = self.ic2eq_l + a2 * self.ic1eq_l + a3 * v3_l;
        self.ic1eq_l = 2.0 * v1_l - self.ic1eq_l;
        self.ic2eq_l = 2.0 * v2_l - self.ic2eq_l;

        let v3_r = r - self.ic2eq_r;
        let v1_r = a1 * self.ic1eq_r + a2 * v3_r;
        let v2_r = self.ic2eq_r + a2 * self.ic1eq_r + a3 * v3_r;
        self.ic1eq_r = 2.0 * v1_r - self.ic1eq_r;
        self.ic2eq_r = 2.0 * v2_r - self.ic2eq_r;

        let (flt_l, flt_r) = if self.is_hpf {
            (l - k * v1_l - v2_l, r - k * v1_r - v2_r)
        } else {
            (v2_l, v2_r)
        };

        let dry = 1.0 - wet;
        (l * dry + flt_l * wet, r * dry + flt_r * wet)
    }
}

/// Phase-coherent `180 Hz` low/high split for symmetric center `Bass Swap`.
///
/// Uses a 2nd-order Butterworth TPT SVF low-pass `lp_180` and reconstructs
/// `out = mid_high + bass_gain * lp_180 = x + (bass_gain - 1.0) * lp_180`,
/// guaranteeing exact `1.0` bit-transparent unity response when `bass_gain == 1.0`!
#[derive(Clone, Debug)]
pub struct BassSwapFilter {
    ic1_l: f32,
    ic2_l: f32,
    ic1_r: f32,
    ic2_r: f32,
    a1: f32,
    a2: f32,
    a3: f32,
    bass_smoother: OnePoleSmoother,
}

impl BassSwapFilter {
    pub const CROSSOVER_HZ: f32 = 180.0;

    pub fn new(sample_rate: u32) -> Self {
        let sr = sample_rate.max(8_000);
        let mut s = Self {
            ic1_l: 0.0,
            ic2_l: 0.0,
            ic1_r: 0.0,
            ic2_r: 0.0,
            a1: 0.0,
            a2: 0.0,
            a3: 0.0,
            bass_smoother: OnePoleSmoother::new(1.0, sr, 0.008),
        };
        s.set_sample_rate(sr);
        s
    }

    pub fn set_sample_rate(&mut self, sample_rate: u32) {
        let fs = sample_rate.max(8_000) as f32;
        let g = (PI * Self::CROSSOVER_HZ / fs).tan();
        let k = 1.0 / FRAC_1_SQRT_2; // SQRT_2
        let a1 = 1.0 / (1.0 + g * (g + k));
        self.a1 = a1;
        self.a2 = g * a1;
        self.a3 = g * self.a2;
        self.bass_smoother.set_sample_rate(sample_rate.max(8_000), 0.008);
    }

    pub fn reset(&mut self) {
        self.ic1_l = 0.0;
        self.ic2_l = 0.0;
        self.ic1_r = 0.0;
        self.ic2_r = 0.0;
        self.bass_smoother.reset(1.0);
    }

    #[inline]
    pub fn set_bass_gain(&mut self, bass_gain: f32) {
        self.bass_smoother.set_target(bass_gain.clamp(0.0, 1.0));
    }

    #[inline]
    pub fn process_frame(&mut self, l: f32, r: f32) -> (f32, f32) {
        let bg = self.bass_smoother.next_sample();
        let v3_l = l - self.ic2_l;
        let v1_l = self.a1 * self.ic1_l + self.a2 * v3_l;
        let v2_l = self.ic2_l + self.a2 * self.ic1_l + self.a3 * v3_l;
        self.ic1_l = 2.0 * v1_l - self.ic1_l;
        self.ic2_l = 2.0 * v2_l - self.ic2_l;

        let v3_r = r - self.ic2_r;
        let v1_r = self.a1 * self.ic1_r + self.a2 * v3_r;
        let v2_r = self.ic2_r + self.a2 * self.ic1_r + self.a3 * v3_r;
        self.ic1_r = 2.0 * v1_r - self.ic1_r;
        self.ic2_r = 2.0 * v2_r - self.ic2_r;

        let delta = bg - 1.0;
        (l + delta * v2_l, r + delta * v2_r)
    }
}

/// Evaluates equal-power crossfader gains `(gain_a, gain_b)` and `< 180 Hz` bass gains
/// `(bass_a, bass_b)` for `position` in `[0.0, 1.0]`.
#[inline]
pub fn evaluate_manual_crossfader(position: f32, bass_swap_enabled: bool) -> (f32, f32, f32, f32) {
    let p = position.clamp(0.0, 1.0);
    let angle = p * FRAC_PI_2;
    let gain_a = angle.cos();
    let gain_b = angle.sin();

    if !bass_swap_enabled {
        return (gain_a, gain_b, 1.0, 1.0);
    }

    #[inline]
    fn symmetric_bass_curve(dist_from_home: f32) -> f32 {
        // Full bass (`1.0`) across `0.0..=0.30`; smooth cosine swap across `0.30..0.70`
        // (`0.7071` at `0.50` so `gain * bass = 0.50` per deck => `0.5 + 0.5 = 1.0` max sub!);
        // complete `< 180 Hz` cut (`0.0`) on incoming/outgoing side `>= 0.70`.
        if dist_from_home <= 0.30 {
            1.0
        } else if dist_from_home >= 0.70 {
            0.0
        } else {
            let t = (dist_from_home - 0.30) * (1.0 / 0.40);
            (t * FRAC_PI_2).cos()
        }
    }

    let bass_a = symmetric_bass_curve(p);
    let bass_b = symmetric_bass_curve(1.0 - p);
    (gain_a, gain_b, bass_a, bass_b)
}

#[derive(Clone)]
pub struct PinnedPcmWindow {
    pub start_frame: u64,
    pub frames: usize,
    pub stereo_s16: Vec<i16>,
}

impl PinnedPcmWindow {
    #[inline]
    pub fn contains_frame(&self, frame: u64) -> bool {
        frame >= self.start_frame && frame < self.start_frame + (self.frames as u64)
    }

    #[inline]
    pub fn sample_f32(&self, frame: u64) -> Option<(f32, f32)> {
        if !self.contains_frame(frame) {
            return None;
        }
        let idx = ((frame - self.start_frame) as usize) * 2;
        let scale = 1.0 / 32768.0;
        Some((
            (self.stereo_s16[idx] as f32) * scale,
            (self.stereo_s16[idx + 1] as f32) * scale,
        ))
    }
}

pub struct RamChunkSlot {
    pub chunk_index: u64,
    pub valid_frames: usize,
    pub stereo_s16: Vec<i16>,
}

/// Shared per-deck RAM PCM store accessed with `< 3 µs` chunk-publish locks
/// (never locked during file open, seek, or codec decompression).
pub struct DeckRamStore {
    pub sample_rate: u32,
    pub total_frames: u64,
    pub cue_window: Option<Arc<PinnedPcmWindow>>,
    /// Full-track in-memory buffer (used by WASM simulator & unit tests, or short snippets).
    pub full_pcm: Option<Arc<PinnedPcmWindow>>,
    pub chunks: Vec<RamChunkSlot>,
    /// Pre-decimated `5 ms` (`200 Hz`) envelope buckets: `(peak_u8, kick_u8)`.
    pub env_buckets: Vec<(u8, u8)>,
    pub fallback_contour: [u8; 256],
}

impl DeckRamStore {
    pub fn new(sample_rate: u32, total_frames: u64, contour: [u8; 256]) -> Self {
        let sr = sample_rate.max(8_000);
        let est_ms = ((total_frames * 1000) / (sr as u64)) as usize;
        let n_buckets = (est_ms / (ENV_BUCKET_MS as usize) + 8).min(600_000);
        Self {
            sample_rate: sr,
            total_frames,
            cue_window: None,
            full_pcm: None,
            chunks: Vec::with_capacity(MAX_RESIDENT_CHUNKS),
            env_buckets: vec![(0, 0); n_buckets],
            fallback_contour: contour,
        }
    }

    #[inline]
    pub fn read_frame_f32(&self, frame: u64) -> Option<(f32, f32)> {
        if let Some(ref full) = self.full_pcm {
            if let Some(pair) = full.sample_f32(frame) {
                return Some(pair);
            }
        }
        if let Some(ref cue) = self.cue_window {
            if let Some(pair) = cue.sample_f32(frame) {
                return Some(pair);
            }
        }
        let cidx = frame / (RAM_CHUNK_FRAMES as u64);
        let off = (frame % (RAM_CHUNK_FRAMES as u64)) as usize;
        for slot in &self.chunks {
            if slot.chunk_index == cidx && off < slot.valid_frames {
                let scale = 1.0 / 32768.0;
                return Some((
                    (slot.stereo_s16[off * 2] as f32) * scale,
                    (slot.stereo_s16[off * 2 + 1] as f32) * scale,
                ));
            }
        }
        None
    }

    #[inline]
    pub fn read_frame_s16(&self, frame: u64) -> Option<(i16, i16)> {
        if let Some(ref full) = self.full_pcm {
            if full.contains_frame(frame) {
                let idx = ((frame - full.start_frame) as usize) * 2;
                return Some((full.stereo_s16[idx], full.stereo_s16[idx + 1]));
            }
        }
        if let Some(ref cue) = self.cue_window {
            if cue.contains_frame(frame) {
                let idx = ((frame - cue.start_frame) as usize) * 2;
                return Some((cue.stereo_s16[idx], cue.stereo_s16[idx + 1]));
            }
        }
        let cidx = frame / (RAM_CHUNK_FRAMES as u64);
        let off = (frame % (RAM_CHUNK_FRAMES as u64)) as usize;
        for slot in &self.chunks {
            if slot.chunk_index == cidx && off < slot.valid_frames {
                return Some((slot.stereo_s16[off * 2], slot.stereo_s16[off * 2 + 1]));
            }
        }
        None
    }

    pub fn insert_chunk(&mut self, chunk_index: u64, pcm_s16: &[i16], center_chunk: u64) {
        let valid_frames = (pcm_s16.len() / 2).min(RAM_CHUNK_FRAMES);
        if valid_frames == 0 {
            return;
        }
        if let Some(existing) = self.chunks.iter_mut().find(|c| c.chunk_index == chunk_index) {
            if valid_frames >= existing.valid_frames {
                existing.stereo_s16.clear();
                existing.stereo_s16.extend_from_slice(&pcm_s16[..valid_frames * 2]);
                existing.valid_frames = valid_frames;
            }
            return;
        }
        if self.chunks.len() >= MAX_RESIDENT_CHUNKS {
            // Evict chunk furthest behind (or furthest away from) `center_chunk`
            let mut worst_i = 0usize;
            let mut worst_score = 0u64;
            for (i, c) in self.chunks.iter().enumerate() {
                let dist = if c.chunk_index < center_chunk {
                    (center_chunk - c.chunk_index) * 3 // bias eviction toward already-passed audio
                } else {
                    c.chunk_index - center_chunk
                };
                if dist > worst_score {
                    worst_score = dist;
                    worst_i = i;
                }
            }
            self.chunks.swap_remove(worst_i);
        }
        self.chunks.push(RamChunkSlot {
            chunk_index,
            valid_frames,
            stereo_s16: pcm_s16[..valid_frames * 2].to_vec(),
        });
    }

    pub fn has_chunk(&self, chunk_index: u64) -> bool {
        if let Some(ref full) = self.full_pcm {
            let start = chunk_index * (RAM_CHUNK_FRAMES as u64);
            if full.contains_frame(start) {
                return true;
            }
        }
        self.chunks.iter().any(|c| c.chunk_index == chunk_index)
    }

    pub fn update_envelope_from_s16(&mut self, start_frame: u64, stereo_s16: &[i16]) {
        let frames = stereo_s16.len() / 2;
        if frames == 0 {
            return;
        }
        let sr = self.sample_rate.max(8_000);
        let hop_frames = ((sr as usize) * (ENV_BUCKET_MS as usize) / 1000).max(32);
        let alpha_160 = (1.0 - (-2.0 * PI * 160.0 / (sr as f32)).exp()).clamp(0.005, 0.25);
        let scale = 1.0 / 32768.0;
        let mut lp = 0.0f32;

        let end_frame = start_frame + (frames as u64);
        let end_bucket = ((end_frame * 1000) / ((sr as u64) * (ENV_BUCKET_MS as u64))) as usize + 2;
        if end_bucket > self.env_buckets.len() && end_bucket <= 600_000 {
            self.env_buckets.resize(end_bucket, (0, 0));
        }

        let mut f = 0usize;
        while f < frames {
            let chunk_end = (f + hop_frames).min(frames);
            let mut peak = 0.0f32;
            let mut low_peak = 0.0f32;
            for i in f..chunk_end {
                let m = ((stereo_s16[i * 2] as f32) + (stereo_s16[i * 2 + 1] as f32)) * 0.5 * scale;
                lp += alpha_160 * (m - lp);
                peak = peak.max(m.abs());
                low_peak = low_peak.max(lp.abs());
            }
            let abs_frame = start_frame + (f as u64);
            let b_idx = ((abs_frame * 1000) / ((sr as u64) * (ENV_BUCKET_MS as u64))) as usize;
            if b_idx < self.env_buckets.len() {
                // Slight perceptual gamma (`sqrt`) so medium grooves & crisp kicks read cleanly on 4-shade LCDs
                let pk_u8 = ((peak.clamp(0.0, 1.0).powf(0.68)) * 255.0).round() as u8;
                let kk_u8 = (((low_peak * 1.35).clamp(0.0, 1.0).powf(0.72)) * 255.0).round() as u8;
                let prev = self.env_buckets[b_idx];
                self.env_buckets[b_idx] = (prev.0.max(pk_u8), prev.1.max(kk_u8));
            }
            f = chunk_end;
        }
    }
}

/// Seamless RAM-pinned Beat Loop buffer with `2 ms` equal-power (`cos`/`sin`) crossfade seam.
#[derive(Clone, Debug)]
pub struct PinnedBeatLoop {
    pub start_frame: u64,
    pub beat_frames: f64,
    pub max_4beat_frames: usize,
    pub active_len_frames: usize,
    pub seam_frames: usize,
    pub loop_kind: DjLoopLength,
    /// Interleaved stereo `f32` covering `[start_frame .. start_frame + max_4beat_frames]`.
    pub body_stereo: Vec<f32>,
    /// Pre-roll `[start_frame - seam_frames .. start_frame)` when `has_preroll == true`;
    /// otherwise post-roll `[start_frame + active_len .. + seam_frames)`.
    pub pre_roll_stereo: Vec<f32>,
    pub post_roll_stereo: Vec<f32>,
    pub has_preroll: bool,
    /// Exact modulo cursor `[0.0, active_len_frames as f64)` — zero cumulative drift!
    pub cursor: f64,
}

impl PinnedBeatLoop {
    pub fn capture(
        ram: &DeckRamStore,
        start_frame: u64,
        beat_frames: f64,
        loop_kind: DjLoopLength,
        initial_offset: f64,
    ) -> Option<Self> {
        let beats_f = loop_kind.beats_f32()? as f64;
        let sr = ram.sample_rate.max(8_000) as usize;
        let seam_frames = ((sr * 2) / 1000).clamp(32, 256); // Exact ~2.0 ms seam
        let max_4beat_frames = ((4.0 * beat_frames).round() as usize).max(seam_frames * 4);
        let active_len_frames = ((beats_f * beat_frames).round() as usize)
            .clamp(seam_frames * 4, max_4beat_frames);

        let mut body_stereo = vec![0.0f32; (max_4beat_frames + seam_frames) * 2];
        for i in 0..(max_4beat_frames + seam_frames) {
            if let Some((l, r)) = ram.read_frame_f32(start_frame + (i as u64)) {
                body_stereo[i * 2] = l;
                body_stereo[i * 2 + 1] = r;
            }
        }

        let has_preroll = start_frame >= (seam_frames as u64);
        let mut pre_roll_stereo = vec![0.0f32; seam_frames * 2];
        if has_preroll {
            let pre_base = start_frame - (seam_frames as u64);
            for i in 0..seam_frames {
                if let Some((l, r)) = ram.read_frame_f32(pre_base + (i as u64)) {
                    pre_roll_stereo[i * 2] = l;
                    pre_roll_stereo[i * 2 + 1] = r;
                }
            }
        }

        let mut post_roll_stereo = vec![0.0f32; seam_frames * 2];
        for i in 0..seam_frames {
            let src_i = (active_len_frames + i).min(max_4beat_frames + seam_frames - 1);
            post_roll_stereo[i * 2] = body_stereo[src_i * 2];
            post_roll_stereo[i * 2 + 1] = body_stereo[src_i * 2 + 1];
        }

        let cursor = initial_offset.rem_euclid(active_len_frames as f64);
        Some(Self {
            start_frame,
            beat_frames,
            max_4beat_frames,
            active_len_frames,
            seam_frames,
            loop_kind,
            body_stereo,
            pre_roll_stereo,
            post_roll_stereo,
            has_preroll,
            cursor,
        })
    }

    pub fn set_length(&mut self, kind: DjLoopLength) -> bool {
        let Some(beats_f) = kind.beats_f32() else {
            return false;
        };
        let new_len = (((beats_f as f64) * self.beat_frames).round() as usize)
            .clamp(self.seam_frames * 4, self.max_4beat_frames);
        self.active_len_frames = new_len;
        self.loop_kind = kind;
        for i in 0..self.seam_frames {
            let src_i = (new_len + i).min(self.body_stereo.len() / 2 - 1);
            self.post_roll_stereo[i * 2] = self.body_stereo[src_i * 2];
            self.post_roll_stereo[i * 2 + 1] = self.body_stereo[src_i * 2 + 1];
        }
        self.cursor = self.cursor.rem_euclid(new_len as f64);
        true
    }

    #[inline]
    fn sample_with_seam(&self, idx: usize) -> (f32, f32) {
        let len = self.active_len_frames;
        let seam = self.seam_frames;
        let i = idx % len;
        let base_l = self.body_stereo[i * 2];
        let base_r = self.body_stereo[i * 2 + 1];

        if self.has_preroll {
            // Causal pre-roll crossfade over `[len - seam .. len)` into `start_frame - 1 -> start_frame`
            let tail_start = len - seam;
            if i >= tail_start {
                let k = i - tail_start;
                let angle = ((k + 1) as f32) / ((seam + 1) as f32) * FRAC_PI_2;
                let g_body = angle.cos();
                let g_pre = angle.sin();
                return (
                    base_l * g_body + self.pre_roll_stereo[k * 2] * g_pre,
                    base_r * g_body + self.pre_roll_stereo[k * 2 + 1] * g_pre,
                );
            }
        } else if i < seam {
            // Post-roll crossfade over `[0 .. seam)` when `start_frame == 0`
            let angle = ((i + 1) as f32) / ((seam + 1) as f32) * FRAC_PI_2;
            let g_post = angle.cos();
            let g_body = angle.sin();
            return (
                self.post_roll_stereo[i * 2] * g_post + base_l * g_body,
                self.post_roll_stereo[i * 2 + 1] * g_post + base_r * g_body,
            );
        }
        (base_l, base_r)
    }

    /// Pull `count` continuous looped source stereo frames (with exact modulo wrap and zero drift).
    pub fn read_source_frames(&mut self, out_interleaved: &mut [f32]) {
        let frames = out_interleaved.len() / 2;
        let len = self.active_len_frames;
        let mut idx = (self.cursor.floor() as usize) % len;
        for f in 0..frames {
            let (l, r) = self.sample_with_seam(idx);
            out_interleaved[f * 2] = l;
            out_interleaved[f * 2 + 1] = r;
            idx += 1;
            if idx >= len {
                idx = 0;
            }
        }
        self.cursor = (self.cursor + frames as f64).rem_euclid(len as f64);
    }
}

/// Metadata & shared RAM handle installed when loading a track into a deck.
pub struct LoadedDeckTrack {
    pub path: String,
    pub title: String,
    pub artist: String,
    pub duration_ms: u32,
    pub total_frames: u64,
    pub native_bpm: f32,
    pub first_beat_ms: u32,
    pub initial_cue_frame: u64,
    pub ram: Arc<Mutex<DeckRamStore>>,
    pub playhead_hint_frame: Arc<AtomicU64>,
    pub cue_hint_frame: Arc<AtomicU64>,
}

/// Lock-free atomic control slot updated from UI thread (`60 Hz`) and consumed on audio thread.
pub struct DeckAtomicControls {
    pub playing: AtomicBool,
    pub quantize_on_start: AtomicBool,
    pub sync_locked: AtomicBool,
    pub loop_beats: AtomicU8,
    pub tempo_mode: AtomicU8,
    pub target_bpm_ratio_bits: AtomicU32,
    pub momentary_bend_bits: AtomicU32,
    pub filter_val_bits: AtomicU32,
    /// `0 = none`, `1 = CueReturn`, `2 = CueStutter`, `3 = SetCueHere`
    pub pending_cue_cmd: AtomicU8,
    pub pending_seek_beats: AtomicI32,
    pub pending_sync_snap: AtomicBool,
}

impl Default for DeckAtomicControls {
    fn default() -> Self {
        Self {
            playing: AtomicBool::new(false),
            quantize_on_start: AtomicBool::new(false),
            sync_locked: AtomicBool::new(false),
            loop_beats: AtomicU8::new(DjLoopLength::Off as u8),
            tempo_mode: AtomicU8::new(DjTempoMode::MasterTempo as u8),
            target_bpm_ratio_bits: AtomicU32::new(1.0f32.to_bits()),
            momentary_bend_bits: AtomicU32::new(0.0f32.to_bits()),
            filter_val_bits: AtomicU32::new(0.0f32.to_bits()),
            pending_cue_cmd: AtomicU8::new(0),
            pending_seek_beats: AtomicI32::new(0),
            pending_sync_snap: AtomicBool::new(false),
        }
    }
}

impl DeckAtomicControls {
    pub fn reset_for_new_track(&self, start_paused: bool) {
        self.playing.store(!start_paused, Ordering::Release);
        self.quantize_on_start.store(false, Ordering::Release);
        self.sync_locked.store(false, Ordering::Release);
        self.loop_beats
            .store(DjLoopLength::Off as u8, Ordering::Release);
        self.target_bpm_ratio_bits
            .store(1.0f32.to_bits(), Ordering::Release);
        self.momentary_bend_bits
            .store(0.0f32.to_bits(), Ordering::Release);
        self.filter_val_bits
            .store(0.0f32.to_bits(), Ordering::Release);
        self.pending_cue_cmd.store(0, Ordering::Release);
        self.pending_seek_beats.store(0, Ordering::Release);
        self.pending_sync_snap.store(false, Ordering::Release);
    }
}

/// Real-time per-deck DSP & transport state owned exclusively by `ManualDjCore`.
struct DeckRtState {
    track: Option<LoadedDeckTrack>,
    playing: bool,
    playhead_frame: f64,
    cue_frame: u64,
    applied_loop: DjLoopLength,
    active_loop: Option<PinnedBeatLoop>,

    wsola: WsolaStretcher,
    vinyl: VinylResampler,
    macro_filter: DjMacroFilter,
    bass_swap: BassSwapFilter,
    gate_smoother: OnePoleSmoother,

    // `< 3 ms` (`128`-frame) raised-cosine anti-click micro-ramp on Cue/Jump
    ramp_old_tail: Vec<f32>,
    ramp_pos: usize,
    ramp_len: usize,
    last_out_l: f32,
    last_out_r: f32,

    scratch_src: Vec<f32>,
    scratch_stretched: Vec<f32>,
}

impl DeckRtState {
    fn new(sample_rate: u32) -> Self {
        let sr = sample_rate.max(8_000);
        Self {
            track: None,
            playing: false,
            playhead_frame: 0.0,
            cue_frame: 0,
            applied_loop: DjLoopLength::Off,
            active_loop: None,
            wsola: WsolaStretcher::new(sr),
            vinyl: VinylResampler::new(),
            macro_filter: DjMacroFilter::new(sr),
            bass_swap: BassSwapFilter::new(sr),
            gate_smoother: OnePoleSmoother::new(0.0, sr, 0.0025),
            ramp_old_tail: vec![0.0; SUB_BLOCK_FRAMES * 2],
            ramp_pos: 0,
            ramp_len: 0,
            last_out_l: 0.0,
            last_out_r: 0.0,
            scratch_src: vec![0.0; SUB_BLOCK_FRAMES * 20 * 2],
            scratch_stretched: vec![0.0; SUB_BLOCK_FRAMES * 2],
        }
    }

    fn set_sample_rate(&mut self, sample_rate: u32) {
        let sr = sample_rate.max(8_000);
        self.wsola = WsolaStretcher::new(sr);
        self.vinyl.reset();
        self.macro_filter.reset(sr);
        self.bass_swap.set_sample_rate(sr);
        self.gate_smoother.set_sample_rate(sr, 0.0025);
    }

    #[inline]
    fn arm_anti_click_ramp(&mut self) {
        let len = SUB_BLOCK_FRAMES;
        for i in 0..len {
            let decay = 1.0 - (i as f32) / (len as f32);
            self.ramp_old_tail[i * 2] = self.last_out_l * decay;
            self.ramp_old_tail[i * 2 + 1] = self.last_out_r * decay;
        }
        self.ramp_pos = 0;
        self.ramp_len = len;
        self.wsola.reset();
        self.vinyl.reset();
    }

    #[inline]
    fn beat_frames(&self, sample_rate: u32) -> f64 {
        let bpm = self
            .track
            .as_ref()
            .map(|t| t.native_bpm)
            .unwrap_or(120.0)
            .clamp(40.0, 240.0) as f64;
        (sample_rate.max(8_000) as f64) * 60.0 / bpm
    }

    #[inline]
    fn first_beat_frame(&self, sample_rate: u32) -> f64 {
        let ms = self.track.as_ref().map(|t| t.first_beat_ms).unwrap_or(0) as f64;
        ms * (sample_rate.max(8_000) as f64) * 0.001
    }

    #[inline]
    fn current_audible_frame(&self, ratio: f32, mode: DjTempoMode) -> f64 {
        if let Some(ref lp) = self.active_loop {
            return (lp.start_frame as f64) + lp.cursor;
        }
        let unplayed = match mode {
            DjTempoMode::MasterTempo => self.wsola.unplayed_input_frames(ratio),
            DjTempoMode::Vinyl => self.vinyl.unplayed_input_frames(),
        };
        (self.playhead_frame - unplayed).max(0.0)
    }

    #[inline]
    fn beat_coord(&self, sample_rate: u32, ratio: f32, mode: DjTempoMode) -> f64 {
        let bf = self.beat_frames(sample_rate).max(1.0);
        let f0 = self.first_beat_frame(sample_rate);
        (self.current_audible_frame(ratio, mode) - f0) / bf
    }
}

/// Real-time Dual-Deck DJ processor (safe for both native ALSA/SDL threads and single-threaded WASM/tests).
pub struct ManualDjCore {
    sample_rate: u32,
    decks: [DeckRtState; 2],
    pub controls: [Arc<DeckAtomicControls>; 2],
    pending_load: [Mutex<Option<(LoadedDeckTrack, bool, u64)>>; 2],
    pub crossfader_bits: Arc<AtomicU32>,
    pub bass_swap_enabled: Arc<AtomicBool>,
    xfade_smoother: OnePoleSmoother,
    seq_gen: u32,
}

impl ManualDjCore {
    pub fn new(sample_rate: u32) -> Self {
        let sr = sample_rate.clamp(8_000, 192_000);
        Self {
            sample_rate: sr,
            decks: [DeckRtState::new(sr), DeckRtState::new(sr)],
            controls: [
                Arc::new(DeckAtomicControls::default()),
                Arc::new(DeckAtomicControls::default()),
            ],
            pending_load: [Mutex::new(None), Mutex::new(None)],
            crossfader_bits: Arc::new(AtomicU32::new(0.0f32.to_bits())),
            bass_swap_enabled: Arc::new(AtomicBool::new(true)),
            xfade_smoother: OnePoleSmoother::new(0.0, sr, 0.006),
            seq_gen: 0,
        }
    }

    #[inline]
    pub fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    pub fn set_sample_rate(&mut self, sr: u32) {
        let sr = sr.clamp(8_000, 192_000);
        if sr != self.sample_rate {
            self.sample_rate = sr;
            self.decks[0].set_sample_rate(sr);
            self.decks[1].set_sample_rate(sr);
            self.xfade_smoother.set_sample_rate(sr, 0.006);
        }
    }

    pub fn reset_session(&mut self) {
        for i in 0..2 {
            if let Ok(mut g) = self.pending_load[i].lock() {
                *g = None;
            }
            self.controls[i].reset_for_new_track(true);
            self.decks[i] = DeckRtState::new(self.sample_rate);
        }
        self.crossfader_bits
            .store(0.0f32.to_bits(), Ordering::Release);
        self.bass_swap_enabled.store(true, Ordering::Release);
        self.xfade_smoother.reset(0.0);
    }

    pub fn enqueue_deck_track(
        &self,
        deck: DjDeckId,
        track: LoadedDeckTrack,
        start_paused: bool,
        start_frame: u64,
    ) {
        let idx = deck.index();
        self.controls[idx].reset_for_new_track(start_paused);
        if let Ok(mut g) = self.pending_load[idx].lock() {
            *g = Some((track, start_paused, start_frame));
        }
    }

    #[inline]
    pub fn set_crossfader(&self, position: f32, bass_swap: bool) {
        self.crossfader_bits
            .store(position.clamp(0.0, 1.0).to_bits(), Ordering::Release);
        self.bass_swap_enabled.store(bass_swap, Ordering::Release);
    }

    #[inline]
    pub fn set_deck_playing(&self, deck: DjDeckId, playing: bool, quantize: bool) {
        let c = &self.controls[deck.index()];
        if playing && quantize {
            c.quantize_on_start.store(true, Ordering::Release);
        }
        c.playing.store(playing, Ordering::Release);
    }

    #[inline]
    pub fn trigger_cue(&self, deck: DjDeckId, action: DjCueAction) {
        let code = match action {
            DjCueAction::CueReturn => 1u8,
            DjCueAction::CueStutter => 2u8,
            DjCueAction::SetCueHere => 3u8,
        };
        self.controls[deck.index()]
            .pending_cue_cmd
            .store(code, Ordering::Release);
    }

    #[inline]
    pub fn seek_beats(&self, deck: DjDeckId, delta_beats: i32) {
        if delta_beats != 0 {
            self.controls[deck.index()]
                .pending_seek_beats
                .fetch_add(delta_beats, Ordering::AcqRel);
        }
    }

    #[inline]
    pub fn set_deck_tempo(
        &self,
        deck: DjDeckId,
        target_bpm_ratio: f32,
        mode: DjTempoMode,
        momentary_bend: f32,
    ) {
        let c = &self.controls[deck.index()];
        c.target_bpm_ratio_bits
            .store(target_bpm_ratio.clamp(0.75, 1.25).to_bits(), Ordering::Release);
        c.tempo_mode.store(mode as u8, Ordering::Release);
        c.momentary_bend_bits
            .store(momentary_bend.clamp(-0.04, 0.04).to_bits(), Ordering::Release);
    }

    #[inline]
    pub fn set_deck_sync(&self, deck: DjDeckId, enabled: bool) {
        let c = &self.controls[deck.index()];
        c.sync_locked.store(enabled, Ordering::Release);
        if enabled {
            c.pending_sync_snap.store(true, Ordering::Release);
        } else {
            c.target_bpm_ratio_bits
                .store(1.0f32.to_bits(), Ordering::Release);
        }
    }

    #[inline]
    pub fn set_deck_loop(&self, deck: DjDeckId, loop_beats: DjLoopLength) {
        self.controls[deck.index()]
            .loop_beats
            .store(loop_beats as u8, Ordering::Release);
    }

    #[inline]
    pub fn set_deck_filter(&self, deck: DjDeckId, filter_value: f32) {
        self.controls[deck.index()]
            .filter_val_bits
            .store(filter_value.clamp(-1.0, 1.0).to_bits(), Ordering::Release);
    }

    fn apply_control_updates(&mut self) {
        let sr = self.sample_rate;

        // 1. Check pending track swaps (`try_lock` so audio thread never blocks)
        for idx in 0..2 {
            let incoming = if let Ok(mut g) = self.pending_load[idx].try_lock() {
                g.take()
            } else {
                None
            };
            if let Some((trk, start_paused, start_frame)) = incoming {
                let d = &mut self.decks[idx];
                d.arm_anti_click_ramp();
                d.cue_frame = trk.initial_cue_frame;
                d.playhead_frame = start_frame.min(trk.total_frames) as f64;
                d.playing = !start_paused;
                d.applied_loop = DjLoopLength::Off;
                d.active_loop = None;
                d.macro_filter.reset(sr);
                d.bass_swap.reset();
                d.gate_smoother
                    .reset(if d.playing { 1.0 } else { 0.0 });
                trk.playhead_hint_frame
                    .store(start_frame, Ordering::Relaxed);
                trk.cue_hint_frame.store(d.cue_frame, Ordering::Relaxed);
                d.track = Some(trk);
            }
        }

        // 2. Per-deck transport, Cue, Beat-Seek, Loop, Sync, & Filter updates
        for idx in 0..2 {
            let other = 1 - idx;
            let ctrl = Arc::clone(&self.controls[idx]);
            let other_ctrl = Arc::clone(&self.controls[other]);

            let mode = DjTempoMode::from_u8(ctrl.tempo_mode.load(Ordering::Relaxed));
            let mut base_ratio = f32::from_bits(ctrl.target_bpm_ratio_bits.load(Ordering::Relaxed));
            let bend = f32::from_bits(ctrl.momentary_bend_bits.load(Ordering::Relaxed));
            let sync_on = ctrl.sync_locked.load(Ordering::Acquire);

            // Live BPM ratio tracking when `SYNC` is locked and opposite deck has a track
            if sync_on {
                if let (Some(ref my_trk), Some(ref oth_trk)) =
                    (&self.decks[idx].track, &self.decks[other].track)
                {
                    let oth_ratio = f32::from_bits(
                        other_ctrl.target_bpm_ratio_bits.load(Ordering::Relaxed),
                    );
                    let oth_eff_bpm = (oth_trk.native_bpm * oth_ratio).clamp(50.0, 220.0);
                    let my_folded = fold_bpm_to_reference(my_trk.native_bpm, oth_eff_bpm);
                    if my_folded > 20.0 {
                        base_ratio = (oth_eff_bpm / my_folded).clamp(0.75, 1.25);
                        ctrl.target_bpm_ratio_bits
                            .store(base_ratio.to_bits(), Ordering::Relaxed);
                    }
                }
            }

            let eff_ratio = (base_ratio * (1.0 + bend)).clamp(0.70, 1.30);

            // Handle Cue command
            let cue_cmd = ctrl.pending_cue_cmd.swap(0, Ordering::AcqRel);
            if cue_cmd != 0 && self.decks[idx].track.is_some() {
                let cur_aud = self.decks[idx].current_audible_frame(eff_ratio, mode);
                match cue_cmd {
                    1 => {
                        // CueReturn: jump to Cue & pause
                        let d = &mut self.decks[idx];
                        d.arm_anti_click_ramp();
                        d.active_loop = None;
                        d.applied_loop = DjLoopLength::Off;
                        ctrl.loop_beats
                            .store(DjLoopLength::Off as u8, Ordering::Release);
                        d.playhead_frame = d.cue_frame as f64;
                        d.playing = false;
                        ctrl.playing.store(false, Ordering::Release);
                    }
                    2 => {
                        // CueStutter: jump to Cue & play immediately
                        let d = &mut self.decks[idx];
                        d.arm_anti_click_ramp();
                        d.active_loop = None;
                        d.applied_loop = DjLoopLength::Off;
                        ctrl.loop_beats
                            .store(DjLoopLength::Off as u8, Ordering::Release);
                        d.playhead_frame = d.cue_frame as f64;
                        d.playing = true;
                        ctrl.playing.store(true, Ordering::Release);
                    }
                    3 => {
                        // SetCueHere: stamp Cue at current audible playhead (preserving paused state!)
                        let new_cue = cur_aud.round().max(0.0) as u64;
                        let d = &mut self.decks[idx];
                        let max_f = d.track.as_ref().map(|t| t.total_frames).unwrap_or(0);
                        d.cue_frame = new_cue.min(max_f);
                        if let Some(ref trk) = d.track {
                            trk.cue_hint_frame.store(d.cue_frame, Ordering::Release);
                        }
                    }
                    _ => {}
                }
            }

            // Handle Beat-Grid Seek (`L1 / R1` while paused keeps deck paused!)
            let delta_beats = ctrl.pending_seek_beats.swap(0, Ordering::AcqRel);
            if delta_beats != 0 && self.decks[idx].track.is_some() {
                let bf = self.decks[idx].beat_frames(sr);
                let f0 = self.decks[idx].first_beat_frame(sr);
                let cur = self.decks[idx].current_audible_frame(eff_ratio, mode);
                let beat_idx = ((cur - f0) / bf).round() + (delta_beats as f64);
                let max_f = self.decks[idx]
                    .track
                    .as_ref()
                    .map(|t| t.total_frames)
                    .unwrap_or(0) as f64;
                let snapped = (f0 + beat_idx * bf).clamp(0.0, max_f);
                let d = &mut self.decks[idx];
                if d.playing {
                    d.arm_anti_click_ramp();
                } else {
                    d.wsola.reset();
                    d.vinyl.reset();
                }
                d.active_loop = None;
                d.applied_loop = DjLoopLength::Off;
                ctrl.loop_beats
                    .store(DjLoopLength::Off as u8, Ordering::Release);
                d.playhead_frame = snapped;
            }

            // Handle Play / Pause + Quantized Beat-Phase Snap
            let want_play = ctrl.playing.load(Ordering::Acquire);
            let want_quant = ctrl.quantize_on_start.swap(false, Ordering::AcqRel);
            let want_sync_snap = ctrl.pending_sync_snap.swap(false, Ordering::AcqRel);
            let starting_now = want_play && !self.decks[idx].playing;

            if (starting_now && (want_quant || sync_on)) || (want_sync_snap && self.decks[other].playing)
            {
                if self.decks[other].playing && self.decks[other].track.is_some() {
                    let oth_mode =
                        DjTempoMode::from_u8(other_ctrl.tempo_mode.load(Ordering::Relaxed));
                    let oth_ratio = (f32::from_bits(
                        other_ctrl.target_bpm_ratio_bits.load(Ordering::Relaxed),
                    ) * (1.0
                        + f32::from_bits(other_ctrl.momentary_bend_bits.load(Ordering::Relaxed))))
                    .clamp(0.70, 1.30);
                    let oth_phase = self.decks[other]
                        .beat_coord(sr, oth_ratio, oth_mode)
                        .rem_euclid(1.0);

                    let my_bf = self.decks[idx].beat_frames(sr);
                    let my_f0 = self.decks[idx].first_beat_frame(sr);
                    let my_cur = self.decks[idx].current_audible_frame(eff_ratio, mode);
                    let my_coord = (my_cur - my_f0) / my_bf;
                    let my_phase = my_coord.rem_euclid(1.0);
                    let mut phase_err = oth_phase - my_phase;
                    if phase_err > 0.5 {
                        phase_err -= 1.0;
                    } else if phase_err < -0.5 {
                        phase_err += 1.0;
                    }
                    let max_f = self.decks[idx]
                        .track
                        .as_ref()
                        .map(|t| t.total_frames)
                        .unwrap_or(0) as f64;
                    let aligned = (my_cur + phase_err * my_bf).clamp(0.0, max_f);
                    let d = &mut self.decks[idx];
                    if d.playing {
                        d.arm_anti_click_ramp();
                    } else {
                        d.wsola.reset();
                        d.vinyl.reset();
                    }
                    d.playhead_frame = aligned;
                }
            }

            self.decks[idx].playing = want_play;
            self.decks[idx]
                .gate_smoother
                .set_target(if want_play { 1.0 } else { 0.0 });

            // Handle Seamless RAM Beat Loop (`4 -> 2 -> 1 -> 1/2 -> Off`)
            let want_loop = DjLoopLength::from_u8(ctrl.loop_beats.load(Ordering::Acquire));
            if want_loop != self.decks[idx].applied_loop {
                match want_loop {
                    DjLoopLength::Off => {
                        let d = &mut self.decks[idx];
                        if let Some(lp) = d.active_loop.take() {
                            d.playhead_frame = (lp.start_frame as f64) + lp.cursor;
                            d.wsola.reset();
                            d.vinyl.reset();
                        }
                        d.applied_loop = DjLoopLength::Off;
                    }
                    kind => {
                        let mut updated = false;
                        if let Some(ref mut lp) = self.decks[idx].active_loop {
                            updated = lp.set_length(kind);
                        }
                        if !updated {
                            let bf = self.decks[idx].beat_frames(sr);
                            let f0 = self.decks[idx].first_beat_frame(sr);
                            let cur = self.decks[idx].current_audible_frame(eff_ratio, mode);
                            let beat_n = ((cur - f0) / bf).floor();
                            let loop_start = (f0 + beat_n * bf).round().max(0.0) as u64;
                            let init_off = (cur - (loop_start as f64)).max(0.0);
                            let captured = self.decks[idx].track.as_ref().and_then(|trk| {
                                trk.ram.try_lock().ok().and_then(|ram| {
                                    PinnedBeatLoop::capture(&ram, loop_start, bf, kind, init_off)
                                })
                            });
                            if let Some(lp) = captured {
                                self.decks[idx].active_loop = Some(lp);
                                self.decks[idx].wsola.reset();
                                self.decks[idx].vinyl.reset();
                                updated = true;
                            }
                        }
                        if updated {
                            self.decks[idx].applied_loop = kind;
                        }
                    }
                }
            }

            // Update Filter target
            let flt_val = f32::from_bits(ctrl.filter_val_bits.load(Ordering::Relaxed));
            self.decks[idx].macro_filter.set_target(flt_val);

            // Publish playhead hint to background chunk refiller
            if let Some(ref trk) = self.decks[idx].track {
                trk.playhead_hint_frame.store(
                    self.decks[idx].playhead_frame.max(0.0) as u64,
                    Ordering::Relaxed,
                );
            }
        }
    }

    fn render_single_deck_subblock(&mut self, idx: usize, out_frames: usize) {
        let sr = self.sample_rate;
        let ctrl = &self.controls[idx];
        let mode = DjTempoMode::from_u8(ctrl.tempo_mode.load(Ordering::Relaxed));
        let base_ratio = f32::from_bits(ctrl.target_bpm_ratio_bits.load(Ordering::Relaxed));
        let bend = f32::from_bits(ctrl.momentary_bend_bits.load(Ordering::Relaxed));
        let ratio = (base_ratio * (1.0 + bend)).clamp(0.70, 1.30);

        let d = &mut self.decks[idx];
        for v in &mut d.scratch_stretched[..out_frames * 2] {
            *v = 0.0;
        }

        let has_track = d.track.is_some();
        let gate_active = d.playing || d.gate_smoother.current() > 1e-4;

        if has_track && gate_active {
            if let Some(ref mut lp) = d.active_loop {
                if (ratio - 1.0).abs() < 0.0005 {
                    lp.read_source_frames(&mut d.scratch_stretched[..out_frames * 2]);
                } else {
                    match mode {
                        DjTempoMode::MasterTempo => {
                            let mut written = 0usize;
                            let mut guard = 0usize;
                            while written < out_frames && guard < 12 {
                                guard += 1;
                                let pull = SUB_BLOCK_FRAMES * 8;
                                lp.read_source_frames(&mut d.scratch_src[..pull * 2]);
                                let (_, prod) = d.wsola.process_with_eof(
                                    &d.scratch_src[..pull * 2],
                                    ratio,
                                    false,
                                    &mut d.scratch_stretched[written * 2..out_frames * 2],
                                );
                                written += prod;
                            }
                        }
                        DjTempoMode::Vinyl => {
                            let need_in = (((out_frames as f32) * ratio).ceil() as usize + 8)
                                .min(SUB_BLOCK_FRAMES * 8);
                            lp.read_source_frames(&mut d.scratch_src[..need_in * 2]);
                            let _ = d.vinyl.process(
                                &d.scratch_src[..need_in * 2],
                                ratio,
                                &mut d.scratch_stretched[..out_frames * 2],
                            );
                        }
                    }
                }
            } else if let Some(ref trk) = d.track {
                let total_f = trk.total_frames;
                let cur_f = d.playhead_frame.floor().max(0.0) as u64;
                if cur_f >= total_f && total_f > 0 {
                    d.playing = false;
                    ctrl.playing.store(false, Ordering::Release);
                    d.gate_smoother.set_target(0.0);
                } else if let Ok(ram) = trk.ram.try_lock() {
                    let max_pull = (SUB_BLOCK_FRAMES * 20).min(
                        total_f.saturating_sub(cur_f) as usize,
                    );
                    for i in 0..max_pull {
                        let (l, r) = ram.read_frame_f32(cur_f + (i as u64)).unwrap_or((0.0, 0.0));
                        d.scratch_src[i * 2] = l;
                        d.scratch_src[i * 2 + 1] = r;
                    }
                    let is_eof = cur_f + (max_pull as u64) >= total_f;
                    match mode {
                        DjTempoMode::MasterTempo => {
                            let (cons, _) = d.wsola.process_with_eof(
                                &d.scratch_src[..max_pull * 2],
                                ratio,
                                is_eof,
                                &mut d.scratch_stretched[..out_frames * 2],
                            );
                            d.playhead_frame += cons as f64;
                        }
                        DjTempoMode::Vinyl => {
                            let (cons, _) = d.vinyl.process_with_eof(
                                &d.scratch_src[..max_pull * 2],
                                ratio,
                                is_eof,
                                &mut d.scratch_stretched[..out_frames * 2],
                            );
                            d.playhead_frame += cons as f64;
                        }
                    }
                }
            }
        }

        // Apply gate smoother, anti-click micro-ramp, bipolar Macro Filter, & Bass Swap
        for i in 0..out_frames {
            let gate = d.gate_smoother.next_sample();
            let mut l = d.scratch_stretched[i * 2] * gate;
            let mut r = d.scratch_stretched[i * 2 + 1] * gate;

            if d.ramp_pos < d.ramp_len {
                let u = ((d.ramp_pos + 1) as f32) / ((d.ramp_len + 1) as f32);
                let g_new = 0.5 * (1.0 - (PI * u).cos());
                let g_old = 1.0 - g_new;
                l = d.ramp_old_tail[d.ramp_pos * 2] * g_old + l * g_new;
                r = d.ramp_old_tail[d.ramp_pos * 2 + 1] * g_old + r * g_new;
                d.ramp_pos += 1;
            }

            let (fl, fr) = d.macro_filter.process_frame(l, r);
            let (bl, br) = d.bass_swap.process_frame(fl, fr);
            d.scratch_stretched[i * 2] = bl;
            d.scratch_stretched[i * 2 + 1] = br;
            d.last_out_l = bl;
            d.last_out_r = br;
        }
        let _ = sr;
    }

    /// Render `out_stereo_s16.len() / 2` mixed stereo `S16` frames.
    pub fn render_s16(&mut self, out_stereo_s16: &mut [i16]) -> usize {
        let total_frames = out_stereo_s16.len() / 2;
        let mut done = 0usize;

        while done < total_frames {
            let blk = (total_frames - done).min(SUB_BLOCK_FRAMES);
            self.apply_control_updates();

            let xfade_tgt = f32::from_bits(self.crossfader_bits.load(Ordering::Acquire));
            self.xfade_smoother.set_target(xfade_tgt);
            let bswap = self.bass_swap_enabled.load(Ordering::Relaxed);
            let (_, _, bass_a, bass_b) = evaluate_manual_crossfader(xfade_tgt, bswap);
            self.decks[0].bass_swap.set_bass_gain(bass_a);
            self.decks[1].bass_swap.set_bass_gain(bass_b);

            self.render_single_deck_subblock(0, blk);
            self.render_single_deck_subblock(1, blk);

            for i in 0..blk {
                let p = self.xfade_smoother.next_sample();
                let ang = p.clamp(0.0, 1.0) * FRAC_PI_2;
                let ga = ang.cos();
                let gb = ang.sin();

                let la = self.decks[0].scratch_stretched[i * 2];
                let ra = self.decks[0].scratch_stretched[i * 2 + 1];
                let lb = self.decks[1].scratch_stretched[i * 2];
                let rb = self.decks[1].scratch_stretched[i * 2 + 1];

                // Soft limiter guard above ±0.96 so resonant filter peaks never hard-clip S16
                let mix_l = soft_clip_guard(la * ga + lb * gb);
                let mix_r = soft_clip_guard(ra * ga + rb * gb);

                let dst_base = (done + i) * 2;
                out_stereo_s16[dst_base] =
                    (mix_l * 32767.0).round().clamp(-32768.0, 32767.0) as i16;
                out_stereo_s16[dst_base + 1] =
                    (mix_r * 32767.0).round().clamp(-32768.0, 32767.0) as i16;
            }
            done += blk;
        }
        total_frames
    }

    /// Snapshot full 60 FPS `PluginDjTelemetry` (including 24-bar zoomed 4-beat waveforms).
    pub fn snapshot_telemetry(&mut self, session_active: bool) -> PluginDjTelemetry {
        self.apply_control_updates();
        self.seq_gen = self.seq_gen.wrapping_add(1);
        let sr = self.sample_rate.max(8_000);
        let crossfader = f32::from_bits(self.crossfader_bits.load(Ordering::Relaxed));
        let bass_swap = self.bass_swap_enabled.load(Ordering::Relaxed);
        let tempo_mode = self.controls[0].tempo_mode.load(Ordering::Relaxed);

        let mut out = PluginDjTelemetry {
            seq_generation: self.seq_gen,
            session_active: u8::from(session_active),
            bass_swap_enabled: u8::from(bass_swap),
            tempo_mode,
            reserved: 0,
            crossfader,
            analog_lx: 0,
            analog_ly: 0,
            analog_rx: 0,
            analog_ry: 0,
            decks: [
                PluginDjDeckTelemetry::default(),
                PluginDjDeckTelemetry::default(),
            ],
        };

        for idx in 0..2 {
            let d = &self.decks[idx];
            let ctrl = &self.controls[idx];
            let Some(ref trk) = d.track else {
                continue;
            };

            let mode = DjTempoMode::from_u8(ctrl.tempo_mode.load(Ordering::Relaxed));
            let base_ratio = f32::from_bits(ctrl.target_bpm_ratio_bits.load(Ordering::Relaxed));
            let bend = f32::from_bits(ctrl.momentary_bend_bits.load(Ordering::Relaxed));
            let eff_ratio = (base_ratio * (1.0 + bend)).clamp(0.70, 1.30);

            let aud_frame = d.current_audible_frame(eff_ratio, mode);
            let elapsed_ms = ((aud_frame * 1000.0) / (sr as f64)).round().max(0.0) as u32;
            let dur_ms = trk.duration_ms.max(elapsed_ms);
            let remaining_ms = dur_ms.saturating_sub(elapsed_ms);
            let cue_ms = ((d.cue_frame * 1000) / (sr as u64)) as u32;

            let bf = d.beat_frames(sr).max(1.0);
            let f0 = d.first_beat_frame(sr);
            let beat_coord = (aud_frame - f0) / bf;
            let beat_phase = (beat_coord.rem_euclid(1.0)) as f32;
            let bar_phase = ((beat_coord.rem_euclid(4.0)) * 0.25) as f32;

            let mut bars_peak = [0u8; 24];
            let mut bars_kick = [0u8; 24];
            if let Ok(ram) = trk.ram.try_lock() {
                compute_zoomed_24_bars(
                    &ram,
                    sr,
                    trk.total_frames,
                    f0,
                    bf,
                    beat_coord,
                    d.active_loop.as_ref(),
                    &mut bars_peak,
                    &mut bars_kick,
                );
            }

            let mut dt = PluginDjDeckTelemetry {
                loaded: 1,
                playing: u8::from(d.playing),
                loop_beats: d.applied_loop as u8,
                sync_locked: u8::from(ctrl.sync_locked.load(Ordering::Relaxed)),
                elapsed_ms,
                remaining_ms,
                duration_ms: dur_ms,
                cue_ms,
                native_bpm: trk.native_bpm,
                effective_bpm: trk.native_bpm * eff_ratio,
                beat_phase,
                bar_phase,
                filter_val: f32::from_bits(ctrl.filter_val_bits.load(Ordering::Relaxed)),
                bars_peak_24: bars_peak,
                bars_kick_24: bars_kick,
                title: [0; 96],
                artist: [0; 80],
            };
            write_fixed_cstr(&mut dt.title, &trk.title);
            write_fixed_cstr(&mut dt.artist, &trk.artist);
            out.decks[idx] = dt;
        }

        out
    }

    pub fn deck_path(&self, deck: DjDeckId) -> Option<String> {
        let idx = deck.index();
        if let Ok(g) = self.pending_load[idx].try_lock() {
            if let Some((ref t, _, _)) = *g {
                return Some(t.path.clone());
            }
        }
        self.decks[idx].track.as_ref().map(|t| t.path.clone())
    }
}

/// Computes `24` discrete beat-quantized bars (`6` sub-steps per beat × `4` beats: `-2 .. +2` beats)
/// centered on `beat_coord`, respecting track `[0 .. total_frames)` bounds and active `Beat Loop` wrap!
#[allow(clippy::too_many_arguments)]
pub fn compute_zoomed_24_bars(
    ram: &DeckRamStore,
    sample_rate: u32,
    total_frames: u64,
    first_beat_frame: f64,
    beat_frames: f64,
    beat_coord: f64,
    active_loop: Option<&PinnedBeatLoop>,
    out_peak_24: &mut [u8; 24],
    out_kick_24: &mut [u8; 24],
) {
    let sr = sample_rate.max(8_000) as f64;
    let substep_frames = (beat_frames / 6.0).max(1.0);
    let center_slot = (beat_coord * 6.0).floor() as i64;

    let loop_bounds = active_loop.and_then(|lp| {
        let loop_beats = lp.loop_kind.beats_f32()? as f64;
        let start_slot = (((lp.start_frame as f64) - first_beat_frame) / substep_frames).round() as i64;
        let len_slots = ((loop_beats * 6.0).round() as i64).max(3);
        Some((start_slot, len_slots))
    });

    for i in 0..24 {
        let mut slot = center_slot - 12 + (i as i64);
        if let Some((ls, llen)) = loop_bounds {
            if slot >= ls {
                slot = ls + (slot - ls).rem_euclid(llen);
            }
        }

        let s_start_f = first_beat_frame + (slot as f64) * substep_frames;
        let s_end_f = s_start_f + substep_frames;

        if s_end_f <= 0.0 || (total_frames > 0 && s_start_f >= (total_frames as f64)) {
            out_peak_24[i] = 0;
            out_kick_24[i] = 0;
            continue;
        }

        let clamped_start = s_start_f.max(0.0);
        let clamped_end = if total_frames > 0 {
            s_end_f.min(total_frames as f64)
        } else {
            s_end_f
        };
        if clamped_end <= clamped_start {
            out_peak_24[i] = 0;
            out_kick_24[i] = 0;
            continue;
        }

        let b0 = ((clamped_start * 1000.0) / (sr * (ENV_BUCKET_MS as f64))).floor() as usize;
        let b1 = ((clamped_end * 1000.0) / (sr * (ENV_BUCKET_MS as f64))).ceil() as usize;

        let mut pk = 0u8;
        let mut kk = 0u8;
        if b0 < ram.env_buckets.len() {
            let hi = b1.max(b0 + 1).min(ram.env_buckets.len());
            for &(bp, bk) in &ram.env_buckets[b0..hi] {
                pk = pk.max(bp);
                kk = kk.max(bk);
            }
        }

        // Fallback to persistent 256-bucket `energy_contour` while distant lookahead chunks refill
        if pk == 0 && total_frames > 0 {
            let mid_f = 0.5 * (clamped_start + clamped_end);
            let c_idx = (((mid_f / (total_frames as f64)) * 255.0).round() as usize).min(255);
            let contour_v = ram.fallback_contour[c_idx];
            if contour_v > 0 {
                let is_on_beat = slot.rem_euclid(6) == 0;
                let is_off_eighth = slot.rem_euclid(6) == 3;
                let shape = if is_on_beat {
                    1.00f32
                } else if is_off_eighth {
                    0.68
                } else {
                    0.46
                };
                pk = (((contour_v as f32) * shape).round().clamp(18.0, 245.0)) as u8;
                kk = if is_on_beat {
                    pk.saturating_add(24)
                } else {
                    pk / 2
                };
            }
        }

        out_peak_24[i] = pk;
        out_kick_24[i] = kk;
    }
}

#[inline]
fn soft_clip_guard(x: f32) -> f32 {
    let a = x.abs();
    if a <= 0.92 {
        x
    } else {
        let excess = a - 0.92;
        let comp = 0.92 + 0.08 * (1.0 - (-excess * 12.5).exp());
        comp.copysign(x)
    }
}

/// Helper to build an in-memory `LoadedDeckTrack` directly from stereo `S16` PCM
/// (used by WebAssembly simulator, `pcmbuf` handoff seeding, and unit tests).
#[allow(clippy::too_many_arguments)]
pub fn build_in_memory_deck_track(
    path: &str,
    title: &str,
    artist: &str,
    sample_rate: u32,
    stereo_s16: Vec<i16>,
    native_bpm: f32,
    first_beat_ms: u32,
    contour: [u8; 256],
) -> LoadedDeckTrack {
    let sr = sample_rate.max(8_000);
    let total_frames = (stereo_s16.len() / 2) as u64;
    let duration_ms = ((total_frames * 1000) / (sr as u64)) as u32;
    let initial_cue_frame = (((first_beat_ms as u64) * (sr as u64)) / 1000).min(total_frames);

    let mut store = DeckRamStore::new(sr, total_frames, contour);
    store.update_envelope_from_s16(0, &stereo_s16);
    let full_win = Arc::new(PinnedPcmWindow {
        start_frame: 0,
        frames: total_frames as usize,
        stereo_s16,
    });
    store.cue_window = Some(Arc::clone(&full_win));
    store.full_pcm = Some(full_win);

    LoadedDeckTrack {
        path: path.to_string(),
        title: title.to_string(),
        artist: artist.to_string(),
        duration_ms,
        total_frames,
        native_bpm: native_bpm.clamp(50.0, 220.0),
        first_beat_ms,
        initial_cue_frame,
        ram: Arc::new(Mutex::new(store)),
        playhead_hint_frame: Arc::new(AtomicU64::new(initial_cue_frame)),
        cue_hint_frame: Arc::new(AtomicU64::new(initial_cue_frame)),
    }
}

// ============================================================================
// Native Background Time-Sliced Dual-Deck RAM Window Refiller (`non-wasm32`)
// ============================================================================

#[cfg(not(target_arch = "wasm32"))]
mod bg_refill {
    use super::*;
    use crate::decoder::DeckFileDecoder;
    use std::sync::mpsc::{self, Receiver, Sender};
    use std::thread;
    use std::time::Duration;

    pub enum RefillCmd {
        InstallDeck {
            deck: DjDeckId,
            path: String,
            sample_rate: u32,
            duration_hint_ms: u32,
            ram: Arc<Mutex<DeckRamStore>>,
            playhead_hint: Arc<AtomicU64>,
            cue_hint: Arc<AtomicU64>,
            seeded_until_frame: u64,
        },
        ClearAll,
    }

    struct ActiveDeckStream {
        path: String,
        sample_rate: u32,
        decoder: Option<DeckFileDecoder>,
        ram: Arc<Mutex<DeckRamStore>>,
        playhead_hint: Arc<AtomicU64>,
        cue_hint: Arc<AtomicU64>,
        pinned_cue_center: u64,
        scratch_chunk: Vec<i16>,
    }

    impl ActiveDeckStream {
        fn ensure_cue_window(&mut self, cue_frame: u64) {
            let Some(ref mut dec) = self.decoder else {
                return;
            };
            let sr = self.sample_rate as u64;
            let pre = (sr * (CUE_PRE_ROLL_MS as u64)) / 1000;
            let post = (sr * (CUE_POST_ROLL_MS as u64)) / 1000;
            let win_start = cue_frame.saturating_sub(pre);
            let want_frames = ((cue_frame + post).saturating_sub(win_start)) as usize;
            if want_frames == 0 {
                return;
            }

            dec.seek_to_dst_frame(win_start);
            let mut buf = vec![0i16; want_frames * 2];
            let got = dec.decode_dst_s16(&mut buf);
            buf.truncate(got * 2);
            if got > 0 {
                if let Ok(mut ram) = self.ram.lock() {
                    ram.update_envelope_from_s16(win_start, &buf);
                    ram.cue_window = Some(Arc::new(PinnedPcmWindow {
                        start_frame: win_start,
                        frames: got,
                        stereo_s16: buf,
                    }));
                }
            }
            self.pinned_cue_center = cue_frame;
        }

        /// Decode at most ONE `RAM_CHUNK_FRAMES` (`8192`-frame) chunk per time-slice so Deck A
        /// and Deck B alternate smoothly without starving each other!
        fn step_one_slice(&mut self) -> bool {
            let cur_cue = self.cue_hint.load(Ordering::Acquire);
            if cur_cue != self.pinned_cue_center {
                self.ensure_cue_window(cur_cue);
                return true;
            }

            let Some(ref mut dec) = self.decoder else {
                return false;
            };
            let play_frame = self.playhead_hint.load(Ordering::Relaxed);
            let center_chunk = play_frame / (RAM_CHUNK_FRAMES as u64);
            // Window: `[-4 chunks (~0.7s) .. +60 chunks (~11.1s)]` around `center_chunk`
            let start_c = center_chunk.saturating_sub(2);
            let end_c = center_chunk + 60;

            let mut target_chunk: Option<u64> = None;
            if let Ok(ram) = self.ram.lock() {
                let max_c = (ram.total_frames + (RAM_CHUNK_FRAMES as u64) - 1)
                    / (RAM_CHUNK_FRAMES as u64);
                // Prioritize chunks `[center_chunk .. end_c]` ahead of playhead first!
                for c in center_chunk..=end_c.min(max_c.saturating_sub(1)) {
                    if !ram.has_chunk(c) {
                        target_chunk = Some(c);
                        break;
                    }
                }
                if target_chunk.is_none() {
                    for c in start_c..center_chunk.min(max_c) {
                        if !ram.has_chunk(c) {
                            target_chunk = Some(c);
                            break;
                        }
                    }
                }
            }

            let Some(cidx) = target_chunk else {
                return false;
            };

            let target_frame = cidx * (RAM_CHUNK_FRAMES as u64);
            if dec.decoded_dst_frame() != target_frame {
                dec.seek_to_dst_frame(target_frame);
            }
            let got = dec.decode_dst_s16(&mut self.scratch_chunk);
            if got == 0 {
                return false;
            }
            if let Ok(mut ram) = self.ram.lock() {
                ram.update_envelope_from_s16(target_frame, &self.scratch_chunk[..got * 2]);
                ram.insert_chunk(cidx, &self.scratch_chunk[..got * 2], center_chunk);
                if got < RAM_CHUNK_FRAMES && dec.is_eof() {
                    let exact_end = target_frame + (got as u64);
                    if exact_end > 0 {
                        ram.total_frames = exact_end;
                    }
                }
            }
            let _ = &self.path;
            true
        }
    }

    pub struct DeckRefillWorker {
        tx: Sender<RefillCmd>,
    }

    impl DeckRefillWorker {
        pub fn spawn() -> Self {
            let (tx, rx) = mpsc::channel::<RefillCmd>();
            let _ = thread::Builder::new()
                .name("uipod-dj-refill".to_string())
                .spawn(move || worker_loop(rx));
            Self { tx }
        }

        pub fn send(&self, cmd: RefillCmd) {
            let _ = self.tx.send(cmd);
        }
    }

    fn worker_loop(rx: Receiver<RefillCmd>) {
        let mut slots: [Option<ActiveDeckStream>; 2] = [None, None];
        loop {
            while let Ok(cmd) = rx.try_recv() {
                handle_cmd(cmd, &mut slots);
            }

            let mut did_work = false;
            for slot in slots.iter_mut().flatten() {
                if slot.step_one_slice() {
                    did_work = true;
                }
            }

            if !did_work {
                match rx.recv_timeout(Duration::from_millis(12)) {
                    Ok(cmd) => handle_cmd(cmd, &mut slots),
                    Err(mpsc::RecvTimeoutError::Timeout) => {}
                    Err(mpsc::RecvTimeoutError::Disconnected) => break,
                }
            } else {
                thread::yield_now();
            }
        }
    }

    fn handle_cmd(cmd: RefillCmd, slots: &mut [Option<ActiveDeckStream>; 2]) {
        match cmd {
            RefillCmd::ClearAll => {
                slots[0] = None;
                slots[1] = None;
            }
            RefillCmd::InstallDeck {
                deck,
                path,
                sample_rate,
                duration_hint_ms,
                ram,
                playhead_hint,
                cue_hint,
                seeded_until_frame,
            } => {
                let mut dec = DeckFileDecoder::open(&path, sample_rate, duration_hint_ms);
                if let Some(ref d) = dec {
                    let dur_ms = d.duration_ms();
                    if dur_ms > 0 {
                        if let Ok(mut store) = ram.lock() {
                            if store.total_frames == 0 {
                                store.total_frames =
                                    ((dur_ms as u64) * (sample_rate as u64)) / 1000;
                            }
                        }
                    }
                }
                let init_cue = cue_hint.load(Ordering::Acquire);
                let mut stream = ActiveDeckStream {
                    path,
                    sample_rate,
                    decoder: None,
                    ram,
                    playhead_hint,
                    cue_hint,
                    pinned_cue_center: u64::MAX,
                    scratch_chunk: vec![0i16; RAM_CHUNK_FRAMES * 2],
                };
                if seeded_until_frame == 0 {
                    // Decode immediate Cue window first so paused/stutter cueing is ready in < 15 ms!
                    stream.decoder = dec.take();
                    stream.ensure_cue_window(init_cue);
                } else {
                    if let Some(ref mut d) = dec {
                        d.seek_to_dst_frame(seeded_until_frame);
                    }
                    stream.decoder = dec;
                    stream.pinned_cue_center = init_cue;
                }
                slots[deck.index()] = Some(stream);
            }
        }
    }
}

#[cfg(not(target_arch = "wasm32"))]
pub use bg_refill::{DeckRefillWorker, RefillCmd};

#[cfg(not(target_arch = "wasm32"))]
#[allow(clippy::too_many_arguments)]
pub fn prepare_streaming_deck_track(
    worker: &DeckRefillWorker,
    deck: DjDeckId,
    path: &str,
    title: &str,
    artist: &str,
    sample_rate: u32,
    duration_ms: u32,
    start_ms: u32,
    native_bpm: f32,
    first_beat_ms: u32,
    contour: [u8; 256],
    seeded_pcm_s16: Option<&[i16]>,
) -> LoadedDeckTrack {
    let sr = sample_rate.max(8_000);
    let est_dur_ms = duration_ms.max(start_ms + 30_000);
    let total_frames = ((est_dur_ms as u64) * (sr as u64)) / 1000;
    let initial_cue_frame = (((first_beat_ms as u64) * (sr as u64)) / 1000).min(total_frames);
    let start_frame = (((start_ms as u64) * (sr as u64)) / 1000).min(total_frames);

    let mut store = DeckRamStore::new(sr, total_frames, contour);
    let mut seeded_until = 0u64;

    if let Some(seed) = seeded_pcm_s16 {
        let seed_frames = seed.len() / 2;
        if seed_frames > 0 {
            store.update_envelope_from_s16(start_frame, seed);
            // Pin seeded live `pcmbuf` audio directly into `cue_window` so `t = 0` plays with ZERO gap!
            store.cue_window = Some(Arc::new(PinnedPcmWindow {
                start_frame,
                frames: seed_frames,
                stereo_s16: seed.to_vec(),
            }));
            seeded_until = start_frame + (seed_frames as u64);
        }
    }

    let ram = Arc::new(Mutex::new(store));
    let playhead_hint = Arc::new(AtomicU64::new(start_frame));
    let cue_hint = Arc::new(AtomicU64::new(initial_cue_frame));

    worker.send(RefillCmd::InstallDeck {
        deck,
        path: path.to_string(),
        sample_rate: sr,
        duration_hint_ms: duration_ms,
        ram: Arc::clone(&ram),
        playhead_hint: Arc::clone(&playhead_hint),
        cue_hint: Arc::clone(&cue_hint),
        seeded_until_frame: seeded_until,
    });

    LoadedDeckTrack {
        path: path.to_string(),
        title: title.to_string(),
        artist: artist.to_string(),
        duration_ms,
        total_frames,
        native_bpm: native_bpm.clamp(50.0, 220.0),
        first_beat_ms,
        initial_cue_frame,
        ram,
        playhead_hint_frame: playhead_hint,
        cue_hint_frame: cue_hint,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn synth_sine_and_kicks_s16(sr: u32, sec: f32, bpm: f32, carrier_hz: f32) -> Vec<i16> {
        let frames = ((sr as f32) * sec).round() as usize;
        let beat_frames = (sr as f32) * 60.0 / bpm;
        let mut out = vec![0i16; frames * 2];
        for i in 0..frames {
            let t = (i as f32) / (sr as f32);
            let carrier = 0.45 * (2.0 * PI * carrier_hz * t).sin();
            let in_beat = (i as f32).rem_euclid(beat_frames);
            let kick = if in_beat < (sr as f32) * 0.055 {
                let kt = in_beat / (sr as f32);
                0.45 * (-kt * 42.0).exp() * (2.0 * PI * 64.0 * kt).sin()
            } else {
                0.0
            };
            let s = ((carrier + kick).clamp(-0.95, 0.95) * 32767.0).round() as i16;
            out[i * 2] = s;
            out[i * 2 + 1] = s;
        }
        out
    }

    #[test]
    fn seamless_ram_beat_loop_4_2_1_half_zero_drift_and_click_free_seam() {
        let sr = 48_000u32;
        let bpm = 120.0f32; // Exact 1 beat = 24,000 frames!
        let pcm = synth_sine_and_kicks_s16(sr, 8.0, bpm, 220.0);
        let trk = build_in_memory_deck_track(
            "/test/loop.wav",
            "Loop Test",
            "DJ Unit",
            sr,
            pcm,
            bpm,
            0,
            [180; 256],
        );

        let ram = trk.ram.lock().unwrap();
        let beat_frames = (sr as f64) * 60.0 / (bpm as f64); // 24,000.0
        let loop_start = 48_000u64; // Beat 2 (has clean pre-roll `[47904..48000)`)
        let mut lp =
            PinnedBeatLoop::capture(&ram, loop_start, beat_frames, DjLoopLength::Beats4, 0.0)
                .expect("capture 4-beat loop");
        drop(ram);

        for (kind, expected_len) in [
            (DjLoopLength::Beats4, 96_000usize),
            (DjLoopLength::Beats2, 48_000usize),
            (DjLoopLength::Beat1, 24_000usize),
            (DjLoopLength::HalfBeat, 12_000usize),
        ] {
            assert!(lp.set_length(kind));
            assert_eq!(lp.active_len_frames, expected_len);
            lp.cursor = 0.0;

            // Render exactly 6 full loop cycles in arbitrary prime-sized chunks (`509` frames)
            // and verify:
            // 1. Exact cursor wrap (`0.0` sample count drift after `6 * expected_len` frames!)
            // 2. Click-free `2 ms` crossfade seam (`max_step` across wrap boundary has zero discontinuity!)
            let total_to_pull = expected_len * 6;
            let mut pulled = 0usize;
            let mut prev_l = lp.sample_with_seam(0).0;
            let mut max_step = 0.0f32;
            let mut chunk = vec![0.0f32; 509 * 2];

            while pulled < total_to_pull {
                let n = (total_to_pull - pulled).min(509);
                lp.read_source_frames(&mut chunk[..n * 2]);
                for i in 0..n {
                    let l = chunk[i * 2];
                    if pulled + i > 0 {
                        max_step = max_step.max((l - prev_l).abs());
                    }
                    prev_l = l;
                }
                pulled += n;
            }

            assert!(
                lp.cursor.abs() < 1e-9,
                "Expected zero sample drift for {kind:?}, got cursor={}",
                lp.cursor
            );
            // Max natural step of `0.45*sin(2*pi*220*t) + 0.45*sin(2*pi*64*t)` @ 48 kHz is < 0.04;
            // any un-crossfaded phase jump would spike to ~0.6..0.9!
            assert!(
                max_step < 0.08,
                "Loop seam clicked on {kind:?}: max sample-to-sample step = {max_step}"
            );
        }
    }

    #[test]
    fn crossfader_equal_power_and_bass_swap_symmetry_plus_bipolar_filter() {
        // 1. Exact equal-power & Bass Swap symmetry across `[0.0 ..= 1.0]`
        for step in 0..=100 {
            let p = (step as f32) * 0.01;
            let (ga1, gb1, ba1, bb1) = evaluate_manual_crossfader(p, true);
            let (ga2, gb2, ba2, bb2) = evaluate_manual_crossfader(1.0 - p, true);

            let power = ga1 * ga1 + gb1 * gb1;
            assert!(
                (power - 1.0).abs() < 1e-5,
                "Equal-power violated at p={p}: {power}"
            );
            assert!((ga1 - gb2).abs() < 1e-5);
            assert!((gb1 - ga2).abs() < 1e-5);
            assert!(
                (ba1 - bb2).abs() < 1e-5 && (bb1 - ba2).abs() < 1e-5,
                "Bass Swap asymmetry at p={p}: ({ba1}, {bb1}) vs ({ba2}, {bb2})"
            );
            // Center `p = 0.5`: combined sub-bass amplitude `ga*ba + gb*bb <= 1.01` (never muddies/clips!)
            assert!(ga1 * ba1 + gb1 * bb1 <= 1.005);
        }

        // 2. Resonant bipolar Macro Filter (`LPF -1.0` cuts `3 kHz`, `0.0` flat bypass, `HPF +1.0` cuts `80 Hz`)
        let sr = 48_000u32;
        let mut flt = DjMacroFilter::new(sr);

        // Flat deadzone `0.0`
        flt.set_target(0.0);
        for i in 0..512 {
            let x = (2.0 * PI * 1000.0 * (i as f32) / (sr as f32)).sin() * 0.7;
            let (y, _) = flt.process_frame(x, x);
            if i > 64 {
                assert!((y - x).abs() < 1e-5);
            }
        }

        // Full LPF (`-1.0` -> `120 Hz`): `3000 Hz` tone strongly attenuated
        flt.set_target(-1.0);
        let mut lpf_hi_peak = 0.0f32;
        for i in 0..8000 {
            let x = (2.0 * PI * 3000.0 * (i as f32) / (sr as f32)).sin() * 0.8;
            let (y, _) = flt.process_frame(x, x);
            if i > 6000 {
                lpf_hi_peak = lpf_hi_peak.max(y.abs());
            }
        }
        assert!(
            lpf_hi_peak < 0.02,
            "Expected 120 Hz LPF to attenuate 3 kHz strongly, got {lpf_hi_peak}"
        );

        // Full HPF (`+1.0` -> `3.5 kHz`): `80 Hz` sub-bass strongly attenuated
        flt.set_target(1.0);
        let mut hpf_lo_peak = 0.0f32;
        for i in 0..8000 {
            let x = (2.0 * PI * 80.0 * (i as f32) / (sr as f32)).sin() * 0.8;
            let (y, _) = flt.process_frame(x, x);
            if i > 6000 {
                hpf_lo_peak = hpf_lo_peak.max(y.abs());
            }
        }
        assert!(
            hpf_lo_peak < 0.02,
            "Expected 3.5 kHz HPF to attenuate 80 Hz strongly, got {hpf_lo_peak}"
        );
    }

    #[test]
    fn zoomed_24_bar_telemetry_across_track_boundaries_and_active_loops() {
        let sr = 48_000u32;
        let bpm = 120.0f32; // 1 beat = 24,000 frames, 1 substep = 4,000 frames
        let pcm = synth_sine_and_kicks_s16(sr, 4.0, bpm, 0.0); // Pure kicks on beats 0..7!
        let trk = build_in_memory_deck_track(
            "/test/kicks.wav",
            "Kicks",
            "DJ Unit",
            sr,
            pcm,
            bpm,
            0,
            [0; 256],
        );

        let ram = trk.ram.lock().unwrap();
        let bf = (sr as f64) * 60.0 / (bpm as f64);

        // 1. At `0:00` (`beat_coord = 0.0`): left half (`0..12`, `-2..0` beats before start) must be 0,
        //    while beat 0 (`idx 12`) and beat 1 (`idx 18`) have strong kick bars!
        let mut pk = [0u8; 24];
        let mut kk = [0u8; 24];
        compute_zoomed_24_bars(&ram, sr, trk.total_frames, 0.0, bf, 0.0, None, &mut pk, &mut kk);
        assert!(pk[..12].iter().all(|&v| v == 0), "Pre-0:00 bars should be 0: {pk:?}");
        assert!(kk[12] > 140 && kk[18] > 140, "Expected strong kicks at indices 12 & 18: {kk:?}");
        assert!(kk[15] < 40, "Mid-beat silence should stay low: {kk:?}");

        // 2. At EOF (`beat_coord = 8.0`): right half (`12..24`, past EOF) must be 0 when not looping!
        compute_zoomed_24_bars(&ram, sr, trk.total_frames, 0.0, bf, 8.0, None, &mut pk, &mut kk);
        assert!(pk[12..].iter().all(|&v| v == 0), "Post-EOF bars should be 0: {pk:?}");
        assert!(kk[0] > 140 && kk[6] > 140);

        // 3. With active 1-beat loop starting at beat 7 (`start_frame = 7 * 24000`), bars past beat 8
        //    wrap seamlessly back into beat 7 instead of dropping to 0 at EOF!
        let lp = PinnedBeatLoop::capture(&ram, 7 * 24_000, bf, DjLoopLength::Beat1, 0.0).unwrap();
        compute_zoomed_24_bars(
            &ram,
            sr,
            trk.total_frames,
            0.0,
            bf,
            7.5,
            Some(&lp),
            &mut pk,
            &mut kk,
        );
        // At `beat_coord = 7.5` (`center_slot = 45`), `i = 15` corresponds to `slot = 48` (beat 8 -> wraps to beat 7 kick!)
        assert!(
            kk[15] > 140 && kk[21] > 140,
            "Active loop should wrap future bars across EOF: {kk:?}"
        );
    }
}
