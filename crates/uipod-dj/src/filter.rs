//! Click-free per-deck 2nd-order Butterworth Cytomic TPT SVF High-Pass Filter
//! and tempo-synced stereo Ping-Pong Wash tail for DJ transitions.

use std::f32::consts::{FRAC_1_SQRT_2, PI, SQRT_2};

/// 1-pole exponential smoother (`tau_sec` time constant, default ~10 ms).
#[derive(Clone, Copy, Debug)]
pub struct OnePoleSmoother {
    current: f32,
    target: f32,
    alpha: f32,
}

impl OnePoleSmoother {
    pub fn new(initial: f32, sample_rate: u32, tau_sec: f32) -> Self {
        let fs = (sample_rate.max(8_000)) as f32;
        let alpha = 1.0 - (-1.0 / (tau_sec.max(0.0005) * fs)).exp();
        Self {
            current: initial,
            target: initial,
            alpha: alpha.clamp(0.00005, 1.0),
        }
    }

    #[inline]
    pub fn set_sample_rate(&mut self, sample_rate: u32, tau_sec: f32) {
        let fs = (sample_rate.max(8_000)) as f32;
        self.alpha = (1.0 - (-1.0 / (tau_sec.max(0.0005) * fs)).exp()).clamp(0.00005, 1.0);
    }

    #[inline]
    pub fn reset(&mut self, val: f32) {
        self.current = val;
        self.target = val;
    }

    #[inline]
    pub fn set_target(&mut self, target: f32) {
        self.target = target;
    }

    #[inline]
    pub fn current(&self) -> f32 {
        self.current
    }

    #[inline]
    pub fn next_sample(&mut self) -> f32 {
        self.current += self.alpha * (self.target - self.current);
        self.current
    }

    #[inline]
    pub fn is_settled(&self, eps: f32) -> bool {
        (self.current - self.target).abs() <= eps
    }
}

/// Stereo 2nd-order Butterworth (`Q = 1/sqrt(2)`) Cytomic Topology-Preserving Transform (TPT)
/// State-Variable High-Pass Filter + smoothed deck gain.
///
/// Unconditionally stable under rapid cutoff modulation (`20 Hz <-> 900 Hz`) with
/// exact `1.0` passband unity gain and zero zipper noise or DC discontinuities.
#[derive(Clone, Debug)]
pub struct DeckChannelStrip {
    sample_rate: u32,
    cutoff_smoother: OnePoleSmoother,
    gain_smoother: OnePoleSmoother,
    ic1eq_l: f32,
    ic2eq_l: f32,
    ic1eq_r: f32,
    ic2eq_r: f32,
    a1: f32,
    a2: f32,
    a3: f32,
    last_coeff_hz: f32,
    coeff_countdown: u8,
}

impl DeckChannelStrip {
    pub const MIN_CUTOFF_HZ: f32 = 20.0;
    pub const MAX_CUTOFF_HZ: f32 = 4_000.0;
    const K_BUTTERWORTH: f32 = SQRT_2; // 1 / Q where Q = 1 / sqrt(2)

    pub fn new(sample_rate: u32) -> Self {
        let _ = FRAC_1_SQRT_2;
        let sr = sample_rate.max(8_000);
        let mut s = Self {
            sample_rate: sr,
            cutoff_smoother: OnePoleSmoother::new(Self::MIN_CUTOFF_HZ, sr, 0.010),
            gain_smoother: OnePoleSmoother::new(1.0, sr, 0.010),
            ic1eq_l: 0.0,
            ic2eq_l: 0.0,
            ic1eq_r: 0.0,
            ic2eq_r: 0.0,
            a1: 0.0,
            a2: 0.0,
            a3: 0.0,
            last_coeff_hz: -1.0,
            coeff_countdown: 0,
        };
        s.update_coeffs(Self::MIN_CUTOFF_HZ);
        s
    }

