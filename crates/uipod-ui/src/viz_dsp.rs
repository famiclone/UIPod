//! Real-time zero-allocation audio analysis DSP for UIPod's 4-shade GPU visualizer.
//!
//! Computes a 1024-point Hann-windowed radix-2 FFT, Milkdrop-style AGC-normalized
//! frequency band envelopes (`sub_bass`, `bass`, `mid`, `treb` + attenuated smooths),
//! spectral-flux onset/beat triggers, and a `256x2` `RGBA8` GPU audio texture packing
//! stereo waveforms (`L`, `R`, `Mono`, `StereoWidth`) + 256 log-spaced spectrum & peak-hold bins.

use std::f32::consts::{PI, TAU};

pub const FFT_SIZE: usize = 1024;
pub const FFT_BINS: usize = FFT_SIZE / 2;
pub const AUDIO_TEX_W: usize = 256;
pub const AUDIO_TEX_H: usize = 2;

/// Snapshot of audio-reactive uniforms uploaded to GLSL every frame.
#[derive(Debug, Clone, Copy, Default)]
pub struct VizAudioFrame {
    pub time: f32,
    pub music_time: f32,
    pub sub_bass: f32,
    pub bass: f32,
    pub mid: f32,
    pub treb: f32,
    pub bass_att: f32,
    pub mid_att: f32,
    pub treb_att: f32,
    pub beat_pulse: f32,
    pub beat_dir: f32,
    pub beat_phase: f32,
    pub stereo_width: f32,
    pub vu_l: f32,
    pub vu_r: f32,
    pub vu_peak_l: f32,
    pub vu_peak_r: f32,
    pub beat_hit: bool,
}

pub struct VizDsp {
    hann: [f32; FFT_SIZE],
    bit_rev: [u16; FFT_SIZE],
    twiddle_re: [f32; FFT_BINS],
    twiddle_im: [f32; FFT_BINS],
    fft_re: [f32; FFT_SIZE],
    fft_im: [f32; FFT_SIZE],
    mag: [f32; FFT_BINS],
    prev_mag: [f32; FFT_BINS],

    spec_smooth: [f32; AUDIO_TEX_W],
    spec_peak: [f32; AUDIO_TEX_W],

    // Rolling averages for Milkdrop-style relative AGC normalization
    avg_sub: f32,
    avg_bass: f32,
    avg_mid: f32,
    avg_treb: f32,
    wave_rms_avg: f32,
    flux_avg: f32,

    // Smoothed instantaneous & attenuated band envelopes
    sub_env: f32,
    bass_env: f32,
    mid_env: f32,
    treb_env: f32,
    bass_att: f32,
    mid_att: f32,
    treb_att: f32,
    stereo_env: f32,

    // Dual-channel analog VU galvanometer state (L / R)
    vu_pos_l: f32,
    vu_vel_l: f32,
    vu_pos_r: f32,
    vu_vel_r: f32,
    vu_peak_l: f32,
    vu_peak_r: f32,

    // Timing & beat state
    time: f32,
    music_time: f32,
    beat_pulse: f32,
    beat_cooldown: f32,
    beat_dir: f32,
    beat_count: u32,
    last_intel_hash: u64,
    last_intel_beat: u64,

    /// Packed `256 x 2 x 4` (`2048` bytes) `RGBA8` texture:
    /// - Row 0: `[L_wave, R_wave, Mono_smooth, Stereo_diff]`
    /// - Row 1: `[Log_spectrum, Peak_hold, Transient_flux, 255]`
    pub audio_tex: [u8; AUDIO_TEX_W * AUDIO_TEX_H * 4],
}

impl Default for VizDsp {
    fn default() -> Self {
        Self::new()
    }
}

