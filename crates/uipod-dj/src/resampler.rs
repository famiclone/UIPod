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
        }
    }

    pub fn reset(&mut self) {
        self.frac = 0.0;
        self.hist_l = [0.0; 4];
        self.hist_r = [0.0; 4];
        self.primed = 0;
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
        let in_avail = src_interleaved.len() / 2;
        let out_cap = out_interleaved.len() / 2;
        if out_cap == 0 || in_avail == 0 {
            return (0, 0);
        }

        let ratio = (tempo_ratio as f64).clamp(0.5, 2.0);

        // Bit-exact unity fast-path when unprimed/aligned and `|ratio - 1.0| < 0.0005`
        if (ratio - 1.0).abs() < 0.0005 && self.primed == 0 && self.frac == 0.0 {
            let n = in_avail.min(out_cap);
            out_interleaved[..n * 2].copy_from_slice(&src_interleaved[..n * 2]);
            return (n, n);
        }

        let mut in_idx = 0usize;
        let mut out_idx = 0usize;

        while self.primed < 4 && in_idx < in_avail {
            let l = src_interleaved[in_idx * 2];
            let r = src_interleaved[in_idx * 2 + 1];
            if self.primed == 0 {
                // Duplicate first sample for causal boundary continuity
                self.hist_l = [l, l, l, l];
                self.hist_r = [r, r, r, r];
                self.primed = 3;
                in_idx += 1;
            } else {
                self.push_input_frame(l, r);
                in_idx += 1;
            }
        }

        if self.primed < 4 {
            return (in_idx, 0);
        }

        while out_idx < out_cap {
            while self.frac >= 1.0 {
                if in_idx >= in_avail {
                    return (in_idx, out_idx);
                }
                self.push_input_frame(
                    src_interleaved[in_idx * 2],
                    src_interleaved[in_idx * 2 + 1],
                );
                in_idx += 1;
                self.frac -= 1.0;
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
