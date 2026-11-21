//! Ricoh 2A03 / 2A07 APU emulation with non-linear DAC mixing, click-free
//! per-channel muting, cycle-skipping oscillator stepping, and 60 Hz telemetry.

use crate::exp_mmc5::LENGTH_TABLE;
use crate::ChannelState;

const GAIN_MAX: i32 = 32_768;
const SLEW_STEP_CYCLES: u32 = 64;
const SLEW_DELTA_Q15: i32 = 1_024;
const DAC_SCALE: f64 = 29_500.0;

const DUTY_TABLE: [[u8; 8]; 4] = [
    [0, 1, 0, 0, 0, 0, 0, 0], // 12.5%
    [0, 1, 1, 0, 0, 0, 0, 0], // 25%
    [0, 1, 1, 1, 1, 0, 0, 0], // 50%
    [1, 0, 0, 1, 1, 1, 1, 1], // 75%
];

const TRIANGLE_SEQ: [u8; 32] = [
    15, 14, 13, 12, 11, 10, 9, 8, 7, 6, 5, 4, 3, 2, 1, 0, 0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11,
    12, 13, 14, 15,
];

const NOISE_PERIOD_NTSC: [u16; 16] = [
    4, 8, 16, 32, 64, 96, 128, 160, 202, 254, 380, 508, 762, 1016, 2034, 4068,
];
const NOISE_PERIOD_PAL: [u16; 16] = [
    4, 8, 14, 30, 60, 88, 118, 148, 188, 236, 354, 472, 708, 944, 1890, 3778,
];

const DMC_RATE_NTSC: [u16; 16] = [
    428, 380, 340, 320, 286, 254, 226, 214, 190, 160, 142, 128, 106, 84, 72, 54,
];
const DMC_RATE_PAL: [u16; 16] = [
    398, 354, 316, 298, 276, 236, 210, 198, 176, 148, 132, 118, 98, 78, 66, 50,
];

#[derive(Clone)]
struct MixerLut {
    pulse: [i32; 32],
    tnd: [i32; 204],
}

impl MixerLut {
    fn build() -> Self {
        let mut pulse = [0i32; 32];
        for (n, slot) in pulse.iter_mut().enumerate().skip(1).take(30) {
            let v = 95.88 / (8128.0 / (n as f64) + 100.0);
            *slot = (v * DAC_SCALE).round() as i32;
        }
        pulse[31] = pulse[30];

        let mut tnd = [0i32; 204];
        for (n, slot) in tnd.iter_mut().enumerate().skip(1).take(202) {
            // Blargg unified TND index: 3*tri + 2*noise + dmc (22638 / 8227 ≈ 2.75, 22638 / 12241 ≈ 1.85)
            let denom = (n as f64) / 22_638.0;
            let v = 159.79 / (1.0 / denom + 100.0);
            *slot = (v * DAC_SCALE).round() as i32;
        }
        tnd[203] = tnd[202];

        Self { pulse, tnd }
    }

    #[inline]
    fn eval(&self, p_sum_q8: i32, tnd_sum_q8: i32) -> i32 {
        let p_clamped = p_sum_q8.clamp(0, 30 << 8);
        let p_idx = (p_clamped >> 8) as usize;
        let p_frac = p_clamped & 0xFF;
        let p_val = self.pulse[p_idx] + (((self.pulse[p_idx + 1] - self.pulse[p_idx]) * p_frac) >> 8);

        let t_clamped = tnd_sum_q8.clamp(0, 202 << 8);
        let t_idx = (t_clamped >> 8) as usize;
        let t_frac = t_clamped & 0xFF;
        let t_val = self.tnd[t_idx] + (((self.tnd[t_idx + 1] - self.tnd[t_idx]) * t_frac) >> 8);

        p_val + t_val
    }
}

#[derive(Clone, Default)]
struct PulseChan {
    is_pulse1: bool,
    enabled: bool,
    duty: u8,
    halt_loop: bool,
    constant_vol: bool,
    vol_period: u8,
    sweep_enabled: bool,
    sweep_period: u8,
    sweep_negate: bool,
    sweep_shift: u8,
    sweep_reload: bool,
    sweep_divider: u8,
    timer_period: u16,
    timer_counter: u32,
    seq_step: u8,
    length_counter: u8,
    env_start: bool,
    env_divider: u8,
    env_decay: u8,
    triggered_frame: bool,
    prev_active: bool,
    prev_env: u8,
    prev_period: u16,
    gain_q15: i32,
    target_gain_q15: i32,
}

impl PulseChan {
    fn reset(&mut self, is_pulse1: bool, muted: bool) {
        let g = if muted { 0 } else { GAIN_MAX };
        *self = Self {
            is_pulse1,
            gain_q15: g,
            target_gain_q15: g,
            ..Self::default()
        };
    }

    #[inline]
    fn sweep_target_period(&self) -> u16 {
        let change = self.timer_period >> self.sweep_shift;
        if self.sweep_negate {
            let sub = if self.is_pulse1 {
                change.saturating_add(1)
            } else {
                change
            };
            self.timer_period.saturating_sub(sub)
        } else {
            self.timer_period.saturating_add(change)
        }
    }

