//! Fast lookahead Beat, Downbeat, and Digital Silence Cue Analyzer (`cue_scanner`).
//!
//! - Trims leading/trailing digital silence (`< -66 dBFS` = `0.000501` RMS) with an 8 ms pre-attack cushion.
//! - Delegates BPM (`60..200 BPM`) and 4/4 bar downbeat phase estimation directly to the unified
//!   `uipod-track-intel` acoustic analyzer (`analyze_mono_window`), eliminating duplicate/biased
//!   per-crate tempo estimators.

use uipod_track_intel::analyze_mono_window;

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
    /// Verified BPM in `[60.0, 200.0]` (`0.0` if silent/unpitched).
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
    analyze_with_mono_fn(total_frames, sr, |f| {
        let l = (stereo_s16[f * 2] as f32) * inv;
        let r = (stereo_s16[f * 2 + 1] as f32) * inv;
        0.5 * (l + r)
    })
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

    analyze_with_mono_fn(total_frames, sr, |f| {
        0.5 * (stereo_f32[f * 2] + stereo_f32[f * 2 + 1])
    })
}

fn analyze_with_mono_fn<F>(total_frames: usize, sr: u32, mono_sample: F) -> DeckCueAnalysis
where
    F: Fn(usize) -> f32,
{
    let win_frames = ((sr as usize) / 50).max(64); // 20 ms RMS silence blocks
    let num_blocks = total_frames.div_ceil(win_frames);
    let thresh_sq = SILENCE_THRESHOLD_RMS * SILENCE_THRESHOLD_RMS;

    let mut first_loud_blk = None;
    let mut last_loud_blk = None;

    for b in 0..num_blocks {
        let f0 = b * win_frames;
        let f1 = (f0 + win_frames).min(total_frames);
        let mut sum_sq = 0.0f32;
        for f in f0..f1 {
            let m = mono_sample(f);
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

    let active_start = cue_in_frame.min(total_frames);
    let active_end = cue_out_frame.clamp(active_start, total_frames);
    let active_len = active_end - active_start;

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

    // Decimate active window to ~11.025 kHz mono before passing to `uipod-track-intel`
    let decim = ((sr / 11_025) as usize).max(1);
    let dec_sr = (sr / (decim as u32)).max(4_000);
    let dec_len = active_len / decim;
    let inv_dec = 1.0 / (decim as f32);
    let mut mono_dec = Vec::with_capacity(dec_len);
    for i in 0..dec_len {
        let base = active_start + i * decim;
        let mut sum = 0.0f32;
        for d in 0..decim {
            sum += mono_sample(base + d);
        }
        mono_dec.push(sum * inv_dec);
    }

    let metrics = analyze_mono_window(&mono_dec, dec_sr, 0, None);
    if metrics.bpm_confidence < 0.08 || metrics.bpm <= 0.0 {
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

    let downbeat_rel_frames =
        (((metrics.first_beat_offset_ms as u64) * (sr as u64)) / 1_000) as usize;
    let first_downbeat_frame = (active_start + downbeat_rel_frames).min(active_end);
    let bar_frames = (((sr as f64) * 240.0) / (metrics.bpm as f64))
        .round()
        .max(1.0) as usize;
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
        bpm: metrics.bpm,
        confidence: metrics.bpm_confidence,
        first_downbeat_frame,
        last_downbeat_frame,
    }
}
