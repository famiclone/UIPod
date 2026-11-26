//! `DjVinylSync` 4-point Catmull-Rom / Hermite cubic stereo pitch-fader resampler.
//!
//! Emulates a Technics SL-1200 analog pitch slider (`±6%..±12%`) with zero WSOLA
//! windowing overhead and bit-exact passthrough when `|tempo_ratio - 1.0| < 0.0005`.

#[derive(Clone, Debug)]
pub struct VinylResampler {
    /// Fractional position `[0.0, 1.0)` between `hist[1]` and `hist[2]`.
    frac: f64,
    hist_l: [f32; 4],
    hist_r: [f32; 4],
    primed: usize,
    eof_pushed: u8,
    /// Last two frames emitted during `1.0x` unity bypass so engaging pitch bend is C1-continuous.
    warm_l: [f32; 2],
    warm_r: [f32; 2],
    warm_count: u8,
}

impl Default for VinylResampler {
    fn default() -> Self {
        Self::new()
    }
}

impl VinylResampler {
    pub fn new() -> Self {
        Self {
            frac: 0.0,
            hist_l: [0.0; 4],
            hist_r: [0.0; 4],
            primed: 0,
            eof_pushed: 0,
            warm_l: [0.0; 2],
            warm_r: [0.0; 2],
            warm_count: 0,
        }
    }

    pub fn reset(&mut self) {
        self.frac = 0.0;
        self.hist_l = [0.0; 4];
        self.hist_r = [0.0; 4];
        self.primed = 0;
        self.eof_pushed = 0;
        self.warm_l = [0.0; 2];
        self.warm_r = [0.0; 2];
        self.warm_count = 0;
    }

    #[inline]
    pub fn is_bypass_ready(&self) -> bool {
        self.primed == 0 && self.frac == 0.0
    }

    /// Upper bound on input frames required to produce `out_frames` at `tempo_ratio`.
    #[inline]
    pub fn needed_input_frames(&self, out_frames: usize, tempo_ratio: f32) -> usize {
        let ratio = (tempo_ratio as f64).clamp(0.5, 2.0);
        if (ratio - 1.0).abs() < 0.0005 && self.is_bypass_ready() {
            return out_frames;
        }
        let prime_need = 4usize.saturating_sub(self.primed);
        let step_need = ((out_frames as f64) * ratio + self.frac).ceil() as usize;
        prime_need + step_need + 4
    }

    /// Exact number of already-consumed input frames still buffered inside the 4-tap Hermite history.
    #[inline]
    pub fn unplayed_input_frames(&self) -> f64 {
        if self.primed == 0 {
            0.0
        } else if self.primed < 4 {
            (self.primed.saturating_sub(1)) as f64
        } else {
            (2.0 - (self.eof_pushed as f64) - self.frac).max(0.0)
        }
    }

    #[inline]
    fn push_input_frame(&mut self, l: f32, r: f32) {
        self.hist_l[0] = self.hist_l[1];
        self.hist_l[1] = self.hist_l[2];
        self.hist_l[2] = self.hist_l[3];
        self.hist_l[3] = l;

        self.hist_r[0] = self.hist_r[1];
        self.hist_r[1] = self.hist_r[2];
        self.hist_r[2] = self.hist_r[3];
        self.hist_r[3] = r;

        if self.primed < 4 {
            self.primed += 1;
        }
    }

    #[inline]
    fn hermite4(y0: f32, y1: f32, y2: f32, y3: f32, t: f32) -> f32 {
        let c0 = y1;
        let c1 = 0.5 * (y2 - y0);
        let c2 = y0 - 2.5 * y1 + 2.0 * y2 - 0.5 * y3;
        let c3 = 0.5 * (y3 - y0) + 1.5 * (y1 - y2);
        ((c3 * t + c2) * t + c1) * t + c0
    }

    /// Pull up to `out_frames` stereo frames from `src_interleaved` at `tempo_ratio`.
    /// Returns `(input_frames_consumed, output_frames_written)`.
    pub fn process(
        &mut self,
        src_interleaved: &[f32],
        tempo_ratio: f32,
        out_interleaved: &mut [f32],
    ) -> (usize, usize) {
        self.process_with_eof(src_interleaved, tempo_ratio, false, out_interleaved)
    }

