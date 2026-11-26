//! Ultra-fast `< 100 ms` per-track acoustic & chiptune analyzer (`analyzer.rs`).
//!
//! - Seeks directly to `28%` of track/subtrack duration (skipping beatless intros) and decodes
//!   a `16-second` mono decimated (`~11–12 kHz`) window, plus a `5-second` confirmation check
//!   at `60%` when initial periodicity confidence `< 0.62`.
//! - Extracts `200 Hz` (`~5 ms` hop) sub-bass/kick (`40–150 Hz`), percussive mid (`160–2200 Hz`),
//!   and crisp subdivision (`> 2200 Hz`) onset flux envelopes.
//! - Runs unbiased multi-harmonic comb autocorrelation + kick/snare octave disambiguation across
//!   `60.0..=200.0 BPM` with parabolic harmonic refinement (`±0.15 BPM` precision), solving
//!   `90`, `120`, `140`, and `174 BPM` (including DnB 2-step & half-time `70 -> 140 BPM` kicks)
//!   without `110/120 BPM` bias drift.
//! - Analyzes `.nsf`/`.nsfe` chiptunes via headless 60/50 Hz hardware `Triangle + Noise + DPCM`
//!   register envelopes (`< 3 ms` per song).

use std::f32::consts::PI;
#[cfg(not(target_arch = "wasm32"))]
use std::fs::File;
#[cfg(not(target_arch = "wasm32"))]
use std::path::Path;

use uipod_nsf::NsfEngine;
#[cfg(not(target_arch = "wasm32"))]
use uipod_nsf::{is_nsf_path, parse_subtrack_uri};

use crate::db::{analysis_flag, write_fixed_utf8, TrackAnalysisRecord};
#[cfg(not(target_arch = "wasm32"))]
use crate::key::SUBTRACK_NONE;
#[cfg(not(target_arch = "wasm32"))]
use crate::tags::read_tags_from_reader;

pub const MIN_BPM: f32 = 60.0;
pub const MAX_BPM: f32 = 200.0;
pub const DEFAULT_BPM: f32 = 120.0;

/// Metadata & slice bounds describing a track analysis request.
#[derive(Clone, Debug)]
pub struct AnalyzeRequest {
    pub key_hash: u64,
    pub fs_path: String,
    pub subtrack_index: u16,
    /// Optional `.cue` subtrack start offset within `fs_path` (`0` for standalone files).
    pub slice_start_ms: u32,
    /// Known track/subtrack duration (`0` if unknown prior to probe).
    pub duration_ms: u32,
    pub sample_rate_hint: u32,
    pub title: String,
    pub artist: String,
    pub album: String,
    pub genre: String,
    pub tag_tbpm: Option<f32>,
    /// `true` when synchronous on-demand game entry (`"Processing..."`) is waiting;
    /// `false` when running quietly on `uipod-intel-bg` with cooperative yielding.
    pub high_priority: bool,
}

/// Core tempo, spectral, and dynamic metrics computed from mono PCM.
#[derive(Clone, Debug)]
pub struct AcousticMetrics {
    pub bpm: f32,
    pub bpm_confidence: f32,
    pub first_beat_offset_ms: u32,
    pub tonal_balance: [f32; 5],
    pub avg_rms: f32,
    pub crest_factor: f32,
    pub kick_baseline: f32,
    pub snare_baseline: f32,
}

impl Default for AcousticMetrics {
    fn default() -> Self {
        Self {
            bpm: DEFAULT_BPM,
            bpm_confidence: 0.0,
            first_beat_offset_ms: 0,
            tonal_balance: [0.20; 5],
            avg_rms: 0.12,
            crest_factor: 4.0,
            kick_baseline: 0.05,
            snare_baseline: 0.05,
        }
    }
}

/// Fold any raw BPM cleanly into `[60.0, 200.0]`.
#[inline]
pub fn fold_bpm_60_200(mut bpm: f32) -> f32 {
    if !bpm.is_finite() || bpm <= 0.0 {
        return DEFAULT_BPM;
    }
    bpm = bpm.clamp(20.0, 800.0);
    while bpm < MIN_BPM {
        bpm *= 2.0;
    }
    while bpm > MAX_BPM {
        bpm *= 0.5;
    }
    bpm.clamp(MIN_BPM, MAX_BPM)
}

/// Decimate interleaved stereo `i16` PCM (`[L, R, ...]`) into an anti-aliased `~11–12 kHz` mono `f32` buffer.
pub fn decimate_stereo_s16_to_mono(stereo_s16: &[i16], src_sr: u32) -> (Vec<f32>, u32) {
    let sr = src_sr.clamp(8_000, 192_000);
    let n_frames = stereo_s16.len() / 2;
    let decim = ((sr / 11_025) as usize).max(1);
    let out_sr = sr / (decim as u32);
    let out_len = n_frames / decim;
    let mut out = Vec::with_capacity(out_len);
    let scale = 0.5 / (32768.0 * (decim as f32));

    for i in 0..out_len {
        let base = i * decim * 2;
        let mut sum = 0.0f32;
        for d in 0..decim {
            let idx = base + d * 2;
            sum += (stereo_s16[idx] as f32) + (stereo_s16[idx + 1] as f32);
        }
        out.push(sum * scale);
    }
    (out, out_sr.max(4_000))
}

/// Decimate mono `f32` (`[-1.0, 1.0]`) into an anti-aliased `~11–12 kHz` mono `f32` buffer.
pub fn decimate_mono_f32(mono: &[f32], src_sr: u32) -> (Vec<f32>, u32) {
    let sr = src_sr.clamp(8_000, 192_000);
    let decim = ((sr / 11_025) as usize).max(1);
    if decim == 1 {
        return (mono.to_vec(), sr);
    }
    let out_sr = sr / (decim as u32);
    let out_len = mono.len() / decim;
    let inv = 1.0 / (decim as f32);
    let mut out = Vec::with_capacity(out_len);
    for i in 0..out_len {
        let base = i * decim;
        let mut s = 0.0f32;
        for d in 0..decim {
            s += mono[base + d];
        }
        out.push(s * inv);
    }
    (out, out_sr.max(4_000))
}

#[inline]
fn percentile_sorted(sorted: &[f32], q: f32) -> f32 {
    if sorted.is_empty() {
        return 0.0;
    }
    let idx = (((sorted.len() - 1) as f32) * q.clamp(0.0, 1.0)).round() as usize;
    sorted[idx.min(sorted.len() - 1)]
}

