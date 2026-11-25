//! Monotonic Strong-Beat & Kick-Band (`40–140 Hz`) Autocorrelation Tempo Tracker for `Beat Stack`.
//!
//! Synchronizes overhead crane rail steps (`1/8` sub-beats), 1-beat claw drop telegraphs,
//! quantized crate gravity, and **Strong-Beat (`Beats 1 & 3` in `4/4`)** cluster/row detonations
//! to live PCM audio + `.nsf` hardware APU percussion/bass telemetry.
//!
//! Tempo estimation runs strictly on a monotonic `100 Hz` (`10 ms` per bin) low-frequency kick
//! onset envelope (`40–140 Hz`, 2-pole cascaded low-pass rejecting mid/high snares and hi-hats)
//! using comb autocorrelation across `75–180 BPM` with harmonic octave folding (`70 -> 140 BPM`)
//! and 2-beat strong-beat phase locking. Falls back smoothly to `120 BPM` only when audio is
//! paused, stopped, or silent.

use core::f32::consts::PI;

const ENV_RATE_HZ: f32 = 100.0;
const BIN_SEC: f32 = 1.0 / ENV_RATE_HZ;
const RING_BINS: usize = 600; // 6.0 seconds @ 100 Hz
const EST_WINDOW_BINS: usize = 520; // 5.2 seconds autocorrelation window
const MIN_READY_BINS: usize = 160; // 1.6 seconds minimum before first lock
const EST_INTERVAL_BINS: usize = 20; // Re-evaluate tempo & phase every 200 ms

/// Per-frame monotonic clock & live audio snapshot fed into [`BeatClock::update`].
#[derive(Clone, Copy)]
pub struct BeatFrameInput<'a> {
    /// Monotonic elapsed seconds since previous update (`get_ticks_ms` / `performance.now` delta).
    pub dt_sec: f32,
    /// `true` when host audio playback is actively playing (`status == 1`).
    pub music_playing: bool,
    /// Interleaved stereo S16 PCM window (`[L0, R0, L1, R1, ...]`) synced to speaker output.
    pub pcm_stereo: &'a [i16],
    /// Output sample rate in Hz (`44_100`, `48_000`, etc.).
    pub sample_rate: u32,
    /// Optional `.nsf` / `.nsfe` hardware APU percussion + bass trigger (`0.0..=1.0`).
    pub apu_pulse: f32,
    /// Optional pre-verified `(bpm, first_beat_offset_ms, playhead_ms, kick_impulse)` from `uipod-track-intel`.
    pub intel_sync: Option<(f32, u32, u32, f32)>,
}

/// Frame-by-frame rhythm triggers and continuous beat phases emitted by [`BeatClock`].
#[derive(Clone, Copy, Debug, Default)]
pub struct BeatStep {
    /// Active BPM (`75..=180` locked to live track, or `120.0` silent/paused fallback).
    pub effective_bpm: f32,
    /// `true` when locked to active audio rather than the internal `120 BPM` fallback clock.
    pub locked_to_audio: bool,
    /// Fractional progress within the current quarter-note beat `[0.0, 1.0)`.
    pub beat_frac: f32,
    /// Monotonic quarter-note beat index (`even` = Strong Beats `1` & `3` in `4/4`).
    pub beat_index: u64,
    /// `true` while inside a strong beat (`beat_index % 2 == 0`).
    pub is_strong_beat: bool,
    /// Monotonic beats elapsed during this frame step.
    pub step_beats: f32,
    /// Quarter-note (`1/4`) beat boundary crossed on this frame.
    pub fire_beat: bool,
    /// Strong-beat (`Beats 1 & 3`, every 2 beats: `beat_index % 2 == 0`) crossed on this frame.
    pub fire_strong_beat: bool,
    /// Eighth-note (`1/8`) sub-beat boundary crossed on this frame (crane rail glide).
    pub fire_half_beat: bool,
    /// Instantaneous low-kick transient detected on this frame.
    pub raw_onset: bool,
}

pub struct BeatClock {
    raw_beats: f64,
    comp_beats: f64,
    audio_latency_sec: f64,
    live_bpm: f32,
    tempo_locked: bool,
    parity_shift: u64,