    #[inline]
    fn is_sweep_muted(&self) -> bool {
        self.timer_period < 8 || (!self.sweep_negate && self.sweep_target_period() > 0x07FF)
    }

    #[inline]
    fn env_output(&self) -> u8 {
        if self.constant_vol {
            self.vol_period
        } else {
            self.env_decay
        }
    }

    #[inline]
    fn is_hw_active(&self) -> bool {
        self.enabled && self.length_counter > 0 && !self.is_sweep_muted() && self.env_output() > 0
    }

    #[inline]
    fn raw_dac(&self) -> i32 {
        if !self.enabled || self.length_counter == 0 || self.is_sweep_muted() {
            return 0;
        }
        if DUTY_TABLE[self.duty as usize][self.seq_step as usize] != 0 {
            self.env_output() as i32
        } else {
            0
        }
    }

    fn clock_envelope(&mut self) {
        if self.env_start {
            self.env_start = false;
            self.env_decay = 15;
            self.env_divider = self.vol_period;
        } else if self.env_divider > 0 {
            self.env_divider -= 1;
        } else {
            self.env_divider = self.vol_period;
            if self.env_decay > 0 {
                self.env_decay -= 1;
            } else if self.halt_loop {
                self.env_decay = 15;
            }
        }
    }

    fn clock_length_and_sweep(&mut self) {
        if !self.halt_loop && self.length_counter > 0 {
            self.length_counter -= 1;
        }
        let target = self.sweep_target_period();
        let muted = self.is_sweep_muted();
        if self.sweep_divider == 0 && self.sweep_enabled && self.sweep_shift > 0 && !muted {
            self.timer_period = target;
        }
        if self.sweep_divider == 0 || self.sweep_reload {
            self.sweep_divider = self.sweep_period;
            self.sweep_reload = false;
        } else {
            self.sweep_divider -= 1;
        }
    }
}

#[derive(Clone, Default)]
struct TriangleChan {
    enabled: bool,
    control_flag: bool,
    linear_reload_val: u8,
    linear_counter: u8,
    linear_reload_flag: bool,
    timer_period: u16,
    timer_counter: u32,
    seq_step: u8,
    length_counter: u8,
    triggered_frame: bool,
    prev_active: bool,
    prev_freq_hz: f32,
    in_kick_sweep: bool,
    gain_q15: i32,
    target_gain_q15: i32,
}

impl TriangleChan {
    fn reset(&mut self, muted: bool) {
        let g = if muted { 0 } else { GAIN_MAX };
        *self = Self {
            gain_q15: g,
            target_gain_q15: g,
            ..Self::default()
        };
    }

    #[inline]
    fn is_clocking(&self) -> bool {
        self.enabled && self.length_counter > 0 && self.linear_counter > 0
    }

    #[inline]
    fn is_hw_active(&self) -> bool {
        self.is_clocking() && self.timer_period >= 2
    }

    #[inline]
    fn raw_dac(&self) -> i32 {
        if self.timer_period < 2 {
            // Ultrasonic Triangle frequencies average to neutral midpoint (7), avoiding ultrasonic buzz
            return 0;
        }
        TRIANGLE_SEQ[self.seq_step as usize] as i32
    }

    fn clock_linear(&mut self) {
        if self.linear_reload_flag {
            self.linear_counter = self.linear_reload_val;
        } else if self.linear_counter > 0 {
            self.linear_counter -= 1;
        }
        if !self.control_flag {
            self.linear_reload_flag = false;
        }
    }

    fn clock_length(&mut self) {
        if !self.control_flag && self.length_counter > 0 {
            self.length_counter -= 1;
        }
    }
}

#[derive(Clone)]
struct NoiseChan {
    enabled: bool,
    halt_loop: bool,
    constant_vol: bool,
    vol_period: u8,
    mode: bool,
    period_idx: u8,
    period_cycles: u16,
    timer_counter: u32,
    shift_reg: u16,
    length_counter: u8,
    env_start: bool,
    env_divider: u8,
    env_decay: u8,
    triggered_frame: bool,
    prev_active: bool,
    prev_env: u8,
    gain_q15: i32,
    target_gain_q15: i32,
}

impl Default for NoiseChan {
    fn default() -> Self {
        Self {
            enabled: false,
            halt_loop: false,
            constant_vol: false,
            vol_period: 0,
            mode: false,
            period_idx: 0,
            period_cycles: 4,
            timer_counter: 4,
            shift_reg: 1,
            length_counter: 0,
            env_start: false,
            env_divider: 0,
            env_decay: 0,
            triggered_frame: false,
            prev_active: false,
            prev_env: 0,
            gain_q15: GAIN_MAX,
            target_gain_q15: GAIN_MAX,
        }
    }
}

