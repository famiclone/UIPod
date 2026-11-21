//! Nintendo MMC5 expansion audio synthesizer (`ChipFlags::MMC5`).
//!
//! Implements MMC5 Pulse 1 (`$5000..$5003`), MMC5 Pulse 2 (`$5004..$5007`),
//! 8-bit Raw/Read PCM (`$5010..$5011`), Status (`$5015`), and hardware 8x8
//! unsigned multiplier (`$5205..$5206`), with click-free muting and telemetry.

use crate::ChannelState;

const GAIN_MAX: i32 = 32_768;
const SLEW_STEP_CYCLES: u32 = 64;
const SLEW_DELTA_Q15: i32 = 1_024;
const MMC5_PULSE_UNIT: i32 = 230;
const MMC5_PCM_UNIT: i32 = 32;

const DUTY_TABLE: [[u8; 8]; 4] = [
    [0, 1, 0, 0, 0, 0, 0, 0], // 12.5%
    [0, 1, 1, 0, 0, 0, 0, 0], // 25%
    [0, 1, 1, 1, 1, 0, 0, 0], // 50%
    [1, 0, 0, 1, 1, 1, 1, 1], // 75% (inverted 25%)
];

pub(crate) const LENGTH_TABLE: [u8; 32] = [
    10, 254, 20, 2, 40, 4, 80, 6, 160, 8, 60, 10, 14, 12, 26, 14, 12, 16, 24, 18, 48, 20, 96, 22,
    192, 24, 72, 26, 16, 28, 32, 30,
];

#[derive(Clone, Default)]
struct Mmc5Pulse {
    duty: u8,
    halt_loop: bool,
    constant_vol: bool,
    vol_period: u8,
    timer_period: u16,
    timer_counter: u32,
    seq_step: u8,
    length_counter: u8,
    enabled: bool,
    env_start: bool,
    env_divider: u8,
    env_decay: u8,
    triggered_frame: bool,
    prev_active: bool,
    prev_env: u8,
    gain_q15: i32,
    target_gain_q15: i32,
}