    // Continuous 40-140 Hz 2-pole kick-band filter state
    hp40_state: f32,
    lp140_a: f32,
    lp140_b: f32,
    kick_env: f32,
    prev_kick_env: f32,
    flux_ema: f32,
    since_onset_sec: f32,

    // Monotonic 100 Hz (10 ms) kick-onset & kick-energy circular buffers
    bin_accum_sec: f32,
    onset_ring: [f32; RING_BINS],
    kick_ring: [f32; RING_BINS],
    ring_pos: usize,
    ring_len: usize,
    bins_since_est: usize,

    silence_sec: f32,
    pll_rate_trim: f32,
    pending_jump_beats: u8,
    last_jump_cand: f32,
    initialized: bool,
}

impl Default for BeatClock {
    fn default() -> Self {
        Self::new(0.025)
    }
}

impl BeatClock {
    pub const MIN_BPM: f32 = 75.0;
    pub const MAX_BPM: f32 = 180.0;
    pub const FALLBACK_BPM: f32 = 120.0;

    pub fn new(audio_latency_sec: f64) -> Self {
        Self {
            raw_beats: 0.0,
            comp_beats: 0.0,
            audio_latency_sec: audio_latency_sec.clamp(0.0, 0.30),
            live_bpm: Self::FALLBACK_BPM,
            tempo_locked: false,
            parity_shift: 0,
            hp40_state: 0.0,
            lp140_a: 0.0,
            lp140_b: 0.0,
            kick_env: 0.0,
            prev_kick_env: 0.0,
            flux_ema: 0.02,
            since_onset_sec: 1.0,
            bin_accum_sec: 0.0,
            onset_ring: [0.0; RING_BINS],
            kick_ring: [0.0; RING_BINS],
            ring_pos: 0,
            ring_len: 0,
            bins_since_est: 0,
            silence_sec: 2.0,
            pll_rate_trim: 0.0,
            pending_jump_beats: 0,
            last_jump_cand: Self::FALLBACK_BPM,
            initialized: false,
        }
    }

    pub fn set_audio_latency_sec(&mut self, latency_sec: f64) {
        self.audio_latency_sec = latency_sec.clamp(0.0, 0.30);
    }

    /// Resets tempo & kick history when a new song starts while keeping `comp_beats` monotonic.
    pub fn on_track_change(&mut self) {
        self.tempo_locked = false;
        self.hp40_state = 0.0;
        self.lp140_a = 0.0;
        self.lp140_b = 0.0;
        self.kick_env = 0.0;
        self.prev_kick_env = 0.0;
        self.flux_ema = 0.02;
        self.since_onset_sec = 1.0;
        self.bin_accum_sec = 0.0;
        self.ring_pos = 0;
        self.ring_len = 0;
        self.bins_since_est = 0;
        self.pll_rate_trim = 0.0;
        self.pending_jump_beats = 0;
    }

    pub fn reset_full(&mut self) {
        self.raw_beats = 0.0;
        self.comp_beats = 0.0;
        self.live_bpm = Self::FALLBACK_BPM;
        self.parity_shift = 0;
        self.silence_sec = 2.0;
        self.initialized = false;
        self.on_track_change();
    }

    /// Folds any tempo harmonic cleanly into `[75.0, 180.0]` (`70 -> 140 BPM`, `280 -> 140 BPM`).
    #[inline]
    pub fn fold_bpm(mut bpm: f32) -> f32 {
        if !bpm.is_finite() || bpm <= 0.0 {
            return Self::FALLBACK_BPM;
        }
        bpm = bpm.clamp(30.0, 360.0);
        while bpm < Self::MIN_BPM {
            bpm *= 2.0;
        }
        while bpm > Self::MAX_BPM {
            bpm *= 0.5;
        }
        bpm.clamp(Self::MIN_BPM, Self::MAX_BPM)
    }

