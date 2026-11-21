//! Monotonic musical bar/beat phase tracker and zero-allocation PCM onset analyzer.
//!
//! Core design invariant: `total_beats` never steps backward during normal playback.
//! Instead of rewinding `bar_phase` when an onset arrives late (which would double-fire
//! or skip weapon triggers and visibly stutter the alien formation), [`BarPhaseTracker`]
//! smoothly steers its internal tempo multiplier within `-18%..+22%` over ~200 ms.

use core::f32::consts::TAU;

/// Synchronization source: deterministic track map or live PLL metronome.
#[allow(dead_code)]
#[derive(Clone, Copy)]
pub enum SyncSource<'a> {
    /// Fixed BPM and first-downbeat offset (from tags or lightweight pre-scan).
    SongMapFixed {
        bpm: f32,
        first_downbeat_sec: f64,
    },
    /// Exact variable-tempo beat grid (cached song map).
    SongMapGrid {
        /// Sorted array of beat timestamps in seconds.
        beat_times_sec: &'a [f32],
        /// Index in `beat_times_sec` corresponding to the bar's first downbeat ("ONE").
        downbeat_beat_index: usize,
    },
    /// Real-time streaming / live audio tracking (Navidrome, Jellyfin, chiptunes, or local tracks).
    LivePll,
}

/// Per-frame audio telemetry input (evaluated at 60 FPS).
#[derive(Clone, Copy, Debug)]
pub struct AudioFrameInput {
    /// Render frame delta time in seconds (e.g. `1.0 / 60.0`).
    pub dt_sec: f32,
    /// Exact playback cursor from the audio engine in seconds (used by `SongMap*` modes).
    pub playback_pos_sec: Option<f64>,
    /// Low-frequency bass energy in `40..120 Hz` (normalized `0.0..=1.0`).
    pub low_energy: f32,
    /// Auxiliary transient hint: broadband spectral flux or chiptune envelope attack (`0.0..=1.0`).
    pub flux_hint: f32,
    /// Overall frame loudness / VU level (`0.0..=1.0`) used to charge the bar-cannon shot.
    pub vu_level: f32,
}

/// Output of the bar tracker for the current frame, driving auto-weapons and swarm animation.
#[derive(Clone, Copy, Debug, Default)]
pub struct BarTick {
    /// Bar phase strictly within `[0.0, 4.0)`.
    pub bar_phase: f32,
    /// Current beat within the 4/4 bar: `0` (downbeat "ONE"), `1`, `2`, `3`. Always `bar_phase as u8`.
    pub beat_index: u8,
    /// Fractional progress within the current beat `[0.0, 1.0)` for smooth animation curves.
    pub beat_frac: f32,
    /// `1/1` trigger: start of a new 4-beat bar (Wave Cannon / piercing volley).
    pub fire_bar: bool,
    /// `1/4` trigger: every quarter-note beat (classic pulse + downbeat accent when `beat_index == 0`).
    pub fire_beat: bool,
    /// `1/8` trigger: every eighth-note half-beat (rapid focus mode while holding `A`).
    pub fire_eighth: bool,
    /// Mean loudness accumulated across the just-completed bar (`0.0..=1.0`) — scales Wave Cannon caliber.
    pub completed_bar_energy: f32,
    /// Active working BPM after automatic octave folding.
    pub effective_bpm: f32,
    /// True if a physical audio transient/onset occurred on this frame (drives visual sparks).
    pub raw_onset: bool,
}

/// Auto-fire weapon gear synchronized to the musical bar grid.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WeaponGear {
    /// `1/1`: High-caliber piercing Wave Cannon fired once per 4-beat bar.
    BarCannon,
    /// `1/4`: Classic quarter-note plasma bolts with a piercing twin-lance accent on beat 1.
    BeatPulse,
    /// `1/8`: Close-range eighth-note Vulcan spread engaged while holding `A` (or selected directly).
    FocusEight,
}

