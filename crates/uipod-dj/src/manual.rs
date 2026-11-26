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

use std::cell::UnsafeCell;
use std::f32::consts::{FRAC_1_SQRT_2, FRAC_PI_2, PI};
use std::sync::atomic::{
    fence, AtomicBool, AtomicI32, AtomicU16, AtomicU32, AtomicU64, AtomicU8, AtomicUsize, Ordering,
};
use std::sync::{Arc, Mutex};

use uipod_plugin_sdk::{
    DjCueAction, DjDeckId, DjLoopLength, DjTempoMode, PluginDjDeckTelemetry, PluginDjTelemetry,
};

use crate::filter::OnePoleSmoother;
use crate::planner::fold_bpm_to_reference;
use crate::resampler::VinylResampler;
use crate::wsola::WsolaStretcher;

const SUB_BLOCK_FRAMES: usize = 128;
const MAX_RAMP_FRAMES: usize = 1024;
/// `8192` stereo frames per sliding RAM chunk (`~185.7 ms` @ `44.1 kHz`, `32 KiB`).
pub const RAM_CHUNK_FRAMES: usize = 8192;
/// Power-of-2 lock-free chunk ring slots (`128 * 8192` frames = `~21.8s` @ `48 kHz`).
const CHUNK_RING_SLOTS: usize = 128;
const CHUNK_RING_MASK: usize = CHUNK_RING_SLOTS - 1;
/// Pinned Cue window duration (`-0.5s .. +3.5s` = `4.0s` around Cue for instant zero-seek stutter).
#[cfg(not(target_arch = "wasm32"))]
const CUE_PRE_ROLL_MS: u32 = 500;
#[cfg(not(target_arch = "wasm32"))]
const CUE_POST_ROLL_MS: u32 = 3_500;
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
    wet_lpf_smoother: OnePoleSmoother,
    wet_hpf_smoother: OnePoleSmoother,
    lp_ic1_l: f32,
    lp_ic2_l: f32,
    lp_ic1_r: f32,
    lp_ic2_r: f32,
    lp_a1: f32,
    lp_a2: f32,
    lp_a3: f32,
    hp_ic1_l: f32,
    hp_ic2_l: f32,
    hp_ic1_r: f32,
    hp_ic2_r: f32,
    hp_a1: f32,
    hp_a2: f32,
    hp_a3: f32,
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
            wet_lpf_smoother: OnePoleSmoother::new(0.0, sr, 0.005),
            wet_hpf_smoother: OnePoleSmoother::new(0.0, sr, 0.005),
            lp_ic1_l: 0.0,
            lp_ic2_l: 0.0,
            lp_ic1_r: 0.0,
            lp_ic2_r: 0.0,
            lp_a1: 0.0,
            lp_a2: 0.0,
            lp_a3: 0.0,
            hp_ic1_l: 0.0,
            hp_ic2_l: 0.0,
            hp_ic1_r: 0.0,
            hp_ic2_r: 0.0,
            hp_a1: 0.0,
            hp_a2: 0.0,
            hp_a3: 0.0,
            coeff_div: 0,
        };
        f.update_coeffs(0.0);
        f
    }

    pub fn reset(&mut self, sample_rate: u32) {
        let sr = sample_rate.max(8_000);
        self.sample_rate = sr;
        self.val_smoother.set_sample_rate(sr, 0.008);
        self.wet_lpf_smoother.set_sample_rate(sr, 0.005);
        self.wet_hpf_smoother.set_sample_rate(sr, 0.005);
        self.val_smoother.reset(0.0);
        self.wet_lpf_smoother.reset(0.0);
        self.wet_hpf_smoother.reset(0.0);
        self.lp_ic1_l = 0.0;
        self.lp_ic2_l = 0.0;
        self.lp_ic1_r = 0.0;
        self.lp_ic2_r = 0.0;
        self.hp_ic1_l = 0.0;
        self.hp_ic2_l = 0.0;
        self.hp_ic1_r = 0.0;
        self.hp_ic2_r = 0.0;
        self.coeff_div = 0;
        self.update_coeffs(0.0);
    }

    #[inline]
    pub fn set_target(&mut self, value: f32) {
        let v = value.clamp(-1.0, 1.0);
        self.val_smoother.set_target(v);
        if v < -Self::DEADZONE {
            self.wet_lpf_smoother.set_target(1.0);
            self.wet_hpf_smoother.set_target(0.0);
        } else if v > Self::DEADZONE {
            self.wet_lpf_smoother.set_target(0.0);
            self.wet_hpf_smoother.set_target(1.0);
        } else {
            self.wet_lpf_smoother.set_target(0.0);
            self.wet_hpf_smoother.set_target(0.0);
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
        let lp_max = Self::LPF_MAX_HZ.min(nyq_guard);
        let hp_max = Self::HPF_MAX_HZ.min(nyq_guard);

        let fc_lp = if val < -Self::DEADZONE {
            let u = ((-val - Self::DEADZONE) / (1.0 - Self::DEADZONE)).clamp(0.0, 1.0);
            lp_max * (Self::LPF_MIN_HZ / lp_max).powf(u)
        } else {
            lp_max
        };
        let fc_hp = if val > Self::DEADZONE {
            let u = ((val - Self::DEADZONE) / (1.0 - Self::DEADZONE)).clamp(0.0, 1.0);
            Self::HPF_MIN_HZ * (hp_max / Self::HPF_MIN_HZ).powf(u)
        } else {
            Self::HPF_MIN_HZ
        };

        let k = Self::K_RESONANT;
        let g_lp = (PI * fc_lp.clamp(15.0, nyq_guard) / fs).tan();
        let lp_a1 = 1.0 / (1.0 + g_lp * (g_lp + k));
        let lp_a2 = g_lp * lp_a1;
        self.lp_a1 = lp_a1;
        self.lp_a2 = lp_a2;
        self.lp_a3 = g_lp * lp_a2;

        let g_hp = (PI * fc_hp.clamp(15.0, nyq_guard) / fs).tan();
        let hp_a1 = 1.0 / (1.0 + g_hp * (g_hp + k));
        let hp_a2 = g_hp * hp_a1;
        self.hp_a1 = hp_a1;
        self.hp_a2 = hp_a2;
        self.hp_a3 = g_hp * hp_a2;
    }

    #[inline]
    pub fn is_neutral_bypass(&self) -> bool {
        self.wet_lpf_smoother.current() <= 1e-5
            && self.wet_lpf_smoother.is_settled(1e-5)
            && self.wet_hpf_smoother.current() <= 1e-5
            && self.wet_hpf_smoother.is_settled(1e-5)
            && self.val_smoother.current().abs() <= Self::DEADZONE
    }

    #[inline]
    pub fn process_frame(&mut self, l: f32, r: f32) -> (f32, f32) {
        let val = self.val_smoother.next_sample();
        let w_lp = self.wet_lpf_smoother.next_sample();
        let w_hp = self.wet_hpf_smoother.next_sample();

        if self.coeff_div == 0 {
            self.update_coeffs(val);
            self.coeff_div = 15;
        } else {
            self.coeff_div -= 1;
        }

        // Step dedicated LPF & HPF stereo TPT states independently so crossing `0.0` (LPF <-> HPF)
        // or engaging out of deadzone NEVER teleports state or clicks, and L/R never share delay lines!
        let lp_v3_l = l - self.lp_ic2_l;
        let lp_v1_l = self.lp_a1 * self.lp_ic1_l + self.lp_a2 * lp_v3_l;
        let lp_v2_l = self.lp_ic2_l + self.lp_a2 * self.lp_ic1_l + self.lp_a3 * lp_v3_l;
        self.lp_ic1_l = 2.0 * lp_v1_l - self.lp_ic1_l;
        self.lp_ic2_l = 2.0 * lp_v2_l - self.lp_ic2_l;

        let lp_v3_r = r - self.lp_ic2_r;
        let lp_v1_r = self.lp_a1 * self.lp_ic1_r + self.lp_a2 * lp_v3_r;
        let lp_v2_r = self.lp_ic2_r + self.lp_a2 * self.lp_ic1_r + self.lp_a3 * lp_v3_r;
        self.lp_ic1_r = 2.0 * lp_v1_r - self.lp_ic1_r;
        self.lp_ic2_r = 2.0 * lp_v2_r - self.lp_ic2_r;

        let hp_v3_l = l - self.hp_ic2_l;
        let hp_v1_l = self.hp_a1 * self.hp_ic1_l + self.hp_a2 * hp_v3_l;
        let hp_v2_l = self.hp_ic2_l + self.hp_a2 * self.hp_ic1_l + self.hp_a3 * hp_v3_l;
        self.hp_ic1_l = 2.0 * hp_v1_l - self.hp_ic1_l;
        self.hp_ic2_l = 2.0 * hp_v2_l - self.hp_ic2_l;

        let hp_v3_r = r - self.hp_ic2_r;
        let hp_v1_r = self.hp_a1 * self.hp_ic1_r + self.hp_a2 * hp_v3_r;
        let hp_v2_r = self.hp_ic2_r + self.hp_a2 * self.hp_ic1_r + self.hp_a3 * hp_v3_r;
        self.hp_ic1_r = 2.0 * hp_v1_r - self.hp_ic1_r;
        self.hp_ic2_r = 2.0 * hp_v2_r - self.hp_ic2_r;

        // True bit-exact flat bypass once settled inside deadzone (`|filter_val| <= 0.05`)
        if w_lp <= 1e-5 && w_hp <= 1e-5 && val.abs() <= Self::DEADZONE {
            return (l, r);
        }

        let k = Self::K_RESONANT;
        let hp_l = l - k * hp_v1_l - hp_v2_l;
        let hp_r = r - k * hp_v1_r - hp_v2_r;

        let tot = w_lp + w_hp;
        let (eff_lp, eff_hp) = if tot > 1.0 {
            (w_lp / tot, w_hp / tot)
        } else {
            (w_lp, w_hp)
        };
        let dry = (1.0 - eff_lp - eff_hp).max(0.0);

        (
            l * dry + lp_v2_l * eff_lp + hp_l * eff_hp,
            r * dry + lp_v2_r * eff_lp + hp_r * eff_hp,
        )
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
    pub fn is_unity_bypass(&self) -> bool {
        (self.bass_smoother.current() - 1.0).abs() <= 1e-6 && self.bass_smoother.is_settled(1e-6)
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
        if delta.abs() <= 1e-6 {
            return (l, r);
        }
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

/// Single lock-free `SeqLock`-guarded stereo `S16` chunk slot (`8192` frames, pre-allocated once).
struct LockFreeChunkSlot {
    seq: AtomicU32,
    chunk_index: AtomicU64,
    valid_frames: AtomicUsize,
    pcm: UnsafeCell<[i16; RAM_CHUNK_FRAMES * 2]>,
}

// SAFETY: Single background producer (`uipod-dj-refill`) mutates `pcm` strictly while `seq` is odd
// with Acquire/Release memory barriers; real-time audio reader copies out and verifies even `seq`.
unsafe impl Sync for LockFreeChunkSlot {}
unsafe impl Send for LockFreeChunkSlot {}

impl LockFreeChunkSlot {
    fn new() -> Self {
        Self {
            seq: AtomicU32::new(0),
            chunk_index: AtomicU64::new(u64::MAX),
            valid_frames: AtomicUsize::new(0),
            pcm: UnsafeCell::new([0i16; RAM_CHUNK_FRAMES * 2]),
        }
    }
}

/// Ping-pong wait-free Cue window buffer (pre-allocated once, swapped atomically on Cue moves).
struct LockFreeCueSlot {
    seq: AtomicU32,
    start_frame: AtomicU64,
    frames: AtomicUsize,
    pcm: UnsafeCell<Vec<i16>>,
}

unsafe impl Sync for LockFreeCueSlot {}
unsafe impl Send for LockFreeCueSlot {}

impl LockFreeCueSlot {
    fn with_capacity(max_frames: usize) -> Self {
        Self {
            seq: AtomicU32::new(0),
            start_frame: AtomicU64::new(0),
            frames: AtomicUsize::new(0),
            pcm: UnsafeCell::new(vec![0i16; max_frames * 2]),
        }
    }
}

/// 100% Lock-Free Per-Deck RAM PCM Store (`ZERO Mutexes` on audio, UI, or refill threads!).
///
/// - `chunks`: `128`-slot `O(1)` power-of-2 SeqLock ring (`~21.8s` @ `48 kHz`)
/// - `cue_slots`: double-buffered wait-free Cue stutter window
/// - `seed_window`: immutable `pcmbuf` handoff seed with `5 ms` crossfade seam into decoded chunks
/// - `full_pcm`: immutable full-track buffer (used by WASM simulator & unit tests)
/// - `env_buckets`: pre-allocated `AtomicU16` `(peak_u8 << 8) | kick_u8` envelope strip
pub struct DeckRamStore {
    pub sample_rate: u32,
    total_frames: AtomicU64,
    pub seed_window: Option<Arc<PinnedPcmWindow>>,
    pub full_pcm: Option<Arc<PinnedPcmWindow>>,
    cue_slots: Box<[LockFreeCueSlot; 2]>,
    active_cue_slot: AtomicU8,
    chunks: Box<[LockFreeChunkSlot]>,
    env_buckets: Box<[AtomicU16]>,
    pub fallback_contour: [u8; 256],
}

impl DeckRamStore {
    pub fn new(sample_rate: u32, total_frames: u64, contour: [u8; 256]) -> Self {
        Self::new_internal(sample_rate, total_frames, contour, true)
    }

    pub fn new_in_memory(
        sample_rate: u32,
        stereo_s16: Vec<i16>,
        contour: [u8; 256],
    ) -> Self {
        let sr = sample_rate.max(8_000);
        let total_frames = (stereo_s16.len() / 2) as u64;
        let mut store = Self::new_internal(sr, total_frames, contour, false);
        store.update_envelope_from_s16(0, &stereo_s16);
        let full_win = Arc::new(PinnedPcmWindow {
            start_frame: 0,
            frames: total_frames as usize,
            stereo_s16,
        });
        store.full_pcm = Some(full_win);
        store
    }

    fn new_internal(
        sample_rate: u32,
        total_frames: u64,
        contour: [u8; 256],
        alloc_streaming_ring: bool,
    ) -> Self {
        let sr = sample_rate.max(8_000);
        let est_ms = ((total_frames * 1000) / (sr as u64)) as usize;
        let n_buckets = (est_ms / (ENV_BUCKET_MS as usize) + 64).clamp(64, 600_000);
        let cue_cap = if alloc_streaming_ring {
            ((sr as usize) * 4) + 1024
        } else {
            0
        };
        let chunk_count = if alloc_streaming_ring {
            CHUNK_RING_SLOTS
        } else {
            0
        };
        let mut chunks_vec = Vec::with_capacity(chunk_count);
        for _ in 0..chunk_count {
            chunks_vec.push(LockFreeChunkSlot::new());
        }
        let mut env_vec = Vec::with_capacity(n_buckets);
        for _ in 0..n_buckets {
            env_vec.push(AtomicU16::new(0));
        }

        Self {
            sample_rate: sr,
            total_frames: AtomicU64::new(total_frames),
            seed_window: None,
            full_pcm: None,
            cue_slots: Box::new([
                LockFreeCueSlot::with_capacity(cue_cap),
                LockFreeCueSlot::with_capacity(cue_cap),
            ]),
            active_cue_slot: AtomicU8::new(0xFF),
            chunks: chunks_vec.into_boxed_slice(),
            env_buckets: env_vec.into_boxed_slice(),
            fallback_contour: contour,
        }
    }

    #[inline]
    pub fn total_frames(&self) -> u64 {
        self.total_frames.load(Ordering::Acquire)
    }

    #[inline]
    pub fn set_total_frames(&self, frames: u64) {
        self.total_frames.store(frames, Ordering::Release);
    }

    #[inline]
    pub fn env_bucket_len(&self) -> usize {
        self.env_buckets.len()
    }

    #[inline]
    pub fn read_env_bucket(&self, idx: usize) -> (u8, u8) {
        if idx >= self.env_buckets.len() {
            return (0, 0);
        }
        let packed = self.env_buckets[idx].load(Ordering::Relaxed);
        ((packed >> 8) as u8, (packed & 0xFF) as u8)
    }

    pub fn publish_cue_window(&self, start_frame: u64, pcm_s16: &[i16]) {
        let cur = self.active_cue_slot.load(Ordering::Relaxed);
        let next_idx = if cur == 0 { 1usize } else { 0usize };
        let slot = &self.cue_slots[next_idx];

        let max_frames = unsafe { (*slot.pcm.get()).len() / 2 };
        let valid = (pcm_s16.len() / 2).min(max_frames);
        if valid == 0 {
            return;
        }

        let s0 = slot.seq.load(Ordering::Relaxed);
        let odd = (s0 | 1).wrapping_add(if (s0 & 1) != 0 { 2 } else { 0 });
        slot.seq.store(odd, Ordering::Release);
        fence(Ordering::SeqCst);

        unsafe {
            let dst = &mut *slot.pcm.get();
            dst[..valid * 2].copy_from_slice(&pcm_s16[..valid * 2]);
        }
        slot.start_frame.store(start_frame, Ordering::Relaxed);
        slot.frames.store(valid, Ordering::Relaxed);

        slot.seq.store(odd.wrapping_add(1), Ordering::Release);
        self.active_cue_slot.store(next_idx as u8, Ordering::Release);
    }

    #[inline]
    fn read_cue_frame_f32(&self, frame: u64) -> Option<(f32, f32)> {
        let idx = self.active_cue_slot.load(Ordering::Acquire);
        if idx > 1 {
            return None;
        }
        let slot = &self.cue_slots[idx as usize];
        let s1 = slot.seq.load(Ordering::Acquire);
        if (s1 & 1) != 0 {
            return None;
        }
        let start = slot.start_frame.load(Ordering::Relaxed);
        let len = slot.frames.load(Ordering::Relaxed);
        if frame < start || frame >= start + (len as u64) {
            return None;
        }
        let off = ((frame - start) as usize) * 2;
        let (l_i16, r_i16) = unsafe {
            let buf = &*slot.pcm.get();
            if off + 1 >= buf.len() {
                return None;
            }
            (buf[off], buf[off + 1])
        };
        fence(Ordering::Acquire);
        if slot.seq.load(Ordering::Relaxed) != s1 {
            return None;
        }
        let scale = 1.0 / 32768.0;
        Some(((l_i16 as f32) * scale, (r_i16 as f32) * scale))
    }

    #[inline]
    fn read_chunk_frame_f32(&self, frame: u64) -> Option<(f32, f32)> {
        if self.chunks.is_empty() {
            return None;
        }
        let cidx = frame / (RAM_CHUNK_FRAMES as u64);
        let off = (frame % (RAM_CHUNK_FRAMES as u64)) as usize;
        let slot = &self.chunks[(cidx as usize) & CHUNK_RING_MASK];

        let s1 = slot.seq.load(Ordering::Acquire);
        if (s1 & 1) != 0 {
            return None;
        }
        let stored_cidx = slot.chunk_index.load(Ordering::Relaxed);
        let valid = slot.valid_frames.load(Ordering::Relaxed);
        if stored_cidx != cidx || off >= valid {
            return None;
        }
        let (l_i16, r_i16) = unsafe {
            let buf = &*slot.pcm.get();
            (buf[off * 2], buf[off * 2 + 1])
        };
        fence(Ordering::Acquire);
        if slot.seq.load(Ordering::Relaxed) != s1 {
            return None;
        }
        let scale = 1.0 / 32768.0;
        Some(((l_i16 as f32) * scale, (r_i16 as f32) * scale))
    }

    #[inline]
    pub fn read_frame_f32(&self, frame: u64) -> Option<(f32, f32)> {
        if let Some(ref full) = self.full_pcm {
            if let Some(pair) = full.sample_f32(frame) {
                return Some(pair);
            }
        }
        if let Some(ref seed) = self.seed_window {
            if let Some((sl, sr)) = seed.sample_f32(frame) {
                let seed_end = seed.start_frame + (seed.frames as u64);
                let seam = (((self.sample_rate as u64) * 5) / 1000).clamp(64, 512);
                if seed.frames as u64 > seam * 2 && frame + seam >= seed_end {
                    if let Some((cl, cr)) = self.read_chunk_frame_f32(frame) {
                        let rem = (seed_end - frame) as f32;
                        let u = (1.0 - rem / (seam as f32)).clamp(0.0, 1.0);
                        let g_chunk = 0.5 * (1.0 - (PI * u).cos());
                        let g_seed = 1.0 - g_chunk;
                        return Some((
                            sl * g_seed + cl * g_chunk,
                            sr * g_seed + cr * g_chunk,
                        ));
                    }
                }
                return Some((sl, sr));
            }
        }
        if let Some(pair) = self.read_chunk_frame_f32(frame) {
            return Some(pair);
        }
        self.read_cue_frame_f32(frame)
    }

    /// Fast lock-free contiguous block reader (`O(1)` chunk lookup per `8192`-frame boundary).
    /// Returns how many consecutive stereo frames were read starting at `start_frame`.
    pub fn read_block_f32(&self, start_frame: u64, dst_interleaved: &mut [f32]) -> usize {
        let want_frames = dst_interleaved.len() / 2;
        if want_frames == 0 {
            return 0;
        }
        let scale = 1.0 / 32768.0;

        // 1. Fast path for full in-memory tracks (WASM & unit tests)
        if let Some(ref full) = self.full_pcm {
            if start_frame >= full.start_frame {
                let rel = (start_frame - full.start_frame) as usize;
                if rel < full.frames {
                    let take = want_frames.min(full.frames - rel);
                    let src = &full.stereo_s16[rel * 2..(rel + take) * 2];
                    for i in 0..take * 2 {
                        dst_interleaved[i] = (src[i] as f32) * scale;
                    }
                    return take;
                }
            }
            return 0;
        }

        // 2. If overlapping `seed_window`, use seamless per-frame crossfader across the `5 ms` seam
        if let Some(ref seed) = self.seed_window {
            let seed_end = seed.start_frame + (seed.frames as u64);
            if start_frame < seed_end && start_frame + (want_frames as u64) > seed.start_frame {
                let mut got = 0usize;
                for i in 0..want_frames {
                    if let Some((l, r)) = self.read_frame_f32(start_frame + (i as u64)) {
                        dst_interleaved[i * 2] = l;
                        dst_interleaved[i * 2 + 1] = r;
                        got += 1;
                    } else {
                        break;
                    }
                }
                return got;
            }
        }

        // 3. O(1) SeqLock chunk-span copy across `chunks` (with `cue_slots` fallback)
        let mut done = 0usize;
        while done < want_frames {
            let cur_f = start_frame + (done as u64);
            if !self.chunks.is_empty() {
                let cidx = cur_f / (RAM_CHUNK_FRAMES as u64);
                let off = (cur_f % (RAM_CHUNK_FRAMES as u64)) as usize;
                let slot = &self.chunks[(cidx as usize) & CHUNK_RING_MASK];
                let s1 = slot.seq.load(Ordering::Acquire);
                if (s1 & 1) == 0 {
                    let stored_cidx = slot.chunk_index.load(Ordering::Relaxed);
                    let valid = slot.valid_frames.load(Ordering::Relaxed);
                    if stored_cidx == cidx && off < valid {
                        let take = (want_frames - done).min(valid - off);
                        unsafe {
                            let buf = &*slot.pcm.get();
                            let src = &buf[off * 2..(off + take) * 2];
                            let dst = &mut dst_interleaved[done * 2..(done + take) * 2];
                            for k in 0..take * 2 {
                                dst[k] = (src[k] as f32) * scale;
                            }
                        }
                        fence(Ordering::Acquire);
                        if slot.seq.load(Ordering::Relaxed) == s1 {
                            done += take;
                            continue;
                        }
                    }
                }
            }

            if let Some((l, r)) = self.read_cue_frame_f32(cur_f) {
                dst_interleaved[done * 2] = l;
                dst_interleaved[done * 2 + 1] = r;
                done += 1;
            } else {
                break;
            }
        }
        done
    }

    pub fn insert_chunk(&self, chunk_index: u64, pcm_s16: &[i16], _center_chunk: u64) {
        if self.chunks.is_empty() {
            return;
        }
        let valid_frames = (pcm_s16.len() / 2).min(RAM_CHUNK_FRAMES);
        if valid_frames == 0 {
            return;
        }
        let slot = &self.chunks[(chunk_index as usize) & CHUNK_RING_MASK];
        if slot.chunk_index.load(Ordering::Relaxed) == chunk_index
            && slot.valid_frames.load(Ordering::Relaxed) >= valid_frames
        {
            return;
        }

        let s0 = slot.seq.load(Ordering::Relaxed);
        let odd = (s0 | 1).wrapping_add(if (s0 & 1) != 0 { 2 } else { 0 });
        slot.seq.store(odd, Ordering::Release);
        fence(Ordering::SeqCst);

        unsafe {
            let dst = &mut *slot.pcm.get();
            dst[..valid_frames * 2].copy_from_slice(&pcm_s16[..valid_frames * 2]);
        }
        slot.valid_frames.store(valid_frames, Ordering::Relaxed);
        slot.chunk_index.store(chunk_index, Ordering::Relaxed);

        slot.seq.store(odd.wrapping_add(1), Ordering::Release);
    }

    #[inline]
    pub fn has_chunk(&self, chunk_index: u64) -> bool {
        if let Some(ref full) = self.full_pcm {
            let start = chunk_index * (RAM_CHUNK_FRAMES as u64);
            if full.contains_frame(start) {
                return true;
            }
        }
        if self.chunks.is_empty() {
            return false;
        }
        let slot = &self.chunks[(chunk_index as usize) & CHUNK_RING_MASK];
        let s1 = slot.seq.load(Ordering::Acquire);
        (s1 & 1) == 0
            && slot.chunk_index.load(Ordering::Relaxed) == chunk_index
            && slot.valid_frames.load(Ordering::Relaxed) > 0
    }

    pub fn update_envelope_from_s16(&self, start_frame: u64, stereo_s16: &[i16]) {
        let frames = stereo_s16.len() / 2;
        if frames == 0 || self.env_buckets.is_empty() {
            return;
        }
        let sr = self.sample_rate.max(8_000);
        let hop_frames = ((sr as usize) * (ENV_BUCKET_MS as usize) / 1000).max(32);
        let alpha_160 = (1.0 - (-2.0 * PI * 160.0 / (sr as f32)).exp()).clamp(0.005, 0.25);
        let scale = 1.0 / 32768.0;
        let mut lp = 0.0f32;

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
                // Fast hardware `sqrt` gamma approximation (`0.68..0.72` curve, zero `libm::powf` calls!)
                let pc = peak.clamp(0.0, 1.0);
                let kc = (low_peak * 1.35).clamp(0.0, 1.0);
                let pk_gamma = pc.sqrt() * (0.64 + 0.36 * pc.sqrt());
                let kk_gamma = kc.sqrt() * (0.56 + 0.44 * kc.sqrt());
                let pk_u8 = (pk_gamma * 255.0).round() as u8;
                let kk_u8 = (kk_gamma * 255.0).round() as u8;

                let prev = self.env_buckets[b_idx].load(Ordering::Relaxed);
                let prev_pk = (prev >> 8) as u8;
                let prev_kk = (prev & 0xFF) as u8;
                let next_packed =
                    ((prev_pk.max(pk_u8) as u16) << 8) | (prev_kk.max(kk_u8) as u16);
                if next_packed != prev {
                    self.env_buckets[b_idx].store(next_packed, Ordering::Relaxed);
                }
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

    /// Peek continuous looped source stereo frames starting at `self.cursor` without advancing.
    pub fn peek_source_frames(&self, out_interleaved: &mut [f32]) {
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
    }

    #[inline]
    pub fn advance_cursor(&mut self, consumed_frames: usize) {
        let len = self.active_len_frames.max(1) as f64;
        self.cursor = (self.cursor + consumed_frames as f64).rem_euclid(len);
    }

    /// Pull `count` continuous looped source stereo frames (with exact modulo wrap and zero drift).
    pub fn read_source_frames(&mut self, out_interleaved: &mut [f32]) {
        let frames = out_interleaved.len() / 2;
        self.peek_source_frames(out_interleaved);
        self.advance_cursor(frames);
    }
}

/// Metadata & lock-free shared RAM handle installed when loading a track into a deck.
pub struct LoadedDeckTrack {
    pub path: String,
    pub title: String,
    pub artist: String,
    pub sample_rate: u32,
    pub duration_ms: u32,
    pub total_frames: u64,
    pub native_bpm: f32,
    pub first_beat_ms: u32,
    pub initial_cue_frame: u64,
    pub ram: Arc<DeckRamStore>,
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

    /// Per-deck stateful sample-rate converter when `track.sample_rate != core.sample_rate`.
    src_rate_conv: VinylResampler,
    rate_fifo_len: usize,
    last_mode: DjTempoMode,
    wsola: WsolaStretcher,
    vinyl: VinylResampler,
    macro_filter: DjMacroFilter,
    bass_swap: BassSwapFilter,
    gate_smoother: OnePoleSmoother,

    // `5 ms` raised-cosine anti-click ramp on handoff, Cue, & quantized Jump (post-filter C1-continuous)
    ramp_old_tail: Vec<f32>,
    ramp_pos: usize,
    ramp_len: usize,
    prev_out_l: f32,
    prev_out_r: f32,
    last_out_l: f32,
    last_out_r: f32,

    scratch_raw_trk: Vec<f32>,
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
            src_rate_conv: VinylResampler::new(),
            rate_fifo_len: 0,
            last_mode: DjTempoMode::MasterTempo,
            wsola: WsolaStretcher::new(sr),
            vinyl: VinylResampler::new(),
            macro_filter: DjMacroFilter::new(sr),
            bass_swap: BassSwapFilter::new(sr),
            gate_smoother: OnePoleSmoother::new(0.0, sr, 0.0025),
            ramp_old_tail: vec![0.0; MAX_RAMP_FRAMES * 2],
            ramp_pos: 0,
            ramp_len: 0,
            prev_out_l: 0.0,
            prev_out_r: 0.0,
            last_out_l: 0.0,
            last_out_r: 0.0,
            scratch_raw_trk: vec![0.0; SUB_BLOCK_FRAMES * 32 * 2],
            scratch_src: vec![0.0; SUB_BLOCK_FRAMES * 24 * 2],
            scratch_stretched: vec![0.0; SUB_BLOCK_FRAMES * 2],
        }
    }

    fn set_sample_rate(&mut self, sample_rate: u32) {
        let sr = sample_rate.max(8_000);
        self.src_rate_conv.reset();
        self.rate_fifo_len = 0;
        self.wsola = WsolaStretcher::new(sr);
        self.vinyl.reset();
        self.macro_filter.reset(sr);
        self.bass_swap.set_sample_rate(sr);
        self.gate_smoother.set_sample_rate(sr, 0.0025);
    }

    #[inline]
    fn arm_anti_click_ramp(&mut self, sample_rate: u32) {
        let len = (((sample_rate.max(8_000) as usize) * 5) / 1000).clamp(128, MAX_RAMP_FRAMES);
        let mut cur_l = self.last_out_l;
        let mut cur_r = self.last_out_r;
        let mut vel_l = (self.last_out_l - self.prev_out_l).clamp(-0.12, 0.12);
        let mut vel_r = (self.last_out_r - self.prev_out_r).clamp(-0.12, 0.12);
        for i in 0..len {
            let decay = 1.0 - (i as f32) / (len as f32);
            cur_l = (cur_l + vel_l).clamp(-1.05, 1.05);
            cur_r = (cur_r + vel_r).clamp(-1.05, 1.05);
            vel_l *= 0.94;
            vel_r *= 0.94;
            self.ramp_old_tail[i * 2] = cur_l * decay;
            self.ramp_old_tail[i * 2 + 1] = cur_r * decay;
        }
        self.ramp_pos = 0;
        self.ramp_len = len;
        self.src_rate_conv.reset();
        self.rate_fifo_len = 0;
        self.wsola.reset();
        self.vinyl.reset();
    }

    #[inline]
    fn track_sample_rate(&self, fallback_sr: u32) -> u32 {
        self.track
            .as_ref()
            .map(|t| t.sample_rate.max(8_000))
            .unwrap_or_else(|| fallback_sr.max(8_000))
    }

    #[inline]
    fn beat_frames(&self, fallback_sr: u32) -> f64 {
        let trk_sr = self.track_sample_rate(fallback_sr);
        let bpm = self
            .track
            .as_ref()
            .map(|t| t.native_bpm)
            .unwrap_or(120.0)
            .clamp(40.0, 240.0) as f64;
        (trk_sr as f64) * 60.0 / bpm
    }

    #[inline]
    fn first_beat_frame(&self, fallback_sr: u32) -> f64 {
        let trk_sr = self.track_sample_rate(fallback_sr);
        let ms = self.track.as_ref().map(|t| t.first_beat_ms).unwrap_or(0) as f64;
        ms * (trk_sr as f64) * 0.001
    }

    #[inline]
    fn current_audible_frame(&self, sink_sr: u32, ratio: f32, mode: DjTempoMode) -> f64 {
        if let Some(ref lp) = self.active_loop {
            return (lp.start_frame as f64) + lp.cursor;
        }
        let trk_sr = self.track_sample_rate(sink_sr);
        let sr_scale = (trk_sr as f64) / (sink_sr.max(8_000) as f64);
        let unplayed_trk = match mode {
            DjTempoMode::MasterTempo => {
                let w_unplayed = self.wsola.unplayed_input_frames(ratio);
                if trk_sr == sink_sr {
                    w_unplayed
                } else {
                    self.src_rate_conv.unplayed_input_frames()
                        + ((self.rate_fifo_len as f64) + w_unplayed) * sr_scale
                }
            }
            DjTempoMode::Vinyl => self.vinyl.unplayed_input_frames(),
        };
        (self.playhead_frame - unplayed_trk).max(0.0)
    }

    #[inline]
    fn beat_coord(&self, sink_sr: u32, ratio: f32, mode: DjTempoMode) -> f64 {
        let bf = self.beat_frames(sink_sr).max(1.0);
        let f0 = self.first_beat_frame(sink_sr);
        (self.current_audible_frame(sink_sr, ratio, mode) - f0) / bf
    }
}

/// Real-time Dual-Deck DJ processor (safe for both native ALSA/SDL threads and single-threaded WASM/tests).
pub struct ManualDjCore {
    sample_rate: u32,
    decks: [DeckRtState; 2],
    pub controls: [Arc<DeckAtomicControls>; 2],
    pub pending_load: [Arc<Mutex<Option<(LoadedDeckTrack, bool, u64)>>>; 2],
    pub crossfader_bits: Arc<AtomicU32>,
    pub bass_swap_enabled: Arc<AtomicBool>,
    xfade_smoother: OnePoleSmoother,
    seq_gen: AtomicU32,
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
            pending_load: [Arc::new(Mutex::new(None)), Arc::new(Mutex::new(None))],
            crossfader_bits: Arc::new(AtomicU32::new(0.0f32.to_bits())),
            bass_swap_enabled: Arc::new(AtomicBool::new(true)),
            xfade_smoother: OnePoleSmoother::new(0.0, sr, 0.006),
            seq_gen: AtomicU32::new(0),
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

    /// Step pending atomic control commands when the hardware audio sink is idle/paused.
    pub fn step_idle_controls(&mut self) {
        self.apply_control_updates();
    }

    /// Snap an idle/pre-buffered deck's playhead to the exact live handoff timestamp (`ms`) right
    /// before flipping `MANUAL_DJ_ACTIVE = true` so zero ms elapse between pre-buffering and audio output.
    pub fn sync_idle_playhead_ms(&mut self, deck: DjDeckId, playhead_ms: u32) {
        self.apply_control_updates();
        let idx = deck.index();
        let fallback_sr = self.sample_rate;
        let d = &mut self.decks[idx];
        let trk_sr = d.track_sample_rate(fallback_sr);
        if let Some(ref trk) = d.track {
            let total_f = trk.ram.total_frames().max(trk.total_frames);
            let target_f = (((playhead_ms as u64) * (trk_sr as u64)) / 1000).min(total_f);
            d.playhead_frame = target_f as f64;
            d.src_rate_conv.reset();
            d.rate_fifo_len = 0;
            d.wsola.reset();
            d.vinyl.reset();
            trk.playhead_hint_frame.store(target_f, Ordering::Release);
        }
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
                d.arm_anti_click_ramp(sr);
                let total_f = trk.ram.total_frames().max(trk.total_frames);
                d.cue_frame = trk.initial_cue_frame.min(total_f);
                d.playhead_frame = start_frame.min(total_f) as f64;
                d.playing = !start_paused;
                d.applied_loop = DjLoopLength::Off;
                d.active_loop = None;
                d.last_mode = DjTempoMode::from_u8(
                    self.controls[idx].tempo_mode.load(Ordering::Relaxed),
                );
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
                let cur_aud = self.decks[idx].current_audible_frame(sr, eff_ratio, mode);
                match cue_cmd {
                    1 => {
                        // CueReturn: jump to Cue & pause
                        let d = &mut self.decks[idx];
                        d.arm_anti_click_ramp(sr);
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
                        d.arm_anti_click_ramp(sr);
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
                        let max_f = d
                            .track
                            .as_ref()
                            .map(|t| t.ram.total_frames())
                            .unwrap_or(0);
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
                let cur = self.decks[idx].current_audible_frame(sr, eff_ratio, mode);
                let beat_idx = ((cur - f0) / bf).round() + (delta_beats as f64);
                let max_f = self.decks[idx]
                    .track
                    .as_ref()
                    .map(|t| t.ram.total_frames())
                    .unwrap_or(0) as f64;
                let snapped = (f0 + beat_idx * bf).clamp(0.0, max_f);
                let d = &mut self.decks[idx];
                if d.playing {
                    d.arm_anti_click_ramp(sr);
                } else {
                    d.src_rate_conv.reset();
                    d.rate_fifo_len = 0;
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
                    let my_cur = self.decks[idx].current_audible_frame(sr, eff_ratio, mode);
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
                        .map(|t| t.ram.total_frames())
                        .unwrap_or(0) as f64;
                    let aligned = (my_cur + phase_err * my_bf).clamp(0.0, max_f);
                    let d = &mut self.decks[idx];
                    if d.playing {
                        d.arm_anti_click_ramp(sr);
                    } else {
                        d.src_rate_conv.reset();
                        d.rate_fifo_len = 0;
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
                            if d.playing
                                && (!d.wsola.is_bypass_ready()
                                    || !d.vinyl.is_bypass_ready()
                                    || d.rate_fifo_len > 0)
                            {
                                d.arm_anti_click_ramp(sr);
                            } else {
                                d.src_rate_conv.reset();
                                d.rate_fifo_len = 0;
                                d.wsola.reset();
                                d.vinyl.reset();
                            }
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
                            let cur = self.decks[idx].current_audible_frame(sr, eff_ratio, mode);
                            let beat_n = ((cur - f0) / bf).floor();
                            let loop_start = (f0 + beat_n * bf).round().max(0.0) as u64;
                            let init_off = (cur - (loop_start as f64)).max(0.0);
                            let captured = self.decks[idx].track.as_ref().and_then(|trk| {
                                PinnedBeatLoop::capture(&trk.ram, loop_start, bf, kind, init_off)
                            });
                            if let Some(lp) = captured {
                                let d = &mut self.decks[idx];
                                if d.playing
                                    && (!d.wsola.is_bypass_ready()
                                        || !d.vinyl.is_bypass_ready()
                                        || d.rate_fifo_len > 0)
                                {
                                    d.arm_anti_click_ramp(sr);
                                } else {
                                    d.src_rate_conv.reset();
                                    d.rate_fifo_len = 0;
                                    d.wsola.reset();
                                    d.vinyl.reset();
                                }
                                d.active_loop = Some(lp);
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
        let sr = self.sample_rate.max(8_000);
        let ctrl = &self.controls[idx];
        let mode = DjTempoMode::from_u8(ctrl.tempo_mode.load(Ordering::Relaxed));
        let base_ratio = f32::from_bits(ctrl.target_bpm_ratio_bits.load(Ordering::Relaxed));
        let bend = f32::from_bits(ctrl.momentary_bend_bits.load(Ordering::Relaxed));
        let ratio = (base_ratio * (1.0 + bend)).clamp(0.70, 1.30);

        if self.decks[idx].last_mode != mode {
            let prev_mode = self.decks[idx].last_mode;
            let aud = self.decks[idx].current_audible_frame(sr, ratio, prev_mode);
            let d = &mut self.decks[idx];
            if d.active_loop.is_none() {
                d.playhead_frame = aud;
            }
            if d.playing
                && (!d.wsola.is_bypass_ready()
                    || !d.vinyl.is_bypass_ready()
                    || d.rate_fifo_len > 0)
            {
                d.arm_anti_click_ramp(sr);
            } else {
                d.src_rate_conv.reset();
                d.rate_fifo_len = 0;
                d.wsola.reset();
                d.vinyl.reset();
            }
            d.last_mode = mode;
        }

        let trk_sr = self.decks[idx].track_sample_rate(sr);
        let sr_ratio = (trk_sr as f32) / (sr as f32);

        let d = &mut self.decks[idx];
        let has_track = d.track.is_some();
        let gate_active = d.playing || d.gate_smoother.current() > 1e-6;
        let mut produced = 0usize;

        if has_track && gate_active {
            match mode {
                DjTempoMode::Vinyl => {
                    d.rate_fifo_len = 0;
                    let eff_vinyl_ratio = (ratio * sr_ratio).clamp(0.5, 2.0);
                    let need_in = d
                        .vinyl
                        .needed_input_frames(out_frames, eff_vinyl_ratio)
                        .min(SUB_BLOCK_FRAMES * 20);

                    if let Some(ref mut lp) = d.active_loop {
                        if (eff_vinyl_ratio - 1.0).abs() < 0.0005 && d.vinyl.is_bypass_ready() {
                            lp.read_source_frames(&mut d.scratch_src[..out_frames * 2]);
                            let (_, prod) = d.vinyl.process_with_eof(
                                &d.scratch_src[..out_frames * 2],
                                1.0,
                                false,
                                &mut d.scratch_stretched[..out_frames * 2],
                            );
                            produced = prod;
                        } else {
                            let mut written = 0usize;
                            let mut guard = 0usize;
                            while written < out_frames && guard < 8 {
                                guard += 1;
                                let rem = out_frames - written;
                                let pull = d
                                    .vinyl
                                    .needed_input_frames(rem, eff_vinyl_ratio)
                                    .min(SUB_BLOCK_FRAMES * 20);
                                lp.peek_source_frames(&mut d.scratch_src[..pull * 2]);
                                let (cons, prod) = d.vinyl.process_with_eof(
                                    &d.scratch_src[..pull * 2],
                                    eff_vinyl_ratio,
                                    false,
                                    &mut d.scratch_stretched[written * 2..out_frames * 2],
                                );
                                lp.advance_cursor(cons);
                                written += prod;
                                if prod == 0 && cons == 0 {
                                    break;
                                }
                            }
                            produced = written;
                        }
                    } else if let Some(ref trk) = d.track {
                        let total_f = trk.ram.total_frames();
                        let cur_f = d.playhead_frame.floor().max(0.0) as u64;
                        if total_f > 0 && cur_f >= total_f && d.vinyl.is_bypass_ready() {
                            d.playing = false;
                            ctrl.playing.store(false, Ordering::Release);
                            d.gate_smoother.set_target(0.0);
                        } else {
                            let avail = if total_f > 0 {
                                total_f.saturating_sub(cur_f) as usize
                            } else {
                                need_in
                            };
                            let want_pull = need_in.min(avail);
                            let got = trk
                                .ram
                                .read_block_f32(cur_f, &mut d.scratch_src[..want_pull * 2]);
                            let is_eof = total_f > 0 && cur_f + (got as u64) >= total_f;
                            let (cons, prod) = d.vinyl.process_with_eof(
                                &d.scratch_src[..got * 2],
                                eff_vinyl_ratio,
                                is_eof,
                                &mut d.scratch_stretched[..out_frames * 2],
                            );
                            d.playhead_frame += cons as f64;
                            produced = prod;
                            if prod == 0 && is_eof && cons == 0 {
                                d.playing = false;
                                ctrl.playing.store(false, Ordering::Release);
                                d.gate_smoother.set_target(0.0);
                            }
                        }
                    }
                }
                DjTempoMode::MasterTempo => {
                    if trk_sr == sr {
                        d.rate_fifo_len = 0;
                        let need_in = d
                            .wsola
                            .needed_input_frames(out_frames, ratio)
                            .min(SUB_BLOCK_FRAMES * 20);

                        if let Some(ref mut lp) = d.active_loop {
                            if (ratio - 1.0).abs() < 0.0005 && d.wsola.is_bypass_ready() {
                                lp.read_source_frames(&mut d.scratch_stretched[..out_frames * 2]);
                                produced = out_frames;
                            } else {
                                let mut written = 0usize;
                                let mut guard = 0usize;
                                while written < out_frames && guard < 8 {
                                    guard += 1;
                                    let rem = out_frames - written;
                                    let pull = d
                                        .wsola
                                        .needed_input_frames(rem, ratio)
                                        .min(SUB_BLOCK_FRAMES * 20);
                                    lp.peek_source_frames(&mut d.scratch_src[..pull * 2]);
                                    let (cons, prod) = d.wsola.process_with_eof(
                                        &d.scratch_src[..pull * 2],
                                        ratio,
                                        false,
                                        &mut d.scratch_stretched[written * 2..out_frames * 2],
                                    );
                                    lp.advance_cursor(cons);
                                    written += prod;
                                    if prod == 0 && cons == 0 {
                                        break;
                                    }
                                }
                                produced = written;
                            }
                        } else if let Some(ref trk) = d.track {
                            let total_f = trk.ram.total_frames();
                            let cur_f = d.playhead_frame.floor().max(0.0) as u64;
                            if total_f > 0 && cur_f >= total_f && d.wsola.is_bypass_ready() {
                                d.playing = false;
                                ctrl.playing.store(false, Ordering::Release);
                                d.gate_smoother.set_target(0.0);
                            } else {
                                let avail = if total_f > 0 {
                                    total_f.saturating_sub(cur_f) as usize
                                } else {
                                    need_in
                                };
                                let want_pull = need_in.min(avail);
                                let got = trk
                                    .ram
                                    .read_block_f32(cur_f, &mut d.scratch_src[..want_pull * 2]);
                                let is_eof = total_f > 0 && cur_f + (got as u64) >= total_f;
                                let (cons, prod) = d.wsola.process_with_eof(
                                    &d.scratch_src[..got * 2],
                                    ratio,
                                    is_eof,
                                    &mut d.scratch_stretched[..out_frames * 2],
                                );
                                d.playhead_frame += cons as f64;
                                produced = prod;
                                if prod == 0 && is_eof && cons == 0 {
                                    d.playing = false;
                                    ctrl.playing.store(false, Ordering::Release);
                                    d.gate_smoother.set_target(0.0);
                                }
                            }
                        }
                    } else {
                        // `trk_sr != sr`: stateful `src_rate_conv` (`trk_sr -> sr`) into `rate_fifo` -> `wsola`
                        let fifo_cap = SUB_BLOCK_FRAMES * 22;
                        let need_sink = d
                            .wsola
                            .needed_input_frames(out_frames, ratio)
                            .min(fifo_cap);
                        let mut is_sink_eof = false;

                        if let Some(ref mut lp) = d.active_loop {
                            let mut guard = 0usize;
                            while d.rate_fifo_len < need_sink && guard < 8 {
                                guard += 1;
                                let want_gen = need_sink - d.rate_fifo_len;
                                let need_trk = d
                                    .src_rate_conv
                                    .needed_input_frames(want_gen, sr_ratio)
                                    .min(SUB_BLOCK_FRAMES * 30);
                                lp.peek_source_frames(&mut d.scratch_raw_trk[..need_trk * 2]);
                                let dst_off = d.rate_fifo_len * 2;
                                let dst_end = (d.rate_fifo_len + want_gen) * 2;
                                let (cons_trk, prod_sink) = d.src_rate_conv.process_with_eof(
                                    &d.scratch_raw_trk[..need_trk * 2],
                                    sr_ratio,
                                    false,
                                    &mut d.scratch_src[dst_off..dst_end],
                                );
                                lp.advance_cursor(cons_trk);
                                d.rate_fifo_len += prod_sink;
                                if prod_sink == 0 && cons_trk == 0 {
                                    break;
                                }
                            }
                        } else if let Some(ref trk) = d.track {
                            let total_f = trk.ram.total_frames();
                            if d.rate_fifo_len < need_sink {
                                let want_gen = need_sink - d.rate_fifo_len;
                                let need_trk = d
                                    .src_rate_conv
                                    .needed_input_frames(want_gen, sr_ratio)
                                    .min(SUB_BLOCK_FRAMES * 30);
                                let cur_f = d.playhead_frame.floor().max(0.0) as u64;
                                let avail = if total_f > 0 {
                                    total_f.saturating_sub(cur_f) as usize
                                } else {
                                    need_trk
                                };
                                let want_pull = need_trk.min(avail);
                                let got_trk = trk.ram.read_block_f32(
                                    cur_f,
                                    &mut d.scratch_raw_trk[..want_pull * 2],
                                );
                                let trk_eof = total_f > 0 && cur_f + (got_trk as u64) >= total_f;
                                let dst_off = d.rate_fifo_len * 2;
                                let dst_end = (d.rate_fifo_len + want_gen) * 2;
                                let (cons_trk, prod_sink) = d.src_rate_conv.process_with_eof(
                                    &d.scratch_raw_trk[..got_trk * 2],
                                    sr_ratio,
                                    trk_eof,
                                    &mut d.scratch_src[dst_off..dst_end],
                                );
                                d.playhead_frame += cons_trk as f64;
                                d.rate_fifo_len += prod_sink;
                                is_sink_eof = trk_eof && prod_sink < want_gen;
                            }
                        }

                        let (cons_sink, prod_out) = d.wsola.process_with_eof(
                            &d.scratch_src[..d.rate_fifo_len * 2],
                            ratio,
                            is_sink_eof,
                            &mut d.scratch_stretched[..out_frames * 2],
                        );
                        if cons_sink > 0 {
                            if cons_sink < d.rate_fifo_len {
                                d.scratch_src
                                    .copy_within(cons_sink * 2..d.rate_fifo_len * 2, 0);
                                d.rate_fifo_len -= cons_sink;
                            } else {
                                d.rate_fifo_len = 0;
                            }
                        }
                        produced = prod_out;
                        if prod_out == 0 && is_sink_eof && d.rate_fifo_len == 0 {
                            d.playing = false;
                            ctrl.playing.store(false, Ordering::Release);
                            d.gate_smoother.set_target(0.0);
                        }
                    }
                }
            }
        }

        if produced < out_frames {
            // Smooth exponential tail decay instead of a hard step-to-zero if an extreme seek ever underruns
            let mut hold_l = if produced > 0 {
                d.scratch_stretched[(produced - 1) * 2]
            } else {
                d.last_out_l
            };
            let mut hold_r = if produced > 0 {
                d.scratch_stretched[(produced - 1) * 2 + 1]
            } else {
                d.last_out_r
            };
            let decay = if d.playing && has_track { 0.96f32 } else { 0.0f32 };
            for f in produced..out_frames {
                hold_l *= decay;
                hold_r *= decay;
                d.scratch_stretched[f * 2] = hold_l;
                d.scratch_stretched[f * 2 + 1] = hold_r;
            }
        }

        // Apply gate smoother, bipolar Macro Filter, Bass Swap, & post-filter C1 anti-click micro-ramp
        for i in 0..out_frames {
            let gate = d.gate_smoother.next_sample();
            let l = if (gate - 1.0).abs() <= 1e-6 {
                d.scratch_stretched[i * 2]
            } else {
                d.scratch_stretched[i * 2] * gate
            };
            let r = if (gate - 1.0).abs() <= 1e-6 {
                d.scratch_stretched[i * 2 + 1]
            } else {
                d.scratch_stretched[i * 2 + 1] * gate
            };

            let (fl, fr) = d.macro_filter.process_frame(l, r);
            let (mut bl, mut br) = d.bass_swap.process_frame(fl, fr);

            if d.ramp_pos < d.ramp_len {
                let u = ((d.ramp_pos + 1) as f32) / ((d.ramp_len + 1) as f32);
                let g_new = 0.5 * (1.0 - (PI * u).cos());
                let g_old = 1.0 - g_new;
                bl = d.ramp_old_tail[d.ramp_pos * 2] * g_old + bl * g_new;
                br = d.ramp_old_tail[d.ramp_pos * 2 + 1] * g_old + br * g_new;
                d.ramp_pos += 1;
            }

            d.scratch_stretched[i * 2] = bl;
            d.scratch_stretched[i * 2 + 1] = br;
            d.prev_out_l = d.last_out_l;
            d.prev_out_r = d.last_out_r;
            d.last_out_l = bl;
            d.last_out_r = br;
        }
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

            let deck_a_silent = !self.decks[0].playing
                && self.decks[0].gate_smoother.current() <= 1e-6
                && self.decks[0].ramp_pos >= self.decks[0].ramp_len;
            let deck_b_silent = !self.decks[1].playing
                && self.decks[1].gate_smoother.current() <= 1e-6
                && self.decks[1].ramp_pos >= self.decks[1].ramp_len;
            let flt_a_unity = self.decks[0].macro_filter.is_neutral_bypass()
                && self.decks[0].bass_swap.is_unity_bypass();
            let flt_b_unity = self.decks[1].macro_filter.is_neutral_bypass()
                && self.decks[1].bass_swap.is_unity_bypass();

            for i in 0..blk {
                let p = self.xfade_smoother.next_sample().clamp(0.0, 1.0);
                let la = self.decks[0].scratch_stretched[i * 2];
                let ra = self.decks[0].scratch_stretched[i * 2 + 1];
                let lb = self.decks[1].scratch_stretched[i * 2];
                let rb = self.decks[1].scratch_stretched[i * 2 + 1];

                // True single-deck unity bypass when Crossfader is parked on A (`0.0`) or B (`1.0`)
                // with opposite deck silent and Filter / Bass Swap at unity -> 100% bit-exact S16!
                let (mix_l, mix_r) = if p <= 1e-6 && deck_b_silent && flt_a_unity {
                    (la, ra)
                } else if p >= 1.0 - 1e-6 && deck_a_silent && flt_b_unity {
                    (lb, rb)
                } else {
                    let ang = p * FRAC_PI_2;
                    let ga = ang.cos();
                    let gb = ang.sin();
                    (
                        soft_clip_guard(la * ga + lb * gb),
                        soft_clip_guard(ra * ga + rb * gb),
                    )
                };

                let dst_base = (done + i) * 2;
                out_stereo_s16[dst_base] =
                    (mix_l * 32768.0).round().clamp(-32768.0, 32767.0) as i16;
                out_stereo_s16[dst_base + 1] =
                    (mix_r * 32768.0).round().clamp(-32768.0, 32767.0) as i16;
            }
            done += blk;
        }
        total_frames
    }

    /// Snapshot full 60 FPS `PluginDjTelemetry` (read-only — safe to call at 60 Hz without mutating DSP).
    pub fn snapshot_telemetry(&self, session_active: bool) -> PluginDjTelemetry {
        self.snapshot_telemetry_internal(session_active, true)
    }

    /// Lightweight audio-thread telemetry snapshot (omits 24-bar scan so UI thread computes bars on demand).
    pub fn snapshot_telemetry_fast(&self, session_active: bool) -> PluginDjTelemetry {
        self.snapshot_telemetry_internal(session_active, false)
    }

    /// Lightweight lock-free bar-computation descriptors for the UI thread.
    pub fn deck_bar_snapshots(&self) -> [Option<DeckBarSnapshot>; 2] {
        let sr = self.sample_rate.max(8_000);
        let mut out: [Option<DeckBarSnapshot>; 2] = [None, None];
        for idx in 0..2 {
            let d = &self.decks[idx];
            let ctrl = &self.controls[idx];
            let Some(ref trk) = d.track else {
                continue;
            };
            let trk_sr = trk.sample_rate.max(8_000);
            let mode = DjTempoMode::from_u8(ctrl.tempo_mode.load(Ordering::Relaxed));
            let base_ratio = f32::from_bits(ctrl.target_bpm_ratio_bits.load(Ordering::Relaxed));
            let bend = f32::from_bits(ctrl.momentary_bend_bits.load(Ordering::Relaxed));
            let eff_ratio = (base_ratio * (1.0 + bend)).clamp(0.70, 1.30);

            let aud_frame = d.current_audible_frame(sr, eff_ratio, mode);
            let bf = d.beat_frames(sr).max(1.0);
            let f0 = d.first_beat_frame(sr);
            let beat_coord = (aud_frame - f0) / bf;
            let loop_bounds = d.active_loop.as_ref().map(|lp| (lp.loop_kind, lp.start_frame));

            out[idx] = Some(DeckBarSnapshot {
                ram: Arc::clone(&trk.ram),
                sample_rate: trk_sr,
                first_beat_frame: f0,
                beat_frames: bf,
                beat_coord,
                loop_bounds,
            });
        }
        out
    }

    fn snapshot_telemetry_internal(
        &self,
        session_active: bool,
        include_bars: bool,
    ) -> PluginDjTelemetry {
        let gen = self.seq_gen.fetch_add(1, Ordering::Relaxed).wrapping_add(1);
        let sr = self.sample_rate.max(8_000);
        let crossfader = f32::from_bits(self.crossfader_bits.load(Ordering::Relaxed));
        let bass_swap = self.bass_swap_enabled.load(Ordering::Relaxed);
        let tempo_mode = self.controls[0].tempo_mode.load(Ordering::Relaxed);

        let mut out = PluginDjTelemetry {
            seq_generation: gen,
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
            let trk_sr = trk.sample_rate.max(8_000);

            let mode = DjTempoMode::from_u8(ctrl.tempo_mode.load(Ordering::Relaxed));
            let base_ratio = f32::from_bits(ctrl.target_bpm_ratio_bits.load(Ordering::Relaxed));
            let bend = f32::from_bits(ctrl.momentary_bend_bits.load(Ordering::Relaxed));
            let eff_ratio = (base_ratio * (1.0 + bend)).clamp(0.70, 1.30);

            let aud_frame = d.current_audible_frame(sr, eff_ratio, mode);
            let elapsed_ms = ((aud_frame * 1000.0) / (trk_sr as f64)).round().max(0.0) as u32;
            let total_f = trk.ram.total_frames().max(trk.total_frames);
            let dyn_dur_ms = ((total_f * 1000) / (trk_sr as u64)) as u32;
            let dur_ms = trk.duration_ms.max(dyn_dur_ms).max(elapsed_ms);
            let remaining_ms = dur_ms.saturating_sub(elapsed_ms);
            let cue_ms = ((d.cue_frame * 1000) / (trk_sr as u64)) as u32;

            let bf = d.beat_frames(sr).max(1.0);
            let f0 = d.first_beat_frame(sr);
            let beat_coord = (aud_frame - f0) / bf;
            let beat_phase = (beat_coord.rem_euclid(1.0)) as f32;
            let bar_phase = ((beat_coord.rem_euclid(4.0)) * 0.25) as f32;

            let mut bars_peak = [0u8; 24];
            let mut bars_kick = [0u8; 24];
            if include_bars {
                compute_zoomed_24_bars(
                    &trk.ram,
                    trk_sr,
                    total_f,
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

/// Lock-free UI-thread descriptor for computing 24 beat-quantized waveform bars off the audio callback.
#[derive(Clone)]
pub struct DeckBarSnapshot {
    pub ram: Arc<DeckRamStore>,
    pub sample_rate: u32,
    pub first_beat_frame: f64,
    pub beat_frames: f64,
    pub beat_coord: f64,
    pub loop_bounds: Option<(DjLoopLength, u64)>,
}

impl DeckBarSnapshot {
    pub fn compute_bars(&self, out_peak_24: &mut [u8; 24], out_kick_24: &mut [u8; 24]) {
        compute_zoomed_24_bars_bounds(
            &self.ram,
            self.sample_rate,
            self.ram.total_frames(),
            self.first_beat_frame,
            self.beat_frames,
            self.beat_coord,
            self.loop_bounds,
            out_peak_24,
            out_kick_24,
        );
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
    let loop_bounds = active_loop.map(|lp| (lp.loop_kind, lp.start_frame));
    compute_zoomed_24_bars_bounds(
        ram,
        sample_rate,
        total_frames,
        first_beat_frame,
        beat_frames,
        beat_coord,
        loop_bounds,
        out_peak_24,
        out_kick_24,
    );
}

#[allow(clippy::too_many_arguments)]
fn compute_zoomed_24_bars_bounds(
    ram: &DeckRamStore,
    sample_rate: u32,
    total_frames: u64,
    first_beat_frame: f64,
    beat_frames: f64,
    beat_coord: f64,
    active_loop_info: Option<(DjLoopLength, u64)>,
    out_peak_24: &mut [u8; 24],
    out_kick_24: &mut [u8; 24],
) {
    let sr = sample_rate.max(8_000) as f64;
    let substep_frames = (beat_frames / 6.0).max(1.0);
    let center_slot = (beat_coord * 6.0).floor() as i64;

    let loop_bounds = active_loop_info.and_then(|(kind, start_f)| {
        let loop_beats = kind.beats_f32()? as f64;
        let start_slot = (((start_f as f64) - first_beat_frame) / substep_frames).round() as i64;
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
        let nb = ram.env_bucket_len();
        if b0 < nb {
            let hi = b1.max(b0 + 1).min(nb);
            for bi in b0..hi {
                let (bp, bk) = ram.read_env_bucket(bi);
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
    if a <= 0.95 {
        x
    } else {
        let excess = a - 0.95;
        let comp = 0.95 + 0.0499 * (1.0 - (-excess * 20.0).exp());
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

    let store = DeckRamStore::new_in_memory(sr, stereo_s16, contour);

    LoadedDeckTrack {
        path: path.to_string(),
        title: title.to_string(),
        artist: artist.to_string(),
        sample_rate: sr,
        duration_ms,
        total_frames,
        native_bpm: native_bpm.clamp(50.0, 220.0),
        first_beat_ms,
        initial_cue_frame,
        ram: Arc::new(store),
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
            decoder: Option<DeckFileDecoder>,
            ram: Arc<DeckRamStore>,
            playhead_hint: Arc<AtomicU64>,
            cue_hint: Arc<AtomicU64>,
            seeded_until_frame: u64,
            start_paused: bool,
        },
        ClearAll,
    }

    struct ActiveDeckStream {
        path: String,
        sample_rate: u32,
        decoder: Option<DeckFileDecoder>,
        ram: Arc<DeckRamStore>,
        playhead_hint: Arc<AtomicU64>,
        cue_hint: Arc<AtomicU64>,
        pinned_cue_center: u64,
        scratch_chunk: Vec<i16>,
        scratch_cue: Vec<i16>,
    }

    impl ActiveDeckStream {
        ///Urgent lookahead check: returns `true` if any of the next `8` chunks (`~1.4s`) ahead of
        /// `playhead_hint` are missing so playing decks are ALWAYS serviced before paused Cue windows!
        fn needs_urgent_playhead_chunks(&self) -> bool {
            let play_frame = self.playhead_hint.load(Ordering::Relaxed);
            let center_chunk = play_frame / (RAM_CHUNK_FRAMES as u64);
            let total_f = self.ram.total_frames();
            let max_c = (total_f + (RAM_CHUNK_FRAMES as u64) - 1) / (RAM_CHUNK_FRAMES as u64);
            let urgent_end = (center_chunk + 8).min(max_c.saturating_sub(1));
            for c in center_chunk..=urgent_end {
                if !self.ram.has_chunk(c) {
                    return true;
                }
            }
            false
        }

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
            if self.scratch_cue.len() < want_frames * 2 {
                self.scratch_cue.resize(want_frames * 2, 0);
            }

            dec.seek_to_dst_frame(win_start);
            let got = dec.decode_dst_s16(&mut self.scratch_cue[..want_frames * 2]);
            if got > 0 {
                let slice = &self.scratch_cue[..got * 2];
                self.ram.update_envelope_from_s16(win_start, slice);
                self.ram.publish_cue_window(win_start, slice);
            }
            self.pinned_cue_center = cue_frame;
        }

        /// Decode at most ONE `RAM_CHUNK_FRAMES` (`8192`-frame) chunk per time-slice so Deck A
        /// and Deck B alternate smoothly without starving each other!
        fn step_one_slice(&mut self, allow_cue_work: bool) -> bool {
            if self.decoder.is_none() {
                return false;
            }
            let play_frame = self.playhead_hint.load(Ordering::Relaxed);
            let center_chunk = play_frame / (RAM_CHUNK_FRAMES as u64);
            // Window: `[-2 chunks (~0.35s) .. +96 chunks (~16.4s)]` inside 128-slot power-of-2 ring
            let start_c = center_chunk.saturating_sub(2);
            let end_c = center_chunk + 96;

            let total_f = self.ram.total_frames();
            let max_c = (total_f + (RAM_CHUNK_FRAMES as u64) - 1) / (RAM_CHUNK_FRAMES as u64);

            // 1. Always keep immediate `[center_chunk .. center_chunk + 8]` (`~1.4s`) full FIRST!
            let mut target_chunk: Option<u64> = None;
            let urgent_hi = (center_chunk + 8).min(max_c.saturating_sub(1));
            for c in center_chunk..=urgent_hi {
                if !self.ram.has_chunk(c) {
                    target_chunk = Some(c);
                    break;
                }
            }

            // 2. If immediate playhead is safe and Cue changed, refresh lock-free Cue slot
            if target_chunk.is_none() && allow_cue_work {
                let cur_cue = self.cue_hint.load(Ordering::Acquire);
                if cur_cue != self.pinned_cue_center {
                    self.ensure_cue_window(cur_cue);
                    return true;
                }
            }

            // 3. Fill remaining ahead chunks `[urgent_hi + 1 ..= end_c]`, then behind `[start_c .. center_chunk)`
            if target_chunk.is_none() {
                for c in (urgent_hi + 1)..=end_c.min(max_c.saturating_sub(1)) {
                    if !self.ram.has_chunk(c) {
                        target_chunk = Some(c);
                        break;
                    }
                }
            }
            if target_chunk.is_none() {
                for c in start_c..center_chunk.min(max_c) {
                    if !self.ram.has_chunk(c) {
                        target_chunk = Some(c);
                        break;
                    }
                }
            }

            let Some(cidx) = target_chunk else {
                return false;
            };

            let dec = self.decoder.as_mut().unwrap();
            let target_frame = cidx * (RAM_CHUNK_FRAMES as u64);
            if dec.decoded_dst_frame() != target_frame {
                dec.seek_to_dst_frame(target_frame);
            }
            let got = dec.decode_dst_s16(&mut self.scratch_chunk);
            if got == 0 {
                return false;
            }
            let slice = &self.scratch_chunk[..got * 2];
            self.ram.update_envelope_from_s16(target_frame, slice);
            self.ram.insert_chunk(cidx, slice, center_chunk);
            if got < RAM_CHUNK_FRAMES && dec.is_eof() {
                let exact_end = target_frame + (got as u64);
                if exact_end > 0 {
                    self.ram.set_total_frames(exact_end);
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

            let urgent_0 = slots[0]
                .as_ref()
                .map(|s| s.needs_urgent_playhead_chunks())
                .unwrap_or(false);
            let urgent_1 = slots[1]
                .as_ref()
                .map(|s| s.needs_urgent_playhead_chunks())
                .unwrap_or(false);
            let any_urgent = urgent_0 || urgent_1;

            let mut did_work = false;
            if any_urgent {
                if urgent_0 {
                    if let Some(ref mut s0) = slots[0] {
                        did_work |= s0.step_one_slice(false);
                    }
                }
                if urgent_1 {
                    if let Some(ref mut s1) = slots[1] {
                        did_work |= s1.step_one_slice(false);
                    }
                }
            } else {
                for slot in slots.iter_mut().flatten() {
                    if slot.step_one_slice(true) {
                        did_work = true;
                    }
                }
            }

            if !did_work {
                match rx.recv_timeout(Duration::from_millis(12)) {
                    Ok(cmd) => handle_cmd(cmd, &mut slots),
                    Err(mpsc::RecvTimeoutError::Timeout) => {}
                    Err(mpsc::RecvTimeoutError::Disconnected) => break,
                }
            } else {
                // Polite sleep so background refill never starves audio/UI threads on Cortex-A53
                thread::sleep(Duration::from_millis(1));
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
                decoder,
                ram,
                playhead_hint,
                cue_hint,
                seeded_until_frame,
                start_paused,
            } => {
                let dec =
                    decoder.or_else(|| DeckFileDecoder::open(&path, sample_rate, duration_hint_ms));
                if let Some(ref d) = dec {
                    let dur_ms = d.duration_ms();
                    if dur_ms > 0 {
                        let exact_est = ((dur_ms as u64) * (sample_rate as u64)) / 1000;
                        if exact_est > 0 {
                            ram.set_total_frames(exact_est);
                        }
                    }
                }
                let init_cue = cue_hint.load(Ordering::Acquire);
                let cue_samples = (((sample_rate as usize) * 4) + 2048) * 2;
                let mut stream = ActiveDeckStream {
                    path,
                    sample_rate,
                    decoder: None,
                    ram,
                    playhead_hint,
                    cue_hint,
                    pinned_cue_center: if !start_paused && seeded_until_frame > 0 {
                        init_cue
                    } else {
                        u64::MAX
                    },
                    scratch_chunk: vec![0i16; RAM_CHUNK_FRAMES * 2],
                    scratch_cue: vec![0i16; cue_samples],
                };
                if !start_paused && seeded_until_frame > 0 && dec.is_some() {
                    stream.decoder = dec;
                } else {
                    stream.decoder = dec;
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
    start_paused: bool,
    native_bpm: f32,
    first_beat_ms: u32,
    contour: [u8; 256],
    seeded_pcm_s16: Option<&[i16]>,
) -> LoadedDeckTrack {
    use crate::decoder::DeckFileDecoder;

    let sr = sample_rate.max(8_000);
    let mut dec = DeckFileDecoder::open(path, sr, duration_ms);
    let resolved_dur_ms = dec
        .as_ref()
        .map(|d| d.duration_ms())
        .filter(|&m| m > 0)
        .unwrap_or(duration_ms);
    let est_dur_ms = if resolved_dur_ms > 0 {
        resolved_dur_ms
    } else {
        start_ms + 300_000
    };
    let mut total_frames = ((est_dur_ms as u64) * (sr as u64)) / 1000;
    let initial_cue_frame = (((first_beat_ms as u64) * (sr as u64)) / 1000).min(total_frames);
    let start_frame = (((start_ms as u64) * (sr as u64)) / 1000).min(total_frames);

    let mut store = DeckRamStore::new(sr, total_frames, contour);
    let mut seeded_until = 0u64;

    if dec.is_none() {
        if let Some(seed) = seeded_pcm_s16 {
            let seed_frames = seed.len() / 2;
            if seed_frames > 0 {
                store.update_envelope_from_s16(start_frame, seed);
                store.seed_window = Some(Arc::new(PinnedPcmWindow {
                    start_frame,
                    frames: seed_frames,
                    stereo_s16: seed.to_vec(),
                }));
                seeded_until = start_frame + (seed_frames as u64);
            }
        }
    }

    // Synchronous `>= 1.0s` (`6` chunks = `49,152` frames) pre-buffer for active decks (and `2` chunks
    // around `start_frame` for paused decks) BEFORE audio callback activation so `t = 0` NEVER starves!
    if let Some(ref mut d) = dec {
        let prebuf_chunks: u64 = if start_paused { 2 } else { 6 };
        let first_chunk = start_frame / (RAM_CHUNK_FRAMES as u64);
        let mut scratch = vec![0i16; RAM_CHUNK_FRAMES * 2];
        for k in 0..prebuf_chunks {
            let cidx = first_chunk + k;
            let c_start = cidx * (RAM_CHUNK_FRAMES as u64);
            if total_frames > 0 && c_start >= total_frames {
                break;
            }
            if d.decoded_dst_frame() != c_start {
                d.seek_to_dst_frame(c_start);
            }
            let got = d.decode_dst_s16(&mut scratch);
            if got == 0 {
                break;
            }
            let slice = &scratch[..got * 2];
            store.update_envelope_from_s16(c_start, slice);
            store.insert_chunk(cidx, slice, first_chunk);
            seeded_until = seeded_until.max(c_start + (got as u64));
            if got < RAM_CHUNK_FRAMES && d.is_eof() {
                total_frames = c_start + (got as u64);
                store.set_total_frames(total_frames);
                break;
            }
        }
    }

    let ram = Arc::new(store);
    let playhead_hint = Arc::new(AtomicU64::new(start_frame));
    let cue_hint = Arc::new(AtomicU64::new(initial_cue_frame));

    worker.send(RefillCmd::InstallDeck {
        deck,
        path: path.to_string(),
        sample_rate: sr,
        duration_hint_ms: resolved_dur_ms,
        decoder: dec,
        ram: Arc::clone(&ram),
        playhead_hint: Arc::clone(&playhead_hint),
        cue_hint: Arc::clone(&cue_hint),
        seeded_until_frame: seeded_until,
        start_paused,
    });

    LoadedDeckTrack {
        path: path.to_string(),
        title: title.to_string(),
        artist: artist.to_string(),
        sample_rate: sr,
        duration_ms: resolved_dur_ms,
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

    fn synth_dual_tone_stereo_s16(sr: u32, frames: usize, hz_l: f32, hz_r: f32, amp: f32) -> Vec<i16> {
        let mut out = vec![0i16; frames * 2];
        for i in 0..frames {
            let t = (i as f64) / (sr as f64);
            let l = (amp as f64) * (2.0 * std::f64::consts::PI * (hz_l as f64) * t).sin();
            let r = (amp as f64) * (2.0 * std::f64::consts::PI * (hz_r as f64) * t).sin();
            out[i * 2] = (l * 32768.0).round().clamp(-32768.0, 32767.0) as i16;
            out[i * 2 + 1] = (r * 32768.0).round().clamp(-32768.0, 32767.0) as i16;
        }
        out
    }

    fn goertzel_amp(samples: &[f32], sr: u32, target_hz: f32) -> f64 {
        let n = samples.len();
        if n == 0 {
            return 0.0;
        }
        let w = 2.0 * std::f64::consts::PI * (target_hz as f64) / (sr as f64);
        let mut re = 0.0f64;
        let mut im = 0.0f64;
        for (i, &s) in samples.iter().enumerate() {
            let ang = w * (i as f64);
            re += (s as f64) * ang.cos();
            im -= (s as f64) * ang.sin();
        }
        2.0 * (re * re + im * im).sqrt() / (n as f64)
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

        let beat_frames = (sr as f64) * 60.0 / (bpm as f64); // 24,000.0
        let loop_start = 48_000u64; // Beat 2 (has clean pre-roll `[47904..48000)`)
        let mut lp =
            PinnedBeatLoop::capture(&trk.ram, loop_start, beat_frames, DjLoopLength::Beats4, 0.0)
                .expect("capture 4-beat loop");

        for (kind, expected_len) in [
            (DjLoopLength::Beats4, 96_000usize),
            (DjLoopLength::Beats2, 48_000usize),
            (DjLoopLength::Beat1, 24_000usize),
            (DjLoopLength::HalfBeat, 12_000usize),
        ] {
            assert!(lp.set_length(kind));
            assert_eq!(lp.active_len_frames, expected_len);
            lp.cursor = 0.0;

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
            assert!(
                max_step < 0.08,
                "Loop seam clicked on {kind:?}: max sample-to-sample step = {max_step}"
            );
        }
    }

    #[test]
    fn crossfader_equal_power_and_bass_swap_symmetry_plus_bipolar_filter() {
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
            assert!(ga1 * ba1 + gb1 * bb1 <= 1.005);
        }

        let sr = 48_000u32;
        let mut flt = DjMacroFilter::new(sr);

        flt.set_target(0.0);
        for i in 0..512 {
            let x = (2.0 * PI * 1000.0 * (i as f32) / (sr as f32)).sin() * 0.7;
            let (y, _) = flt.process_frame(x, x);
            if i > 64 {
                assert!((y - x).abs() < 1e-5);
            }
        }

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
        let bpm = 120.0f32;
        let pcm = synth_sine_and_kicks_s16(sr, 4.0, bpm, 0.0);
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

        let bf = (sr as f64) * 60.0 / (bpm as f64);

        let mut pk = [0u8; 24];
        let mut kk = [0u8; 24];
        compute_zoomed_24_bars(
            &trk.ram,
            sr,
            trk.total_frames,
            0.0,
            bf,
            0.0,
            None,
            &mut pk,
            &mut kk,
        );
        assert!(pk[..12].iter().all(|&v| v == 0), "Pre-0:00 bars should be 0: {pk:?}");
        assert!(kk[12] > 140 && kk[18] > 140, "Expected strong kicks at indices 12 & 18: {kk:?}");
        assert!(kk[15] < 40, "Mid-beat silence should stay low: {kk:?}");

        compute_zoomed_24_bars(
            &trk.ram,
            sr,
            trk.total_frames,
            0.0,
            bf,
            8.0,
            None,
            &mut pk,
            &mut kk,
        );
        assert!(pk[12..].iter().all(|&v| v == 0), "Post-EOF bars should be 0: {pk:?}");
        assert!(kk[0] > 140 && kk[6] > 140);

        let lp = PinnedBeatLoop::capture(&trk.ram, 7 * 24_000, bf, DjLoopLength::Beat1, 0.0).unwrap();
        compute_zoomed_24_bars(
            &trk.ram,
            sr,
            trk.total_frames,
            0.0,
            bf,
            7.5,
            Some(&lp),
            &mut pk,
            &mut kk,
        );
        assert!(
            kk[15] > 140 && kk[21] > 140,
            "Active loop should wrap future bars across EOF: {kk:?}"
        );
    }

    #[test]
    fn continuity_100_chunks_256_frames_sine_440_660_neutral_sweeps_and_zero_crosstalk() {
        let sr = 48_000u32;
        let total_src_frames = (sr as usize) * 8;
        let src_a = synth_dual_tone_stereo_s16(sr, total_src_frames, 440.0, 660.0, 0.70);
        let src_b = synth_dual_tone_stereo_s16(sr, total_src_frames, 440.0, 660.0, 0.70);

        let mut core = ManualDjCore::new(sr);
        let trk_a = build_in_memory_deck_track(
            "/test/sine_a.wav",
            "Sine A",
            "Continuity",
            sr,
            src_a.clone(),
            120.0,
            0,
            [160; 256],
        );
        let trk_b = build_in_memory_deck_track(
            "/test/sine_b.wav",
            "Sine B",
            "Continuity",
            sr,
            src_b,
            120.0,
            0,
            [160; 256],
        );
        core.enqueue_deck_track(DjDeckId::DeckA, trk_a, false, 0);
        core.enqueue_deck_track(DjDeckId::DeckB, trk_b, true, 0);

        let mut cb_buf = [0i16; 256 * 2];
        let mut ch_l = Vec::with_capacity(100 * 256);
        let mut ch_r = Vec::with_capacity(100 * 256);
        let mut prev_l = 0.0f32;
        let mut prev_r = 0.0f32;
        let mut max_step_l = 0.0f32;
        let mut max_step_r = 0.0f32;

        // Phase 1: 100 consecutive 256-frame callbacks at neutral settings (+ 60 Hz UI telemetry polls!)
        for chunk_idx in 0..100 {
            let _ = core.snapshot_telemetry(true);
            let n = core.render_s16(&mut cb_buf);
            assert_eq!(n, 256);

            for i in 0..256 {
                let abs_f = chunk_idx * 256 + i;
                let s_l = cb_buf[i * 2];
                let s_r = cb_buf[i * 2 + 1];
                let fl = (s_l as f32) * (1.0 / 32768.0);
                let fr = (s_r as f32) * (1.0 / 32768.0);
                if abs_f > 0 {
                    max_step_l = max_step_l.max((fl - prev_l).abs());
                    max_step_r = max_step_r.max((fr - prev_r).abs());
                }
                // After the 5 ms (240-frame) initial handoff ramp settles, output MUST be bit-for-bit identical!
                if abs_f >= 256 {
                    assert_eq!(
                        s_l,
                        src_a[abs_f * 2],
                        "Bit-exact bypass mismatch on L at frame {abs_f}"
                    );
                    assert_eq!(
                        s_r,
                        src_a[abs_f * 2 + 1],
                        "Bit-exact bypass mismatch on R at frame {abs_f}"
                    );
                    ch_l.push(fl);
                    ch_r.push(fr);
                }
                prev_l = fl;
                prev_r = fr;
            }
        }

        // Exact continuous sine derivative bound at 48 kHz (0.70 * 2*pi*440/48000 = 0.0403, R = 0.0605)
        assert!(
            max_step_l < 0.044,
            "Sample discontinuity on L at neutral settings: max_step_l = {max_step_l}"
        );
        assert!(
            max_step_r < 0.065,
            "Sample discontinuity on R at neutral settings: max_step_r = {max_step_r}"
        );

        // Exact 24,000-frame (0.500s = 220 exact cycles of 440 Hz & 330 exact cycles of 660 Hz)
        // orthogonal DFT projection -> zero rectangular window spectral leakage!
        let l_440 = goertzel_amp(&ch_l[..24_000], sr, 440.0);
        let l_660 = goertzel_amp(&ch_l[..24_000], sr, 660.0);
        let r_660 = goertzel_amp(&ch_r[..24_000], sr, 660.0);
        let r_440 = goertzel_amp(&ch_r[..24_000], sr, 440.0);
        assert!((l_440 - 0.70).abs() < 0.01 && l_660 < 5e-5, "L crosstalk: 440={l_440}, 660={l_660}");
        assert!((r_660 - 0.70).abs() < 0.01 && r_440 < 5e-5, "R crosstalk: 660={r_660}, 440={r_440}");

        // Phase 2: 100 chunks sweeping bipolar FILTER (-0.85 -> 0.0 -> +0.85 -> 0.0), Crossfader + Bass Swap,
        // Vinyl pitch bend, and Master Tempo WSOLA (+ return to 1.0x COLA bypass!)
        core.set_deck_playing(DjDeckId::DeckB, true, false);
        let mut sweep_l = Vec::with_capacity(100 * 256);
        let mut sweep_r = Vec::with_capacity(100 * 256);
        let mut max_sweep_step_l = 0.0f32;
        let mut max_sweep_step_r = 0.0f32;

        for chunk_idx in 0..112 {
            let flt = if chunk_idx < 35 {
                -0.80 * ((chunk_idx as f32) / 34.0 * PI).sin()
            } else if chunk_idx < 70 {
                0.80 * (((chunk_idx - 35) as f32) / 34.0 * PI).sin()
            } else if chunk_idx < 85 {
                0.0
            } else {
                // Hold active resonant filter + center Bass Swap steady across chunks 85..112
                // to measure exact L/R channel isolation while biquads & Bass Swap are engaged!
                0.55
            };
            let xfade = if chunk_idx < 85 {
                let u = (chunk_idx as f32) / 84.0;
                0.5 * (1.0 - (2.0 * PI * u).cos())
            } else {
                0.50
            };
            let (mode, ratio, bend) = if chunk_idx < 40 {
                let b = 0.03 * (2.0 * PI * (chunk_idx as f32) / 39.0).sin();
                (DjTempoMode::Vinyl, 1.04, b)
            } else if chunk_idx < 75 {
                let r = 1.0 + 0.06 * (2.0 * PI * ((chunk_idx - 40) as f32) / 34.0).sin();
                (DjTempoMode::MasterTempo, r, 0.0)
            } else {
                // Chunks 75..112 return strictly to 1.0x MasterTempo COLA bypass!
                (DjTempoMode::MasterTempo, 1.0, 0.0)
            };

            core.set_deck_filter(DjDeckId::DeckA, flt);
            core.set_deck_filter(DjDeckId::DeckB, -flt);
            core.set_crossfader(xfade, true);
            core.set_deck_tempo(DjDeckId::DeckA, ratio, mode, bend);
            core.set_deck_tempo(DjDeckId::DeckB, 1.0, DjTempoMode::MasterTempo, 0.0);
            let _ = core.snapshot_telemetry(true);

            core.render_s16(&mut cb_buf);
            for i in 0..256 {
                let fl = (cb_buf[i * 2] as f32) * (1.0 / 32768.0);
                let fr = (cb_buf[i * 2 + 1] as f32) * (1.0 / 32768.0);
                max_sweep_step_l = max_sweep_step_l.max((fl - prev_l).abs());
                max_sweep_step_r = max_sweep_step_r.max((fr - prev_r).abs());
                if chunk_idx >= 100 {
                    sweep_l.push(fl);
                    sweep_r.push(fr);
                }
                prev_l = fl;
                prev_r = fr;
            }
        }

        assert!(
            max_sweep_step_l < 0.14 && max_sweep_step_r < 0.16,
            "Discontinuity during Filter/Crossfader/WSOLA/Vinyl sweeps: L={max_sweep_step_l}, R={max_sweep_step_r}"
        );
        // Exact 2,400-frame (0.050s = 22 exact cycles of 440 Hz & 33 exact cycles of 660 Hz) check
        let post_l_660 = goertzel_amp(&sweep_l[..2_400], sr, 660.0);
        let post_r_440 = goertzel_amp(&sweep_r[..2_400], sr, 440.0);
        assert!(
            post_l_660 < 1e-4 && post_r_440 < 1e-4,
            "Biquad/WSOLA L/R crosstalk detected: L@660={post_l_660}, R@440={post_r_440}"
        );
    }

    #[test]
    fn sample_rate_44100_source_into_48000_output_bus_continuous_zero_underrun() {
        let src_sr = 44_100u32;
        let dst_sr = 48_000u32;
        let src_frames = (src_sr as usize) * 5;
        let src_pcm = synth_dual_tone_stereo_s16(src_sr, src_frames, 440.0, 660.0, 0.70);

        for mode in [DjTempoMode::MasterTempo, DjTempoMode::Vinyl] {
            let mut core = ManualDjCore::new(dst_sr);
            let trk = build_in_memory_deck_track(
                "/test/sr_44k1.wav",
                "44.1k Track",
                "SRC Test",
                src_sr,
                src_pcm.clone(),
                124.0,
                0,
                [170; 256],
            );
            core.enqueue_deck_track(DjDeckId::DeckA, trk, false, 0);
            core.set_deck_tempo(DjDeckId::DeckA, 1.0, mode, 0.0);

            let mut cb_buf = [0i16; 256 * 2];
            let mut out_l = Vec::with_capacity(120 * 256);
            let mut out_r = Vec::with_capacity(120 * 256);
            let mut prev_l = 0.0f32;
            let mut prev_r = 0.0f32;
            let mut max_step_l = 0.0f32;
            let mut max_step_r = 0.0f32;

            for chunk_idx in 0..120 {
                let got = core.render_s16(&mut cb_buf);
                assert_eq!(got, 256);
                let mut chunk_energy = 0.0f64;

                for i in 0..256 {
                    let abs_f = chunk_idx * 256 + i;
                    let fl = (cb_buf[i * 2] as f32) * (1.0 / 32768.0);
                    let fr = (cb_buf[i * 2 + 1] as f32) * (1.0 / 32768.0);
                    if abs_f > 0 {
                        max_step_l = max_step_l.max((fl - prev_l).abs());
                        max_step_r = max_step_r.max((fr - prev_r).abs());
                    }
                    if chunk_idx >= 2 {
                        chunk_energy += (fl as f64) * (fl as f64) + (fr as f64) * (fr as f64);
                        out_l.push(fl);
                        out_r.push(fr);
                    }
                    prev_l = fl;
                    prev_r = fr;
                }

                if chunk_idx >= 2 {
                    let rms = (chunk_energy / 512.0).sqrt();
                    assert!(
                        (rms - 0.495).abs() < 0.02,
                        "Underrun or amplitude drop in {mode:?} chunk {chunk_idx}: rms={rms}"
                    );
                }
            }

            assert!(
                max_step_l < 0.046 && max_step_r < 0.068,
                "44.1k->48k discontinuity in {mode:?}: max_step_l={max_step_l}, max_step_r={max_step_r}"
            );

            // Verify exact 440 Hz (L) & 660 Hz (R) pitch at 48 kHz output (NOT 478.9 Hz!)
            let amp_440 = goertzel_amp(&out_l, dst_sr, 440.0);
            let amp_479 = goertzel_amp(&out_l, dst_sr, 478.9);
            let amp_660 = goertzel_amp(&out_r, dst_sr, 660.0);
            assert!(
                (amp_440 - 0.70).abs() < 0.015 && amp_479 < 0.01 && (amp_660 - 0.70).abs() < 0.015,
                "44.1k->48k pitch mismatch in {mode:?}: 440={amp_440}, 478.9={amp_479}, 660={amp_660}"
            );
        }
    }
}
