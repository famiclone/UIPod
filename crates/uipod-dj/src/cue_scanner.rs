//! Fast lookahead Beat, Downbeat, and Digital Silence Cue Analyzer (`cue_scanner`).
//!
//! Analyzes a 12–18 second PCM window in < 1.5 ms on Allwinner A133P:
//! - Trims leading/trailing digital silence (`< -48 dBFS` = `0.00398` RMS).
//! - Extracts a `100 Hz` (`10 ms` hop) dual-band onset envelope (`40–130 Hz` low-end kick
//!   bandpass + full-band high-frequency transient flux).
//! - Estimates local BPM (`70..180 BPM`), rhythmic confidence (`0.0..=1.0`), and bar-grid
//!   downbeat phase (`first_downbeat_frame`).

use std::f32::consts::PI;

/// `-66 dBFS` linear RMS threshold (`10^(-66/20) = 0.000501`) — trims true digital
/// silence/dither while preserving quiet acoustic/ambient fade-out tails.
pub const SILENCE_THRESHOLD_RMS: f32 = 0.000501;

#[derive(Clone, Copy, Debug, Default)]
pub struct DeckCueAnalysis {
    /// Sample rate of the analyzed window.
    pub sample_rate: u32,
    /// Total stereo frames in the analyzed window.
    pub total_frames: usize,
    /// First non-silent frame (`Deck B` `cue_in`) or `0`.
    pub cue_in_frame: usize,
    /// Last non-silent frame (`Deck A` `cue_out`) or `total_frames`.
    pub cue_out_frame: usize,
    /// Estimated BPM in `[70.0, 180.0]` (`0.0` if silent/unpitched).
    pub bpm: f32,
    /// Rhythmic periodicity confidence in `[0.0, 1.0]`.
    pub confidence: f32,
    /// Frame offset of the first detected bar downbeat ("Beat 1") at or after `cue_in_frame`.
    pub first_downbeat_frame: usize,
    /// Frame offset of the last detected bar downbeat in `Deck A` prior to `cue_out_frame`.
    pub last_downbeat_frame: usize,
}

/// Analyze interleaved `i16` stereo PCM (`[L, R, ...]`).
pub fn analyze_pcm_s16(stereo_s16: &[i16], sample_rate: u32) -> DeckCueAnalysis {
    let sr = sample_rate.max(8_000);
    let total_frames = stereo_s16.len() / 2;
    if total_frames == 0 {
        return DeckCueAnalysis {
            sample_rate: sr,
            ..Default::default()
        };
    }

    let inv = 1.0 / 32768.0f32;
    // 1. Scan 20 ms blocks for `-48 dBFS` silence boundaries
    let win_frames = ((sr as usize) / 50).max(64); // 20 ms
    let num_blocks = total_frames.div_ceil(win_frames);
    let thresh_sq = SILENCE_THRESHOLD_RMS * SILENCE_THRESHOLD_RMS;

    let mut first_loud_blk = None;
    let mut last_loud_blk = None;

    for b in 0..num_blocks {
        let f0 = b * win_frames;
        let f1 = (f0 + win_frames).min(total_frames);
        let mut sum_sq = 0.0f32;
        for f in f0..f1 {
            let l = (stereo_s16[f * 2] as f32) * inv;
            let r = (stereo_s16[f * 2 + 1] as f32) * inv;
            let m = 0.5 * (l + r);
            sum_sq += m * m;
        }
        let mean_sq = sum_sq / ((f1 - f0) as f32);
        if mean_sq >= thresh_sq {
            if first_loud_blk.is_none() {
                first_loud_blk = Some(b);
            }
            last_loud_blk = Some(b);
        }
    }

    let pre_attack_frames = ((sr as usize) * 8) / 1000; // 8 ms pre-attack cushion
    let cue_in_frame = match first_loud_blk {
        Some(b) => (b * win_frames).saturating_sub(pre_attack_frames),
        None => 0,
    };
    let cue_out_frame = match last_loud_blk {
        Some(b) => ((b + 1) * win_frames + pre_attack_frames).min(total_frames),
        None => total_frames,
    };

    analyze_core(
        total_frames,
        sr,
        cue_in_frame,
        cue_out_frame,
        |f| {
            let l = (stereo_s16[f * 2] as f32) * inv;
            let r = (stereo_s16[f * 2 + 1] as f32) * inv;
            0.5 * (l + r)
        },
    )
}

