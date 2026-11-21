//! Pure-Rust, zero-per-frame-allocation band-limited step buffer (`BlipBuf`).
//!
//! Converts cycle-accurate 1.789773 MHz / 1.662607 MHz APU amplitude deltas into
//! alias-free PCM samples at any target output rate (e.g. 44100 Hz or 48000 Hz)
//! using a 32-phase, 16-tap Blackman-windowed sinc impulse bank.

use std::f64::consts::PI;

const PHASE_BITS: u32 = 24;
const KERNEL_PHASE_BITS: u32 = 5;
const KERNEL_PHASES: usize = 1 << KERNEL_PHASE_BITS; // 32 sub-sample phases
const KERNEL_WIDTH: usize = 16;
const KERNEL_UNIT: i64 = 1 << 14; // 16384 fixed-point kernel sum

/// Precomputed 32-phase x 16-tap windowed-sinc impulse table.
/// Every phase row sums to exact `KERNEL_UNIT` (16384) to guarantee zero DC drift.
#[derive(Clone)]
struct SincTable {
    taps: [[i16; KERNEL_WIDTH]; KERNEL_PHASES],
}

impl SincTable {
    fn build() -> Self {
        let mut taps = [[0i16; KERNEL_WIDTH]; KERNEL_PHASES];
        let cutoff = 0.86_f64; // Normalized to Nyquist (0.43 * sample_rate)
        let half = (KERNEL_WIDTH as f64) * 0.5;

        for (phase, row) in taps.iter_mut().enumerate() {
            let frac = (phase as f64) / (KERNEL_PHASES as f64);
            let mut weights = [0.0_f64; KERNEL_WIDTH];
            let mut sum = 0.0_f64;

            for (k, w_slot) in weights.iter_mut().enumerate() {
                // Center of impulse shifts smoothly from 7.5 to 8.5 as frac goes 0..1
                let x = (k as f64) - (half - 0.5) - frac;
                let sinc = if x.abs() < 1e-9 {
                    cutoff
                } else {
                    (PI * cutoff * x).sin() / (PI * x)
                };
                // Blackman window over [-half, +half]
                let win_pos = ((k as f64) + 0.5 - frac + 0.5) / ((KERNEL_WIDTH as f64) + 1.0);
                let win = 0.42 - 0.5 * (2.0 * PI * win_pos).cos()
                    + 0.08 * (4.0 * PI * win_pos).cos();
                let val = sinc * win.max(0.0);
                *w_slot = val;
                sum += val;
            }

            let mut int_sum = 0i64;
            for k in 0..KERNEL_WIDTH {
                let q = ((weights[k] / sum) * (KERNEL_UNIT as f64)).round() as i16;
                row[k] = q;
                int_sum += q as i64;
            }
            // Compensate rounding residual on center tap so row sums to exact KERNEL_UNIT
            let residual = (KERNEL_UNIT - int_sum) as i16;
            row[KERNEL_WIDTH / 2 - 1] = row[KERNEL_WIDTH / 2 - 1].saturating_add(residual);
        }

        Self { taps }
    }
}

/// Band-limited step buffer with pre-allocated difference storage.
#[derive(Clone)]
pub(crate) struct BlipBuf {
    factor: u64,
    offset_fixed: u64,
    buf: Vec<i64>,
    avail_samples: usize,
    integrator: i64,
    hp_shift: u32,
    kernel: SincTable,
}

impl BlipBuf {
    pub fn new(cpu_clock_hz: u32, sample_rate: u32, max_buffered_samples: usize) -> Self {
        let safe_rate = sample_rate.clamp(8_000, 192_000);
        let safe_clock = cpu_clock_hz.max(100_000);
        let factor = ((safe_rate as u64) << PHASE_BITS) / (safe_clock as u64);
        let cap = max_buffered_samples.max(2048) + KERNEL_WIDTH + 8;
        // ~15 Hz high-pass time constant scaled to output sample rate
        let hp_shift = if safe_rate >= 64_000 { 10 } else { 9 };

        Self {
            factor,
            offset_fixed: 0,
            buf: vec![0i64; cap],
            avail_samples: 0,
            integrator: 0,
            hp_shift,
            kernel: SincTable::build(),
        }
    }