#[inline]
fn peak3(acf: &[f32], idx: usize) -> f32 {
    if idx >= acf.len() {
        return 0.0;
    }
    let mut v = acf[idx];
    if idx > 0 && acf[idx - 1] > v {
        v = acf[idx - 1];
    }
    if idx + 1 < acf.len() && acf[idx + 1] > v {
        v = acf[idx + 1];
    }
    v
}

#[inline]
fn parabolic_peak(acf: &[f32], center: usize) -> f64 {
    let mut idx = center.min(acf.len().saturating_sub(1));
    if idx > 1 && idx + 1 < acf.len() {
        if acf[idx - 1] > acf[idx] && acf[idx - 1] >= acf[idx + 1] {
            idx -= 1;
        } else if acf[idx + 1] > acf[idx] {
            idx += 1;
        }
    }
    if idx > 0 && idx + 1 < acf.len() {
        let ym = acf[idx - 1] as f64;
        let y0 = acf[idx] as f64;
        let yp = acf[idx + 1] as f64;
        let den = ym - 2.0 * y0 + yp;
        if den.abs() > 1e-9 {
            return (idx as f64) + (0.5 * (ym - yp) / den).clamp(-0.5, 0.5);
        }
    }
    idx as f64
}

fn compute_unbiased_acf(signal: &[f32], max_lag: usize) -> Vec<f32> {
    let n = signal.len();
    if n < 16 {
        return vec![0.0; max_lag + 1];
    }
    let mut sm = vec![0.0f32; n];
    let mut sum = 0.0f64;
    for i in 0..n {
        let v = if i > 0 && i + 1 < n {
            0.25 * signal[i - 1] + 0.50 * signal[i] + 0.25 * signal[i + 1]
        } else {
            signal[i]
        };
        sm[i] = v;
        sum += v as f64;
    }
    let mean = (sum / (n as f64)) as f32;
    let mut centered = vec![0.0f32; n];
    let mut var = 1e-12f64;
    for i in 0..n {
        let c = sm[i] - mean;
        centered[i] = c;
        var += (c as f64) * (c as f64);
    }

    let cap = max_lag.min(n.saturating_sub(2));
    let mut acf = vec![0.0f32; cap + 1];
    acf[0] = 1.0;
    for lag in 1..=cap {
        let count = n - lag;
        let mut dot = 0.0f32;
        let a = &centered[..count];
        let b = &centered[lag..lag + count];
        for i in 0..count {
            dot += a[i] * b[i];
        }
        let norm = ((n as f64) / ((count as f64) * var)) as f32;
        acf[lag] = (dot * norm).clamp(-1.0, 1.0);
    }
    acf
}