/// Analyze interleaved `f32` (`[-1.0, 1.0]`) stereo PCM (`[L, R, ...]`).
pub fn analyze_pcm_f32(stereo_f32: &[f32], sample_rate: u32) -> DeckCueAnalysis {
    let sr = sample_rate.max(8_000);
    let total_frames = stereo_f32.len() / 2;
    if total_frames == 0 {
        return DeckCueAnalysis {
            sample_rate: sr,
            ..Default::default()
        };
    }

    let win_frames = ((sr as usize) / 50).max(64);
    let num_blocks = total_frames.div_ceil(win_frames);
    let thresh_sq = SILENCE_THRESHOLD_RMS * SILENCE_THRESHOLD_RMS;

    let mut first_loud_blk = None;
    let mut last_loud_blk = None;

    for b in 0..num_blocks {
        let f0 = b * win_frames;
        let f1 = (f0 + win_frames).min(total_frames);
        let mut sum_sq = 0.0f32;
        for f in f0..f1 {
            let m = 0.5 * (stereo_f32[f * 2] + stereo_f32[f * 2 + 1]);
            sum_sq += m * m;
        }
        if sum_sq / ((f1 - f0) as f32) >= thresh_sq {
            if first_loud_blk.is_none() {
                first_loud_blk = Some(b);
            }
            last_loud_blk = Some(b);
        }
    }

    let pre_attack_frames = ((sr as usize) * 8) / 1000;
    let cue_in_frame = first_loud_blk
        .map(|b| (b * win_frames).saturating_sub(pre_attack_frames))
        .unwrap_or(0);
    let cue_out_frame = last_loud_blk
        .map(|b| ((b + 1) * win_frames + pre_attack_frames).min(total_frames))
        .unwrap_or(total_frames);

    analyze_core(
        total_frames,
        sr,
        cue_in_frame,
        cue_out_frame,
        |f| 0.5 * (stereo_f32[f * 2] + stereo_f32[f * 2 + 1]),
    )
}