impl NoiseChan {
    fn reset(&mut self, muted: bool, is_pal: bool) {
        let g = if muted { 0 } else { GAIN_MAX };
        *self = Self {
            period_cycles: if is_pal {
                NOISE_PERIOD_PAL[0]
            } else {
                NOISE_PERIOD_NTSC[0]
            },
            shift_reg: 1,
            gain_q15: g,
            target_gain_q15: g,
            ..Self::default()
        };
    }

    #[inline]
    fn env_output(&self) -> u8 {
        if self.constant_vol {
            self.vol_period
        } else {
            self.env_decay
        }
    }

    #[inline]
    fn is_hw_active(&self) -> bool {
        self.enabled && self.length_counter > 0 && self.env_output() > 0
    }

    #[inline]
    fn raw_dac(&self) -> i32 {
        if !self.enabled || self.length_counter == 0 || (self.shift_reg & 1) != 0 {
            0
        } else {
            self.env_output() as i32
        }
    }

    fn clock_envelope(&mut self) {
        if self.env_start {
            self.env_start = false;
            self.env_decay = 15;
            self.env_divider = self.vol_period;
        } else if self.env_divider > 0 {
            self.env_divider -= 1;
        } else {
            self.env_divider = self.vol_period;
            if self.env_decay > 0 {
                self.env_decay -= 1;
            } else if self.halt_loop {
                self.env_decay = 15;
            }
        }
    }

    fn clock_length(&mut self) {
        if !self.halt_loop && self.length_counter > 0 {
            self.length_counter -= 1;
        }
    }

    #[inline]
    fn step_lfsr(&mut self) {
        let tap = if self.mode {
            (self.shift_reg >> 6) & 1
        } else {
            (self.shift_reg >> 1) & 1
        };
        let fb = (self.shift_reg & 1) ^ tap;
        self.shift_reg = (self.shift_reg >> 1) | (fb << 14);
    }
}

#[derive(Clone)]
struct DmcChan {
    irq_enable: bool,
    pub irq_pending: bool,
    loop_flag: bool,
    rate_idx: u8,
    rate_cycles: u16,
    timer_counter: u32,
    output_level: u8,
    sample_addr: u16,
    sample_length: u16,
    current_addr: u16,
    bytes_remaining: u16,
    sample_buffer: u8,
    has_sample_byte: bool,
    shift_reg: u8,
    bits_remaining: u8,
    silence_flag: bool,
    triggered_frame: bool,
    dac_writes_frame: u16,
    min_dac_frame: u8,
    max_dac_frame: u8,
    gain_q15: i32,
    target_gain_q15: i32,
}

impl Default for DmcChan {
    fn default() -> Self {
        Self {
            irq_enable: false,
            irq_pending: false,
            loop_flag: false,
            rate_idx: 0,
            rate_cycles: 428,
            timer_counter: 428,
            output_level: 0,
            sample_addr: 0xC000,
            sample_length: 1,
            current_addr: 0xC000,
            bytes_remaining: 0,
            sample_buffer: 0,
            has_sample_byte: false,
            shift_reg: 0,
            bits_remaining: 8,
            silence_flag: true,
            triggered_frame: false,
            dac_writes_frame: 0,
            min_dac_frame: 0,
            max_dac_frame: 0,
            gain_q15: GAIN_MAX,
            target_gain_q15: GAIN_MAX,
        }
    }
}

impl DmcChan {
    fn reset(&mut self, muted: bool, is_pal: bool) {
        let g = if muted { 0 } else { GAIN_MAX };
        let r0 = if is_pal {
            DMC_RATE_PAL[0]
        } else {
            DMC_RATE_NTSC[0]
        };
        *self = Self {
            rate_cycles: r0,
            timer_counter: r0 as u32,
            gain_q15: g,
            target_gain_q15: g,
            ..Self::default()
        };
    }
}

/// Complete Ricoh 2A03 / 2A07 APU state.
#[derive(Clone)]
pub(crate) struct Apu2A03 {
    is_pal: bool,
    cpu_clock_hz: u32,
    pulse1: PulseChan,
    pulse2: PulseChan,
    triangle: TriangleChan,
    noise: NoiseChan,
    dmc: DmcChan,
    // Frame counter
    five_step_mode: bool,
    frame_irq_inhibit: bool,
    pub frame_irq_pending: bool,
    frame_step: u8,
    frame_divider: u32,
    quarter_frame_cycles: u32,
    slew_counter: u32,
    lut: MixerLut,
    /// Snapshot of recent raw registers `$4000..$4017` for state inspection / loop detection
    pub regs: [u8; 0x18],
}

impl Apu2A03 {
    pub fn new(is_pal: bool, cpu_clock_hz: u32) -> Self {
        let mut apu = Self {
            is_pal,
            cpu_clock_hz,
            pulse1: PulseChan::default(),
            pulse2: PulseChan::default(),
            triangle: TriangleChan::default(),
            noise: NoiseChan::default(),
            dmc: DmcChan::default(),
            five_step_mode: false,
            frame_irq_inhibit: true,
            frame_irq_pending: false,
            frame_step: 0,
            frame_divider: 7_457,
            quarter_frame_cycles: 7_457,
            slew_counter: SLEW_STEP_CYCLES,
            lut: MixerLut::build(),
            regs: [0u8; 0x18],
        };
        apu.reset(0);
        apu
    }