#[derive(Clone, Debug)]
pub struct BarPhaseConfig {
    /// Output audio latency compensation (ALSA / Android AAudio / Bluetooth) in seconds.
    pub audio_latency_sec: f64,
    /// Automatically fold overly fast or slow tempos into a playable range (e.g. `174 BPM` DnB -> `87 BPM`).
    pub auto_octave_fold: bool,
    pub min_playable_bpm: f32,
    pub max_playable_bpm: f32,
    /// Adaptive threshold multiplier for `LivePll` onset detection (lower = more sensitive, typical `1.25..1.75`).
    pub onset_threshold_mul: f32,
    /// Proportional phase-steering gain for `LivePll` (`0.15..0.35`).
    pub pll_kp: f32,
}

impl Default for BarPhaseConfig {
    fn default() -> Self {
        Self {
            audio_latency_sec: 0.025, // ~25 ms default for built-in DAC / USB-C DAC
            auto_octave_fold: true,
            min_playable_bpm: 76.0,
            max_playable_bpm: 152.0,
            onset_threshold_mul: 1.42,
            pll_kp: 0.24,
        }
    }
}

pub struct BarPhaseTracker {
    cfg: BarPhaseConfig,
    /// Monotonically non-decreasing beat counter elapsed since track start.
    total_beats: f64,
    effective_bpm: f32,

    // LivePll and onset-detector state
    prev_low_energy: f32,
    flux_ema: f32,
    since_last_onset_sec: f32,
    silence_sec: f32,
    was_silent_before_onset: bool,
    pll_rate_trim: f32,
    ioi_bpms: [f32; 8],
    ioi_cursor: usize,
    ioi_len: usize,

    // Loudness integrator across the active bar (feeds Wave Cannon caliber)
    energy_accum: f32,
    energy_frames: u32,
    last_bar_energy: f32,

    initialized: bool,
}

impl BarPhaseTracker {
    pub fn new(cfg: BarPhaseConfig) -> Self {
        Self {
            cfg,
            total_beats: 0.0,
            effective_bpm: 120.0,
            prev_low_energy: 0.0,
            flux_ema: 0.05,
            since_last_onset_sec: 0.0,
            silence_sec: 0.0,
            was_silent_before_onset: false,
            pll_rate_trim: 0.0,
            ioi_bpms: [120.0; 8],
            ioi_cursor: 0,
            ioi_len: 0,
            energy_accum: 0.0,
            energy_frames: 0,
            last_bar_energy: 0.5,
            initialized: false,
        }
    }

    /// Reset phase and tempo estimator state when switching tracks.
    pub fn reset(&mut self, initial_bpm: f32) {
        self.total_beats = 0.0;
        self.effective_bpm = self.fold_bpm(initial_bpm.clamp(40.0, 240.0));
        self.prev_low_energy = 0.0;
        self.flux_ema = 0.05;
        self.since_last_onset_sec = 0.0;
        self.silence_sec = 0.0;
        self.was_silent_before_onset = false;
        self.pll_rate_trim = 0.0;
        self.ioi_cursor = 0;
        self.ioi_len = 0;
        self.energy_accum = 0.0;
        self.energy_frames = 0;
        self.last_bar_energy = 0.5;
        self.initialized = false;
    }

    pub fn set_audio_latency(&mut self, latency_sec: f64) {
        self.cfg.audio_latency_sec = latency_sec.clamp(-0.5, 0.5);
    }