impl Mmc5Pulse {
    fn reset(&mut self, muted: bool) {
        let g = if muted { 0 } else { GAIN_MAX };
        *self = Self {
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
        self.enabled && self.length_counter > 0 && self.env_output() > 0 && self.timer_period >= 2
    }

    #[inline]
    fn raw_dac(&self) -> i32 {
        if !self.enabled || self.length_counter == 0 {
            return 0;
        }
        if DUTY_TABLE[self.duty as usize][self.seq_step as usize] != 0 {
            self.env_output() as i32
        } else {
            0
        }
    }

    fn clock_240hz(&mut self) {
        // MMC5 clocks BOTH envelope and length counter at 240 Hz
        if !self.halt_loop && self.length_counter > 0 {
            self.length_counter -= 1;
        }
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
}

#[derive(Clone, Default)]
pub(crate) struct Mmc5Apu {
    pulse1: Mmc5Pulse,
    pulse2: Mmc5Pulse,
    pcm_read_mode: bool,
    pcm_irq_enable: bool,
    pcm_dac: u8,
    pcm_writes_frame: u16,
    pcm_min_frame: u8,
    pcm_max_frame: u8,
    pcm_gain_q15: i32,
    pcm_target_gain_q15: i32,
    frame_divider: u32,
    frame_period_cycles: u32,
    slew_counter: u32,
    pub mul_a: u8,
    pub mul_b: u8,
    base_bit: u8,
}

impl Mmc5Apu {
    pub fn new(base_bit: u8) -> Self {
        let mut s = Self {
            base_bit,
            frame_period_cycles: 7_457,
            ..Self::default()
        };
        s.reset(0, 1_789_773);
        s
    }

    pub fn reset(&mut self, mute_mask: u16, cpu_clock_hz: u32) {
        let b = self.base_bit;
        self.frame_period_cycles = (cpu_clock_hz / 240).max(1_000);
        self.frame_divider = self.frame_period_cycles;
        self.slew_counter = SLEW_STEP_CYCLES;
        self.pulse1.reset((mute_mask & (1 << b)) != 0);
        self.pulse2.reset((mute_mask & (1 << (b + 1))) != 0);
        self.pcm_read_mode = false;
        self.pcm_irq_enable = false;
        self.pcm_dac = 0;
        self.pcm_writes_frame = 0;
        self.pcm_min_frame = 0;
        self.pcm_max_frame = 0;
        let gp = if (mute_mask & (1 << (b + 2))) != 0 {
            0
        } else {
            GAIN_MAX
        };
        self.pcm_gain_q15 = gp;
        self.pcm_target_gain_q15 = gp;
        self.mul_a = 0xFF;
        self.mul_b = 0xFF;
    }

    pub fn set_mute_mask(&mut self, mute_mask: u16, immediate: bool) {
        let b = self.base_bit;
        self.pulse1.target_gain_q15 = if (mute_mask & (1 << b)) != 0 {
            0
        } else {
            GAIN_MAX
        };
        self.pulse2.target_gain_q15 = if (mute_mask & (1 << (b + 1))) != 0 {
            0
        } else {
            GAIN_MAX
        };
        self.pcm_target_gain_q15 = if (mute_mask & (1 << (b + 2))) != 0 {
            0
        } else {
            GAIN_MAX
        };
        if immediate {
            self.pulse1.gain_q15 = self.pulse1.target_gain_q15;
            self.pulse2.gain_q15 = self.pulse2.target_gain_q15;
            self.pcm_gain_q15 = self.pcm_target_gain_q15;
        }
    }

    pub fn write_register(&mut self, addr: u16, val: u8) {
        match addr {
            0x5000 => {
                self.pulse1.duty = (val >> 6) & 0x03;
                self.pulse1.halt_loop = (val & 0x20) != 0;
                self.pulse1.constant_vol = (val & 0x10) != 0;
                self.pulse1.vol_period = val & 0x0F;
            }
            0x5001 => {
                // MMC5 Pulse has no sweep unit
            }
            0x5002 => {
                self.pulse1.timer_period = (self.pulse1.timer_period & 0x0700) | (val as u16);
            }
            0x5003 => {
                self.pulse1.timer_period =
                    (self.pulse1.timer_period & 0x00FF) | (((val & 0x07) as u16) << 8);
                if self.pulse1.enabled {
                    self.pulse1.length_counter = LENGTH_TABLE[((val >> 3) & 0x1F) as usize];
                }
                self.pulse1.seq_step = 0;
                self.pulse1.env_start = true;
                self.pulse1.triggered_frame = true;
            }
            0x5004 => {
                self.pulse2.duty = (val >> 6) & 0x03;
                self.pulse2.halt_loop = (val & 0x20) != 0;
                self.pulse2.constant_vol = (val & 0x10) != 0;
                self.pulse2.vol_period = val & 0x0F;
            }
            0x5005 => {}
            0x5006 => {
                self.pulse2.timer_period = (self.pulse2.timer_period & 0x0700) | (val as u16);
            }
            0x5007 => {
                self.pulse2.timer_period =
                    (self.pulse2.timer_period & 0x00FF) | (((val & 0x07) as u16) << 8);
                if self.pulse2.enabled {
                    self.pulse2.length_counter = LENGTH_TABLE[((val >> 3) & 0x1F) as usize];
                }
                self.pulse2.seq_step = 0;
                self.pulse2.env_start = true;
                self.pulse2.triggered_frame = true;
            }
            0x5010 => {
                self.pcm_read_mode = (val & 0x01) != 0;
                self.pcm_irq_enable = (val & 0x80) != 0;
            }
            0x5011 => {
                if !self.pcm_read_mode && val != 0 {
                    self.update_pcm_dac(val);
                }
            }
            0x5015 => {
                self.pulse1.enabled = (val & 0x01) != 0;
                if !self.pulse1.enabled {
                    self.pulse1.length_counter = 0;
                }
                self.pulse2.enabled = (val & 0x02) != 0;
                if !self.pulse2.enabled {
                    self.pulse2.length_counter = 0;
                }
            }
            0x5205 => {
                self.mul_a = val;
            }
            0x5206 => {
                self.mul_b = val;
            }
            _ => {}
        }
    }

    pub fn read_register(&self, addr: u16) -> Option<u8> {
        match addr {
            0x5010 => Some(u8::from(self.pcm_irq_enable) << 7),
            0x5015 => {
                let mut st = 0u8;
                if self.pulse1.length_counter > 0 {
                    st |= 0x01;
                }
                if self.pulse2.length_counter > 0 {
                    st |= 0x02;
                }
                Some(st)
            }
            0x5205 => Some(((self.mul_a as u16) * (self.mul_b as u16)) as u8),
            0x5206 => Some((((self.mul_a as u16) * (self.mul_b as u16)) >> 8) as u8),
            _ => None,
        }
    }

    #[inline]
    pub fn notify_rom_read(&mut self, addr: u16, val: u8) {
        if self.pcm_read_mode && (0x8000..=0xBFFF).contains(&addr) && val != 0 {
            self.update_pcm_dac(val);
        }
    }

    #[inline]
    fn update_pcm_dac(&mut self, val: u8) {
        if self.pcm_writes_frame == 0 {
            self.pcm_min_frame = val;
            self.pcm_max_frame = val;
        } else {
            self.pcm_min_frame = self.pcm_min_frame.min(val);
            self.pcm_max_frame = self.pcm_max_frame.max(val);
        }
        self.pcm_writes_frame = self.pcm_writes_frame.saturating_add(1);
        self.pcm_dac = val;
    }

    #[inline]
    fn slewing(&self) -> bool {
        self.pulse1.gain_q15 != self.pulse1.target_gain_q15
            || self.pulse2.gain_q15 != self.pulse2.target_gain_q15
            || self.pcm_gain_q15 != self.pcm_target_gain_q15
    }

    #[inline]
    pub fn next_event_cycles(&self) -> u32 {
        let mut min_c = self.frame_divider.max(1);
        if self.pulse1.is_hw_active() {
            min_c = min_c.min(self.pulse1.timer_counter + 1);
        }
        if self.pulse2.is_hw_active() {
            min_c = min_c.min(self.pulse2.timer_counter + 1);
        }
        if self.slewing() {
            min_c = min_c.min(self.slew_counter.max(1));
        }
        min_c
    }

    pub fn step_cycles(&mut self, cycles: u32, headless: bool) {
        // Advance 240 Hz frame clock
        let mut rem_fc = cycles;
        while rem_fc >= self.frame_divider {
            rem_fc -= self.frame_divider;
            self.frame_divider = self.frame_period_cycles;
            self.pulse1.clock_240hz();
            self.pulse2.clock_240hz();
        }
        self.frame_divider -= rem_fc;

        if !headless {
            step_pulse_osc(&mut self.pulse1, cycles);
            step_pulse_osc(&mut self.pulse2, cycles);
        }

        if self.slewing() {
            if self.slew_counter > cycles {
                self.slew_counter -= cycles;
            } else {
                self.slew_counter = SLEW_STEP_CYCLES;
                slew_toward(&mut self.pulse1.gain_q15, self.pulse1.target_gain_q15);
                slew_toward(&mut self.pulse2.gain_q15, self.pulse2.target_gain_q15);
                slew_toward(&mut self.pcm_gain_q15, self.pcm_target_gain_q15);
            }
        }
    }

    #[inline]
    pub fn mixed_output(&self) -> i32 {
        let p1 = (self.pulse1.raw_dac() * MMC5_PULSE_UNIT * self.pulse1.gain_q15) >> 15;
        let p2 = (self.pulse2.raw_dac() * MMC5_PULSE_UNIT * self.pulse2.gain_q15) >> 15;
        let pcm = ((self.pcm_dac as i32) * MMC5_PCM_UNIT * self.pcm_gain_q15) >> 15;
        p1 + p2 + pcm
    }

    pub fn snapshot_telemetry(
        &mut self,
        cpu_clock_hz: u32,
        mute_mask: u16,
        out_5_to_7: &mut [ChannelState; 3],
    ) {
        let clk = cpu_clock_hz as f32;
        let b = self.base_bit;

        // MMC5 Pulse 1
        let m1 = u8::from((mute_mask & (1 << b)) != 0);
        let a1 = self.pulse1.is_hw_active();
        let env1 = self.pulse1.env_output();
        let onset1 = a1
            && (self.pulse1.triggered_frame
                || !self.pulse1.prev_active
                || env1 > self.pulse1.prev_env.saturating_add(2));
        out_5_to_7[0] = ChannelState {
            active: u8::from(a1),
            muted: m1,
            onset_trigger: u8::from(onset1),
            duty_or_mode: self.pulse1.duty,
            volume: if a1 && m1 == 0 {
                (env1 as f32) / 15.0
            } else {
                0.0
            },
            freq_hz: if a1 {
                clk / (16.0 * (self.pulse1.timer_period as f32 + 1.0))
            } else {
                0.0
            },
        };
        self.pulse1.triggered_frame = false;
        self.pulse1.prev_active = a1;
        self.pulse1.prev_env = if a1 { env1 } else { 0 };

        // MMC5 Pulse 2
        let m2 = u8::from((mute_mask & (1 << (b + 1))) != 0);
        let a2 = self.pulse2.is_hw_active();
        let env2 = self.pulse2.env_output();
        let onset2 = a2
            && (self.pulse2.triggered_frame
                || !self.pulse2.prev_active
                || env2 > self.pulse2.prev_env.saturating_add(2));
        out_5_to_7[1] = ChannelState {
            active: u8::from(a2),
            muted: m2,
            onset_trigger: u8::from(onset2),
            duty_or_mode: self.pulse2.duty,
            volume: if a2 && m2 == 0 {
                (env2 as f32) / 15.0
            } else {
                0.0
            },
            freq_hz: if a2 {
                clk / (16.0 * (self.pulse2.timer_period as f32 + 1.0))
            } else {
                0.0
            },
        };
        self.pulse2.triggered_frame = false;
        self.pulse2.prev_active = a2;
        self.pulse2.prev_env = if a2 { env2 } else { 0 };

        // MMC5 PCM
        let mp = u8::from((mute_mask & (1 << (b + 2))) != 0);
        let ap = self.pcm_writes_frame > 0;
        let swing = self.pcm_max_frame.saturating_sub(self.pcm_min_frame);
        out_5_to_7[2] = ChannelState {
            active: u8::from(ap),
            muted: mp,
            onset_trigger: u8::from(ap && swing >= 16),
            duty_or_mode: u8::from(self.pcm_read_mode),
            volume: if ap && mp == 0 {
                ((swing as f32) / 255.0).clamp(0.15, 1.0)
            } else {
                0.0
            },
            freq_hz: 0.0,
        };
        self.pcm_writes_frame = 0;
        self.pcm_min_frame = self.pcm_dac;
        self.pcm_max_frame = self.pcm_dac;
    }
}

#[inline]
fn step_pulse_osc(p: &mut Mmc5Pulse, cycles: u32) {
    if p.timer_counter >= cycles {
        p.timer_counter -= cycles;
    } else {
        // Pulse duty steps once every `2 * (timer_period + 1)` CPU cycles
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
