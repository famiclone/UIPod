//! Lightweight pitch-preserving stereo phase-locked WSOLA (Waveform Similarity Overlap-Add)
//! time-stretcher for `DjMasterTempo`.
//!
//! - Uses a ~23.2 ms (`1024`-frame at 44.1/48 kHz) periodic Hann COLA window (`Hs = 512`,
//!   exact `w[n] + w[n + Hs] == 1.0` unity gain reconstruction).
//! - Coarse-to-fine normalized cross-correlation search (`±10 ms`) on mono-summed `(L+R)`
//!   locks kick transients and bass phase identically across L & R channels.
//! - Zero heap allocations during audio rendering; automatic bit-exact passthrough bypass
//!   when `|tempo_ratio - 1.0| < 0.0005`.

use std::f32::consts::PI;

pub struct WsolaStretcher {
    win_len: usize,
    hop_s: usize,
    search_radius: usize,
    hann: Vec<f32>,

    /// Sliding stereo input FIFO (`[L, R, ...]`).
    in_buf: Vec<f32>,
    /// Number of valid stereo frames currently stored in `in_buf` (`0..=in_cap`), including EOF pad.
    in_frames: usize,
    /// Number of real (non-EOF-padded) stereo frames currently stored in `in_buf`.
    real_in_frames: usize,
    /// Ideal fractional input cursor relative to index `0` of `in_buf`.
    ideal_pos: f64,
    /// Actual start index of the most recently synthesized grain (or `None` before first grain).
    last_grain_pos: Option<usize>,

    /// Overlap-add stereo accumulator (`win_len` stereo frames).
    ola_buf: Vec<f32>,
    /// Ready-to-read stereo output FIFO (`hop_s * 4` capacity).
    out_fifo: Vec<f32>,
    out_read: usize,
    out_len: usize,
    bypass_active: bool,
}

impl WsolaStretcher {
    pub fn new(sample_rate: u32) -> Self {
        let sr = sample_rate.clamp(8_000, 192_000) as usize;
        // ~23-24 ms even window length (1024 @ 44.1/48k, 2048 @ 96k)
        let raw_w = ((sr * 24) / 1000).next_power_of_two().clamp(256, 4096);
        let win_len = raw_w & !1;
        let hop_s = win_len / 2;
        // ~10 ms search window (`±448` frames @ 44.1 kHz)
        let search_radius = ((sr * 10) / 1000).clamp(64, win_len);

        // Periodic Hann window satisfies exact COLA: `hann[i] + hann[i + hop_s] == 1.0`
        let mut hann = vec![0.0f32; win_len];
        for (i, w) in hann.iter_mut().enumerate() {
            *w = 0.5 * (1.0 - (2.0 * PI * (i as f32) / (win_len as f32)).cos());
        }

        let in_cap = win_len * 8;
        let out_cap = hop_s * 6;
        Self {
            win_len,
            hop_s,
            search_radius,
            hann,
            in_buf: vec![0.0; in_cap * 2],
            in_frames: 0,
            real_in_frames: 0,
            ideal_pos: 0.0,
            last_grain_pos: None,
            ola_buf: vec![0.0; win_len * 2],
            out_fifo: vec![0.0; out_cap * 2],
            out_read: 0,
            out_len: 0,
            bypass_active: true,
        }
    }

    pub fn reset(&mut self) {
        self.in_frames = 0;
        self.real_in_frames = 0;
        self.ideal_pos = 0.0;
        self.last_grain_pos = None;
        self.ola_buf.fill(0.0);
        self.out_read = 0;
        self.out_len = 0;
        self.bypass_active = true;
    }

    #[inline]
    pub fn available_output_frames(&self) -> usize {
        self.out_len - self.out_read
    }

    /// Exact number of real input frames already ingested into WSOLA (`in_buf` + `out_fifo`)
    /// that have not yet been consumed by the caller.
    #[inline]
    pub fn unplayed_input_frames(&self, tempo_ratio: f32) -> f64 {
        let ratio = (tempo_ratio as f64).clamp(0.5, 2.0);
        let in_unplayed = (self.real_in_frames as f64 - self.ideal_pos).max(0.0);
        in_unplayed + (self.available_output_frames() as f64) * ratio
    }