    pub fn update(&mut self, input: BeatFrameInput<'_>) -> BeatStep {
        let dt = input.dt_sec.clamp(0.0, 0.10);
        let prev_comp = self.comp_beats;

        if let (true, Some((ibpm, offset_ms, playhead_ms, kick_imp))) =
            (input.music_playing, input.intel_sync)
        {
            let folded = Self::fold_bpm(ibpm);
            self.live_bpm = folded;
            self.tempo_locked = true;
            self.silence_sec = 0.0;
            self.since_onset_sec += dt;
            let raw_onset = if kick_imp >= 0.56 && self.since_onset_sec >= 0.18 {
                self.since_onset_sec = 0.0;
                true
            } else {
                false
            };

            let user_trim_ms = (self.audio_latency_sec * 1000.0).clamp(0.0, 300.0);
            let target_beats = (((playhead_ms as f64) - user_trim_ms - (offset_ms as f64))
                * (folded as f64 / 60_000.0))
                .max(0.0);
            let nominal_step = (dt as f64) * (folded as f64 / 60.0);

            if !self.initialized {
                self.comp_beats = target_beats;
                self.raw_beats = target_beats;
                self.parity_shift = 0;
            } else {
                let err = target_beats - self.comp_beats;
                if err.abs() > 3.0 {
                    // Track seeked/restarted: align fractional phase monotonically without rewinding
                    let target_frac = target_beats.rem_euclid(2.0);
                    let cur_base = (self.comp_beats * 0.5).floor() * 2.0;
                    let mut next_b = cur_base + target_frac;
                    if next_b < self.comp_beats {
                        next_b += 2.0;
                    }
                    self.comp_beats = next_b;
                    self.parity_shift = 0;
                } else {
                    let rate_mul = (1.0 + err * 1.8).clamp(0.84, 1.18);
                    self.comp_beats += nominal_step * rate_mul;
                }
                self.raw_beats = self.comp_beats;
            }

            let (fire_beat, fire_half_beat) = if !self.initialized {
                self.initialized = true;
                (false, false)
            } else {
                let pb = prev_comp.floor() as i64;
                let cb = self.comp_beats.floor() as i64;
                let ph = (prev_comp * 2.0).floor() as i64;
                let ch = self.comp_beats * 2.0;
                (cb > pb, (ch.floor() as i64) > ph)
            };

            let beat_floor = self.comp_beats.floor().max(0.0) as u64;
            let beat_index = beat_floor.wrapping_add(self.parity_shift);
            let is_strong_beat = (beat_index & 1) == 0;
            let fire_strong_beat = fire_beat && is_strong_beat;
            let beat_frac = ((self.comp_beats - (beat_floor as f64)) as f32).clamp(0.0, 0.999_99);
            let actual_step = (self.comp_beats - prev_comp).max(0.0) as f32;

            return BeatStep {
                effective_bpm: folded,
                locked_to_audio: true,
                beat_frac,
                beat_index,
                is_strong_beat,
                step_beats: actual_step,
                fire_beat,
                fire_strong_beat,
                fire_half_beat,
                raw_onset,
            };
        }

        let raw_onset = if input.music_playing {
            self.ingest_audio_monotonic(
                dt,
                input.pcm_stereo,
                input.sample_rate,
                input.apu_pulse,
            )
        } else {
            self.silence_sec = (self.silence_sec + dt).min(10.0);
            self.pll_rate_trim *= (1.0 - dt * 6.0).clamp(0.0, 1.0);
            false
        };

        let using_fallback = !input.music_playing || self.silence_sec >= 1.8;
        if using_fallback {
            self.tempo_locked = false;
            self.live_bpm += (Self::FALLBACK_BPM - self.live_bpm) * (dt * 3.0).clamp(0.0, 1.0);
        }

        let active_bpm = self.live_bpm.clamp(Self::MIN_BPM, Self::MAX_BPM);
        self.pll_rate_trim *= (1.0 - dt * 4.5).clamp(0.0, 1.0);
        let rate_mul = if using_fallback {
            1.0
        } else {
            (1.0 + self.pll_rate_trim).clamp(0.86, 1.14)
        };

        let step_beats_f64 = (dt * active_bpm * rate_mul / 60.0) as f64;
        self.raw_beats += step_beats_f64;

        let latency_beats = if using_fallback {
            0.0
        } else {
            self.audio_latency_sec * (active_bpm as f64 / 60.0)
        };
        let desired_comp = (self.raw_beats - latency_beats).max(0.0);
        if !self.initialized {
            self.comp_beats = desired_comp;
        } else if desired_comp > self.comp_beats {
            self.comp_beats = desired_comp;
        } else {
            self.comp_beats += step_beats_f64 * 0.15;
        }

        let (fire_beat, fire_half_beat) = if !self.initialized {
            self.initialized = true;
            (false, false)
        } else {
            let pb = prev_comp.floor() as i64;
            let cb = self.comp_beats.floor() as i64;
            let ph = (prev_comp * 2.0).floor() as i64;
            let ch = (self.comp_beats * 2.0).floor() as i64;
            (cb > pb, ch > ph)
        };

        let beat_floor = self.comp_beats.floor().max(0.0) as u64;
        let beat_index = beat_floor.wrapping_add(self.parity_shift);
        let is_strong_beat = (beat_index & 1) == 0;
        let fire_strong_beat = fire_beat && is_strong_beat;
        let beat_frac = ((self.comp_beats - (beat_floor as f64)) as f32).clamp(0.0, 0.999_99);
        let actual_step = (self.comp_beats - prev_comp).max(0.0) as f32;

        BeatStep {
            effective_bpm: active_bpm,
            locked_to_audio: !using_fallback && self.tempo_locked,
            beat_frac,
            beat_index,
            is_strong_beat,
            step_beats: actual_step,
            fire_beat,
            fire_strong_beat,
            fire_half_beat,
            raw_onset,
        }
    }