    /// Pull up to `out_frames` stereo frames from `src_interleaved` at `tempo_ratio`,
    /// optionally draining the 2-sample Hermite lookahead tail when `is_eof` is `true`.
    pub fn process_with_eof(
        &mut self,
        src_interleaved: &[f32],
        tempo_ratio: f32,
        is_eof: bool,
        out_interleaved: &mut [f32],
    ) -> (usize, usize) {
        let in_avail = src_interleaved.len() / 2;
        let out_cap = out_interleaved.len() / 2;
        if out_cap == 0 || (in_avail == 0 && (!is_eof || self.primed == 0 || self.eof_pushed >= 2))
        {
            return (0, 0);
        }

        let ratio = (tempo_ratio as f64).clamp(0.5, 2.0);
        let unity = (ratio - 1.0).abs() < 0.0005;

        // Bit-exact unity fast-path when unprimed/aligned and `|ratio - 1.0| < 0.0005`
        if unity && self.primed == 0 && self.frac == 0.0 {
            let n = in_avail.min(out_cap);
            out_interleaved[..n * 2].copy_from_slice(&src_interleaved[..n * 2]);
            if n >= 2 {
                self.warm_l = [
                    src_interleaved[(n - 2) * 2],
                    src_interleaved[(n - 1) * 2],
                ];
                self.warm_r = [
                    src_interleaved[(n - 2) * 2 + 1],
                    src_interleaved[(n - 1) * 2 + 1],
                ];
                self.warm_count = 2;
            } else if n == 1 {
                self.warm_l = [self.warm_l[1], src_interleaved[0]];
                self.warm_r = [self.warm_r[1], src_interleaved[1]];
                self.warm_count = (self.warm_count + 1).min(2);
            }
            return (n, n);
        }

        let mut in_idx = 0usize;
        let mut out_idx = 0usize;

        if self.primed == 0 && self.warm_count >= 2 {
            // Seed from last 2 bypass frames so `1.0x -> ratio` starts at exact C1 phase
            self.hist_l = [0.0, 0.0, self.warm_l[0], self.warm_l[1]];
            self.hist_r = [0.0, 0.0, self.warm_r[0], self.warm_r[1]];
            self.primed = 2;
            self.frac = ratio;
            self.warm_count = 0;
        }

        let cold_start = self.primed == 0;
        while self.primed < 4 && in_idx < in_avail {
            let l = src_interleaved[in_idx * 2];
            let r = src_interleaved[in_idx * 2 + 1];
            if self.primed == 0 {
                self.hist_l = [l, l, l, l];
                self.hist_r = [r, r, r, r];
                self.primed = 2;
                in_idx += 1;
            } else {
                self.push_input_frame(l, r);
                in_idx += 1;
                if cold_start && self.primed == 4 {
                    self.hist_l[0] = 2.0 * self.hist_l[1] - self.hist_l[2];
                    self.hist_r[0] = 2.0 * self.hist_r[1] - self.hist_r[2];
                }
            }
        }

        if self.primed < 4 {
            if is_eof {
                while self.primed < 4 && self.primed >= 2 && self.eof_pushed < 2 {
                    let l = self.hist_l[3];
                    let r = self.hist_r[3];
                    self.push_input_frame(l, r);
                    self.eof_pushed += 1;
                }
            }
            if self.primed < 4 {
                return (in_idx, 0);
            }
        }

        while out_idx < out_cap {
            while self.frac >= 1.0 {
                if in_idx < in_avail {
                    self.push_input_frame(
                        src_interleaved[in_idx * 2],
                        src_interleaved[in_idx * 2 + 1],
                    );
                    in_idx += 1;
                    self.frac -= 1.0;
                } else if is_eof && self.eof_pushed < 2 {
                    let l = self.hist_l[3];
                    let r = self.hist_r[3];
                    self.push_input_frame(l, r);
                    self.eof_pushed += 1;
                    self.frac -= 1.0;
                } else {
                    return (in_idx, out_idx);
                }
            }

            if is_eof && in_idx >= in_avail && self.eof_pushed >= 2 {
                break;
            }

            let t = self.frac as f32;
            out_interleaved[out_idx * 2] = Self::hermite4(
                self.hist_l[0],
                self.hist_l[1],
                self.hist_l[2],
                self.hist_l[3],
                t,
            );
            out_interleaved[out_idx * 2 + 1] = Self::hermite4(
                self.hist_r[0],
                self.hist_r[1],
                self.hist_r[2],
                self.hist_r[3],
                t,
            );
            out_idx += 1;
            self.frac += ratio;
        }

        (in_idx, out_idx)
    }
}