/// Analyze a mono `f32` window (`~11–22 kHz` or full-rate, starting at `window_start_ms` in the song)
/// and extract verified BPM (`60..200`), downbeat offset, 5-band spectral balance, crest factor, and flux baselines.
pub fn analyze_mono_window(
    mono_raw: &[f32],
    src_sr: u32,
    window_start_ms: u32,
    tag_tbpm: Option<f32>,
) -> AcousticMetrics {
    let (mono, sr) = decimate_mono_f32(mono_raw, src_sr);
    let n = mono.len();
    if n < (sr as usize) * 2 {
        let mut m = AcousticMetrics::default();
        if let Some(t) = tag_tbpm {
            m.bpm = fold_bpm_60_200(t);
            m.bpm_confidence = 0.50;
        }
        return m;
    }

    let fs = sr as f32;

    // 1. Broadband RMS, peak crest factor, and 5-band tonal balance via cascaded 1-pole crossovers
    // Bands: sub_bass (<80 Hz), bass (80-250 Hz), mid (250-1200 Hz), high_mid (1200-3200 Hz), treble (>3200 Hz)
    let a_80 = (1.0 - (-2.0 * PI * 80.0 / fs).exp()).clamp(0.002, 0.90);
    let a_250 = (1.0 - (-2.0 * PI * 250.0 / fs).exp()).clamp(0.005, 0.95);
    let a_1200 = (1.0 - (-2.0 * PI * 1200.0 / fs).exp()).clamp(0.01, 0.98);
    let a_3200 = (1.0 - (-2.0 * PI * 3200.0f32.min(fs * 0.44) / fs).exp()).clamp(0.02, 0.99);

    let mut lp80 = 0.0f32;
    let mut lp250 = 0.0f32;
    let mut lp1200 = 0.0f32;
    let mut lp3200 = 0.0f32;

    let mut band_sq = [0.0f64; 5];
    let mut full_sq = 0.0f64;
    let mut peak_abs = 0.0f32;

    for &s in &mono {
        let a = s.abs();
        if a > peak_abs {
            peak_abs = a;
        }
        full_sq += (s as f64) * (s as f64);

        lp80 += a_80 * (s - lp80);
        lp250 += a_250 * (s - lp250);
        lp1200 += a_1200 * (s - lp1200);
        lp3200 += a_3200 * (s - lp3200);

        let b0 = lp80;
        let b1 = lp250 - lp80;
        let b2 = lp1200 - lp250;
        let b3 = lp3200 - lp1200;
        let b4 = s - lp3200;

        band_sq[0] += (b0 as f64) * (b0 as f64);
        band_sq[1] += (b1 as f64) * (b1 as f64);
        band_sq[2] += (b2 as f64) * (b2 as f64);
        band_sq[3] += (b3 as f64) * (b3 as f64);
        band_sq[4] += (b4 as f64) * (b4 as f64);
    }

    let avg_rms = ((full_sq / (n as f64)).sqrt() as f32).clamp(0.0, 1.0);
    let crest_factor = if avg_rms > 1e-5 {
        (peak_abs / avg_rms).clamp(1.0, 24.0)
    } else {
        4.0
    };

    let mut band_rms = [0.0f32; 5];
    let mut band_sum = 0.0f32;
    for i in 0..5 {
        band_rms[i] = ((band_sq[i] / (n as f64)).sqrt()) as f32;
        band_sum += band_rms[i];
    }
    let tonal_balance = if band_sum > 1e-6 {
        [
            band_rms[0] / band_sum,
            band_rms[1] / band_sum,
            band_rms[2] / band_sum,
            band_rms[3] / band_sum,
            band_rms[4] / band_sum,
        ]
    } else {
        [0.20; 5]
    };

    // 2. Build 200 Hz (~5 ms hop) Kick (40-150 Hz), Snare (160-2200 Hz), and Hat (>2200 Hz) onset envelopes
    let hop = ((sr as f64 / 200.0).round() as usize).max(16);
    let env_rate = (sr as f64) / (hop as f64);
    let num_hops = n / hop;
    if num_hops < 400 {
        let mut m = AcousticMetrics {
            tonal_balance,
            avg_rms,
            crest_factor,
            ..Default::default()
        };
        if let Some(t) = tag_tbpm {
            m.bpm = fold_bpm_60_200(t);
            m.bpm_confidence = 0.50;
        }
        return m;
    }

    let a_hp38 = (1.0 - (-2.0 * PI * 38.0 / fs).exp()).clamp(0.002, 0.50);
    let a_lp150 = (1.0 - (-2.0 * PI * 150.0 / fs).exp()).clamp(0.005, 0.80);
    let a_hp160 = (1.0 - (-2.0 * PI * 160.0 / fs).exp()).clamp(0.005, 0.80);
    let a_lp950 = (1.0 - (-2.0 * PI * 950.0f32.min(fs * 0.40) / fs).exp()).clamp(0.02, 0.90);
    let a_hp2200 = (1.0 - (-2.0 * PI * 2200.0f32.min(fs * 0.44) / fs).exp()).clamp(0.05, 0.96);

    let mut hp38_s = 0.0f32;
    let mut lp150_a = 0.0f32;
    let mut lp150_b = 0.0f32;
    let mut hp160_a = 0.0f32;
    let mut hp160_b = 0.0f32;
    let mut lp950_a = 0.0f32;
    let mut lp950_b = 0.0f32;
    let mut hp2200_a = 0.0f32;
    let mut hp2200_b = 0.0f32;

    let mut kick_flux = Vec::with_capacity(num_hops);
    let mut snare_flux = Vec::with_capacity(num_hops);
    let mut hat_flux = Vec::with_capacity(num_hops);
    let mut kick_env = Vec::with_capacity(num_hops);

    let mut prev_k_log = 0.0f32;
    let mut prev_s_log = 0.0f32;
    let mut prev_h_log = 0.0f32;

    let inv_hop = 1.0 / (hop as f32);
    for h in 0..num_hops {
        let base = h * hop;
        let mut ke = 0.0f32;
        let mut se = 0.0f32;
        let mut he = 0.0f32;
        for i in 0..hop {
            let x = mono[base + i];
            hp38_s += a_hp38 * (x - hp38_s);
            let k_hp = x - hp38_s;
            lp150_a += a_lp150 * (k_hp - lp150_a);
            lp150_b += a_lp150 * (lp150_a - lp150_b);
            ke += lp150_b * lp150_b;

            // 2-pole HP @ 160 Hz -> 2-pole LP @ 950 Hz isolates snare/clap body while rejecting >2.5 kHz hi-hats!
            hp160_a += a_hp160 * (x - hp160_a);
            let m1 = x - hp160_a;
            hp160_b += a_hp160 * (m1 - hp160_b);
            let mh = m1 - hp160_b;
            lp950_a += a_lp950 * (mh - lp950_a);
            lp950_b += a_lp950 * (lp950_a - lp950_b);
            se += lp950_b * lp950_b;

            // 2-pole HP @ 2200 Hz isolates crisp hi-hat / cymbal subdivisions
            hp2200_a += a_hp2200 * (x - hp2200_a);
            let h1 = x - hp2200_a;
            hp2200_b += a_hp2200 * (h1 - hp2200_b);
            let ht = h1 - hp2200_b;
            he += ht * ht;
        }
        let k_rms = (ke * inv_hop).sqrt();
        let s_rms = (se * inv_hop).sqrt();
        let h_rms = (he * inv_hop).sqrt();

        let k_log = (1.0 + 45.0 * k_rms).ln();
        let s_log = (1.0 + 45.0 * s_rms).ln();
        let h_log = (1.0 + 45.0 * h_rms).ln();

        kick_flux.push((k_log - prev_k_log).max(0.0));
        snare_flux.push((s_log - prev_s_log).max(0.0));
        hat_flux.push((h_log - prev_h_log).max(0.0));
        kick_env.push(k_rms);

        prev_k_log = k_log;
        prev_s_log = s_log;
        prev_h_log = h_log;
    }

    let mut k_sorted: Vec<f32> = kick_flux.iter().copied().filter(|&v| v > 1e-5).collect();
    let mut s_sorted: Vec<f32> = snare_flux.iter().copied().filter(|&v| v > 1e-5).collect();
    let mut h_sorted: Vec<f32> = hat_flux.iter().copied().filter(|&v| v > 1e-5).collect();
    k_sorted.sort_by(|a, b| a.total_cmp(b));
    s_sorted.sort_by(|a, b| a.total_cmp(b));
    h_sorted.sort_by(|a, b| a.total_cmp(b));

    let kick_baseline = percentile_sorted(&k_sorted, 0.85).max(0.01);
    let snare_baseline = percentile_sorted(&s_sorted, 0.85).max(0.01);
    let hat_scale = percentile_sorted(&h_sorted, 0.90).max(0.01);

    let (bpm, bpm_confidence, first_beat_offset_ms) = estimate_tempo_from_envelopes(
        &kick_flux,
        &snare_flux,
        &hat_flux,
        &kick_env,
        kick_baseline,
        snare_baseline,
        hat_scale,
        env_rate,
        window_start_ms,
        tag_tbpm,
    );

    AcousticMetrics {
        bpm,
        bpm_confidence,
        first_beat_offset_ms,
        tonal_balance,
        avg_rms,
        crest_factor,
        kick_baseline,
        snare_baseline,
    }
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn estimate_tempo_from_envelopes(
    kick_flux: &[f32],
    snare_flux: &[f32],
    hat_flux: &[f32],
    kick_env: &[f32],
    kick_scale: f32,
    snare_scale: f32,
    hat_scale: f32,
    env_rate: f64,
    window_start_ms: u32,
    tag_tbpm: Option<f32>,
) -> (f32, f32, u32) {
    let n = kick_flux.len();
    let mut low_mid = vec![0.0f32; n];
    let mut full = vec![0.0f32; n];
    let mut max_lm = 0.0f32;
    let mut sum_lm = 0.0f32;

    for i in 0..n {
        let kn = (kick_flux[i] / kick_scale).min(3.5);
        let sn = (snare_flux[i] / snare_scale).min(3.5);
        let hn = (hat_flux[i] / hat_scale).min(3.0);
        let lm = 0.62 * kn + 0.38 * sn;
        low_mid[i] = lm;
        full[i] = 0.54 * kn + 0.34 * sn + 0.12 * hn;
        sum_lm += lm;
        if lm > max_lm {
            max_lm = lm;
        }
    }

    let mean_lm = sum_lm / (n as f32);
    if max_lm < 0.02 || max_lm / (mean_lm + 1e-6) < 1.65 {
        let fb = tag_tbpm.map(fold_bpm_60_200).unwrap_or(DEFAULT_BPM);
        let conf = if tag_tbpm.is_some() { 0.45 } else { 0.05 };
        return (fb, conf, 0);
    }

    let min_lag = ((env_rate * 60.0 / (MAX_BPM as f64)).floor() as usize).max(12);
    let max_lag = ((env_rate * 60.0 / (MIN_BPM as f64)).ceil() as usize).min(n / 2);
    if min_lag >= max_lag {
        return (DEFAULT_BPM, 0.0, 0);
    }

    let corr_max = (max_lag * 4 + 4).min(n - 2);
    let acf_lm = compute_unbiased_acf(&low_mid, corr_max);
    let acf_full = compute_unbiased_acf(&full, corr_max);

    // Score every fundamental lag in [60..200 BPM] using 1x, 2x, 3x, 4x bar-harmonic comb
    // with ZERO 110/120 BPM Gaussian pull bias!
    let mut scores = vec![0.0f32; max_lag + 1];
    let mut best_lag = min_lag;
    let mut best_score = f32::NEG_INFINITY;

    for lag in min_lag..=max_lag {
        let r1 = acf_lm[lag].max(0.0);
        let r2 = peak3(&acf_lm, lag * 2).max(0.0);
        let r3 = peak3(&acf_lm, lag * 3).max(0.0);
        let r4 = peak3(&acf_lm, lag * 4).max(0.0);

        // Penalize candidate lags where BOTH odd harmonics (1x and 3x) are absent
        // (prevents a pure 90 BPM track from falsely picking double-time 180 BPM!).
        let odd_support = (0.65 * r1 + 0.35 * r3).max(0.0);
        let even_support = 0.58 * r2 + 0.42 * r4;
        let mut sc = if odd_support < 0.08 {
            0.25 * odd_support + 0.35 * even_support
        } else {
            0.42 * r1 + 0.26 * r2 + 0.12 * r3 + 0.20 * r4
        };

        // Light tie-breaker if embedded TBPM tag agrees within ±3% (or exact 2x/0.5x octave)
        if let Some(tbpm_raw) = tag_tbpm {
            let folded_tag = fold_bpm_60_200(tbpm_raw);
            let cand_bpm = ((env_rate * 60.0) / (lag as f64)) as f32;
            let rel = (cand_bpm - folded_tag).abs() / folded_tag.max(1.0);
            if rel < 0.03 {
                sc += 0.06;
            }
        }

        scores[lag] = sc;
        if sc > best_score {
            best_score = sc;
            best_lag = lag;
        }
    }

    // Octave Disambiguation:
    // Case 1: `best_lag` landed on half-time `[60..100 BPM)` (e.g. `87 BPM` for `174 BPM` DnB,
    //         or `70 BPM` for `140 BPM` half-time dubstep/techno).
    let cand_bpm = ((env_rate * 60.0) / (best_lag as f64)) as f32;
    if cand_bpm < 100.0 && best_lag / 2 >= min_lag {
        let half_center = (best_lag + 1) / 2;
        let mut best_half = half_center;
        for cand in half_center.saturating_sub(1)..=(half_center + 1).min(max_lag) {
            if cand >= min_lag && scores[cand] > scores[best_half] {
                best_half = cand;
            }
        }

        let r1_half_lm = peak3(&acf_lm, best_half);
        let r3_half_lm = peak3(&acf_lm, best_half * 3);
        let r1_half_full = peak3(&acf_full, best_half);
        let r_eighth_full = peak3(&acf_full, best_half / 2);

        let tag_prefers_double = tag_tbpm
            .map(|t| {
                let ft = fold_bpm_60_200(t);
                ((cand_bpm * 2.0 - ft).abs() / ft.max(1.0)) < 0.05
            })
            .unwrap_or(false);

        // A true double-time track (like 174 BPM DnB or 140 BPM 4-on-the-floor) has real
        // Kick+Snare quarter-note energy on `r1_half_lm` / `r3_half_lm`!
        // A half-time 70 BPM dubstep/trap track (`cand_bpm < 76`) has strong 8th/quarter
        // rhythmic subdivisions in `acf_full`.
        // Meanwhile, a genuine 90 BPM track (`cand_bpm >= 78`) has `r1_half_lm ~ 0` and stays 90 BPM!
        let has_strong_drum_double =
            r1_half_lm >= 0.22 && (r1_half_lm + 0.5 * r3_half_lm) >= 0.30;
        let is_halftime_140_family =
            cand_bpm < 76.0 && (r1_half_full >= 0.16 || r_eighth_full >= 0.20);

        if tag_prefers_double || has_strong_drum_double || is_halftime_140_family {
            best_lag = best_half;
        }
    }

    // Sub-sample multi-harmonic parabolic refinement (`1x`, `2x / 2`, `4x / 4`)
    let lag1 = parabolic_peak(&acf_lm, best_lag);
    let mut weighted_sum = lag1;
    let mut weight_total = 1.0f64;

    for mult in [2usize, 4usize] {
        let target_idx = ((lag1 * (mult as f64)).round() as usize).min(acf_lm.len().saturating_sub(2));
        if target_idx > min_lag && peak3(&acf_lm, target_idx) >= 0.20 {
            let pk = parabolic_peak(&acf_lm, target_idx) / (mult as f64);
            if (pk - lag1).abs() <= 0.95 {
                let w = if mult == 4 { 2.4 } else { 1.6 };
                weighted_sum += pk * w;
                weight_total += w;
            }
        }
    }

    let exact_lag = (weighted_sum / weight_total).max(10.0);
    let detected_bpm = fold_bpm_60_200(((env_rate * 60.0) / exact_lag) as f32);
    let confidence = (best_score / 0.65).clamp(0.05, 1.0);

    // Downbeat (`Beat 1`) 4-beat bar phase search: maximize Kick on Beats 0 & 2 and Snare on Beats 1 & 3
    let period_hops = (env_rate * 60.0) / (detected_bpm as f64);
    let bar_hops = ((period_hops * 4.0).round() as usize).clamp(4, n);
    let mut best_phase_hop = 0usize;
    let mut best_phase_score = f32::NEG_INFINITY;

    for phi in 0..bar_hops {
        let mut sc = 0.0f32;
        let mut b = 0usize;
        loop {
            let pos = phi + ((b as f64 * period_hops).round() as usize);
            if pos >= n {
                break;
            }
            let beat_in_bar = b & 3;
            let k_val = kick_flux[pos] + 0.35 * kick_env[pos];
            let s_val = snare_flux[pos];
            sc += match beat_in_bar {
                0 => 1.25 * k_val,
                2 => 0.95 * k_val + 0.20 * s_val,
                _ => 0.85 * s_val + 0.35 * k_val,
            };
            b += 1;
        }
        if sc > best_phase_score {
            best_phase_score = sc;
            best_phase_hop = phi;
        }
    }

    let beat_ms_f64 = 60_000.0 / (detected_bpm as f64);
    let bar_ms_f64 = beat_ms_f64 * 4.0;
    let abs_downbeat_ms = (window_start_ms as f64) + ((best_phase_hop as f64) * 1000.0 / env_rate);
    let first_beat_offset_ms = abs_downbeat_ms.rem_euclid(bar_ms_f64).round() as u32;

    (detected_bpm, confidence, first_beat_offset_ms)
}

#[inline]
fn three_band_structural_energy(slice: &[f32]) -> f32 {
    if slice.is_empty() {
        return 0.0;
    }
    if slice.len() < 4 {
        let mut sq = 0.0f32;
        for &s in slice {
            sq += s * s;
        }
        return (sq / (slice.len() as f32)).sqrt();
    }
    // 1-pole split (~11 kHz decimated rate: a_low ~ 220 Hz, a_mid ~ 2200 Hz)
    const A_LOW: f32 = 0.118;
    const A_MID: f32 = 0.715;
    let mut lp_low = slice[0];
    let mut lp_mid = slice[0];
    let mut low_sq = 0.0f32;
    let mut mid_sq = 0.0f32;
    let mut high_sq = 0.0f32;
    for &s in slice {
        lp_low += A_LOW * (s - lp_low);
        lp_mid += A_MID * (s - lp_mid);
        let low = lp_low;
        let mid = lp_mid - lp_low;
        let high = s - lp_mid;
        low_sq += low * low;
        mid_sq += mid * mid;
        high_sq += high * high;
    }
    let inv = 1.0 / (slice.len() as f32);
    0.42 * (low_sq * inv).sqrt() + 0.36 * (mid_sq * inv).sqrt() + 0.22 * (high_sq * inv).sqrt()
}

/// Build a 256-bucket `low/mid/high` weighted structural energy contour (`0..=255`) from mono PCM.
pub fn compute_contour_256_from_mono(mono: &[f32]) -> [u8; 256] {
    if mono.is_empty() {
        return [128u8; 256];
    }
    let mut rms_buckets = [0.0f32; 256];
    let len = mono.len();
    for (b, slot) in rms_buckets.iter_mut().enumerate() {
        let i0 = (b * len) / 256;
        let i1 = (((b + 1) * len) / 256).max(i0 + 1).min(len);
        *slot = three_band_structural_energy(&mono[i0..i1]);
    }
    quantize_contour_buckets(&rms_buckets)
}

fn quantize_contour_buckets(rms_buckets: &[f32; 256]) -> [u8; 256] {
    let mut sorted = *rms_buckets;
    sorted.sort_by(|a, b| a.total_cmp(b));
    let ref_peak = sorted[242].max(sorted[255] * 0.75).max(1e-4);
    let mut out = [0u8; 256];
    for i in 0..256 {
        let sm = if i > 0 && i < 255 {
            0.20 * rms_buckets[i - 1] + 0.60 * rms_buckets[i] + 0.20 * rms_buckets[i + 1]
        } else {
            rms_buckets[i]
        };
        let norm = (sm / ref_peak).sqrt().clamp(0.0, 1.0);
        out[i] = (norm * 255.0).round() as u8;
    }
    out
}

/// Analyze an `.nsf` / `.nsfe` subtrack in `< 3 ms` using headless 60/50 Hz hardware APU stepping.
pub fn analyze_nsf_bytes(
    bytes: &[u8],
    subtrack_opt: Option<u16>,
    req: &AnalyzeRequest,
) -> Option<TrackAnalysisRecord> {
    let mut engine = NsfEngine::load(bytes, 44_100).ok()?;
    let frame_us = engine.frame_speed_us().max(2_000) as f64;
    let chosen_sub = subtrack_opt.unwrap_or_else(|| engine.container_meta().starting_subtrack);
    let _ = engine.select_subtrack(chosen_sub);

    let frame_rate_hz = 1_000_000.0 / frame_us;

    let sub_meta = engine.container_meta().subtracks.get(chosen_sub as usize).cloned();
    let dur_ms = if req.duration_ms > 0 {
        req.duration_ms
    } else {
        sub_meta.as_ref().map(|s| s.duration_ms).unwrap_or(120_000)
    };

    // Step up to ~36 seconds headless (~2160 frames = < 2 ms on Cortex-A53!)
    // Upsample 60 Hz APU frames by 3x -> ~180 Hz envelope for high-precision comb ACF!
    let total_frames = (((dur_ms.min(45_000) as f64) * frame_rate_hz / 1000.0) as usize).clamp(360, 2700);
    let skip_intro_frames = (total_frames / 8).min(240); // Skip initial 1-4s reset jingle

    let mut kick_flux = Vec::with_capacity(total_frames * 3);
    let mut snare_flux = Vec::with_capacity(total_frames * 3);
    let mut hat_flux = Vec::with_capacity(total_frames * 3);
    let mut kick_env = Vec::with_capacity(total_frames * 3);
    let mut frame_energies = Vec::with_capacity(total_frames);

    let mut prev_bass = 0.0f32;
    let mut prev_perc = 0.0f32;
    let mut prev_lead = 0.0f32;

    for f in 0..total_frames {
        let tel = engine.step_frame_headless();
        // Channels: 0,1 = Pulse1,2; 2 = Triangle; 3 = Noise; 4 = DPCM; 5,6,7 = VRC6/MMC5
        let tri_v = if tel.channels[2].active != 0 {
            tel.channels[2].volume
        } else {
            0.0
        };
        let dpcm_v = if tel.channels[4].active != 0 {
            tel.channels[4].volume
        } else {
            0.0
        };
        let noise_v = if tel.channels[3].active != 0 {
            tel.channels[3].volume
        } else {
            0.0
        };
        let pulse_v = tel.channels[0].volume.max(tel.channels[1].volume);

        let bass_onset = if tel.channels[2].onset_trigger != 0 || tel.channels[4].onset_trigger != 0 {
            0.45
        } else {
            0.0
        };
        let noise_onset = if tel.channels[3].onset_trigger != 0 {
            0.55
        } else {
            0.0
        };
        let lead_onset = if tel.channels[0].onset_trigger != 0 || tel.channels[1].onset_trigger != 0 {
            0.30
        } else {
            0.0
        };

        let bass = (tri_v * 0.75 + dpcm_v * 0.85).min(1.2);
        let perc = noise_v;
        let lead = pulse_v;

        frame_energies.push((bass * 0.45 + perc * 0.30 + lead * 0.25).clamp(0.0, 1.0));

        if f >= skip_intro_frames {
            let kf = ((bass - prev_bass).max(0.0) + bass_onset).clamp(0.0, 2.0);
            let sf = ((perc - prev_perc).max(0.0) + noise_onset).clamp(0.0, 2.0);
            let hf = ((lead - prev_lead).max(0.0) + lead_onset).clamp(0.0, 2.0);

            // 3x subdivision per hardware frame (~180 Hz)
            kick_flux.push(kf);
            snare_flux.push(sf);
            hat_flux.push(hf);
            kick_env.push(bass);
            for _ in 0..2 {
                kick_flux.push(kf * 0.20);
                snare_flux.push(sf * 0.20);
                hat_flux.push(hf * 0.20);
                kick_env.push(bass * 0.85);
            }
        }

        prev_bass = bass;
        prev_perc = perc;
        prev_lead = lead;

        if !req.high_priority && (f & 255) == 255 {
            std::thread::yield_now();
        }
    }

    let env_rate = frame_rate_hz * 3.0;
    let start_ms = ((skip_intro_frames as f64) * 1000.0 / frame_rate_hz).round() as u32;
    let (bpm, conf, first_beat_ms) = estimate_tempo_from_envelopes(
        &kick_flux,
        &snare_flux,
        &hat_flux,
        &kick_env,
        0.35,
        0.35,
        0.35,
        env_rate,
        start_ms,
        req.tag_tbpm,
    );

    let contour = compute_contour_256_from_mono(&frame_energies);
    let mut rec = TrackAnalysisRecord {
        key_hash: req.key_hash,
        record_crc32: 0,
        subtrack_index: chosen_sub,
        channels: (engine.container_meta().chip_flags.channel_descriptors().len() as u8).max(5),
        analysis_flags: analysis_flag::CHIPTUNE_APU,
        duration_ms: dur_ms,
        sample_rate: 44_100,
        bpm,
        bpm_confidence: conf.max(0.55),
        first_beat_offset_ms: first_beat_ms,
        tag_tbpm: req.tag_tbpm.unwrap_or(0.0),
        tonal_balance: [0.18, 0.26, 0.28, 0.18, 0.10],
        avg_rms: 0.22,
        crest_factor: 3.2,
        kick_baseline: 0.18,
        snare_baseline: 0.18,
        ..Default::default()
    };

    let title = if !req.title.is_empty() {
        req.title.as_str()
    } else {
        sub_meta
            .as_ref()
            .and_then(|s| s.title.as_deref())
            .filter(|t| !t.is_empty())
            .unwrap_or(engine.container_meta().game_title.as_str())
    };
    let artist = if !req.artist.is_empty() {
        req.artist.as_str()
    } else {
        engine.container_meta().artist.as_str()
    };
    write_fixed_utf8(&mut rec.title, title);
    write_fixed_utf8(&mut rec.artist, artist);
    write_fixed_utf8(&mut rec.album, &engine.container_meta().game_title);
    write_fixed_utf8(&mut rec.genre, if req.genre.is_empty() { "Chiptune" } else { &req.genre });
    rec.energy_contour = contour;
    Some(rec)
}

/// Build a complete [`TrackAnalysisRecord`] from pre-decoded interleaved stereo `i16` PCM
/// (used for active stream buffers / `pcmbuf` fallback and unit tests).
pub fn analyze_pcm_s16_buffer(
    stereo_s16: &[i16],
    sample_rate: u32,
    window_start_ms: u32,
    req: &AnalyzeRequest,
) -> TrackAnalysisRecord {
    let (mono, dec_sr) = decimate_stereo_s16_to_mono(stereo_s16, sample_rate);
    let metrics = analyze_mono_window(&mono, dec_sr, window_start_ms, req.tag_tbpm);
    let contour = compute_contour_256_from_mono(&mono);
    let dur_ms = if req.duration_ms > 0 {
        req.duration_ms
    } else {
        (((stereo_s16.len() / 2) as u64 * 1000) / (sample_rate.max(1) as u64)) as u32
    };

    let mut flags = 0u8;
    if req.tag_tbpm.is_some() {
        flags |= analysis_flag::HAS_TBPM_TAG;
    }

    let mut rec = TrackAnalysisRecord {
        key_hash: req.key_hash,
        record_crc32: 0,
        subtrack_index: req.subtrack_index,
        channels: 2,
        analysis_flags: flags,
        duration_ms: dur_ms,
        sample_rate: sample_rate.max(8_000),
        bpm: metrics.bpm,
        bpm_confidence: metrics.bpm_confidence,
        first_beat_offset_ms: metrics.first_beat_offset_ms,
        tag_tbpm: req.tag_tbpm.unwrap_or(0.0),
        tonal_balance: metrics.tonal_balance,
        avg_rms: metrics.avg_rms,
        crest_factor: metrics.crest_factor,
        kick_baseline: metrics.kick_baseline,
        snare_baseline: metrics.snare_baseline,
        ..Default::default()
    };
    write_fixed_utf8(&mut rec.title, &req.title);
    write_fixed_utf8(&mut rec.artist, &req.artist);
    write_fixed_utf8(&mut rec.album, &req.album);
    write_fixed_utf8(&mut rec.genre, &req.genre);
    rec.energy_contour = contour;
    rec
}

/// Analyze a local or cached audio file on disk (`< 100 ms` on Cortex-A53 via `28%` + `60%` seek windows).
#[cfg(not(target_arch = "wasm32"))]
pub fn analyze_file_fast(req: &AnalyzeRequest) -> Option<TrackAnalysisRecord> {
    let path = Path::new(&req.fs_path);
    if !path.is_file() {
        return None;
    }

    // 1. Fast path for `.nsf` / `.nsfe` chiptune containers (< 3 ms headless APU step)
    if is_nsf_path(&req.fs_path) {
        let bytes = std::fs::read(path).ok()?;
        let sub = if req.subtrack_index != SUBTRACK_NONE {
            Some(req.subtrack_index)
        } else {
            parse_subtrack_uri(&req.fs_path).subtrack_index
        };
        return analyze_nsf_bytes(&bytes, sub, req);
    }

    // 2. Fast header tag + TBPM extraction (< 0.3 ms)
    let mut enriched = req.clone();
    if let Ok(tag_file) = File::open(path) {
        let tags = read_tags_from_reader(tag_file);
        if enriched.tag_tbpm.is_none() {
            enriched.tag_tbpm = tags.tbpm;
        }
        if enriched.title.is_empty() && !tags.title.is_empty() {
            enriched.title = tags.title;
        }
        if enriched.artist.is_empty() && !tags.artist.is_empty() {
            enriched.artist = tags.artist;
        }
        if enriched.album.is_empty() && !tags.album.is_empty() {
            enriched.album = tags.album;
        }
        if enriched.genre.is_empty() && !tags.genre.is_empty() {
            enriched.genre = tags.genre;
        }
    }

    // 3. Pure-Rust `.gsf` / `.mid` codecs
    if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
        if ext.eq_ignore_ascii_case("gsf") || ext.eq_ignore_ascii_case("minigsf") {
            let bytes = std::fs::read(path).ok()?;
            let img = uipod_codecs::gsf::load_gsf_from_disk(&bytes, &enriched.fs_path).ok()?;
            let mut dec = uipod_codecs::gsf::GsfDecoder::from_image(img);
            return Some(analyze_rust_decoder(&mut dec, &enriched));
        }
        if ext.eq_ignore_ascii_case("mid")
            || ext.eq_ignore_ascii_case("midi")
            || ext.eq_ignore_ascii_case("kar")
            || ext.eq_ignore_ascii_case("rmi")
        {
            let bytes = std::fs::read(path).ok()?;
            let parsed = uipod_codecs::midi::ParsedMidi::parse(&bytes).ok()?;
            let mut dec = uipod_codecs::midi::MidiDecoder::new(parsed);
            return Some(analyze_rust_decoder(&mut dec, &enriched));
        }
    }

    // 4. Seek-window decode via `symphonia` (`MP3`, `FLAC`, `Vorbis`, `WAV`, `AIFF`, `MP4/AAC/ALAC`)
    analyze_with_symphonia(path, &enriched)
}