    pub fn reset(&mut self, mute_mask: u16) {
        self.quarter_frame_cycles = if self.is_pal { 8_313 } else { 7_457 };
        self.frame_divider = self.quarter_frame_cycles;
        self.frame_step = 0;
        self.five_step_mode = false;
        self.frame_irq_inhibit = true;
        self.frame_irq_pending = false;
        self.slew_counter = SLEW_STEP_CYCLES;
        self.regs = [0u8; 0x18];

        self.pulse1.reset(true, (mute_mask & (1 << 0)) != 0);
        self.pulse2.reset(false, (mute_mask & (1 << 1)) != 0);
        self.triangle.reset((mute_mask & (1 << 2)) != 0);
        self.noise.reset((mute_mask & (1 << 3)) != 0, self.is_pal);
        self.dmc.reset((mute_mask & (1 << 4)) != 0, self.is_pal);
    }

    pub fn set_mute_mask(&mut self, mute_mask: u16, immediate: bool) {
        self.pulse1.target_gain_q15 = if (mute_mask & (1 << 0)) != 0 {
            0
        } else {
            GAIN_MAX
        };
        self.pulse2.target_gain_q15 = if (mute_mask & (1 << 1)) != 0 {
            0
        } else {
            GAIN_MAX
        };
        self.triangle.target_gain_q15 = if (mute_mask & (1 << 2)) != 0 {
            0
        } else {
            GAIN_MAX
        };
        self.noise.target_gain_q15 = if (mute_mask & (1 << 3)) != 0 {
            0
        } else {
            GAIN_MAX
        };
        self.dmc.target_gain_q15 = if (mute_mask & (1 << 4)) != 0 {
            0
        } else {
            GAIN_MAX
        };

        if immediate {
            self.pulse1.gain_q15 = self.pulse1.target_gain_q15;
            self.pulse2.gain_q15 = self.pulse2.target_gain_q15;
            self.triangle.gain_q15 = self.triangle.target_gain_q15;
            self.noise.gain_q15 = self.noise.target_gain_q15;
            self.dmc.gain_q15 = self.dmc.target_gain_q15;
        }
    }

    #[inline]
    pub fn irq_asserted(&self) -> bool {
        self.frame_irq_pending || self.dmc.irq_pending
    }