    fn ingest_audio_monotonic(
        &mut self,
        dt: f32,
        pcm_stereo: &[i16],
        sample_rate: u32,
        apu_pulse: f32,
    ) -> bool {
        self.since_onset_sec += dt;
        let n_frames = pcm_stereo.len() / 2;
        let sr = sample_rate.clamp(16_000, 96_000) as f32;
        const INV_I16: f32 = 1.0 / 32768.0;

        // Check broadband loudness for silence detection
        let mut full_sq = 0.0_f32;
        if n_frames > 0 {
            let step = (n_frames / 128).max(1);
            let mut cnt = 0usize;
            let mut idx = 0usize;
            while idx < n_frames {
                let m = ((pcm_stereo[idx * 2] as f32) + (pcm_stereo[idx * 2 + 1] as f32))
                    * (0.5 * INV_I16);
                full_sq += m * m;
                cnt += 1;
                idx += step;
            }
            full_sq /= cnt.max(1) as f32;
        }
        let broadband_rms = full_sq.sqrt();
        if broadband_rms < 0.004 && apu_pulse < 0.03 {
            self.silence_sec = (self.silence_sec + dt).min(10.0);
        } else {
            self.silence_sec = 0.0;
        }

        // Advance monotonic 10 ms bins
        self.bin_accum_sec += dt;
        let bins_due = (self.bin_accum_sec / BIN_SEC).floor() as usize;
        if bins_due == 0 {
            return false;
        }
        let k = bins_due.min(8);
        self.bin_accum_sec = (self.bin_accum_sec - (bins_due as f32) * BIN_SEC).clamp(0.0, BIN_SEC);

        // 40-140 Hz 2-pole kick bandpass coefficients
        let a_hp40 = (1.0 - (-2.0 * PI * 40.0 / sr).exp()).clamp(0.001, 0.25);
        let a_lp140 = (1.0 - (-2.0 * PI * 140.0 / sr).exp()).clamp(0.003, 0.45);

        let hop_frames = ((sr * BIN_SEC).round() as usize).max(32);
        let tail_frames = (k * hop_frames).min(n_frames);
        let start_frame = n_frames.saturating_sub(tail_frames);
        let apu_clamped = apu_pulse.clamp(0.0, 1.0);

        let mut frame_onset = false;
        for b in 0..k {
            let k_rms = if tail_frames >= k && n_frames > 0 {
                let f0 = start_frame + (b * tail_frames) / k;
                let f1 = start_frame + ((b + 1) * tail_frames) / k;
                let mut sum_sq = 0.0_f32;
                let count = f1.saturating_sub(f0).max(1);
                for f in f0..f1 {
                    let m = ((pcm_stereo[f * 2] as f32) + (pcm_stereo[f * 2 + 1] as f32))
                        * (0.5 * INV_I16);
                    self.hp40_state += a_hp40 * (m - self.hp40_state);
                    let hp = m - self.hp40_state;
                    self.lp140_a += a_lp140 * (hp - self.lp140_a);
                    self.lp140_b += a_lp140 * (self.lp140_a - self.lp140_b);
                    sum_sq += self.lp140_b * self.lp140_b;
                }
                (sum_sq / (count as f32)).sqrt()
            } else {
                0.0
            };

            let combined_kick = k_rms.max(apu_clamped * 0.25);
            self.kick_env = self.kick_env * 0.35 + combined_kick * 0.65;
            let k_flux = (self.kick_env - self.prev_kick_env).max(0.0);
            self.prev_kick_env = self.kick_env;

            self.push_ring_bin(k_flux, self.kick_env);

            let thr = (self.flux_ema * 1.65).max(0.006);
            if k_flux > thr && self.since_onset_sec >= 0.24 {
                frame_onset = true;
                self.since_onset_sec = 0.0;
            }
            self.flux_ema += (k_flux - self.flux_ema) * 0.08;
        }

        if self.silence_sec < 1.0
            && self.ring_len >= MIN_READY_BINS
            && self.bins_since_est >= EST_INTERVAL_BINS
        {
            self.bins_since_est = 0;
            self.estimate_tempo_and_lock_phase();
        }

        frame_onset
    }