    /// Primary per-frame update step (call once per frame prior to weapon & enemy simulation).
    pub fn update(&mut self, source: SyncSource<'_>, input: AudioFrameInput) -> BarTick {
        let dt = input.dt_sec.clamp(0.0, 0.1);
        let prev_beats = self.total_beats;

        // 1. Run onset detector (always active for visual sparks; steers phase in LivePll mode)
        let raw_onset = self.detect_onset(dt, input.low_energy, input.flux_hint, input.vu_level);

        // 2. Integrate loudness across the active bar
        self.energy_accum += input.vu_level.clamp(0.0, 1.0);
        self.energy_frames += 1;

        // 3. Advance monotonic `total_beats` according to the active sync source
        let mut seek_jumped = false;
        match source {
            SyncSource::SongMapFixed {
                bpm,
                first_downbeat_sec,
            } => {
                let folded_bpm = self.fold_bpm(bpm.max(30.0));
                self.effective_bpm = folded_bpm;
                if let Some(pos_sec) = input.playback_pos_sec {
                    let heard_sec = pos_sec - self.cfg.audio_latency_sec;
                    let target_beats =
                        (heard_sec - first_downbeat_sec) * (folded_bpm as f64 / 60.0);
                    seek_jumped = self.slew_to_map_beats(target_beats, dt, folded_bpm);
                } else {
                    self.total_beats += (dt * folded_bpm / 60.0) as f64;
                }
            }
            SyncSource::SongMapGrid {
                beat_times_sec,
                downbeat_beat_index,
            } => {
                if let (Some(pos_sec), true) = (input.playback_pos_sec, beat_times_sec.len() >= 2) {
                    let heard_sec = (pos_sec - self.cfg.audio_latency_sec) as f32;
                    let (raw_grid_beats, local_bpm) =
                        Self::sample_beat_grid(beat_times_sec, heard_sec, downbeat_beat_index);
                    let scale = self.octave_scale_factor(local_bpm);
                    self.effective_bpm = local_bpm * scale as f32;
                    let target_beats = raw_grid_beats * scale;
                    seek_jumped = self.slew_to_map_beats(target_beats, dt, self.effective_bpm);
                } else {
                    self.total_beats += (dt * self.effective_bpm / 60.0) as f64;
                }
            }
            SyncSource::LivePll => {
                self.advance_live_pll(dt, raw_onset, input.vu_level);
            }
        }

        // 4. Detect 1/1, 1/4, and 1/8 grid crossings without skips or double-fires
        let (fire_bar, fire_beat, fire_eighth) = if !self.initialized || seek_jumped {
            self.initialized = true;
            (false, false, false)
        } else {
            let prev_bar = prev_beats.div_euclid(4.0) as i64;
            let curr_bar = self.total_beats.div_euclid(4.0) as i64;

            let prev_beat = prev_beats.floor() as i64;
            let curr_beat = self.total_beats.floor() as i64;

            let prev_eighth = (prev_beats * 2.0).floor() as i64;
            let curr_eighth = (self.total_beats * 2.0).floor() as i64;

            (
                curr_bar > prev_bar,
                curr_beat > prev_beat,
                curr_eighth > prev_eighth,
            )
        };

        if fire_bar && self.energy_frames > 0 {
            self.last_bar_energy =
                (self.energy_accum / self.energy_frames as f32).clamp(0.0, 1.0);
            self.energy_accum = 0.0;
            self.energy_frames = 0;
        }

        // 5. Build consistent f32 phase values while preventing 3.9999999 -> 4.0 rounding overflow
        let curr_beat_floor = self.total_beats.floor();
        let beat_index = (curr_beat_floor as i64).rem_euclid(4) as u8;
        let beat_frac = ((self.total_beats - curr_beat_floor) as f32).clamp(0.0, 0.999_999);
        let bar_phase = beat_index as f32 + beat_frac; // guaranteed within [0.0, 4.0)

        BarTick {
            bar_phase,
            beat_index,
            beat_frac,
            fire_bar,
            fire_beat,
            fire_eighth,
            completed_bar_energy: self.last_bar_energy,
            effective_bpm: self.effective_bpm,
            raw_onset,
        }
    }

    /// Check whether a player action (e.g. Phase Roll dash on `B`/`X`) lands inside `±tolerance_ms` of a beat.
    pub fn is_in_beat_window(&self, tolerance_ms: f32) -> bool {
        let beat_dur_ms = 60_000.0 / self.effective_bpm.max(1.0);
        let frac = (self.total_beats - self.total_beats.floor()) as f32;
        let dist_beats = frac.min(1.0 - frac);
        (dist_beats * beat_dur_ms) <= tolerance_ms
    }