    pub fn write_register<F>(&mut self, addr: u16, val: u8, mut dma_read: F)
    where
        F: FnMut(u16) -> u8,
    {
        if (0x4000..=0x4017).contains(&addr) {
            self.regs[(addr - 0x4000) as usize] = val;
        }
        match addr {
            0x4000 => {
                self.pulse1.duty = (val >> 6) & 0x03;
                self.pulse1.halt_loop = (val & 0x20) != 0;
                self.pulse1.constant_vol = (val & 0x10) != 0;
                self.pulse1.vol_period = val & 0x0F;
            }
            0x4001 => {
                self.pulse1.sweep_enabled = (val & 0x80) != 0;
                self.pulse1.sweep_period = (val >> 4) & 0x07;
                self.pulse1.sweep_negate = (val & 0x08) != 0;
                self.pulse1.sweep_shift = val & 0x07;
                self.pulse1.sweep_reload = true;
            }
            0x4002 => {
                self.pulse1.timer_period = (self.pulse1.timer_period & 0x0700) | (val as u16);
            }
            0x4003 => {
                self.pulse1.timer_period =
                    (self.pulse1.timer_period & 0x00FF) | (((val & 0x07) as u16) << 8);
                if self.pulse1.enabled {
                    self.pulse1.length_counter = LENGTH_TABLE[((val >> 3) & 0x1F) as usize];
                }
                self.pulse1.seq_step = 0;
                self.pulse1.env_start = true;
                self.pulse1.triggered_frame = true;
            }
            0x4004 => {
                self.pulse2.duty = (val >> 6) & 0x03;
                self.pulse2.halt_loop = (val & 0x20) != 0;
                self.pulse2.constant_vol = (val & 0x10) != 0;
                self.pulse2.vol_period = val & 0x0F;
            }
            0x4005 => {
                self.pulse2.sweep_enabled = (val & 0x80) != 0;
                self.pulse2.sweep_period = (val >> 4) & 0x07;
                self.pulse2.sweep_negate = (val & 0x08) != 0;
                self.pulse2.sweep_shift = val & 0x07;
                self.pulse2.sweep_reload = true;
            }
            0x4006 => {
                self.pulse2.timer_period = (self.pulse2.timer_period & 0x0700) | (val as u16);
            }
            0x4007 => {
                self.pulse2.timer_period =
                    (self.pulse2.timer_period & 0x00FF) | (((val & 0x07) as u16) << 8);
                if self.pulse2.enabled {
                    self.pulse2.length_counter = LENGTH_TABLE[((val >> 3) & 0x1F) as usize];
                }
                self.pulse2.seq_step = 0;
                self.pulse2.env_start = true;
                self.pulse2.triggered_frame = true;
            }
            0x4008 => {
                self.triangle.control_flag = (val & 0x80) != 0;
                self.triangle.linear_reload_val = val & 0x7F;
            }
            0x400A => {
                self.triangle.timer_period = (self.triangle.timer_period & 0x0700) | (val as u16);
            }
            0x400B => {
                self.triangle.timer_period =
                    (self.triangle.timer_period & 0x00FF) | (((val & 0x07) as u16) << 8);
                if self.triangle.enabled {
                    self.triangle.length_counter = LENGTH_TABLE[((val >> 3) & 0x1F) as usize];
                }
                self.triangle.linear_reload_flag = true;
                self.triangle.triggered_frame = true;
            }
            0x400C => {
                self.noise.halt_loop = (val & 0x20) != 0;
                self.noise.constant_vol = (val & 0x10) != 0;
                self.noise.vol_period = val & 0x0F;
            }
            0x400E => {
                self.noise.mode = (val & 0x80) != 0;
                self.noise.period_idx = val & 0x0F;
                self.noise.period_cycles = if self.is_pal {
                    NOISE_PERIOD_PAL[self.noise.period_idx as usize]
                } else {
                    NOISE_PERIOD_NTSC[self.noise.period_idx as usize]
                };
            }
            0x400F => {
                if self.noise.enabled {
                    self.noise.length_counter = LENGTH_TABLE[((val >> 3) & 0x1F) as usize];
                }
                self.noise.env_start = true;
                self.noise.triggered_frame = true;
            }
            0x4010 => {
                self.dmc.irq_enable = (val & 0x80) != 0;
                if !self.dmc.irq_enable {
                    self.dmc.irq_pending = false;
                }
                self.dmc.loop_flag = (val & 0x40) != 0;
                self.dmc.rate_idx = val & 0x0F;
                self.dmc.rate_cycles = if self.is_pal {
                    DMC_RATE_PAL[self.dmc.rate_idx as usize]
                } else {
                    DMC_RATE_NTSC[self.dmc.rate_idx as usize]
                };
            }
            0x4011 => {
                let lv = val & 0x7F;
                if lv != self.dmc.output_level {
                    if self.dmc.dac_writes_frame == 0 {
                        self.dmc.min_dac_frame = self.dmc.output_level.min(lv);
                        self.dmc.max_dac_frame = self.dmc.output_level.max(lv);
                    } else {
                        self.dmc.min_dac_frame = self.dmc.min_dac_frame.min(lv);
                        self.dmc.max_dac_frame = self.dmc.max_dac_frame.max(lv);
                    }
                    self.dmc.dac_writes_frame = self.dmc.dac_writes_frame.saturating_add(1);
                }
                self.dmc.output_level = lv;
            }
            0x4012 => {
                self.dmc.sample_addr = 0xC000 | ((val as u16) << 6);
            }
            0x4013 => {
                self.dmc.sample_length = ((val as u16) << 4) + 1;
            }
            0x4015 => {
                self.dmc.irq_pending = false;

                self.pulse1.enabled = (val & 0x01) != 0;
                if !self.pulse1.enabled {
                    self.pulse1.length_counter = 0;
                }
                self.pulse2.enabled = (val & 0x02) != 0;
                if !self.pulse2.enabled {
                    self.pulse2.length_counter = 0;
                }
                self.triangle.enabled = (val & 0x04) != 0;
                if !self.triangle.enabled {
                    self.triangle.length_counter = 0;
                }
                self.noise.enabled = (val & 0x08) != 0;
                if !self.noise.enabled {
                    self.noise.length_counter = 0;
                }

                if (val & 0x10) == 0 {
                    self.dmc.bytes_remaining = 0;
                } else if self.dmc.bytes_remaining == 0 {
                    self.dmc.current_addr = self.dmc.sample_addr;
                    self.dmc.bytes_remaining = self.dmc.sample_length;
                    self.dmc.triggered_frame = true;
                    if !self.dmc.has_sample_byte {
                        self.fetch_dmc_byte(&mut dma_read);
                    }
                }
            }
            0x4017 => {
                self.five_step_mode = (val & 0x80) != 0;
                self.frame_irq_inhibit = (val & 0x40) != 0;
                if self.frame_irq_inhibit {
                    self.frame_irq_pending = false;
                }
                self.frame_step = 0;
                self.frame_divider = self.quarter_frame_cycles;
                if self.five_step_mode {
                    self.clock_quarter_frame();
                    self.clock_half_frame();
                }
            }
            _ => {}
        }
    }

    pub fn read_status(&mut self) -> u8 {
        let mut st = 0u8;
        if self.pulse1.length_counter > 0 {
            st |= 0x01;
        }
        if self.pulse2.length_counter > 0 {
            st |= 0x02;
        }
        if self.triangle.length_counter > 0 {
            st |= 0x04;
        }
        if self.noise.length_counter > 0 {
            st |= 0x08;
        }
        if self.dmc.bytes_remaining > 0 {
            st |= 0x10;
        }
        if self.frame_irq_pending {
            st |= 0x40;
        }
        if self.dmc.irq_pending {
            st |= 0x80;
        }
        self.frame_irq_pending = false;
        st
    }