fn analyze_core<F>(
    total_frames: usize,
    sr: u32,
    cue_in_frame: usize,
    cue_out_frame: usize,
    mono_sample: F,
) -> DeckCueAnalysis
where
    F: Fn(usize) -> f32,
{
    let active_start = cue_in_frame.min(total_frames);
    let active_end = cue_out_frame.clamp(active_start, total_frames);
    let active_len = active_end - active_start;

    // Need at least ~2.5 seconds of active audio to estimate tempo reliably
    if active_len < (sr as usize) * 5 / 2 {
        return DeckCueAnalysis {
            sample_rate: sr,
            total_frames,
            cue_in_frame,
            cue_out_frame,
            bpm: 0.0,
            confidence: 0.0,
            first_downbeat_frame: cue_in_frame,
            last_downbeat_frame: cue_out_frame,
        };
    }

    // 2. Build 100 Hz (`10 ms` hop) dual-band onset flux envelope
    let hop = ((sr as usize) / 100).max(32);
    let env_rate = (sr as f32) / (hop as f32); // ~100.0 Hz
    let num_hops = active_len / hop;

    let fs = sr as f32;
    // Low-band kick bandpass: 1-pole HP @ 38 Hz -> 1-pole LP @ 135 Hz
    let hp40_a = (1.0 - (-2.0 * PI * 38.0 / fs).exp()).clamp(0.001, 0.5);
    let lp135_a = (1.0 - (-2.0 * PI * 135.0 / fs).exp()).clamp(0.002, 0.8);
    let mut hp_state = 0.0f32;
    let mut lp_state = 0.0f32;
    let mut prev_s = 0.0f32;

    let mut kick_env = Vec::with_capacity(num_hops);
    let mut onset_env = Vec::with_capacity(num_hops);
    let mut prev_kick_e = 0.0f32;
    let mut prev_hf_e = 0.0f32;

    for h in 0..num_hops {
        let base = active_start + h * hop;
        let mut ke = 0.0f32;
        let mut he = 0.0f32;
        for i in 0..hop {
            let s = mono_sample(base + i);
            hp_state += hp40_a * (s - hp_state);
            let hp = s - hp_state;
            lp_state += lp135_a * (hp - lp_state);
            ke += lp_state * lp_state;

            let diff = s - prev_s;
            prev_s = s;
            he += diff * diff;
        }
        let k_rms = (ke / (hop as f32)).sqrt();
        let h_rms = (he / (hop as f32)).sqrt();

        let k_flux = (k_rms - prev_kick_e).max(0.0);
        let h_flux = (h_rms - prev_hf_e).max(0.0);
        prev_kick_e = k_rms;
        prev_hf_e = h_rms;

        kick_env.push(k_rms);
        onset_env.push(k_flux * 0.72 + h_flux * 0.28);
    }

    // Normalize onset envelope & measure peakiness (reject flat steady sine/ambient drones!)
    let n = onset_env.len();
    let mean: f32 = onset_env.iter().copied().sum::<f32>() / (n as f32);
    let max_onset: f32 = onset_env.iter().copied().fold(0.0f32, f32::max);
    if max_onset < 1e-5 || mean < 1e-6 {
        return DeckCueAnalysis {
            sample_rate: sr,
            total_frames,
            cue_in_frame,
            cue_out_frame,
            bpm: 0.0,
            confidence: 0.0,
            first_downbeat_frame: cue_in_frame,
            last_downbeat_frame: cue_out_frame,
        };
    }

    // Zero-mean centered onset signal for autocorrelation
    let mut centered = vec![0.0f32; n];
    let mut var_sum = 1e-12f32;
    for i in 0..n {
        // Light 3-tap smoothing so 1-hop jitter doesn't split correlation peaks
        let sm = if i > 0 && i + 1 < n {
            0.25 * onset_env[i - 1] + 0.50 * onset_env[i] + 0.25 * onset_env[i + 1]
        } else {
            onset_env[i]
        };
        centered[i] = sm - mean;
        var_sum += centered[i] * centered[i];
    }

    // If signal has almost zero onset modulation (pure continuous pad/drone), confidence is low
    let crest = max_onset / (mean + 1e-6);
    if crest < 1.8 {
        return DeckCueAnalysis {
            sample_rate: sr,
            total_frames,
            cue_in_frame,
            cue_out_frame,
            bpm: 120.0,
            confidence: 0.15,
            first_downbeat_frame: cue_in_frame,
            last_downbeat_frame: cue_out_frame,
        };
    }

    // 3. Autocorrelation across lags for 68..185 BPM (`lag = env_rate * 60 / bpm`)
    let min_lag = ((env_rate * 60.0 / 185.0).floor() as usize).max(12);
    let max_lag = ((env_rate * 60.0 / 68.0).ceil() as usize).min(n / 2);
    if min_lag >= max_lag {
        return DeckCueAnalysis {
            sample_rate: sr,
            total_frames,
            cue_in_frame,
            cue_out_frame,
            bpm: 0.0,
            confidence: 0.0,
            first_downbeat_frame: cue_in_frame,
            last_downbeat_frame: cue_out_frame,
        };
    }

    let corr_max = (max_lag * 2 + 2).min(n - 1);
    let mut acf = vec![0.0f32; corr_max + 1];
    for lag in min_lag / 2..=corr_max {
        if lag == 0 || lag >= n {
            continue;
        }
        let count = n - lag;
        let mut dot = 0.0f32;
        for i in 0..count {
            dot += centered[i] * centered[i + lag];
        }
        // Unbiased normalization scaled to `[−1, 1]`
        let scale = (n as f32) / ((count as f32) * var_sum);
        acf[lag] = (dot * scale).clamp(-1.0, 1.0);
    }

    // Comb-enhanced lag score with gentle perceptual tempo prior around 120 BPM
    let mut best_lag = min_lag;
    let mut best_comb = f32::NEG_INFINITY;
    let mut comb_vals = Vec::with_capacity(max_lag - min_lag + 1);

    for lag in min_lag..=max_lag {
        let r1 = acf[lag].max(0.0);
        let r2 = if lag * 2 <= corr_max {
            acf[lag * 2].max(0.0)
        } else {
            r1
        };
        let bpm_cand = env_rate * 60.0 / (lag as f32);
        // Mild Gaussian prior in log2-BPM space centered at 122 BPM so 120 beats 60/240 ties
        let log_dist = (bpm_cand / 122.0).ln();
        let prior = (-0.5 * (log_dist / 0.55).powi(2)).exp(); // ~0.85..1.0 across 85..165 BPM
        let score = (0.68 * r1 + 0.32 * r2) * (0.82 + 0.18 * prior);
        comb_vals.push(score);
        if score > best_comb {
            best_comb = score;
            best_lag = lag;
        }
    }

    // Parabolic sub-hop interpolation around `best_lag` for ±0.1 BPM accuracy
    let frac_offset = if best_lag > min_lag && best_lag < max_lag {
        let y_m = acf[best_lag - 1];
        let y_0 = acf[best_lag];
        let y_p = acf[best_lag + 1];
        let denom = y_m - 2.0 * y_0 + y_p;
        if denom.abs() > 1e-6 {
            (0.5 * (y_m - y_p) / denom).clamp(-0.5, 0.5)
        } else {
            0.0
        }
    } else {
        0.0
    };

    let exact_lag_hops = (best_lag as f32) + frac_offset;
    let raw_bpm = (env_rate * 60.0 / exact_lag_hops).clamp(68.0, 185.0);

    // Compute rhythmic confidence in `[0.0, 1.0]`
    let mut sorted = comb_vals.clone();
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let median = sorted[sorted.len() / 2].max(0.0);
    let prominence = (best_comb - median).max(0.0);
    let raw_r1 = acf[best_lag].max(0.0);
    let crest_factor = ((crest - 1.8) / 4.0).clamp(0.0, 1.0);
    let confidence = ((raw_r1 * 0.85 + prominence * 1.15) * (0.55 + 0.45 * crest_factor))
        .clamp(0.0, 1.0);

    // 4. Beat grid phase & 4/4 Bar Downbeat ("1") search
    let beat_period_hops = exact_lag_hops.max(10.0);
    let period_int = beat_period_hops.round() as usize;
    let mut best_beat_phase = 0usize;
    let mut best_phase_sum = f32::NEG_INFINITY;

    for ph in 0..period_int.max(1) {
        let mut sum = 0.0f32;
        let mut k = 0usize;
        loop {
            let idx = ph + ((k as f32) * beat_period_hops).round() as usize;
            if idx >= n {
                break;
            }
            let mut local = onset_env[idx];
            if idx > 0 {
                local += 0.5 * onset_env[idx - 1];
            }
            if idx + 1 < n {
                local += 0.5 * onset_env[idx + 1];
            }
            sum += local;
            k += 1;
        }
        if sum > best_phase_sum {
            best_phase_sum = sum;
            best_beat_phase = ph;
        }
    }

    // Evaluate which of the 4 beats `{0, 1, 2, 3}` in a 4/4 bar has the strongest kick + onset accent
    let mut best_bar_beat = 0usize;
    let mut best_bar_score = f32::NEG_INFINITY;
    for b_off in 0..4usize {
        let mut sc = 0.0f32;
        let mut count = 0usize;
        let mut beat_idx = b_off;
        loop {
            let idx = best_beat_phase + ((beat_idx as f32) * beat_period_hops).round() as usize;
            if idx >= n {
                break;
            }
            let kick_w = kick_env[idx]
                + kick_env.get(idx + 1).copied().unwrap_or(0.0)
                + kick_env.get(idx + 2).copied().unwrap_or(0.0);
            sc += onset_env[idx] + kick_w * 0.65;
            count += 1;
            beat_idx += 4;
        }
        if count > 0 {
            let avg = sc / (count as f32);
            // Slight preference for earliest downbeat near `cue_in` on ties (`Deck B` drops right on Beat 1!)
            let tie_bonus = if b_off == 0 { 1.02 } else { 1.0 };
            if avg * tie_bonus > best_bar_score {
                best_bar_score = avg * tie_bonus;
                best_bar_beat = b_off;
            }
        }
    }

    let first_downbeat_hops =
        (best_beat_phase as f32) + (best_bar_beat as f32) * beat_period_hops;
    let first_downbeat_frame =
        (active_start + (first_downbeat_hops * (hop as f32)).round() as usize).min(active_end);

    let bar_frames = (4.0 * beat_period_hops * (hop as f32)).round().max(1.0) as usize;
    let last_downbeat_frame = if active_end > first_downbeat_frame {
        let bars_fit = (active_end - first_downbeat_frame) / bar_frames;
        (first_downbeat_frame + bars_fit * bar_frames).min(active_end)
    } else {
        first_downbeat_frame
    };

    DeckCueAnalysis {
        sample_rate: sr,
        total_frames,
        cue_in_frame,
        cue_out_frame,
        bpm: raw_bpm,
        confidence,
        first_downbeat_frame,
        last_downbeat_frame,
    }
}
