//! DJ Transition Choreography Planner:
//! - Same-Album Gapless protection (`Keep Album Gapless`).
//! - Octave-folded (`0.5x`, `1.0x`, `2.0x`) BPM matching within `±6%` / `±10%` / `±12%`.
//! - Case A: Beatmatched 4/8-Bar Mix with shared tempo ramp & center-phrase Bass Swap.
//! - Case B: Automatic Filter Wash (`20 -> 900 Hz` + Ping-Pong Echo Tail) fallback.

use std::f32::consts::{FRAC_PI_2, PI};

use crate::cue_scanner::DeckCueAnalysis;
use crate::{ActiveMixKind, CrossfadeMode, DjConfig, TransitionLength};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PlannedTransitionKind {
    /// Bit-exact gapless transition (Crossfade Off or Same-Album Gapless protection).
    Gapless,
    /// Pure Equal-Power cosine/sine overlap (`ClassicFade`).
    ClassicFade,
    /// Phrase-aligned Beatmatch with shared tempo ramp & center Bass Swap (`p = 0.46..0.54`).
    Beatmatch,
    /// Unstretched Filter Wash (`20 -> 900 Hz` HPF + Ping-Pong echo tail) when BPM/confidence mismatch.
    FilterWash,
}

impl PlannedTransitionKind {
    #[inline]
    pub fn active_mix_kind(self) -> ActiveMixKind {
        match self {
            Self::Gapless => ActiveMixKind::None,
            Self::ClassicFade => ActiveMixKind::Classic,
            Self::Beatmatch => ActiveMixKind::Beatmatch,
            Self::FilterWash => ActiveMixKind::FilterWash,
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct TransitionPlan {
    pub kind: PlannedTransitionKind,
    pub mode: CrossfadeMode,
    pub sample_rate: u32,
    /// Frame index in `Deck A`'s buffer where the dual-deck mix begins (`p = 0.0`).
    pub mix_start_a_frame: usize,
    /// Frame index in `Deck A`'s buffer where `Deck A` ends (`p = 1.0`).
    pub cue_out_a_frame: usize,
    /// Frame index in `Deck B`'s buffer where `Deck B` enters at `p = 0.0`.
    pub start_b_frame: usize,
    /// Total output stereo frames of the transition (`p: 0.0 -> 1.0`).
    pub mix_output_frames: usize,
    /// Native BPM of `Deck A` (`0.0` if unknown).
    pub bpm_a: f32,
    /// Octave-folded target BPM of `Deck B` matching `bpm_a`'s octave (`0.0` if unknown).
    pub folded_bpm_b: f32,
    /// Raw detected BPM of `Deck B` (`0.0` if unknown).
    pub raw_bpm_b: f32,
    /// Number of musical bars (`4` or `8`) when `kind == Beatmatch`.
    pub bars: u8,
}

/// Instantaneous per-block control parameters at normalized transition progress `p` (`0.0..=1.0`).
#[derive(Clone, Copy, Debug)]
pub struct MixCurveStep {
    pub tempo_a: f32,
    pub tempo_b: f32,
    pub current_bpm: f32,
    pub gain_a: f32,
    pub gain_b: f32,
    pub hpf_a_hz: f32,
    pub hpf_b_hz: f32,
    pub wash_wet_a: f32,
    pub bass_swap_flash: bool,
}

/// Check `Keep Album Gapless` rule: consecutive tracks (`N -> N+1`) of the same non-empty album
/// while Shuffle is `OFF` bypass DJ crossfading to preserve concept albums & live mixes.
pub fn should_respect_album_gapless(
    keep_album_gapless: bool,
    shuffle_active: bool,
    album_a: &str,
    track_num_a: i32,
    album_b: &str,
    track_num_b: i32,
) -> bool {
    if !keep_album_gapless || shuffle_active {
        return false;
    }
    let a = album_a.trim();
    let b = album_b.trim();
    if a.is_empty() || b.is_empty() {
        return false;
    }
    a.eq_ignore_ascii_case(b) && track_num_a >= 1 && track_num_b == track_num_a + 1
}

/// Fold `bpm_b` by `0.5x`, `1.0x`, or `2.0x` so half-time/double-time genres (e.g. `87 BPM <-> 174 BPM`)
/// match cleanly within the DJ tempo window.
pub fn fold_bpm_to_reference(bpm_a: f32, bpm_b: f32) -> f32 {
    if bpm_a <= 0.0 || bpm_b <= 0.0 {
        return bpm_b;
    }
    let candidates = [bpm_b, bpm_b * 0.5, bpm_b * 2.0];
    let mut best = bpm_b;
    let mut best_err = f32::INFINITY;
    for &c in &candidates {
        let ratio = bpm_a / c;
        let err = (ratio - 1.0).abs();
        if err < best_err {
            best_err = err;
            best = c;
        }
    }
    best
}

pub const MIN_BEATMATCH_CONFIDENCE: f32 = 0.55;

/// Compute the optimal `TransitionPlan` from `Deck A` outgoing tail analysis,
/// `Deck B` incoming intro analysis, and user `DjConfig`.
pub fn plan_transition(
    cfg: &DjConfig,
    cue_a: &DeckCueAnalysis,
    cue_b: &DeckCueAnalysis,
    album_a: &str,
    track_num_a: i32,
    album_b: &str,
    track_num_b: i32,
    shuffle_active: bool,
    manual_quick_cut: bool,
) -> TransitionPlan {
    let sr = cue_a.sample_rate.max(8_000);
    let cue_out_a = cue_a.cue_out_frame.min(cue_a.total_frames);
    let cue_in_b = cue_b.cue_in_frame.min(cue_b.total_frames.saturating_sub(1));

    // 0. Off or Same-Album Gapless protection (when not manually skipping)
    if cfg.mode == CrossfadeMode::Off
        || (!manual_quick_cut
            && should_respect_album_gapless(
                cfg.keep_album_gapless,
                shuffle_active,
                album_a,
                track_num_a,
                album_b,
                track_num_b,
            ))
    {
        return TransitionPlan {
            kind: PlannedTransitionKind::Gapless,
            mode: cfg.mode,
            sample_rate: sr,
            mix_start_a_frame: cue_out_a,
            cue_out_a_frame: cue_out_a,
            start_b_frame: 0,
            mix_output_frames: 0,
            bpm_a: cue_a.bpm,
            folded_bpm_b: cue_b.bpm,
            raw_bpm_b: cue_b.bpm,
            bars: 0,
        };
    }

    // Manual `R1` Quick DJ Cut (~2.0s Filter Wash)
    if manual_quick_cut {
        let want_frames = ((sr as usize) * 2).min(cue_out_a);
        let start_a = cue_out_a.saturating_sub(want_frames);
        return TransitionPlan {
            kind: PlannedTransitionKind::FilterWash,
            mode: cfg.mode,
            sample_rate: sr,
            mix_start_a_frame: start_a,
            cue_out_a_frame: cue_out_a,
            start_b_frame: cue_in_b,
            mix_output_frames: cue_out_a - start_a,
            bpm_a: cue_a.bpm,
            folded_bpm_b: cue_b.bpm,
            raw_bpm_b: cue_b.bpm,
            bars: 0,
        };
    }

    // Too short to crossfade cleanly (< 3 seconds in either track)
    let min_frames = (sr as usize) * 3;
    if cue_out_a < min_frames || cue_b.total_frames.saturating_sub(cue_in_b) < min_frames {
        return TransitionPlan {
            kind: PlannedTransitionKind::Gapless,
            mode: cfg.mode,
            sample_rate: sr,
            mix_start_a_frame: cue_out_a,
            cue_out_a_frame: cue_out_a,
            start_b_frame: 0,
            mix_output_frames: 0,
            bpm_a: cue_a.bpm,
            folded_bpm_b: cue_b.bpm,
            raw_bpm_b: cue_b.bpm,
            bars: 0,
        };
    }

    // 1. Classic Equal-Power Fade (`4s` / `8s` / `12s`)
    if cfg.mode == CrossfadeMode::ClassicFade {
        let dur_sec: usize = match cfg.length {
            TransitionLength::Short4Bars => 4,
            TransitionLength::Club8Bars => 8,
            TransitionLength::Long12s => 12,
        };
        let avail_b = cue_b.total_frames.saturating_sub(cue_in_b);
        let mix_frames = ((sr as usize) * dur_sec)
            .min(cue_out_a)
            .min(avail_b.max((sr as usize) * 2));
        let start_a = cue_out_a.saturating_sub(mix_frames);
        return TransitionPlan {
            kind: PlannedTransitionKind::ClassicFade,
            mode: cfg.mode,
            sample_rate: sr,
            mix_start_a_frame: start_a,
            cue_out_a_frame: cue_out_a,
            start_b_frame: cue_in_b,
            mix_output_frames: mix_frames,
            bpm_a: cue_a.bpm,
            folded_bpm_b: cue_b.bpm,
            raw_bpm_b: cue_b.bpm,
            bars: 0,
        };
    }

    // 2. Smart DJ Modes (`DjVinylSync` & `DjMasterTempo`): check Beatmatch eligibility
    let max_stretch = cfg.max_stretch.as_fraction();
    let folded_b = fold_bpm_to_reference(cue_a.bpm, cue_b.bpm);
    let bpm_valid = cue_a.bpm >= 60.0 && folded_b >= 50.0;
    let stretch_delta = if bpm_valid {
        (cue_a.bpm / folded_b - 1.0).abs()
    } else {
        1.0
    };
    let confident =
        cue_a.confidence >= MIN_BEATMATCH_CONFIDENCE && cue_b.confidence >= MIN_BEATMATCH_CONFIDENCE;

    if bpm_valid && confident && stretch_delta <= max_stretch {
        // Case A: Beatmatched Bass-Swap Mix (4 or 8 bars)
        let avg_bpm = 0.5 * (cue_a.bpm + folded_b);
        let bar_sec_a = 240.0 / cue_a.bpm;
        let bar_frames_a = ((bar_sec_a * (sr as f32)).round() as usize).max(sr as usize);

        let want_bars: u8 = match cfg.length {
            TransitionLength::Short4Bars => 4,
            TransitionLength::Club8Bars | TransitionLength::Long12s => 8,
        };
        let chosen_bars: u8 = if want_bars == 8 && cue_out_a >= bar_frames_a * 8 + (sr as usize) {
            8
        } else {
            4
        };

        let raw_span_a = bar_frames_a * (chosen_bars as usize);
        // Snap `mix_start_a` onto `Deck A`'s bar downbeat grid before `cue_out_a`
        let target_start_a = cue_out_a.saturating_sub(raw_span_a);
        let aligned_start_a = if cue_a.last_downbeat_frame > bar_frames_a
            && cue_a.last_downbeat_frame <= cue_out_a
        {
            let anchor = cue_a.last_downbeat_frame;
            if anchor >= raw_span_a {
                anchor - raw_span_a
            } else {
                target_start_a
            }
        } else {
            target_start_a
        };

        let effective_end_a = (aligned_start_a + raw_span_a).min(cue_out_a);
        // Output duration at average mix tempo (`0.5 * bpm_a + 0.5 * folded_bpm_b`)
        let mix_sec = (chosen_bars as f32) * (240.0 / avg_bpm);
        let mix_out_frames = ((mix_sec * (sr as f32)).round() as usize)
            .clamp((sr as usize) * 3, (sr as usize) * 20);

        // Align `Deck B` so its first strong downbeat lands on Beat 1 (`p = 0.0`)
        let beat_frames_b = ((60.0 / cue_b.bpm.max(60.0)) * (sr as f32)).round() as usize;
        let start_b = if cue_b.first_downbeat_frame >= cue_in_b
            && cue_b.first_downbeat_frame - cue_in_b <= beat_frames_b * 2
        {
            cue_b.first_downbeat_frame
        } else {
            cue_in_b
        };

        return TransitionPlan {
            kind: PlannedTransitionKind::Beatmatch,
            mode: cfg.mode,
            sample_rate: sr,
            mix_start_a_frame: aligned_start_a,
            cue_out_a_frame: effective_end_a,
            start_b_frame: start_b.min(cue_b.total_frames.saturating_sub(1)),
            mix_output_frames: mix_out_frames,
            bpm_a: cue_a.bpm,
            folded_bpm_b: folded_b,
            raw_bpm_b: cue_b.bpm,
            bars: chosen_bars,
        };
    }

    // Case B: Automatic Filter Wash + Ping-Pong Tail Fallback (~5.0 seconds, unstretched)
    let wash_sec = match cfg.length {
        TransitionLength::Short4Bars => 4.5f32,
        TransitionLength::Club8Bars | TransitionLength::Long12s => 5.5f32,
    };
    let wash_frames = ((wash_sec * (sr as f32)).round() as usize).min(cue_out_a);
    let start_a = cue_out_a.saturating_sub(wash_frames);

    TransitionPlan {
        kind: PlannedTransitionKind::FilterWash,
        mode: cfg.mode,
        sample_rate: sr,
        mix_start_a_frame: start_a,
        cue_out_a_frame: cue_out_a,
        start_b_frame: cue_in_b,
        mix_output_frames: wash_frames.max(1),
        bpm_a: cue_a.bpm,
        folded_bpm_b: folded_b,
        raw_bpm_b: cue_b.bpm,
        bars: 0,
    }
}

#[inline]
fn smoothstep(t: f32) -> f32 {
    let x = t.clamp(0.0, 1.0);
    x * x * (3.0 - 2.0 * x)
}

/// Evaluate exact gain, tempo ratio, Butterworth HPF cutoff, and echo send at normalized progress `p` (`0.0..=1.0`).
pub fn evaluate_mix_curves(plan: &TransitionPlan, progress: f32) -> MixCurveStep {
    let p = progress.clamp(0.0, 1.0);
    let gain_a = (p * FRAC_PI_2).cos();
    let gain_b = (p * FRAC_PI_2).sin();

    match plan.kind {
        PlannedTransitionKind::Gapless => MixCurveStep {
            tempo_a: 1.0,
            tempo_b: 1.0,
            current_bpm: plan.bpm_a,
            gain_a: if p < 0.5 { 1.0 } else { 0.0 },
            gain_b: if p < 0.5 { 0.0 } else { 1.0 },
            hpf_a_hz: 20.0,
            hpf_b_hz: 20.0,
            wash_wet_a: 0.0,
            bass_swap_flash: false,
        },
        PlannedTransitionKind::ClassicFade => MixCurveStep {
            tempo_a: 1.0,
            tempo_b: 1.0,
            current_bpm: plan.bpm_a,
            gain_a,
            gain_b,
            hpf_a_hz: 20.0,
            hpf_b_hz: 20.0,
            wash_wet_a: 0.0,
            bass_swap_flash: false,
        },
        PlannedTransitionKind::Beatmatch => {
            let ba = plan.bpm_a.max(40.0);
            let bb = plan.folded_bpm_b.max(40.0);

            // Shared tempo ramp:
            //   p in [0.00, 0.25]: lock to bpm_a
            //   p in [0.25, 0.75]: linear ramp bpm_a -> folded_bpm_b
            //   p in [0.75, 1.00]: lock to folded_bpm_b (Deck B exits at exact 1.0x speed!)
            let cur_bpm = if p <= 0.25 {
                ba
            } else if p >= 0.75 {
                bb
            } else {
                let t = (p - 0.25) * 2.0;
                ba + (bb - ba) * t
            };

            let tempo_a = (cur_bpm / ba).clamp(0.80, 1.25);
            let tempo_b = (cur_bpm / bb).clamp(0.80, 1.25);

            // Center-phrase Bass Swap (`p = 0.46 .. 0.54`, landing right on Bar 5 Beat 1)
            let (hpf_a_hz, hpf_b_hz) = if p <= 0.46 {
                (20.0, 250.0)
            } else if p >= 0.54 {
                (280.0, 20.0)
            } else {
                let s = smoothstep((p - 0.46) / 0.08);
                (20.0 + 260.0 * s, 250.0 - 230.0 * s)
            };

            MixCurveStep {
                tempo_a,
                tempo_b,
                current_bpm: cur_bpm,
                gain_a,
                gain_b,
                hpf_a_hz,
                hpf_b_hz,
                wash_wet_a: 0.0,
                bass_swap_flash: (0.47..=0.53).contains(&p),
            }
        }
        PlannedTransitionKind::FilterWash => {
            // Unstretched Filter Wash: sweep Deck A HPF `20 -> 900 Hz` + wet Ping-Pong Echo tail
            let hpf_a_hz = 20.0 * (900.0f32 / 20.0f32).powf(p);
            let wash_wet_a = (p * PI).sin() * 0.45;
            MixCurveStep {
                tempo_a: 1.0,
                tempo_b: 1.0,
                current_bpm: plan.bpm_a,
                gain_a,
                gain_b,
                hpf_a_hz,
                hpf_b_hz: 20.0,
                wash_wet_a,
                bass_swap_flash: false,
            }
        }
    }
}