    fn fetch_dmc_byte<F>(&mut self, dma_read: &mut F)
    where
        F: FnMut(u16) -> u8,
    {
        if self.dmc.bytes_remaining > 0 && !self.dmc.has_sample_byte {
            self.dmc.sample_buffer = dma_read(self.dmc.current_addr);
            self.dmc.has_sample_byte = true;
            self.dmc.current_addr = if self.dmc.current_addr == 0xFFFF {
                0x8000
            } else {
                self.dmc.current_addr + 1
            };
            self.dmc.bytes_remaining -= 1;
            if self.dmc.bytes_remaining == 0 {
                if self.dmc.loop_flag {
                    self.dmc.current_addr = self.dmc.sample_addr;
                    self.dmc.bytes_remaining = self.dmc.sample_length;
                } else if self.dmc.irq_enable {
                    self.dmc.irq_pending = true;
                }
            }
        }
    }

    fn clock_quarter_frame(&mut self) {
        self.pulse1.clock_envelope();
        self.pulse2.clock_envelope();
        self.triangle.clock_linear();
        self.noise.clock_envelope();
    }

    fn clock_half_frame(&mut self) {
        self.pulse1.clock_length_and_sweep();
        self.pulse2.clock_length_and_sweep();
        self.triangle.clock_length();
        self.noise.clock_length();
    }

    fn advance_frame_counter(&mut self) {
        if !self.five_step_mode {
            // 4-step sequence (0, 1, 2, 3)
            match self.frame_step {
                0 | 2 => {
                    self.clock_quarter_frame();
                }
                1 => {
                    self.clock_quarter_frame();
                    self.clock_half_frame();
                }
                _ => {
                    self.clock_quarter_frame();
                    self.clock_half_frame();
                    if !self.frame_irq_inhibit {
                        self.frame_irq_pending = true;
                    }
                }
            }
            self.frame_step = (self.frame_step + 1) & 0x03;
        } else {
            // 5-step sequence (0, 1, 2, 3, 4; step 3 clocks nothing, no IRQ)
            match self.frame_step {
                0 | 2 => {
                    self.clock_quarter_frame();
                }
                1 | 4 => {
                    self.clock_quarter_frame();
                    self.clock_half_frame();
                }
                _ => {}
            }
            self.frame_step = (self.frame_step + 1) % 5;
        }
    }

    #[inline]
    fn slewing(&self) -> bool {
        self.pulse1.gain_q15 != self.pulse1.target_gain_q15
            || self.pulse2.gain_q15 != self.pulse2.target_gain_q15
            || self.triangle.gain_q15 != self.triangle.target_gain_q15
            || self.noise.gain_q15 != self.noise.target_gain_q15
            || self.dmc.gain_q15 != self.dmc.target_gain_q15
    }

    /// Minimum CPU cycles until any internal timer/oscillator/frame-counter/DMA event can change output.
    #[inline]
    pub fn next_event_cycles(&self, headless: bool) -> u32 {
        let mut min_c = self.frame_divider.max(1);
        if self.dmc.bytes_remaining > 0 || !self.dmc.silence_flag || self.dmc.has_sample_byte {
            min_c = min_c.min(self.dmc.timer_counter.max(1));
        }
        if !headless {
            if self.pulse1.is_hw_active() {
                min_c = min_c.min(self.pulse1.timer_counter + 1);
            }
            if self.pulse2.is_hw_active() {
                min_c = min_c.min(self.pulse2.timer_counter + 1);
            }
            if self.triangle.is_hw_active() {
                min_c = min_c.min(self.triangle.timer_counter + 1);
            }
            if self.noise.is_hw_active() {
                min_c = min_c.min(self.noise.timer_counter + 1);
            }
            if self.slewing() {
                min_c = min_c.min(self.slew_counter.max(1));
            }
        }
        min_c
    }