impl VizDsp {
    pub fn new() -> Self {
        let mut hann = [0.0f32; FFT_SIZE];
        for (i, w) in hann.iter_mut().enumerate() {
            *w = 0.5 * (1.0 - (TAU * (i as f32) / (FFT_SIZE as f32)).cos());
        }

        let bits = FFT_SIZE.trailing_zeros();
        let mut bit_rev = [0u16; FFT_SIZE];
        for (i, slot) in bit_rev.iter_mut().enumerate() {
            *slot = ((i as u32).reverse_bits() >> (32 - bits)) as u16;
        }

        let mut twiddle_re = [0.0f32; FFT_BINS];
        let mut twiddle_im = [0.0f32; FFT_BINS];
        for k in 0..FFT_BINS {
            let angle = -TAU * (k as f32) / (FFT_SIZE as f32);
            twiddle_re[k] = angle.cos();
            twiddle_im[k] = angle.sin();
        }

        let mut audio_tex = [0u8; AUDIO_TEX_W * AUDIO_TEX_H * 4];
        // Initialize waveform row to silence center (128) and alpha=255 on spectrum row
        for x in 0..AUDIO_TEX_W {
            let w0 = x * 4;
            audio_tex[w0] = 128;
            audio_tex[w0 + 1] = 128;
            audio_tex[w0 + 2] = 128;
            audio_tex[w0 + 3] = 0;
            let s0 = (AUDIO_TEX_W + x) * 4;
            audio_tex[s0 + 3] = 255;
        }

        Self {
            hann,
            bit_rev,
            twiddle_re,
            twiddle_im,
            fft_re: [0.0; FFT_SIZE],
            fft_im: [0.0; FFT_SIZE],
            mag: [0.0; FFT_BINS],
            prev_mag: [0.0; FFT_BINS],
            spec_smooth: [0.0; AUDIO_TEX_W],
            spec_peak: [0.0; AUDIO_TEX_W],
            avg_sub: 0.02,
            avg_bass: 0.02,
            avg_mid: 0.015,
            avg_treb: 0.008,
            wave_rms_avg: 0.12,
            flux_avg: 0.02,
            sub_env: 0.0,
            bass_env: 0.0,
            mid_env: 0.0,
            treb_env: 0.0,
            bass_att: 0.0,
            mid_att: 0.0,
            treb_att: 0.0,
            stereo_env: 0.0,
            vu_pos_l: 0.0,
            vu_vel_l: 0.0,
            vu_pos_r: 0.0,
            vu_vel_r: 0.0,
            vu_peak_l: 0.0,
            vu_peak_r: 0.0,
            time: 0.0,
            music_time: 0.0,
            beat_pulse: 0.0,
            beat_cooldown: 0.0,
            beat_dir: 1.0,
            beat_count: 0,
            last_intel_hash: 0,
            last_intel_beat: u64::MAX,
            audio_tex,
        }
    }

    /// Resets transient peaks on hard preset cuts while keeping time smoothly advancing.
    pub fn soft_reset_peaks(&mut self) {
        self.beat_pulse = 0.0;
    }