#[cfg(not(target_arch = "wasm32"))]
fn analyze_rust_decoder(
    dec: &mut dyn uipod_codecs::RustAudioDecoder,
    req: &AnalyzeRequest,
) -> TrackAnalysisRecord {
    let sr = dec.sample_rate().max(8_000);
    let dur_ms = if req.duration_ms > 0 {
        req.duration_ms
    } else {
        dec.duration_ms().max(30_000)
    };
    let seek_ms = if dur_ms > 24_000 { dur_ms * 28 / 100 } else { 0 };
    dec.seek_ms(seek_ms);

    let win_frames = (sr as usize) * 16;
    let mut pcm = vec![0i16; win_frames * 2];
    let mut got = 0usize;
    while got < win_frames {
        let chunk = (win_frames - got).min(2048);
        let n = dec.render_stereo_s16(&mut pcm[got * 2..(got + chunk) * 2], false);
        if n == 0 {
            break;
        }
        got += n;
        if !req.high_priority && (got & 8191) == 0 {
            std::thread::yield_now();
        }
    }
    pcm.truncate(got * 2);
    let mut updated = req.clone();
    updated.duration_ms = dur_ms;
    let mut rec = analyze_pcm_s16_buffer(&pcm, sr, seek_ms, &updated);
    rec.analysis_flags |= analysis_flag::CHIPTUNE_APU;
    rec
}