    fn push_ring_bin(&mut self, onset_flux: f32, kick_rms: f32) {
        self.onset_ring[self.ring_pos] = onset_flux;
        self.kick_ring[self.ring_pos] = kick_rms;
        self.ring_pos = (self.ring_pos + 1) % RING_BINS;
        if self.ring_len < RING_BINS {
            self.ring_len += 1;
        }
        self.bins_since_est += 1;
    }

    fn estimate_tempo_and_lock_phase(&mut self) {
        let w = self.ring_len.min(EST_WINDOW_BINS);
        if w < MIN_READY_BINS {
            return;
        }

        let mut onset_win = [0.0_f32; EST_WINDOW_BINS];
        let mut kick_win = [0.0_f32; EST_WINDOW_BINS];
        let oldest = (self.ring_pos + RING_BINS - w) % RING_BINS;
        let mut sum = 0.0_f32;
        let mut max_v = 0.0_f32;
        for i in 0..w {
            let idx = (oldest + i) % RING_BINS;
            let v = self.onset_ring[idx];
            onset_win[i] = v;
            kick_win[i] = self.kick_ring[idx];
            sum += v;
            if v > max_v {
                max_v = v;
            }
        }

        let mean = sum / (w as f32);
        if max_v < 1e-4 || max_v / (mean + 1e-6) < 1.75 {
            return;
        }

        let mut centered = [0.0_f32; EST_WINDOW_BINS];
        let mut var_sum = 1e-12_f32;
        for i in 0..w {
            let sm = if i > 0 && i + 1 < w {
                0.25 * onset_win[i - 1] + 0.50 * onset_win[i] + 0.25 * onset_win[i + 1]
            } else {
                onset_win[i]
            };
            let c = sm - mean;
            centered[i] = c;
            var_sum += c * c;
        }

        // 75..=180 BPM corresponds to lags 33..=80 bins @ 100 Hz; compute up to 2*max_lag = 164
        let min_lag = 33usize;
        let max_lag = 80usize;
        let corr_max = (max_lag * 2 + 4).min(w - 1);
        if min_lag >= corr_max {
            return;
        }

        let mut acf = [0.0_f32; 170];
        let acf_top = corr_max.min(acf.len() - 1);
        for lag in (min_lag / 2)..=acf_top {
            let count = w - lag;
            let mut dot = 0.0_f32;
            for i in 0..count {
                dot += centered[i] * centered[i + lag];
            }
            let scale = (w as f32) / ((count as f32) * var_sum);
            acf[lag] = (dot * scale).clamp(-1.0, 1.0);
        }

        // Comb lag score across 75..=180 BPM (zero 120-BPM bias so 140 BPM stays 140.0 BPM!)
        let mut best_lag = 50usize;
        let mut best_score = f32::NEG_INFINITY;
        for lag in min_lag..=max_lag.min(acf_top) {
            let r1 = acf[lag].max(0.0);
            let r2 = if lag * 2 <= acf_top {
                acf[lag * 2].max(0.0)
            } else {
                r1
            };
            let r4 = if lag * 4 < w && lag * 4 <= acf_top {
                acf[lag * 4].max(0.0)
            } else {
                r2
            };
            let score = 0.58 * r1 + 0.34 * r2 + 0.08 * r4;
            if score > best_score {
                best_score = score;
                best_lag = lag;
            }
        }

        if best_score < 0.08 {
            return;
        }

        // Octave check: if best_lag is 75..91 BPM (`lag >= 66`) and half-lag (`150..180 BPM`) has
        // genuine kick-band energy comparable to `best_lag`, fold up to double-time!
        if best_lag >= 66 && best_lag / 2 >= min_lag {
            let half = best_lag / 2;
            let r_half = acf[half].max(acf[(half + 1).min(acf_top)]);
            if r_half >= 0.72 * acf[best_lag].max(0.05) {
                best_lag = if acf[(half + 1).min(acf_top)] > acf[half] {
                    half + 1
                } else {
                    half
                };
            }
        }

        // Sub-bin parabolic refinement on fundamental `best_lag` and 2-beat harmonic `2 * best_lag`
        let interp_peak = |idx: usize| -> f32 {
            if idx > 1 && idx + 1 <= acf_top {
                let ym = acf[idx - 1];
                let y0 = acf[idx];
                let yp = acf[idx + 1];
                let den = ym - 2.0 * y0 + yp;
                if den.abs() > 1e-6 {
                    (idx as f32) + (0.5 * (ym - yp) / den).clamp(-0.5, 0.5)
                } else {
                    idx as f32
                }
            } else {
                idx as f32
            }
        };

        let lag1 = interp_peak(best_lag);
        let mut l2 = (best_lag * 2).min(acf_top);
        if l2 > 2 && l2 + 1 <= acf_top {
            if acf[l2 - 1] > acf[l2] && acf[l2 - 1] >= acf[l2 + 1] {
                l2 -= 1;
            } else if acf[l2 + 1] > acf[l2] {
                l2 += 1;
            }
        }
        let exact_lag = if acf[best_lag] < 0.12 && l2 <= acf_top && acf[l2] > 0.18 {
            // Pure half-time kick (e.g. 70 BPM kick folded cleanly to 140.0 BPM!)
            interp_peak(l2) * 0.5
        } else if l2 <= acf_top && acf[l2] > 0.25 {
            let lag2_half = interp_peak(l2) * 0.5;
            if (lag1 - lag2_half).abs() < 0.85 {
                0.40 * lag1 + 0.60 * lag2_half
            } else {
                lag1
            }
        } else {
            lag1
        };

        let detected_bpm = Self::fold_bpm(ENV_RATE_HZ * 60.0 / exact_lag.max(20.0));

        if !self.tempo_locked {
            self.live_bpm = detected_bpm;
            self.tempo_locked = true;
        } else {
            let diff = (detected_bpm - self.live_bpm).abs();
            if diff > 5.0 {
                if (detected_bpm - self.last_jump_cand).abs() < 3.0 {
                    self.pending_jump_beats = self.pending_jump_beats.saturating_add(1);
                } else {
                    self.pending_jump_beats = 1;
                    self.last_jump_cand = detected_bpm;
                }
                if self.pending_jump_beats >= 2 {
                    self.live_bpm += (detected_bpm - self.live_bpm) * 0.70;
                    self.pending_jump_beats = 0;
                }
            } else {
                self.pending_jump_beats = 0;
                self.live_bpm += (detected_bpm - self.live_bpm) * 0.28;
            }
        }

        // 2-beat Strong-Beat Phase Search (`Beats 1 & 3` vs `Beats 2 & 4`)
        let period_bins = (ENV_RATE_HZ * 60.0 / self.live_bpm).clamp(30.0, 90.0);
        let two_beat_int = (period_bins * 2.0).round() as usize;
        let newest = w - 1;
        let mut best_age_bins = 0usize;
        let mut best_phase_energy = f32::NEG_INFINITY;

        for age in 0..two_beat_int.max(1) {
            let mut sc = 0.0_f32;
            let mut k = 0usize;
            loop {
                let back = age + ((k as f32) * period_bins * 2.0).round() as usize;
                if back >= w {
                    break;
                }
                let idx = newest - back;
                let mut e_strong = onset_win[idx] + kick_win[idx] * 0.45;
                if idx > 0 {
                    e_strong += 0.45 * onset_win[idx - 1];
                }
                if idx + 1 < w {
                    e_strong += 0.45 * onset_win[idx + 1];
                }
                // Sub-score weak beat 1 period earlier so quarter-note grid stays tight
                let weak_back = back + (period_bins.round() as usize);
                let e_weak = if weak_back < w {
                    onset_win[newest - weak_back] * 0.35
                } else {
                    0.0
                };
                sc += e_strong + e_weak;
                k += 1;
            }
            if sc > best_phase_energy {
                best_phase_energy = sc;
                best_age_bins = age;
            }
        }

        // `target_2b` in `[0.0, 2.0)` = beats elapsed since most recent Strong Beat at speaker output
        let latency_bins = (self.audio_latency_sec as f32) * ENV_RATE_HZ;
        let target_2b = (((best_age_bins as f32) - latency_bins) / period_bins).rem_euclid(2.0);

        let cur_floor = self.comp_beats.floor().max(0.0) as u64;
        let cur_parity = ((cur_floor.wrapping_add(self.parity_shift)) & 1) as f32;
        let cur_frac = (self.comp_beats - self.comp_beats.floor()) as f32;
        let cur_2b = cur_parity + cur_frac;

        // Align quarter-note phase smoothly via PLL
        let target_frac = target_2b.rem_euclid(1.0);
        let mut q_err = target_frac - cur_frac;
        if q_err > 0.5 {
            q_err -= 1.0;
        } else if q_err < -0.5 {
            q_err += 1.0;
        }
        self.pll_rate_trim = (q_err * 0.36).clamp(-0.14, 0.14);

        // Align strong-beat parity (`Beats 1 & 3` = `beat_index % 2 == 0`) without rewinding `comp_beats`
        let two_err = (target_2b - cur_2b).rem_euclid(2.0);
        if (0.65..=1.35).contains(&two_err) {
            self.parity_shift ^= 1;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Synthesizes realistic stereo S16 audio containing:
    /// - 60 Hz sub-bass kick drum hits at `kick_bpm` (with extra accent on Beats 1 & 3),
    /// - Loud 2600 Hz off-beat 8th-note hi-hats (`+0.5` beat),
    /// - Loud 480 Hz mid-band snares on Beats 2 & 4.
    fn synth_drum_track_s16(
        sr: u32,
        duration_sec: f32,
        true_bpm: f32,
        halftime_kick: bool,
    ) -> Vec<i16> {
        let total_frames = (duration_sec * (sr as f32)) as usize;
        let mut out = vec![0i16; total_frames * 2];
        let beat_sec = 60.0 / true_bpm;

        for f in 0..total_frames {
            let t = (f as f32) / (sr as f32);
            let beat_pos = t / beat_sec;
            let beat_idx = beat_pos.floor() as usize;
            let in_beat_sec = (beat_pos - beat_pos.floor()) * beat_sec;

            // Kick on every beat (or only Beats 1 & 3 when `halftime_kick == true`)
            let has_kick = if halftime_kick {
                beat_idx % 2 == 0
            } else {
                true
            };
            let kick = if has_kick && in_beat_sec < 0.075 {
                let env = (1.0 - in_beat_sec / 0.075).powi(2);
                let accent = if beat_idx % 2 == 0 { 0.85 } else { 0.62 };
                (2.0 * PI * 62.0 * in_beat_sec).sin() * env * accent
            } else {
                0.0
            };

            // Off-beat 8th-note hi-hat (`2600 Hz`) right at `0.5` beat (`214 ms` @ 140 BPM!)
            let eighth_sec = (t % (beat_sec * 0.5)).max(0.0);
            let hat = if eighth_sec < 0.025 {
                let env = 1.0 - eighth_sec / 0.025;
                (2.0 * PI * 2650.0 * t).sin() * env * 0.45
            } else {
                0.0
            };

            // Snare body (`480 Hz`) on Beats 2 & 4
            let snare = if beat_idx % 2 == 1 && in_beat_sec < 0.055 {
                let env = 1.0 - in_beat_sec / 0.055;
                (2.0 * PI * 480.0 * in_beat_sec).sin() * env * 0.50
            } else {
                0.0
            };

            let s = ((kick + hat + snare).clamp(-0.98, 0.98) * 32767.0) as i16;
            out[f * 2] = s;
            out[f * 2 + 1] = s;
        }
        out
    }

    #[test]
    fn accurately_detects_140_bpm_within_1_bpm_despite_offbeat_hats_and_jitter() {
        let sr = 44_100u32;
        let pcm = synth_drum_track_s16(sr, 6.0, 140.0, false);
        let mut clock = BeatClock::new(0.025);

        let mut cursor_sec = 0.0_f32;
        let mut frame = 0usize;
        let mut last_step = BeatStep::default();

        while cursor_sec < 5.5 {
            // Simulate realistic variable UI frame times (14 ms .. 19 ms)
            let dt = match frame % 4 {
                0 => 0.015,
                1 => 0.018,
                2 => 0.016,
                _ => 0.017,
            };
            cursor_sec += dt;
            frame += 1;

            let end_f = ((cursor_sec * (sr as f32)) as usize).min(pcm.len() / 2);
            let start_f = end_f.saturating_sub(1024);
            let win = &pcm[start_f * 2..end_f * 2];

            last_step = clock.update(BeatFrameInput {
                dt_sec: dt,
                music_playing: true,
                pcm_stereo: win,
                sample_rate: sr,
                apu_pulse: 0.0,
                intel_sync: None,
            });
        }

        assert!(last_step.locked_to_audio);
        assert!(
            (last_step.effective_bpm - 140.0).abs() <= 1.0,
            "expected 140 BPM ± 1, got {:.2} BPM",
            last_step.effective_bpm
        );
    }

    #[test]
    fn folds_halftime_70_bpm_kick_cleanly_to_140_bpm() {
        let sr = 48_000u32;
        let pcm = synth_drum_track_s16(sr, 6.0, 140.0, true);
        let mut clock = BeatClock::new(0.0);

        let dt = 1.0 / 60.0;
        let mut t = 0.0_f32;
        let mut last = BeatStep::default();
        while t < 5.5 {
            t += dt;
            let end_f = ((t * (sr as f32)) as usize).min(pcm.len() / 2);
            let start_f = end_f.saturating_sub(1024);
            last = clock.update(BeatFrameInput {
                dt_sec: dt,
                music_playing: true,
                pcm_stereo: &pcm[start_f * 2..end_f * 2],
                sample_rate: sr,
                apu_pulse: 0.0,
                intel_sync: None,
            });
        }

        assert!(last.locked_to_audio);
        assert!(
            (last.effective_bpm - 140.0).abs() <= 1.0,
            "expected folded 140 BPM ± 1, got {:.2} BPM",
            last.effective_bpm
        );
    }

    #[test]
    fn silent_fallback_runs_at_120_bpm_and_fires_strong_beats_every_two_beats() {
        let mut clock = BeatClock::new(0.0);
        let dt = 1.0 / 60.0;
        let mut beats = 0usize;
        let mut strong_beats = 0usize;

        // 4.1 seconds @ 120 BPM = 8.2 beats (~4 strong beats)
        for _ in 0..246 {
            let step = clock.update(BeatFrameInput {
                dt_sec: dt,
                music_playing: false,
                pcm_stereo: &[],
                sample_rate: 44_100,
                apu_pulse: 0.0,
                intel_sync: None,
            });
            assert!(!step.locked_to_audio);
            assert!((step.effective_bpm - 120.0).abs() < 0.05);
            if step.fire_beat {
                beats += 1;
            }
            if step.fire_strong_beat {
                strong_beats += 1;
            }
        }
        assert!((7..=9).contains(&beats), "beats={beats}");
        assert!((3..=5).contains(&strong_beats), "strong_beats={strong_beats}");
    }
}