    /// Processes up to `FFT_SIZE` interleaved stereo `i16` PCM frames (`pcm_interleaved.len() == frames * 2`)
    /// and updates all smoothed envelopes and `self.audio_tex`.
    pub fn update(&mut self, pcm_interleaved: &[i16], sample_rate: u32, dt_raw: f32) -> VizAudioFrame {
        let dt = dt_raw.clamp(1.0 / 240.0, 0.1);
        self.time += dt;
        self.beat_cooldown = (self.beat_cooldown - dt).max(0.0);
        self.beat_pulse *= (-dt * 7.5).exp();

        let frames = (pcm_interleaved.len() / 2).min(FFT_SIZE);
        let sr = sample_rate.clamp(8_000, 192_000) as f32;

        if frames < 64 {
            // Playback paused or stopped: smoothly decay envelopes and relax waveform to center.
            let release = (-dt * 6.0).exp();
            let att_rel = (-dt * 3.0).exp();
            self.sub_env *= release;
            self.bass_env *= release;
            self.mid_env *= release;
            self.treb_env *= release;
            self.bass_att *= att_rel;
            self.mid_att *= att_rel;
            self.treb_att *= att_rel;
            self.stereo_env *= release;
            Self::step_vu_needle(&mut self.vu_pos_l, &mut self.vu_vel_l, 0.0, dt);
            Self::step_vu_needle(&mut self.vu_pos_r, &mut self.vu_vel_r, 0.0, dt);
            self.vu_peak_l *= release;
            self.vu_peak_r *= release;
            self.music_time += dt * 0.25;

            for x in 0..AUDIO_TEX_W {
                let w0 = x * 4;
                for c in 0..3 {
                    let cur = self.audio_tex[w0 + c] as f32;
                    self.audio_tex[w0 + c] = (128.0 + (cur - 128.0) * release).round() as u8;
                }
                self.audio_tex[w0 + 3] = ((self.audio_tex[w0 + 3] as f32) * release) as u8;

                self.spec_smooth[x] *= release;
                self.spec_peak[x] = (self.spec_peak[x] - dt * 0.45).max(self.spec_smooth[x]);
                let s0 = (AUDIO_TEX_W + x) * 4;
                self.audio_tex[s0] = (self.spec_smooth[x].clamp(0.0, 1.0) * 255.0) as u8;
                self.audio_tex[s0 + 1] = (self.spec_peak[x].clamp(0.0, 1.0) * 255.0) as u8;
                self.audio_tex[s0 + 2] = 0;
                self.audio_tex[s0 + 3] = 255;
            }

            return VizAudioFrame {
                time: self.time,
                music_time: self.music_time,
                sub_bass: self.sub_env,
                bass: self.bass_env,
                mid: self.mid_env,
                treb: self.treb_env,
                bass_att: self.bass_att,
                mid_att: self.mid_att,
                treb_att: self.treb_att,
                beat_pulse: self.beat_pulse,
                beat_dir: self.beat_dir,
                beat_phase: (self.beat_count & 7) as f32,
                stereo_width: self.stereo_env,
                vu_l: self.vu_pos_l,
                vu_r: self.vu_pos_r,
                vu_peak_l: self.vu_peak_l,
                vu_peak_r: self.vu_peak_r,
                beat_hit: false,
            };
        }

        // 1. Unpack stereo PCM, compute DC offset, per-channel L/R RMS & peaks, and stereo width
        let inv_i16 = 1.0 / 32768.0;
        let mut mono_buf = [0.0f32; FFT_SIZE];
        let mut dc_sum = 0.0f32;
        let mut sq_sum = 0.0f32;
        let mut side_sq_sum = 0.0f32;
        let mut sq_l = 0.0f32;
        let mut sq_r = 0.0f32;
        let mut pk_l = 0.0f32;
        let mut pk_r = 0.0f32;

        // Zero-pad front if fewer than 1024 frames available, placing newest at the end
        let pad = FFT_SIZE - frames;
        for i in 0..frames {
            let l = (pcm_interleaved[2 * i] as f32) * inv_i16;
            let r = (pcm_interleaved[2 * i + 1] as f32) * inv_i16;
            let m = 0.5 * (l + r);
            let s = 0.5 * (l - r);
            mono_buf[pad + i] = m;
            dc_sum += m;
            sq_sum += m * m;
            side_sq_sum += s * s;
            sq_l += l * l;
            sq_r += r * r;
            pk_l = pk_l.max(l.abs());
            pk_r = pk_r.max(r.abs());
        }
        let inv_frames = 1.0 / (frames as f32);
        let dc = dc_sum * inv_frames;
        let rms = (sq_sum * inv_frames).sqrt();
        let side_rms = (side_sq_sum * inv_frames).sqrt();
        let rms_l = (sq_l * inv_frames).sqrt();
        let rms_r = (sq_r * inv_frames).sqrt();

        // Smooth AGC gain for waveform & VU reference (0 VU tracks average program level),
        // seeded immediately on track change from `uipod-track-intel` cached `avg_rms`!
        let intel_rec = uipod_track_intel::read_active_analysis();
        if let Some(ref rec) = intel_rec {
            if rec.key_hash != 0 && rec.key_hash != self.last_intel_hash {
                self.last_intel_hash = rec.key_hash;
                self.last_intel_beat = u64::MAX;
                if rec.avg_rms > 0.01 {
                    self.wave_rms_avg = rec.avg_rms.clamp(0.04, 0.65);
                }
            }
        }
        let agc_alpha = 1.0 - (-dt * 1.8).exp();
        if rms > 0.002 {
            self.wave_rms_avg += (rms - self.wave_rms_avg) * agc_alpha;
        }
        let wave_gain = (0.34 / self.wave_rms_avg.max(0.04)).clamp(0.65, 4.5);
        let vu_gain = (0.28 / self.wave_rms_avg.max(0.035)).clamp(0.55, 4.2);

        let target_vu_l = Self::rms_to_vu_dial(rms_l * vu_gain);
        let target_vu_r = Self::rms_to_vu_dial(rms_r * vu_gain);
        Self::step_vu_needle(&mut self.vu_pos_l, &mut self.vu_vel_l, target_vu_l, dt);
        Self::step_vu_needle(&mut self.vu_pos_r, &mut self.vu_vel_r, target_vu_r, dt);

        let pk_decay = (-dt * 4.8).exp();
        self.vu_peak_l = if pk_l * vu_gain > 1.08 && rms_l > 0.003 {
            1.0
        } else {
            self.vu_peak_l * pk_decay
        };
        self.vu_peak_r = if pk_r * vu_gain > 1.08 && rms_r > 0.003 {
            1.0
        } else {
            self.vu_peak_r * pk_decay
        };

        let raw_width = (side_rms / (rms + 0.005)).clamp(0.0, 1.5);
        self.stereo_env += (raw_width - self.stereo_env) * (1.0 - (-dt * 8.0).exp());

        // Populate Row 0 of `audio_tex` (256-sample stereo + smoothed mono waveform)
        let wave_src_len = frames;
        for x in 0..AUDIO_TEX_W {
            let src_idx = (x * wave_src_len) / AUDIO_TEX_W;
            let l = (pcm_interleaved[2 * src_idx] as f32) * inv_i16 * wave_gain;
            let r = (pcm_interleaved[2 * src_idx + 1] as f32) * inv_i16 * wave_gain;

            // 3-tap box smooth on mono for silky circular/horizontal Milkdrop waves
            let i_prev = src_idx.saturating_sub(1);
            let i_next = (src_idx + 1).min(wave_src_len - 1);
            let m0 = (pcm_interleaved[2 * i_prev] as f32 + pcm_interleaved[2 * i_prev + 1] as f32) * 0.5 * inv_i16;
            let m1 = (pcm_interleaved[2 * src_idx] as f32 + pcm_interleaved[2 * src_idx + 1] as f32) * 0.5 * inv_i16;
            let m2 = (pcm_interleaved[2 * i_next] as f32 + pcm_interleaved[2 * i_next + 1] as f32) * 0.5 * inv_i16;
            let m_smooth = (0.25 * m0 + 0.5 * m1 + 0.25 * m2) * wave_gain;
            let diff = (l - r).abs().clamp(0.0, 1.0);

            let w0 = x * 4;
            self.audio_tex[w0] = ((l.clamp(-1.0, 1.0) * 0.5 + 0.5) * 255.0).round() as u8;
            self.audio_tex[w0 + 1] = ((r.clamp(-1.0, 1.0) * 0.5 + 0.5) * 255.0).round() as u8;
            self.audio_tex[w0 + 2] = ((m_smooth.clamp(-1.0, 1.0) * 0.5 + 0.5) * 255.0).round() as u8;
            self.audio_tex[w0 + 3] = (diff * 255.0).round() as u8;
        }

        // 2. Windowed bit-reversed input for 1024-point radix-2 FFT
        for i in 0..FFT_SIZE {
            let rev = self.bit_rev[i] as usize;
            let sample = if rev >= pad { mono_buf[rev] - dc } else { 0.0 };
            self.fft_re[i] = sample * self.hann[rev];
            self.fft_im[i] = 0.0;
        }

        self.run_fft_in_place();

        // 3. Extract normalized magnitudes + positive spectral flux
        let inv_norm = 2.0 / (FFT_SIZE as f32);
        let hz_per_bin = sr / (FFT_SIZE as f32);
        let mut raw_sub = 0.0f32;
        let mut cnt_sub = 0u32;
        let mut raw_bass = 0.0f32;
        let mut cnt_bass = 0u32;
        let mut raw_mid = 0.0f32;
        let mut cnt_mid = 0u32;
        let mut raw_treb = 0.0f32;
        let mut cnt_treb = 0u32;
        let mut low_mid_flux = 0.0f32;

        for k in 1..FFT_BINS {
            let re = self.fft_re[k];
            let im = self.fft_im[k];
            let m = (re * re + im * im).sqrt() * inv_norm;
            let prev = self.mag[k];
            self.prev_mag[k] = prev;
            self.mag[k] = m;

            let hz = (k as f32) * hz_per_bin;
            if (32.0..=115.0).contains(&hz) {
                raw_sub += m;
                cnt_sub += 1;
            }
            if (40.0..=260.0).contains(&hz) {
                raw_bass += m;
                cnt_bass += 1;
            } else if (260.0..=2600.0).contains(&hz) {
                raw_mid += m;
                cnt_mid += 1;
            } else if (2600.0..=14500.0).contains(&hz) {
                raw_treb += m;
                cnt_treb += 1;
            }

            if (38.0..=1800.0).contains(&hz) {
                let diff = (m - prev).max(0.0);
                // Weight bass kick bins stronger for crisp beat locking
                let weight = if hz < 180.0 { 2.2 } else { 0.8 };
                low_mid_flux += diff * weight;
            }
        }

        if cnt_sub > 0 {
            raw_sub /= cnt_sub as f32;
        }
        if cnt_bass > 0 {
            raw_bass /= cnt_bass as f32;
        }
        if cnt_mid > 0 {
            raw_mid /= cnt_mid as f32;
        }
        if cnt_treb > 0 {
            raw_treb /= cnt_treb as f32;
        }

        // 4. Milkdrop-style rolling-average AGC normalization (noise-floor guarded)
        let slow_alpha = 1.0 - (-dt * 0.45).exp();
        if rms > 0.0015 {
            self.avg_sub += (raw_sub.max(0.0008) - self.avg_sub) * slow_alpha;
            self.avg_bass += (raw_bass.max(0.0008) - self.avg_bass) * slow_alpha;
            self.avg_mid += (raw_mid.max(0.0005) - self.avg_mid) * slow_alpha;
            self.avg_treb += (raw_treb.max(0.00025) - self.avg_treb) * slow_alpha;
        }

        let silence_gate = ((rms - 0.001) / 0.006).clamp(0.0, 1.0);
        let rel_sub = (raw_sub / self.avg_sub.max(0.0008)).clamp(0.0, 3.2) * silence_gate;
        let rel_bass = (raw_bass / self.avg_bass.max(0.0008)).clamp(0.0, 3.2) * silence_gate;
        let rel_mid = (raw_mid / self.avg_mid.max(0.0005)).clamp(0.0, 3.0) * silence_gate;
        let rel_treb = (raw_treb / self.avg_treb.max(0.00025)).clamp(0.0, 3.0) * silence_gate;

        let atk = 1.0 - (-dt * 72.0).exp();
        let rel = 1.0 - (-dt * 7.5).exp();
        let att_a = 1.0 - (-dt * 3.2).exp();

        Self::asym_smooth(&mut self.sub_env, rel_sub, atk, rel);
        Self::asym_smooth(&mut self.bass_env, rel_bass, atk, rel);
        Self::asym_smooth(&mut self.mid_env, rel_mid, atk, rel);
        Self::asym_smooth(&mut self.treb_env, rel_treb, atk, rel);

        self.bass_att += (self.bass_env - self.bass_att) * att_a;
        self.mid_att += (self.mid_env - self.mid_att) * att_a;
        self.treb_att += (self.treb_env - self.treb_att) * att_a;

        // 5. Spectral-flux + lock-free `uipod-track-intel` beat & bar-phase synchronization
        let live_intel = uipod_track_intel::read_live_frame();
        let intel_locked = live_intel.status == 1
            && live_intel.has_analysis != 0
            && live_intel.track_key_hash != 0;

        let flux_alpha = 1.0 - (-dt * 2.5).exp();
        self.flux_avg += (low_mid_flux - self.flux_avg) * flux_alpha;
        let mut beat_hit = false;
        let flux_thresh = (self.flux_avg * 1.48).max(0.004);

        if intel_locked && live_intel.beat_index != self.last_intel_beat {
            let initialized = self.last_intel_beat != u64::MAX;
            self.last_intel_beat = live_intel.beat_index;
            if initialized && silence_gate > 0.15 {
                beat_hit = true;
                self.beat_pulse = (0.78 + 0.22 * live_intel.kick_impulse).clamp(0.0, 1.0);
                self.beat_cooldown = 0.11;
                self.beat_count = live_intel.beat_index as u32;
                if live_intel.bar_beat == 0 || (live_intel.bar_beat == 2 && rel_bass > 1.35) {
                    self.beat_dir = -self.beat_dir;
                }
            }
        } else if silence_gate > 0.25
            && self.beat_cooldown <= 0.0
            && low_mid_flux > flux_thresh
            && (rel_sub > 1.18 || rel_bass > 1.15)
        {
            beat_hit = true;
            self.beat_pulse = 1.0;
            self.beat_cooldown = 0.135; // max ~444 BPM refractory window
            self.beat_count = self.beat_count.wrapping_add(1);
            if rel_bass > 1.65 || (self.beat_count & 3) == 0 {
                self.beat_dir = -self.beat_dir;
            }
        }

        let resolved_beat_phase = if intel_locked {
            ((live_intel.beat_index & 3) as f32) + live_intel.beat_phase
        } else {
            ((self.beat_count & 7) as f32) + (PI * 0.25)
        };

        let energy = (0.5 * self.bass_att + 0.35 * self.mid_att + 0.15 * self.treb_att).clamp(0.0, 2.5);
        self.music_time += dt * (0.35 + 0.85 * energy + 0.45 * self.beat_pulse);

        // 6. Populate Row 1 of `audio_tex`: 256 log-frequency spectrum + peak-hold bins
        let f_min = 38.0f32;
        let f_max = (0.45 * sr).clamp(8_000.0, 16_000.0);
        let log_ratio = (f_max / f_min).ln();
        let spec_atk = 1.0 - (-dt * 68.0).exp();
        let spec_rel = 1.0 - (-dt * 9.0).exp();

        for x in 0..AUDIO_TEX_W {
            let t0 = (x as f32) / (AUDIO_TEX_W as f32);
            let t1 = ((x + 1) as f32) / (AUDIO_TEX_W as f32);
            let hz0 = f_min * (t0 * log_ratio).exp();
            let hz1 = f_min * (t1 * log_ratio).exp();

            let b0 = ((hz0 / hz_per_bin).floor() as usize).clamp(1, FFT_BINS - 1);
            let b1 = ((hz1 / hz_per_bin).ceil() as usize).clamp(b0, FFT_BINS - 1);

            let mut peak_m = 0.0f32;
            let mut flux_m = 0.0f32;
            for k in b0..=b1 {
                peak_m = peak_m.max(self.mag[k]);
                flux_m = flux_m.max((self.mag[k] - self.prev_mag[k]).max(0.0));
            }

            // Pink-noise tilt (+3.5 dB/octave visual compensation so highs look balanced with bass)
            let tilt = 0.75 + 2.6 * t0;
            let scaled = (peak_m * tilt * wave_gain * 16.0).clamp(0.0, 16.0);
            let log_val = ((1.0 + scaled).ln() / (1.0f32 + 16.0).ln()).clamp(0.0, 1.0) * silence_gate;

            Self::asym_smooth(&mut self.spec_smooth[x], log_val, spec_atk, spec_rel);
            if self.spec_smooth[x] >= self.spec_peak[x] {
                self.spec_peak[x] = self.spec_smooth[x];
            } else {
                self.spec_peak[x] = (self.spec_peak[x] - dt * 0.42).max(self.spec_smooth[x]);
            }

            let flux_v = (flux_m * tilt * 28.0).clamp(0.0, 1.0) * silence_gate;
            let s0 = (AUDIO_TEX_W + x) * 4;
            self.audio_tex[s0] = (self.spec_smooth[x] * 255.0).round() as u8;
            self.audio_tex[s0 + 1] = (self.spec_peak[x] * 255.0).round() as u8;
            self.audio_tex[s0 + 2] = (flux_v * 255.0).round() as u8;
            self.audio_tex[s0 + 3] = 255;
        }

        VizAudioFrame {
            time: self.time,
            music_time: self.music_time,
            sub_bass: self.sub_env,
            bass: self.bass_env,
            mid: self.mid_env,
            treb: self.treb_env,
            bass_att: self.bass_att,
            mid_att: self.mid_att,
            treb_att: self.treb_att,
            beat_pulse: self.beat_pulse,
            beat_dir: self.beat_dir,
            beat_phase: resolved_beat_phase,
            stereo_width: self.stereo_env,
            vu_l: self.vu_pos_l,
            vu_r: self.vu_pos_r,
            vu_peak_l: self.vu_peak_l,
            vu_peak_r: self.vu_peak_r,
            beat_hit,
        }
    }