    pub fn reset(&mut self, sample_rate: u32, initial_gain: f32, initial_hpf_hz: f32) {
        let sr = sample_rate.max(8_000);
        self.sample_rate = sr;
        self.cutoff_smoother.set_sample_rate(sr, 0.010);
        self.gain_smoother.set_sample_rate(sr, 0.010);
        let hz = initial_hpf_hz.clamp(Self::MIN_CUTOFF_HZ, Self::MAX_CUTOFF_HZ);
        self.cutoff_smoother.reset(hz);
        self.gain_smoother.reset(initial_gain.clamp(0.0, 2.0));
        self.ic1eq_l = 0.0;
        self.ic2eq_l = 0.0;
        self.ic1eq_r = 0.0;
        self.ic2eq_r = 0.0;
        self.coeff_countdown = 0;
        self.update_coeffs(hz);
    }

    #[inline]
    pub fn set_targets(&mut self, gain: f32, hpf_hz: f32) {
        self.gain_smoother.set_target(gain.clamp(0.0, 2.0));
        self.cutoff_smoother
            .set_target(hpf_hz.clamp(Self::MIN_CUTOFF_HZ, Self::MAX_CUTOFF_HZ));
    }

    #[inline]
    fn update_coeffs(&mut self, hz: f32) {
        let fs = self.sample_rate as f32;
        let clamped = hz.clamp(10.0, (fs * 0.45).max(100.0));
        let g = (PI * clamped / fs).tan();
        let k = Self::K_BUTTERWORTH;
        let a1 = 1.0 / (1.0 + g * (g + k));
        let a2 = g * a1;
        let a3 = g * a2;
        self.a1 = a1;
        self.a2 = a2;
        self.a3 = a3;
        self.last_coeff_hz = clamped;
    }

    /// Process one stereo sample pair `(l, r)` in normalized `[-1.0, 1.0]` float domain.
    #[inline]
    pub fn process_frame(&mut self, l: f32, r: f32) -> (f32, f32) {
        let fc = self.cutoff_smoother.next_sample();
        let gain = self.gain_smoother.next_sample();

        if self.coeff_countdown == 0 {
            if (fc - self.last_coeff_hz).abs() > 0.05 {
                self.update_coeffs(fc);
            }
            self.coeff_countdown = 15;
        } else {
            self.coeff_countdown -= 1;
        }

        let a1 = self.a1;
        let a2 = self.a2;
        let a3 = self.a3;
        let k = Self::K_BUTTERWORTH;

        // Left channel Cytomic TPT SVF
        let v3_l = l - self.ic2eq_l;
        let v1_l = a1 * self.ic1eq_l + a2 * v3_l;
        let v2_l = self.ic2eq_l + a2 * self.ic1eq_l + a3 * v3_l;
        self.ic1eq_l = 2.0 * v1_l - self.ic1eq_l;
        self.ic2eq_l = 2.0 * v2_l - self.ic2eq_l;
        let hp_l = l - k * v1_l - v2_l;

        // Right channel Cytomic TPT SVF
        let v3_r = r - self.ic2eq_r;
        let v1_r = a1 * self.ic1eq_r + a2 * v3_r;
        let v2_r = self.ic2eq_r + a2 * self.ic1eq_r + a3 * v3_r;
        self.ic1eq_r = 2.0 * v1_r - self.ic1eq_r;
        self.ic2eq_r = 2.0 * v2_r - self.ic2eq_r;
        let hp_r = r - k * v1_r - v2_r;

        (hp_l * gain, hp_r * gain)
    }

    /// Process an interleaved stereo buffer `[L0, R0, L1, R1, ...]` in-place.
    pub fn process_interleaved_in_place(&mut self, stereo: &mut [f32]) {
        for pair in stereo.chunks_exact_mut(2) {
            let (ol, or) = self.process_frame(pair[0], pair[1]);
            pair[0] = ol;
            pair[1] = or;
        }
    }
}

/// Pre-allocated tempo-synced stereo Ping-Pong Wash delay used during `FilterWash`
/// transitions and `Quick DJ Cut` skips. Zero per-frame heap allocations.
pub struct StereoWashDelay {
    buf_l: Vec<f32>,
    buf_r: Vec<f32>,
    write_idx: usize,
    delay_frames: usize,
    lp_l: f32,
    lp_r: f32,
    hp_l: f32,
    hp_r: f32,
    lp_coeff: f32,
    hp_coeff: f32,
    feedback: f32,
    wet_smoother: OnePoleSmoother,
}