    /// Push interleaved stereo frames into the internal input window.
    /// Returns how many stereo frames were accepted.
    pub fn push_input(&mut self, src_interleaved: &[f32]) -> usize {
        let src_frames = src_interleaved.len() / 2;
        let cap_frames = self.in_buf.len() / 2;
        let space = cap_frames.saturating_sub(self.in_frames);
        let n = src_frames.min(space);
        if n > 0 {
            let dst_off = self.in_frames * 2;
            self.in_buf[dst_off..dst_off + n * 2].copy_from_slice(&src_interleaved[..n * 2]);
            self.in_frames += n;
            self.real_in_frames += n;
        }
        n
    }

    fn pad_eof_zeros(&mut self, needed_in_frames: usize) {
        if self.in_frames >= needed_in_frames {
            return;
        }
        let cap_frames = self.in_buf.len() / 2;
        if needed_in_frames > cap_frames {
            let safe_drop = (self.ideal_pos.floor().max(0.0) as usize)
                .saturating_sub(self.search_radius + 8)
                .min(self.last_grain_pos.unwrap_or(usize::MAX));
            if safe_drop > 0 {
                self.discard_input_prefix(safe_drop);
            }
        }
        let target = needed_in_frames.min(cap_frames);
        if target > self.in_frames {
            self.in_buf[self.in_frames * 2..target * 2].fill(0.0);
            self.in_frames = target;
        }
    }

    #[inline]
    fn discard_input_prefix(&mut self, drop_frames: usize) {
        if drop_frames == 0 {
            return;
        }
        let drop = drop_frames.min(self.in_frames);
        let rem = self.in_frames - drop;
        if rem > 0 {
            self.in_buf.copy_within(drop * 2..(drop + rem) * 2, 0);
        }
        self.in_frames = rem;
        self.real_in_frames = self.real_in_frames.saturating_sub(drop);
        self.ideal_pos = (self.ideal_pos - drop as f64).max(0.0);
        if let Some(prev) = self.last_grain_pos {
            self.last_grain_pos = Some(prev.saturating_sub(drop));
        }
    }

    #[inline]
    fn mono_at(&self, frame_idx: usize) -> f32 {
        let base = frame_idx * 2;
        (self.in_buf[base] + self.in_buf[base + 1]) * 0.5
    }

    /// Normalized cross-correlation score between `ref_pos..ref_pos+corr_len` and `cand_pos..cand_pos+corr_len`
    /// evaluated with `stride` (`2` during coarse pass, `1` during fine pass).
    #[inline]
    fn xcorr_score(&self, ref_pos: usize, cand_pos: usize, corr_len: usize, stride: usize) -> f32 {
        let mut dot = 0.0f32;
        let mut e_cand = 1e-9f32;
        let mut i = 0usize;
        while i < corr_len {
            let r = self.mono_at(ref_pos + i);
            let c = self.mono_at(cand_pos + i);
            dot += r * c;
            e_cand += c * c;
            i += stride;
        }
        dot / e_cand.sqrt()
    }

    fn find_best_grain_start(&self, center: usize, ref_pos: usize) -> usize {
        let w = self.win_len;
        let corr_len = self.hop_s;
        if self.in_frames <= w || ref_pos + corr_len > self.in_frames {
            return center.min(self.in_frames.saturating_sub(w));
        }

        // Check if reference segment is near-silent: skip search and pick exact `center`
        let mut ref_energy = 0.0f32;
        let mut k = 0usize;
        while k < corr_len {
            let s = self.mono_at(ref_pos + k);
            ref_energy += s * s;
            k += 4;
        }
        let max_start = self.in_frames - w;
        let clamped_center = center.min(max_start);
        if ref_energy < 1e-8 {
            return clamped_center;
        }

        let min_cand = clamped_center.saturating_sub(self.search_radius);
        let max_cand = (clamped_center + self.search_radius).min(max_start);
        if min_cand >= max_cand {
            return clamped_center;
        }

        // Stage 1: Coarse search (step = 4 frames, correlation stride = 2)
        let mut best_pos = clamped_center;
        let mut best_score = self.xcorr_score(ref_pos, clamped_center, corr_len, 2) + 1e-5; // tiny tie-breaker toward ideal center
        let mut cand = min_cand;
        while cand <= max_cand {
            let sc = self.xcorr_score(ref_pos, cand, corr_len, 2);
            if sc > best_score {
                best_score = sc;
                best_pos = cand;
            }
            cand += 4;
        }

        // Stage 2: Fine sample-accurate refinement (`±4` frames around `best_pos`, stride = 1)
        let fine_lo = best_pos.saturating_sub(4).max(min_cand);
        let fine_hi = (best_pos + 4).min(max_cand);
        let mut fine_best_pos = best_pos;
        let mut fine_best_score = f32::NEG_INFINITY;
        for fpos in fine_lo..=fine_hi {
            let sc = self.xcorr_score(ref_pos, fpos, corr_len, 1);
            if sc > fine_best_score {
                fine_best_score = sc;
                fine_best_pos = fpos;
            }
        }

        fine_best_pos
    }