    #[inline]
    fn rms_to_vu_dial(norm_rms: f32) -> f32 {
        if norm_rms <= 0.002 {
            return 0.0;
        }
        let db = 20.0 * norm_rms.log10();
        if db <= 0.0 {
            // -26 dB .. 0 dB -> 0.00 .. 0.72 (0 VU mark at 72% arc)
            (((db + 26.0) / 26.0).clamp(0.0, 1.0)).powf(1.32) * 0.72
        } else {
            // 0 dB .. +3.5 dB -> 0.72 .. 1.00 (redline overload zone)
            0.72 + (db / 3.5).clamp(0.0, 1.0) * 0.28
        }
    }

    #[inline]
    fn step_vu_needle(pos: &mut f32, vel: &mut f32, target: f32, dt: f32) {
        let h = dt * 0.5;
        for _ in 0..2 {
            let acc = (target - *pos) * 380.0 - *vel * 29.0;
            *vel += acc * h;
            *pos += *vel * h;
            if *pos < 0.0 {
                *pos = 0.0;
                if *vel < 0.0 {
                    *vel = 0.0;
                }
            } else if *pos > 1.03 {
                *pos = 1.03;
                if *vel > 0.0 {
                    *vel *= -0.25;
                }
            }
        }
    }

    #[inline]
    fn asym_smooth(cur: &mut f32, target: f32, atk: f32, rel: f32) {
        let a = if target > *cur { atk } else { rel };
        *cur += (target - *cur) * a;
    }