    /// Smooth sinusoidal breathing wave (`-1.0..=1.0`) locked to the beat for the alien swarm grid.
    pub fn swarm_pulse(&self) -> f32 {
        let frac = (self.total_beats - self.total_beats.floor()) as f32;
        (frac * TAU).cos()
    }

    /// Charge progress (`0.0..=1.0`) across the current 4-beat bar for Wave Cannon nose rings.
    pub fn bar_charge_ratio(&self) -> f32 {
        ((self.total_beats.rem_euclid(4.0)) as f32 * 0.25).clamp(0.0, 1.0)
    }

    // --- Internal timing & PLL math ---

    /// Smoothly slew toward the player's reported playback clock (absorbs ALSA/Android buffer jitter).
    fn slew_to_map_beats(&mut self, target_beats: f64, dt: f32, bpm: f32) -> bool {
        if !self.initialized {
            self.total_beats = target_beats;
            return true;
        }
        let err = target_beats - self.total_beats;
        // Discrepancy larger than 1.5 beats indicates a user seek jump
        if err.abs() > 1.5 {
            self.total_beats = target_beats;
            return true;
        }
        let free_step = (dt * bpm / 60.0) as f64;
        // Smooth velocity correction, clamped so the beat counter never moves backward
        let corrected_step = (free_step + err * (dt as f64 * 8.0)).max(0.0);
        self.total_beats += corrected_step;
        false
    }

    fn sample_beat_grid(grid: &[f32], heard_sec: f32, downbeat_idx: usize) -> (f64, f32) {
        let idx = grid.partition_point(|&t| t <= heard_sec);
        let (raw_idx_f, span) = if idx == 0 {
            let dt0 = (grid[1] - grid[0]).max(0.15);
            ((heard_sec - grid[0]) / dt0, dt0)
        } else if idx >= grid.len() {
            let n = grid.len();
            let dt_last = (grid[n - 1] - grid[n - 2]).max(0.15);
            ((n - 1) as f32 + (heard_sec - grid[n - 1]) / dt_last, dt_last)
        } else {
            let t0 = grid[idx - 1];
            let t1 = grid[idx];
            let span = (t1 - t0).max(0.15);
            ((idx - 1) as f32 + (heard_sec - t0) / span, span)
        };
        let aligned_beats = raw_idx_f as f64 - downbeat_idx as f64;
        let local_bpm = (60.0 / span).clamp(30.0, 300.0);
        (aligned_beats, local_bpm)
    }

    fn detect_onset(&mut self, dt: f32, low_energy: f32, flux_hint: f32, vu: f32) -> bool {
        self.since_last_onset_sec += dt;
        let prev_silence = self.silence_sec;
        if vu < 0.03 {
            self.silence_sec += dt;
        } else {
            self.silence_sec = 0.0;
        }

        let low_rise = (low_energy - self.prev_low_energy).max(0.0);
        self.prev_low_energy = low_energy;

        let flux = low_rise * 0.75 + flux_hint.clamp(0.0, 1.0) * 0.25;
        let alpha = (dt * 4.5).clamp(0.01, 0.5);
        let threshold = (self.flux_ema * self.cfg.onset_threshold_mul).max(0.035);

        // 210 ms refractory window prevents double-triggering on a single kick drum transient
        let is_onset = flux > threshold && self.since_last_onset_sec >= 0.21;
        self.flux_ema += (flux - self.flux_ema) * alpha;

        if is_onset {
            self.was_silent_before_onset = prev_silence >= 2.2;
            let ioi = self.since_last_onset_sec;
            self.since_last_onset_sec = 0.0;
            if (0.22..=2.0).contains(&ioi) {
                self.push_ioi_bpm(self.fold_bpm(60.0 / ioi));
            }
        } else {
            self.was_silent_before_onset = false;
        }
        is_onset
    }