    fn synthesize_one_hop(&mut self, tempo_ratio: f64, is_eof: bool) -> bool {
        let w = self.win_len;
        let hs = self.hop_s;

        if is_eof && self.ideal_pos >= self.real_in_frames as f64 {
            return false;
        }

        // Compact output FIFO if needed
        if self.out_read > 0 {
            let avail = self.out_len - self.out_read;
            if avail > 0 {
                self.out_fifo
                    .copy_within(self.out_read * 2..self.out_len * 2, 0);
            }
            self.out_read = 0;
            self.out_len = avail;
        }
        let out_cap_frames = self.out_fifo.len() / 2;
        if self.out_len + hs > out_cap_frames {
            return false;
        }

        let center = self.ideal_pos.round().max(0.0) as usize;
        // Require `center + search_radius + w` frames so correlation window isn't truncated
        let needed = center + self.search_radius + w;
        if self.in_frames < needed {
            if is_eof && self.real_in_frames > 0 {
                self.pad_eof_zeros(needed);
            }
            if self.in_frames < needed {
                return false;
            }
        }

        let chosen_pos = match self.last_grain_pos {
            None => {
                // Seed first half of OLA accumulator with unwindowed left half so t=0 has no fade-in dip!
                let p0 = center.min(self.in_frames - w);
                for i in 0..hs {
                    let g = 1.0 - self.hann[i];
                    self.ola_buf[i * 2] = self.in_buf[(p0 + i) * 2] * g;
                    self.ola_buf[i * 2 + 1] = self.in_buf[(p0 + i) * 2 + 1] * g;
                }
                p0
            }
            Some(prev_p) => {
                let natural_ref = prev_p + hs;
                self.find_best_grain_start(center, natural_ref)
            }
        };

        // Window and overlap-add `chosen_pos..chosen_pos+w` into `ola_buf`
        for i in 0..w {
            let win = self.hann[i];
            let src_base = (chosen_pos + i) * 2;
            let dst_base = i * 2;
            self.ola_buf[dst_base] += self.in_buf[src_base] * win;
            self.ola_buf[dst_base + 1] += self.in_buf[src_base + 1] * win;
        }

        // Emit completed first `hs` stereo frames into `out_fifo`
        let dst_off = self.out_len * 2;
        self.out_fifo[dst_off..dst_off + hs * 2].copy_from_slice(&self.ola_buf[..hs * 2]);
        self.out_len += hs;

        // Shift second half of `ola_buf` down by `hs` and zero tail
        self.ola_buf.copy_within(hs * 2..w * 2, 0);
        self.ola_buf[hs * 2..w * 2].fill(0.0);

        self.last_grain_pos = Some(chosen_pos);
        self.ideal_pos += (hs as f64) * tempo_ratio;

        // Discard old input safely behind both `ideal_pos - search_radius` and `chosen_pos`
        let min_keep = (self.ideal_pos.floor().max(0.0) as usize)
            .saturating_sub(self.search_radius + 8)
            .min(chosen_pos);
        if min_keep >= hs {
            self.discard_input_prefix(min_keep);
        }

        true
    }

    /// Process streaming interleaved stereo input at `tempo_ratio` (`> 1.0` faster, `< 1.0` slower).
    /// Returns `(input_frames_consumed, output_frames_written)`.
    pub fn process(
        &mut self,
        src_interleaved: &[f32],
        tempo_ratio: f32,
        out_interleaved: &mut [f32],
    ) -> (usize, usize) {
        self.process_with_eof(src_interleaved, tempo_ratio, false, out_interleaved)
    }