    fn run_fft_in_place(&mut self) {
        let mut len = 2usize;
        while len <= FFT_SIZE {
            let half = len / 2;
            let step = FFT_SIZE / len;
            let mut i = 0usize;
            while i < FFT_SIZE {
                for j in 0..half {
                    let tw = j * step;
                    let wr = self.twiddle_re[tw];
                    let wi = self.twiddle_im[tw];
                    let a = i + j;
                    let b = a + half;
                    let tr = wr * self.fft_re[b] - wi * self.fft_im[b];
                    let ti = wr * self.fft_im[b] + wi * self.fft_re[b];
                    let ur = self.fft_re[a];
                    let ui = self.fft_im[a];
                    self.fft_re[a] = ur + tr;
                    self.fft_im[a] = ui + ti;
                    self.fft_re[b] = ur - tr;
                    self.fft_im[b] = ui - ti;
                }
                i += len;
            }
            len <<= 1;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn gen_sine_stereo(freq_hz: f32, sr: u32, amp: f32, frames: usize) -> Vec<i16> {
        let mut out = Vec::with_capacity(frames * 2);
        for i in 0..frames {
            let s = ((TAU * freq_hz * (i as f32) / (sr as f32)).sin() * amp * 32767.0) as i16;
            out.push(s);
            out.push(s);
        }
        out
    }

    #[test]
    fn fft_separates_bass_mid_and_treble_accurately() {
        let sr = 44_100;
        let mut dsp = VizDsp::new();

        // Feed 90 Hz bass tone for several frames
        let bass_pcm = gen_sine_stereo(90.0, sr, 0.7, 1024);
        let mut f = VizAudioFrame::default();
        for _ in 0..8 {
            f = dsp.update(&bass_pcm, sr, 1.0 / 60.0);
        }
        assert!(f.bass > f.mid * 2.0 && f.bass > f.treb * 2.0, "expected bass dominance: {f:?}");

        // Feed 1200 Hz mid tone
        let mut dsp_mid = VizDsp::new();
        let mid_pcm = gen_sine_stereo(1200.0, sr, 0.7, 1024);
        for _ in 0..8 {
            f = dsp_mid.update(&mid_pcm, sr, 1.0 / 60.0);
        }
        assert!(f.mid > f.bass * 2.0 && f.mid > f.treb * 2.0, "expected mid dominance: {f:?}");

        // Feed 7500 Hz treble tone
        let mut dsp_treb = VizDsp::new();
        let treb_pcm = gen_sine_stereo(7500.0, sr, 0.7, 1024);
        for _ in 0..8 {
            f = dsp_treb.update(&treb_pcm, sr, 1.0 / 60.0);
        }
        assert!(f.treb > f.bass * 2.0 && f.treb > f.mid * 2.0, "expected treb dominance: {f:?}");
    }

    #[test]
    fn beat_detector_fires_on_kick_transient_and_decays_on_pause() {
        let sr = 44_100;
        let mut dsp = VizDsp::new();
        let quiet = gen_sine_stereo(440.0, sr, 0.08, 1024);
        for _ in 0..15 {
            dsp.update(&quiet, sr, 1.0 / 60.0);
        }

        let kick = gen_sine_stereo(65.0, sr, 0.9, 1024);
        let hit = dsp.update(&kick, sr, 1.0 / 60.0);
        assert!(hit.beat_hit, "expected kick transient to trigger beat_hit");
        assert!((hit.beat_pulse - 1.0).abs() < 1e-5);

        // Pause (empty slice): envelopes decay cleanly without NaNs
        let mut paused = hit;
        for _ in 0..60 {
            paused = dsp.update(&[], sr, 1.0 / 60.0);
        }
        assert!(paused.bass < 0.05 && paused.beat_pulse < 0.01);
        assert!(paused.music_time.is_finite());
    }

    #[test]
    fn stereo_vu_meter_tracks_left_and_right_channels_independently() {
        let sr = 44_100;
        let mut dsp = VizDsp::new();
        let mut left_only = Vec::with_capacity(1024 * 2);
        for i in 0..1024 {
            let s = ((TAU * 440.0 * (i as f32) / (sr as f32)).sin() * 0.65 * 32767.0) as i16;
            left_only.push(s);
            left_only.push(0);
        }
        let mut frame = VizAudioFrame::default();
        for _ in 0..20 {
            frame = dsp.update(&left_only, sr, 1.0 / 60.0);
        }
        assert!(
            frame.vu_l > 0.55 && frame.vu_r < 0.02,
            "expected Left VU deflection only, got L={} R={}",
            frame.vu_l,
            frame.vu_r
        );

        let mut right_only = Vec::with_capacity(1024 * 2);
        for i in 0..1024 {
            let s = ((TAU * 440.0 * (i as f32) / (sr as f32)).sin() * 0.65 * 32767.0) as i16;
            right_only.push(0);
            right_only.push(s);
        }
        for _ in 0..35 {
            frame = dsp.update(&right_only, sr, 1.0 / 60.0);
        }
        assert!(
            frame.vu_r > 0.55 && frame.vu_l < 0.05,
            "expected Right VU deflection only, got L={} R={}",
            frame.vu_l,
            frame.vu_r
        );
    }
}