    /// Advance 2A03 timers, envelopes, sweeps, and DPCM DMA by `cycles` CPU cycles.
    pub fn step_cycles<F>(&mut self, cycles: u32, headless: bool, mut dma_read: F)
    where
        F: FnMut(u16) -> u8,
    {
        // 1. Frame counter (240 Hz / 192 Hz)
        let mut rem_fc = cycles;
        while rem_fc >= self.frame_divider {
            rem_fc -= self.frame_divider;
            self.frame_divider = self.quarter_frame_cycles;
            self.advance_frame_counter();
        }
        self.frame_divider -= rem_fc;

        // 2. DPCM bit unit + DMA reader (always clocked even in headless mode so IRQ/DMA stay in sync!)
        if self.dmc.bytes_remaining > 0 || !self.dmc.silence_flag || self.dmc.has_sample_byte {
            let mut rem_dmc = cycles;
            while rem_dmc >= self.dmc.timer_counter {
                rem_dmc -= self.dmc.timer_counter;
                self.dmc.timer_counter = self.dmc.rate_cycles as u32;

                if !self.dmc.silence_flag {
                    if (self.dmc.shift_reg & 1) != 0 {
                        if self.dmc.output_level <= 125 {
                            self.dmc.output_level += 2;
                        }
                    } else if self.dmc.output_level >= 2 {
                        self.dmc.output_level -= 2;
                    }
                    self.dmc.min_dac_frame = self.dmc.min_dac_frame.min(self.dmc.output_level);
                    self.dmc.max_dac_frame = self.dmc.max_dac_frame.max(self.dmc.output_level);
                }

                self.dmc.shift_reg >>= 1;
                if self.dmc.bits_remaining > 1 {
                    self.dmc.bits_remaining -= 1;
                } else {
                    self.dmc.bits_remaining = 8;
                    if self.dmc.has_sample_byte {
                        self.dmc.silence_flag = false;
                        self.dmc.shift_reg = self.dmc.sample_buffer;
                        self.dmc.has_sample_byte = false;
                        self.fetch_dmc_byte(&mut dma_read);
                    } else {
                        self.dmc.silence_flag = true;
                    }
                }
            }
            self.dmc.timer_counter -= rem_dmc;
        }

        if headless {
            return;
        }

        // 3. Oscillators (Pulse 1, Pulse 2, Triangle, Noise)
        step_pulse(&mut self.pulse1, cycles);
        step_pulse(&mut self.pulse2, cycles);

        if self.triangle.is_clocking() && self.triangle.timer_period >= 2 {
            if self.triangle.timer_counter >= cycles {
                self.triangle.timer_counter -= cycles;
            } else {
                let reload = (self.triangle.timer_period as u32) + 1;
                let overshoot = cycles - self.triangle.timer_counter - 1;
                let ticks = 1 + overshoot / reload;
                self.triangle.seq_step = ((self.triangle.seq_step as u32 + ticks) & 0x1F) as u8;
                self.triangle.timer_counter = reload - 1 - (overshoot % reload);
            }
        }

        if self.noise.enabled && self.noise.length_counter > 0 {
            let mut rem_n = cycles;
            let reload = (self.noise.period_cycles as u32).max(4);
            while rem_n > self.noise.timer_counter {
                rem_n -= self.noise.timer_counter + 1;
                self.noise.timer_counter = reload - 1;
                self.noise.step_lfsr();
            }
            self.noise.timer_counter -= rem_n;
        }

        // 4. Click-free mute gain slewing
        if self.slewing() {
            if self.slew_counter > cycles {
                self.slew_counter -= cycles;
            } else {
                self.slew_counter = SLEW_STEP_CYCLES;
                slew_toward(&mut self.pulse1.gain_q15, self.pulse1.target_gain_q15);
                slew_toward(&mut self.pulse2.gain_q15, self.pulse2.target_gain_q15);
                slew_toward(&mut self.triangle.gain_q15, self.triangle.target_gain_q15);
                slew_toward(&mut self.noise.gain_q15, self.noise.target_gain_q15);
                slew_toward(&mut self.dmc.gain_q15, self.dmc.target_gain_q15);
            }
        }
    }

    #[inline]
    pub fn mixed_output(&self) -> i32 {
        let p1_q8 = (self.pulse1.raw_dac() * self.pulse1.gain_q15) >> 7;
        let p2_q8 = (self.pulse2.raw_dac() * self.pulse2.gain_q15) >> 7;
        let tri_q8 = (3 * self.triangle.raw_dac() * self.triangle.gain_q15) >> 7;
        let nse_q8 = (2 * self.noise.raw_dac() * self.noise.gain_q15) >> 7;
        let dmc_q8 = ((self.dmc.output_level as i32) * self.dmc.gain_q15) >> 7;
        self.lut.eval(p1_q8 + p2_q8, tri_q8 + nse_q8 + dmc_q8)
    }