    pub fn clear(&mut self) {
        self.offset_fixed = 0;
        self.avail_samples = 0;
        self.integrator = 0;
        self.buf.fill(0);
    }

    #[inline]
    pub fn samples_avail(&self) -> usize {
        self.avail_samples
    }

    /// Add a step amplitude change `delta` at `clock_time` CPU cycles into the current frame.
    #[inline]
    pub fn add_delta(&mut self, clock_time: u32, delta: i32) {
        if delta == 0 {
            return;
        }
        let fixed = self.offset_fixed + (clock_time as u64) * self.factor;
        let pos = (fixed >> PHASE_BITS) as usize;
        if pos + KERNEL_WIDTH >= self.buf.len() {
            return;
        }
        let phase =
            ((fixed >> (PHASE_BITS - KERNEL_PHASE_BITS)) & ((KERNEL_PHASES - 1) as u64)) as usize;
        let row = &self.kernel.taps[phase];
        let d = delta as i64;
        let dst = &mut self.buf[pos..pos + KERNEL_WIDTH];
        for k in 0..KERNEL_WIDTH {
            dst[k] += d * (row[k] as i64);
        }
    }

    /// End a CPU frame of `clock_duration` cycles, making generated PCM samples available to read.
    #[inline]
    pub fn end_frame(&mut self, clock_duration: u32) {
        self.offset_fixed += (clock_duration as u64) * self.factor;
        let new_avail = (self.offset_fixed >> PHASE_BITS) as usize;
        let max_avail = self.buf.len().saturating_sub(KERNEL_WIDTH + 1);
        self.avail_samples = new_avail.min(max_avail);
    }

    /// Read up to `out.len()` mono `i16` samples, applying `gain_q15` (`32768 == 1.0`).
    pub fn read_samples(&mut self, out: &mut [i16], gain_q15: i32) -> usize {
        let count = out.len().min(self.avail_samples);
        if count == 0 {
            return 0;
        }

        let mut acc = self.integrator;
        let shift = self.hp_shift;
        let gain = gain_q15 as i64;

        for (i, sample_slot) in out.iter_mut().take(count).enumerate() {
            acc += self.buf[i];
            let raw = acc >> 14;
            // First-order DC blocker
            acc -= acc >> shift;
            let scaled = (raw * gain) >> 15;
            *sample_slot = scaled.clamp(i16::MIN as i64, i16::MAX as i64) as i16;
        }

        self.integrator = acc;
        let rem = self.avail_samples - count;
        let tail_len = rem + KERNEL_WIDTH + 1;
        self.buf.copy_within(count..count + tail_len, 0);
        self.buf[tail_len..tail_len + count].fill(0);
        self.avail_samples = rem;
        self.offset_fixed -= (count as u64) << PHASE_BITS;

        count
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kernel_rows_sum_to_exact_unit() {
        let table = SincTable::build();
        for (phase, row) in table.taps.iter().enumerate() {
            let sum: i64 = row.iter().map(|&x| x as i64).sum();
            assert_eq!(sum, KERNEL_UNIT, "phase {phase} kernel sum mismatch");
        }
    }

    #[test]
    fn synthesizes_square_wave_at_44100_and_48000() {
        for &rate in &[44_100u32, 48_000u32] {
            let mut blip = BlipBuf::new(1_789_773, rate, 4096);
            // 440 Hz square wave toggles every ~2034 CPU cycles
            let mut high = false;
            let mut t = 0u32;
            while t < 29_828 {
                high = !high;
                blip.add_delta(t, if high { 8000 } else { -8000 });
                t += 2034;
            }
            blip.end_frame(29_828);
            assert!(blip.samples_avail() > 700);
            let mut pcm = [0i16; 1024];
            let n = blip.read_samples(&mut pcm, 32768);
            assert!(n > 700);
            let peak = pcm[..n].iter().map(|s| s.unsigned_abs()).max().unwrap_or(0);
            assert!(peak > 2000, "expected audible band-limited wave, got peak={peak}");
        }
    }
}