impl StereoWashDelay {
    /// Supports up to `750 ms` at `48 kHz` (`36_000` frames).
    pub const MAX_DELAY_FRAMES: usize = 36_000;

    pub fn new(sample_rate: u32) -> Self {
        let mut d = Self {
            buf_l: vec![0.0; Self::MAX_DELAY_FRAMES],
            buf_r: vec![0.0; Self::MAX_DELAY_FRAMES],
            write_idx: 0,
            delay_frames: 16_000,
            lp_l: 0.0,
            lp_r: 0.0,
            hp_l: 0.0,
            hp_r: 0.0,
            lp_coeff: 0.30,
            hp_coeff: 0.025,
            feedback: 0.44,
            wet_smoother: OnePoleSmoother::new(0.0, sample_rate, 0.015),
        };
        d.configure(sample_rate, 120.0);
        d
    }

    pub fn configure(&mut self, sample_rate: u32, bpm_hint: f32) {
        let sr = sample_rate.max(8_000);
        let fs = sr as f32;
        // Dotted-eighth ping-pong (`0.75 * 60 / bpm`), clamped to musical 220..520 ms
        let bpm = if bpm_hint >= 60.0 && bpm_hint <= 200.0 {
            bpm_hint
        } else {
            120.0
        };
        let sec = (45.0 / bpm).clamp(0.22, 0.52);
        self.delay_frames = ((sec * fs).round() as usize).clamp(512, Self::MAX_DELAY_FRAMES - 2);
        self.lp_coeff = (1.0 - (-2.0 * PI * 2800.0 / fs).exp()).clamp(0.02, 0.95);
        self.hp_coeff = (1.0 - (-2.0 * PI * 220.0 / fs).exp()).clamp(0.002, 0.40);
        self.wet_smoother.set_sample_rate(sr, 0.015);
    }

    pub fn clear(&mut self) {
        self.buf_l.fill(0.0);
        self.buf_r.fill(0.0);
        self.write_idx = 0;
        self.lp_l = 0.0;
        self.lp_r = 0.0;
        self.hp_l = 0.0;
        self.hp_r = 0.0;
        self.wet_smoother.reset(0.0);
    }

    #[inline]
    pub fn set_wet_send(&mut self, wet: f32) {
        self.wet_smoother.set_target(wet.clamp(0.0, 0.85));
    }

    #[inline]
    pub fn step(&mut self, in_l: f32, in_r: f32) -> (f32, f32) {
        let wet = self.wet_smoother.next_sample();
        let cap = self.buf_l.len();
        let read_idx = (self.write_idx + cap - self.delay_frames) % cap;

        let tap_l = self.buf_l[read_idx];
        let tap_r = self.buf_r[read_idx];

        // Band-limit feedback (~220 Hz .. 2.8 kHz) so echo tail stays airy and never muddies Deck B's sub-bass
        self.lp_l += self.lp_coeff * (tap_l - self.lp_l);
        self.lp_r += self.lp_coeff * (tap_r - self.lp_r);
        self.hp_l += self.hp_coeff * (self.lp_l - self.hp_l);
        self.hp_r += self.hp_coeff * (self.lp_r - self.hp_r);
        let fb_l = self.lp_l - self.hp_l;
        let fb_r = self.lp_r - self.hp_r;

        // Stereo ping-pong cross-feed (`mono_send -> L -> R -> L`)
        let send_l = (in_l * 0.75 + in_r * 0.25) * wet;
        let send_r = (in_r * 0.35 + in_l * 0.15) * wet;
        self.buf_l[self.write_idx] = (send_l + fb_r * self.feedback).clamp(-1.5, 1.5);
        self.buf_r[self.write_idx] = (send_r + fb_l * self.feedback).clamp(-1.5, 1.5);

        self.write_idx += 1;
        if self.write_idx >= cap {
            self.write_idx = 0;
        }

        (fb_l * 0.85, fb_r * 0.85)
    }
}