    pub fn snapshot_telemetry(&mut self, mute_mask: u16, out_0_to_4: &mut [ChannelState; 5]) {
        let clk = self.cpu_clock_hz as f32;

        // 0: Pulse 1
        out_0_to_4[0] = snapshot_pulse(&mut self.pulse1, clk, (mute_mask & (1 << 0)) != 0);
        // 1: Pulse 2
        out_0_to_4[1] = snapshot_pulse(&mut self.pulse2, clk, (mute_mask & (1 << 1)) != 0);

        // 2: Triangle (with kick-sweep onset detector)
        let mt = u8::from((mute_mask & (1 << 2)) != 0);
        let at = self.triangle.is_hw_active();
        let freq_t = if at {
            clk / (32.0 * (self.triangle.timer_period as f32 + 1.0))
        } else {
            0.0
        };
        let mut kick_onset = false;
        if at && self.triangle.prev_active {
            // Detect start of rapid downward Triangle drum/kick sweep or sharp upward jump to start one
            if !self.triangle.in_kick_sweep
                && self.triangle.prev_freq_hz >= 120.0
                && freq_t < self.triangle.prev_freq_hz * 0.80
            {
                kick_onset = true;
                self.triangle.in_kick_sweep = true;
            } else if freq_t >= self.triangle.prev_freq_hz * 0.96 {
                if self.triangle.in_kick_sweep && freq_t > self.triangle.prev_freq_hz * 1.25 {
                    kick_onset = true;
                }
                self.triangle.in_kick_sweep = false;
            }
        } else {
            self.triangle.in_kick_sweep = false;
        }
        let onset_t = at && (self.triangle.triggered_frame || !self.triangle.prev_active || kick_onset);
        out_0_to_4[2] = ChannelState {
            active: u8::from(at),
            muted: mt,
            onset_trigger: u8::from(onset_t),
            duty_or_mode: 0,
            volume: if at && mt == 0 { 1.0 } else { 0.0 },
            freq_hz: freq_t,
        };
        self.triangle.triggered_frame = false;
        self.triangle.prev_active = at;
        self.triangle.prev_freq_hz = freq_t;

        // 3: Noise
        let mn = u8::from((mute_mask & (1 << 3)) != 0);
        let an = self.noise.is_hw_active();
        let env_n = self.noise.env_output();
        let onset_n = an
            && (self.noise.triggered_frame
                || !self.noise.prev_active
                || env_n > self.noise.prev_env.saturating_add(2));
        out_0_to_4[3] = ChannelState {
            active: u8::from(an),
            muted: mn,
            onset_trigger: u8::from(onset_n),
            duty_or_mode: u8::from(self.noise.mode),
            volume: if an && mn == 0 {
                (env_n as f32) / 15.0
            } else {
                0.0
            },
            freq_hz: if an {
                clk / (self.noise.period_cycles.max(4) as f32)
            } else {
                0.0
            },
        };
        self.noise.triggered_frame = false;
        self.noise.prev_active = an;
        self.noise.prev_env = if an { env_n } else { 0 };

        // 4: DPCM
        let md = u8::from((mute_mask & (1 << 4)) != 0);
        let dma_playing = self.dmc.bytes_remaining > 0 || !self.dmc.silence_flag;
        let swing = self.dmc.max_dac_frame.saturating_sub(self.dmc.min_dac_frame);
        let ad = dma_playing || (self.dmc.dac_writes_frame > 0 && swing > 0);
        let onset_d = ad && (self.dmc.triggered_frame || (!dma_playing && swing >= 16));
        let vol_d = if !ad || md != 0 {
            0.0
        } else if dma_playing {
            ((swing as f32) / 64.0).clamp(0.45, 1.0)
        } else {
            ((swing as f32) / 127.0).clamp(0.15, 1.0)
        };
        out_0_to_4[4] = ChannelState {
            active: u8::from(ad),
            muted: md,
            onset_trigger: u8::from(onset_d),
            duty_or_mode: (u8::from(self.dmc.irq_enable) << 1) | u8::from(self.dmc.loop_flag),
            volume: vol_d,
            freq_hz: if ad {
                clk / (self.dmc.rate_cycles.max(50) as f32)
            } else {
                0.0
            },
        };
        self.dmc.triggered_frame = false;
        self.dmc.dac_writes_frame = 0;
        self.dmc.min_dac_frame = self.dmc.output_level;
        self.dmc.max_dac_frame = self.dmc.output_level;
    }
}

fn snapshot_pulse(p: &mut PulseChan, clk: f32, is_muted: bool) -> ChannelState {
    let m = u8::from(is_muted);
    let a = p.is_hw_active();
    let env = p.env_output();
    let pitch_changed = p.prev_active && p.timer_period.abs_diff(p.prev_period) >= 4;
    let onset = a
        && (p.triggered_frame
            || !p.prev_active
            || env > p.prev_env.saturating_add(2)
            || (pitch_changed && p.triggered_frame));
    let state = ChannelState {
        active: u8::from(a),
        muted: m,
        onset_trigger: u8::from(onset),
        duty_or_mode: p.duty,
        volume: if a && m == 0 {
            (env as f32) / 15.0
        } else {
            0.0
        },
        freq_hz: if a {
            clk / (16.0 * (p.timer_period as f32 + 1.0))
        } else {
            0.0
        },
    };
    p.triggered_frame = false;
    p.prev_active = a;
    p.prev_env = if a { env } else { 0 };
    p.prev_period = p.timer_period;
    state
}

#[inline]
fn step_pulse(p: &mut PulseChan, cycles: u32) {
    if p.timer_counter >= cycles {
        p.timer_counter -= cycles;
    } else {
        let reload = ((p.timer_period as u32) + 1) * 2;
        let overshoot = cycles - p.timer_counter - 1;
        let ticks = 1 + overshoot / reload;
        p.seq_step = ((p.seq_step as u32 + ticks) & 0x07) as u8;
        p.timer_counter = reload - 1 - (overshoot % reload);
    }
}

#[inline]
fn slew_toward(cur: &mut i32, target: i32) {
    if *cur < target {
        *cur = (*cur + SLEW_DELTA_Q15).min(target);
    } else if *cur > target {
        *cur = (*cur - SLEW_DELTA_Q15).max(target);
    }
}