    fn push_ioi_bpm(&mut self, candidate_bpm: f32) {
        self.ioi_bpms[self.ioi_cursor] = candidate_bpm;
        self.ioi_cursor = (self.ioi_cursor + 1) % self.ioi_bpms.len();
        if self.ioi_len < self.ioi_bpms.len() {
            self.ioi_len += 1;
        }
        if self.ioi_len >= 3 {
            // Zero-allocation stack median filter: rejects occasional syncopations and missed beats
            let mut scratch = [0.0_f32; 8];
            scratch[..self.ioi_len].copy_from_slice(&self.ioi_bpms[..self.ioi_len]);
            for i in 1..self.ioi_len {
                let mut j = i;
                while j > 0 && scratch[j - 1] > scratch[j] {
                    scratch.swap(j - 1, j);
                    j -= 1;
                }
            }
            let median_bpm = scratch[self.ioi_len / 2];
            self.effective_bpm += (median_bpm - self.effective_bpm) * 0.20;
        }
    }

    fn advance_live_pll(&mut self, dt: f32, raw_onset: bool, vu: f32) {
        if raw_onset && self.since_last_onset_sec == 0.0 && vu > 0.22 {
            // If a heavy drop arrives right after a long silence (>2.2 s), snap forward to the next bar's downbeat
            if self.was_silent_before_onset {
                let rem_in_bar = self.total_beats.rem_euclid(4.0);
                if rem_in_bar > 0.08 {
                    self.total_beats += 4.0 - rem_in_bar;
                }
                self.pll_rate_trim = 0.0;
            } else {
                let frac = (self.total_beats - self.total_beats.floor()) as f32;
                // Wrapped phase error to the nearest beat within [-0.5, +0.5]
                let phase_err = if frac < 0.5 { -frac } else { 1.0 - frac };
                self.pll_rate_trim = (phase_err * self.cfg.pll_kp).clamp(-0.18, 0.22);
            }
        }

        // Exponential decay of the frequency steering trim over ~200 ms
        self.pll_rate_trim *= (1.0 - dt * 5.5).clamp(0.0, 1.0);
        let rate_mul = (1.0 + self.pll_rate_trim).clamp(0.82, 1.22);
        self.total_beats += (dt * self.effective_bpm * rate_mul / 60.0) as f64;
    }

    fn octave_scale_factor(&self, mut bpm: f32) -> f64 {
        if !self.cfg.auto_octave_fold {
            return 1.0;
        }
        let mut scale = 1.0_f64;
        while bpm > self.cfg.max_playable_bpm {
            bpm *= 0.5;
            scale *= 0.5;
        }
        while bpm < self.cfg.min_playable_bpm {
            bpm *= 2.0;
            scale *= 2.0;
        }
        scale
    }

    fn fold_bpm(&self, bpm: f32) -> f32 {
        bpm * self.octave_scale_factor(bpm) as f32
    }
}

/// Zero-allocation time-domain sub-bass (`40..120 Hz`), spectral-flux, and VU extractor
/// operating directly on interleaved stereo `i16` PCM windows at ~44.1 kHz.
pub struct LivePcmAnalyzer {
    lp_120: f32,
    lp_40: f32,
    lp_mid: f32,
    prev_sample: f32,
    prev_mid_e: f32,
    prev_hi_e: f32,
    peak_low_agc: f32,
    peak_flux_agc: f32,
}

impl Default for LivePcmAnalyzer {
    fn default() -> Self {
        Self::new()
    }
}

impl LivePcmAnalyzer {
    pub const fn new() -> Self {
        Self {
            lp_120: 0.0,
            lp_40: 0.0,
            lp_mid: 0.0,
            prev_sample: 0.0,
            prev_mid_e: 0.0,
            prev_hi_e: 0.0,
            peak_low_agc: 0.12,
            peak_flux_agc: 0.08,
        }
    }

