//! Monotonic Strong-Beat Rhythm Clock for `Beat Stack` (`uipod-track-intel` consumer).
//!
//! Synchronizes overhead crane rail steps (`1/8` sub-beats), 1-beat claw drop telegraphs,
//! quantized crate gravity, and **Strong-Beat (`Beats 1 & 3` in `4/4`)** cluster/row detonations
//! directly to `uipod-track-intel` (`PluginTrackAnalysis` + lock-free `PluginLiveTelemetry`),
//! falling back smoothly to `120.0 BPM` only when audio is paused, stopped, or silent.

/// Per-frame monotonic clock & live `uipod-track-intel` telemetry fed into [`BeatClock::update`].
#[derive(Clone, Copy, Default)]
pub struct BeatFrameInput {
    /// Monotonic elapsed seconds since previous update (`get_ticks_ms` / `performance.now` delta).
    pub dt_sec: f32,
    /// `true` when host audio playback is actively playing (`status == 1`).
    pub music_playing: bool,
    /// Optional `.nsf` / `.nsfe` hardware APU percussion + bass trigger (`0.0..=1.0`).
    pub apu_pulse: f32,
    /// Pre-verified `(bpm, first_beat_offset_ms, playhead_ms, kick_impulse)` from `uipod-track-intel`.
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
    comp_beats: f64,
    audio_latency_sec: f64,
    live_bpm: f32,
    tempo_locked: bool,
    since_onset_sec: f32,
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
            comp_beats: 0.0,
            audio_latency_sec: audio_latency_sec.clamp(0.0, 0.30),
            live_bpm: Self::FALLBACK_BPM,
            tempo_locked: false,
            since_onset_sec: 1.0,
            initialized: false,
        }
    }

    pub fn set_audio_latency_sec(&mut self, latency_sec: f64) {
        self.audio_latency_sec = latency_sec.clamp(0.0, 0.30);
    }

    /// Resets lock state when a new song starts while keeping `comp_beats` monotonic.
    pub fn on_track_change(&mut self) {
        self.tempo_locked = false;
        self.since_onset_sec = 1.0;
    }

    pub fn reset_full(&mut self) {
        self.comp_beats = 0.0;
        self.live_bpm = Self::FALLBACK_BPM;
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

    pub fn update(&mut self, input: BeatFrameInput) -> BeatStep {
        let dt = input.dt_sec.clamp(0.0, 0.10);
        let prev_comp = self.comp_beats;

        if let (true, Some((ibpm, offset_ms, playhead_ms, kick_imp))) =
            (input.music_playing, input.intel_sync)
        {
            let folded = Self::fold_bpm(ibpm);
            self.live_bpm = folded;
            self.tempo_locked = true;
            self.since_onset_sec += dt;
            let combined_kick = kick_imp.max(input.apu_pulse);
            let raw_onset = if combined_kick >= 0.56 && self.since_onset_sec >= 0.18 {
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
            } else {
                let err = target_beats - self.comp_beats;
                if err.abs() > 3.0 {
                    // Track seeked/restarted: align 2-beat strong-beat phase monotonically without rewinding
                    let target_frac = target_beats.rem_euclid(2.0);
                    let cur_base = (self.comp_beats * 0.5).floor() * 2.0;
                    let mut next_b = cur_base + target_frac;
                    if next_b < self.comp_beats {
                        next_b += 2.0;
                    }
                    self.comp_beats = next_b;
                } else {
                    let rate_mul = (1.0 + err * 1.8).clamp(0.84, 1.18);
                    self.comp_beats += nominal_step * rate_mul;
                }
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

            let beat_index = self.comp_beats.floor().max(0.0) as u64;
            let is_strong_beat = (beat_index & 1) == 0;
            let fire_strong_beat = fire_beat && is_strong_beat;
            let beat_frac =
                ((self.comp_beats - (beat_index as f64)) as f32).clamp(0.0, 0.999_99);
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

        // Silent / paused internal 120.0 BPM fallback clock
        self.tempo_locked = false;
        self.live_bpm += (Self::FALLBACK_BPM - self.live_bpm) * (dt * 3.0).clamp(0.0, 1.0);
        let active_bpm = self.live_bpm.clamp(Self::MIN_BPM, Self::MAX_BPM);
        let step_beats_f64 = (dt * active_bpm / 60.0) as f64;
        self.comp_beats += step_beats_f64;

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

        let beat_index = self.comp_beats.floor().max(0.0) as u64;
        let is_strong_beat = (beat_index & 1) == 0;
        let fire_strong_beat = fire_beat && is_strong_beat;
        let beat_frac = ((self.comp_beats - (beat_index as f64)) as f32).clamp(0.0, 0.999_99);
        let actual_step = (self.comp_beats - prev_comp).max(0.0) as f32;

        BeatStep {
            effective_bpm: active_bpm,
            locked_to_audio: false,
            beat_frac,
            beat_index,
            is_strong_beat,
            step_beats: actual_step,
            fire_beat,
            fire_strong_beat,
            fire_half_beat,
            raw_onset: false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn locks_to_140_bpm_intel_sync_and_folds_halftime_70_to_140() {
        let mut clock = BeatClock::new(0.025);
        let dt = 1.0 / 60.0;
        let mut pos_ms = 0.0_f64;
        let mut last = BeatStep::default();

        for _ in 0..180 {
            pos_ms += (dt as f64) * 1000.0;
            last = clock.update(BeatFrameInput {
                dt_sec: dt,
                music_playing: true,
                apu_pulse: 0.0,
                intel_sync: Some((140.0, 120, pos_ms as u32, 0.7)),
            });
        }
        assert!(last.locked_to_audio);
        assert!((last.effective_bpm - 140.0).abs() < 1e-4);

        // Half-time 70 BPM folds cleanly to 140.0 BPM
        let folded = clock.update(BeatFrameInput {
            dt_sec: dt,
            music_playing: true,
            apu_pulse: 0.0,
            intel_sync: Some((70.0, 0, 4_000, 0.5)),
        });
        assert!((folded.effective_bpm - 140.0).abs() < 1e-4);
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