    /// Process streaming interleaved stereo input at `tempo_ratio`, optionally draining
    /// the final lookahead window seamlessly when `is_eof` is `true`.
    pub fn process_with_eof(
        &mut self,
        src_interleaved: &[f32],
        tempo_ratio: f32,
        is_eof: bool,
        out_interleaved: &mut [f32],
    ) -> (usize, usize) {
        let out_cap = out_interleaved.len() / 2;
        if out_cap == 0 {
            return (0, 0);
        }

        let ratio = (tempo_ratio as f64).clamp(0.5, 2.0);
        let unity = (ratio - 1.0).abs() < 0.0005;

        // Fast-path bit-exact passthrough (`0%` CPU overhead!) when `|ratio - 1.0| < 0.0005`
        if unity && self.bypass_active && self.available_output_frames() == 0 && self.in_frames == 0
        {
            let in_avail = src_interleaved.len() / 2;
            let n = in_avail.min(out_cap);
            out_interleaved[..n * 2].copy_from_slice(&src_interleaved[..n * 2]);
            return (n, n);
        }
        self.bypass_active = false;

        let mut total_in = 0usize;
        let mut total_out = 0usize;

        loop {
            // 1. Drain any ready output frames first
            let ready = self.available_output_frames();
            if ready > 0 && total_out < out_cap {
                let take = ready.min(out_cap - total_out);
                let src_off = self.out_read * 2;
                let dst_off = total_out * 2;
                out_interleaved[dst_off..dst_off + take * 2]
                    .copy_from_slice(&self.out_fifo[src_off..src_off + take * 2]);
                self.out_read += take;
                total_out += take;
                if self.out_read == self.out_len {
                    self.out_read = 0;
                    self.out_len = 0;
                }
            }

            if total_out >= out_cap {
                break;
            }

            // 2. Top up input FIFO from `src_interleaved`
            if total_in * 2 < src_interleaved.len() {
                let pushed = self.push_input(&src_interleaved[total_in * 2..]);
                total_in += pushed;
            }

            let eof_ready = is_eof && total_in * 2 >= src_interleaved.len();

            // 3. Synthesize next hop if enough input is buffered (or zero-padded at EOF)
            if !self.synthesize_one_hop(ratio, eof_ready) {
                if total_in * 2 < src_interleaved.len() {
                    let pushed = self.push_input(&src_interleaved[total_in * 2..]);
                    total_in += pushed;
                    let eof_now = is_eof && total_in * 2 >= src_interleaved.len();
                    if pushed > 0 && self.synthesize_one_hop(ratio, eof_now) {
                        continue;
                    }
                }
                break;
            }
        }

        (total_in, total_out)
    }

    /// Convenience helper for offline/test stretching of a complete stereo buffer.
    pub fn stretch_offline(&mut self, src_interleaved: &[f32], tempo_ratio: f32) -> Vec<f32> {
        self.reset();
        let in_frames = src_interleaved.len() / 2;
        if in_frames == 0 {
            return Vec::new();
        }
        // Append zero-padding so `synthesize_one_hop` has full lookahead right up to `in_frames`
        let pad_frames = self.win_len * 2 + self.search_radius * 2;
        let mut padded = Vec::with_capacity((in_frames + pad_frames) * 2);
        padded.extend_from_slice(src_interleaved);
        padded.resize((in_frames + pad_frames) * 2, 0.0);

        let expected_out = ((in_frames as f64) / (tempo_ratio as f64).clamp(0.5, 2.0)).round()
            as usize;
        let mut out = Vec::with_capacity((expected_out + self.win_len) * 2);
        let mut chunk_out = vec![0.0f32; 1024 * 2];
        let mut pos = 0usize;

        while (out.len() / 2) < expected_out {
            let slice = if pos * 2 < padded.len() {
                let end = ((pos + 2048) * 2).min(padded.len());
                &padded[pos * 2..end]
            } else {
                &[][..]
            };
            let (c_in, c_out) = self.process(slice, tempo_ratio, &mut chunk_out);
            pos += c_in;
            if c_out > 0 {
                let remain = expected_out.saturating_sub(out.len() / 2);
                let keep = c_out.min(remain);
                out.extend_from_slice(&chunk_out[..keep * 2]);
            }
            if c_in == 0 && c_out == 0 {
                break;
            }
        }

        out
    }
}