    /// Analyze `n_frames` stereo samples (`samples[..n_frames * 2]`) and return `(low_energy, flux_hint, vu_level)`.
    pub fn analyze_window(&mut self, samples: &[i16], n_frames: usize, dt_sec: f32) -> (f32, f32, f32) {
        let n = n_frames.min(samples.len() / 2);
        if n == 0 {
            return (0.0, 0.0, 0.0);
        }

        // One-pole cutoff coefficients @ 44.1 kHz:
        // alpha = 1 - exp(-2*pi*fc/44100): 40 Hz -> ~0.0057, 120 Hz -> ~0.0170, 1.4 kHz -> ~0.18
        const A_40: f32 = 0.0057;
        const A_120: f32 = 0.0170;
        const A_MID: f32 = 0.1800;
        const INV_I16: f32 = 1.0 / 32768.0;

        let mut low_sq = 0.0_f32;
        let mut mid_sq = 0.0_f32;
        let mut hi_sq = 0.0_f32;
        let mut full_sq = 0.0_f32;

        for i in 0..n {
            let l = (samples[i * 2] as f32) * INV_I16;
            let r = (samples[i * 2 + 1] as f32) * INV_I16;
            let mono = (l + r) * 0.5;

            self.lp_40 += A_40 * (mono - self.lp_40);
            self.lp_120 += A_120 * (mono - self.lp_120);
            self.lp_mid += A_MID * (mono - self.lp_mid);

            // Band-pass 40..120 Hz isolates kick drums & basslines cleanly from DC rumble
            let band_40_120 = self.lp_120 - self.lp_40;
            let mid = self.lp_mid - self.lp_120;
            let hi = mono - self.prev_sample;
            self.prev_sample = mono;

            low_sq += band_40_120 * band_40_120;
            mid_sq += mid * mid;
            hi_sq += hi * hi;
            full_sq += mono * mono;
        }

        let inv_n = 1.0 / (n as f32);
        let low_rms = (low_sq * inv_n).sqrt();
        let mid_rms = (mid_sq * inv_n).sqrt();
        let hi_rms = (hi_sq * inv_n).sqrt();
        let full_rms = (full_sq * inv_n).sqrt();

        // Broadband positive spectral flux across mid + high bands (snare, synth stabs, chiptune attacks)
        let raw_flux = (mid_rms - self.prev_mid_e).max(0.0) + (hi_rms - self.prev_hi_e).max(0.0) * 0.8;
        self.prev_mid_e = mid_rms;
        self.prev_hi_e = hi_rms;

        // Slow automatic gain control so quiet acoustic/MIDI tracks and mastered EDM both hit 0..1 cleanly
        let decay = (1.0 - dt_sec * 0.45).clamp(0.90, 0.999);
        self.peak_low_agc = (self.peak_low_agc * decay).max(low_rms).max(0.04);
        self.peak_flux_agc = (self.peak_flux_agc * decay).max(raw_flux).max(0.025);

        let low_norm = (low_rms / self.peak_low_agc).clamp(0.0, 1.0);
        let flux_norm = (raw_flux / self.peak_flux_agc).clamp(0.0, 1.0);
        let vu_norm = (full_rms * 3.1).sqrt().clamp(0.0, 1.0);

        (low_norm, flux_norm, vu_norm)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bar_phase_remains_strictly_monotonic_and_fires_1_4_8_on_schedule() {
        let mut tracker = BarPhaseTracker::new(BarPhaseConfig::default());
        let dt = 1.0 / 60.0;
        let mut prev_unwrapped = -1.0_f32;
        let mut bar_count = 0;
        let mut beat_count = 0;
        let mut eighth_count = 0;

        // Simulate 8 seconds at 120 BPM (= 16 beats = 4 full bars) with occasional late/early kicks
        for frame in 0..480 {
            let t = frame as f32 * dt;
            let kick = ((t + 0.03) % 0.5) < dt; // Slightly late kick (+30 ms)
            let tick = tracker.update(
                SyncSource::LivePll,
                AudioFrameInput {
                    dt_sec: dt,
                    playback_pos_sec: None,
                    low_energy: if kick { 0.95 } else { 0.05 },
                    flux_hint: if kick { 0.80 } else { 0.02 },
                    vu_level: if kick { 0.75 } else { 0.35 },
                },
            );

            assert!(tick.bar_phase >= 0.0 && tick.bar_phase < 4.0);
            assert_eq!(tick.beat_index, tick.bar_phase as u8);

            let unwrapped = (bar_count as f32) * 4.0 + tick.bar_phase;
            if frame > 0 && !tick.fire_bar {
                assert!(
                    unwrapped >= prev_unwrapped - 1e-5,
                    "Phase jumped backward at frame {frame}: {prev_unwrapped} -> {unwrapped}"
                );
            }
            if tick.fire_bar {
                bar_count += 1;
            }
            if tick.fire_beat {
                beat_count += 1;
            }
            if tick.fire_eighth {
                eighth_count += 1;
            }
            prev_unwrapped = (bar_count as f32) * 4.0 + tick.bar_phase;
        }

        assert!(bar_count >= 3 && bar_count <= 5, "bars={bar_count}");
        assert!(beat_count >= 14 && beat_count <= 18, "beats={beat_count}");
        assert!(eighth_count >= 29 && eighth_count <= 36, "eighths={eighth_count}");
    }

    #[test]
    fn octave_folding_normalizes_dnb_and_slow_ambient_tempos() {
        let mut tracker = BarPhaseTracker::new(BarPhaseConfig::default());
        let tick_fast = tracker.update(
            SyncSource::SongMapFixed {
                bpm: 174.0,
                first_downbeat_sec: 0.0,
            },
            AudioFrameInput {
                dt_sec: 1.0 / 60.0,
                playback_pos_sec: Some(1.0),
                low_energy: 0.2,
                flux_hint: 0.1,
                vu_level: 0.5,
            },
        );
        assert!((tick_fast.effective_bpm - 87.0).abs() < 0.01);

        let tick_slow = tracker.update(
            SyncSource::SongMapFixed {
                bpm: 54.0,
                first_downbeat_sec: 0.0,
            },
            AudioFrameInput {
                dt_sec: 1.0 / 60.0,
                playback_pos_sec: Some(2.0),
                low_energy: 0.2,
                flux_hint: 0.1,
                vu_level: 0.5,
            },
        );
        assert!((tick_slow.effective_bpm - 108.0).abs() < 0.01);
    }

    #[test]
    fn beat_window_detects_on_beat_phase_roll_within_65ms() {
        let mut tracker = BarPhaseTracker::new(BarPhaseConfig::default());
        // Initialize at exact beat 2.0 @ 120 BPM (500 ms per beat)
        let _ = tracker.update(
            SyncSource::SongMapFixed {
                bpm: 120.0,
                first_downbeat_sec: 0.0,
            },
            AudioFrameInput {
                dt_sec: 0.016,
                playback_pos_sec: Some(1.025), // minus 25 ms audio latency -> 1.000s = beat 2.0
                low_energy: 0.0,
                flux_hint: 0.0,
                vu_level: 0.5,
            },
        );
        assert!(tracker.is_in_beat_window(65.0));

        // Seek to mid-beat (1.250s + 0.025s = beat 2.5 -> 250 ms away from nearest beat)
        let _ = tracker.update(
            SyncSource::SongMapFixed {
                bpm: 120.0,
                first_downbeat_sec: 0.0,
            },
            AudioFrameInput {
                dt_sec: 0.016,
                playback_pos_sec: Some(3.275), // jump > 1.5 beats -> snaps to beat 6.5
                low_energy: 0.0,
                flux_hint: 0.0,
                vu_level: 0.5,
            },
        );
        assert!(!tracker.is_in_beat_window(65.0));
    }
}