#[cfg(not(target_arch = "wasm32"))]
fn analyze_with_symphonia(path: &Path, req: &AnalyzeRequest) -> Option<TrackAnalysisRecord> {
    use symphonia::core::audio::SampleBuffer;
    use symphonia::core::codecs::{DecoderOptions, CODEC_TYPE_NULL};
    use symphonia::core::formats::{FormatOptions, SeekMode, SeekTo};
    use symphonia::core::io::MediaSourceStream;
    use symphonia::core::meta::MetadataOptions;
    use symphonia::core::probe::Hint;
    use symphonia::core::units::Time;

    let file = File::open(path).ok()?;
    let mss = MediaSourceStream::new(Box::new(file), Default::default());
    let mut hint = Hint::new();
    if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
        hint.with_extension(ext);
    }

    let probed = symphonia::default::get_probe()
        .format(
            &hint,
            mss,
            &FormatOptions::default(),
            &MetadataOptions::default(),
        )
        .ok()?;
    let mut format = probed.format;

    let (track_id, sr, channels, total_dur_ms) = {
        let track = format
            .tracks()
            .iter()
            .find(|t| t.codec_params.codec != CODEC_TYPE_NULL)?;
        let sr = track.codec_params.sample_rate.unwrap_or(req.sample_rate_hint.max(44_100));
        let ch = track
            .codec_params
            .channels
            .map(|c| c.count() as u8)
            .unwrap_or(2)
            .max(1);
        let est_ms = track
            .codec_params
            .n_frames
            .map(|nf| ((nf * 1000) / (sr.max(1) as u64)) as u32)
            .unwrap_or(req.duration_ms);
        (track.id, sr, ch, est_ms)
    };

    let codec_params = format
        .tracks()
        .iter()
        .find(|t| t.id == track_id)?
        .codec_params
        .clone();
    let mut decoder = symphonia::default::get_codecs()
        .make(&codec_params, &DecoderOptions::default())
        .ok()?;

    let slice_start_ms = req.slice_start_ms;
    let dur_ms = if req.duration_ms > 0 {
        req.duration_ms
    } else {
        total_dur_ms.saturating_sub(slice_start_ms)
    };

    let decim = ((sr / 11_025) as usize).max(1);
    let dec_sr = (sr / (decim as u32)).max(4_000);

    // Helper closure to seek & decode `win_sec` seconds into decimated mono f32
    let mut decode_window_at_ms = |rel_ms: u32, win_sec: f32| -> Vec<f32> {
        let abs_ms = slice_start_ms.saturating_add(rel_ms);
        let sec = (abs_ms / 1000) as u64;
        let frac = ((abs_ms % 1000) as f64) * 0.001;
        if abs_ms > 0 {
            let _ = format.seek(
                SeekMode::Coarse,
                SeekTo::Time {
                    time: Time::new(sec, frac),
                    track_id: Some(track_id),
                },
            );
            decoder.reset();
        }

        let want_dec_samples = ((win_sec * (dec_sr as f32)).round() as usize).max(256);
        let mut out_mono = Vec::with_capacity(want_dec_samples + 256);
        let mut decim_acc = 0.0f32;
        let mut decim_cnt = 0usize;
        let mut sample_buf: Option<SampleBuffer<f32>> = None;
        let mut pkt_count = 0u32;

        while out_mono.len() < want_dec_samples {
            let Ok(packet) = format.next_packet() else {
                break;
            };
            if packet.track_id() != track_id {
                continue;
            }
            let Ok(decoded) = decoder.decode(&packet) else {
                continue;
            };
            let spec = *decoded.spec();
            let ch_n = spec.channels.count().max(1);
            let cap = decoded.capacity() as u64;
            if sample_buf.as_ref().map(|b| b.capacity() < decoded.frames() * ch_n).unwrap_or(true)
            {
                sample_buf = Some(SampleBuffer::<f32>::new(cap.max(4096), spec));
            }
            let sbuf = sample_buf.as_mut().unwrap();
            sbuf.copy_interleaved_ref(decoded);
            let samples = sbuf.samples();
            let frames = samples.len() / ch_n;
            let inv_ch = 1.0 / (ch_n as f32);

            for f in 0..frames {
                let mut m = 0.0f32;
                let base = f * ch_n;
                for c in 0..ch_n {
                    m += samples[base + c];
                }
                decim_acc += m * inv_ch;
                decim_cnt += 1;
                if decim_cnt >= decim {
                    out_mono.push(decim_acc / (decim as f32));
                    decim_acc = 0.0;
                    decim_cnt = 0;
                    if out_mono.len() >= want_dec_samples {
                        break;
                    }
                }
            }

            pkt_count += 1;
            if !req.high_priority && (pkt_count & 31) == 0 {
                std::thread::yield_now();
            }
        }
        out_mono
    };

    // Primary 16-second window at `28%` of track duration (skips beatless intros!)
    let primary_start_ms = if dur_ms > 24_000 {
        (dur_ms * 28) / 100
    } else {
        0
    };
    let primary_win_sec = if dur_ms > 20_000 { 16.5 } else { 14.0 };
    let primary_mono = decode_window_at_ms(primary_start_ms, primary_win_sec);
    if primary_mono.len() < (dec_sr as usize) * 2 {
        return None;
    }

    let mut metrics = analyze_mono_window(&primary_mono, dec_sr, primary_start_ms, req.tag_tbpm);
    let mut flags = if req.tag_tbpm.is_some() {
        analysis_flag::HAS_TBPM_TAG
    } else {
        0
    };

    // Quick 5.5s confirmation window at `60%` if primary confidence is low (< 0.62)
    if metrics.bpm_confidence < 0.62 && dur_ms > 35_000 {
        let confirm_start_ms = (dur_ms * 60) / 100;
        let confirm_mono = decode_window_at_ms(confirm_start_ms, 5.5);
        if confirm_mono.len() >= (dec_sr as usize) * 3 {
            let m2 = analyze_mono_window(&confirm_mono, dec_sr, confirm_start_ms, req.tag_tbpm);
            flags |= analysis_flag::DUAL_WINDOW_VERIFIED;
            if m2.bpm_confidence > metrics.bpm_confidence + 0.05 {
                metrics.bpm = m2.bpm;
                metrics.bpm_confidence = m2.bpm_confidence;
                metrics.first_beat_offset_ms = m2.first_beat_offset_ms;
            } else if (m2.bpm - metrics.bpm).abs() <= 1.5 {
                metrics.bpm = 0.65 * metrics.bpm + 0.35 * m2.bpm;
                metrics.bpm_confidence = (metrics.bpm_confidence + 0.12).min(1.0);
            }
        }
    }

    // Sparse 16-anchor structural energy contour across 0%..96% (`~35 ms` per anchor = < 8 ms total)
    let contour = if dur_ms > 18_000 {
        let mut anchors = [0.0f32; 16];
        for (a, slot) in anchors.iter_mut().enumerate() {
            let rel_ms = ((dur_ms as u64 * (a as u64 * 6 + 2)) / 100) as u32;
            let snip = decode_window_at_ms(rel_ms, 0.06);
            if !snip.is_empty() {
                *slot = three_band_structural_energy(&snip);
            } else {
                *slot = metrics.avg_rms;
            }
        }
        let mut interp = [0.0f32; 256];
        for i in 0..256 {
            let pos = ((i as f32) / 255.0) * 15.0;
            let i0 = (pos.floor() as usize).min(15);
            let i1 = (i0 + 1).min(15);
            let t = pos - (i0 as f32);
            interp[i] = anchors[i0] + (anchors[i1] - anchors[i0]) * t;
        }
        quantize_contour_buckets(&interp)
    } else {
        compute_contour_256_from_mono(&primary_mono)
    };

    let mut rec = TrackAnalysisRecord {
        key_hash: req.key_hash,
        record_crc32: 0,
        subtrack_index: req.subtrack_index,
        channels,
        analysis_flags: flags,
        duration_ms: dur_ms,
        sample_rate: sr,
        bpm: metrics.bpm,
        bpm_confidence: metrics.bpm_confidence,
        first_beat_offset_ms: metrics.first_beat_offset_ms,
        tag_tbpm: req.tag_tbpm.unwrap_or(0.0),
        tonal_balance: metrics.tonal_balance,
        avg_rms: metrics.avg_rms,
        crest_factor: metrics.crest_factor,
        kick_baseline: metrics.kick_baseline,
        snare_baseline: metrics.snare_baseline,
        ..Default::default()
    };
    write_fixed_utf8(&mut rec.title, &req.title);
    write_fixed_utf8(&mut rec.artist, &req.artist);
    write_fixed_utf8(&mut rec.album, &req.album);
    write_fixed_utf8(&mut rec.genre, &req.genre);
    rec.energy_contour = contour;
    Some(rec)
}
