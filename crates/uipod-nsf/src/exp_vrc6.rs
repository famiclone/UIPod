//! Konami VRC6 expansion audio synthesizer (`ChipFlags::VRC6`).
//!
//! Implements VRC6 Pulse 1 (`$9000..$9002`), global frequency scaler (`$9003`),
//! VRC6 Pulse 2 (`$A000..$A002`), and 14-step Sawtooth (`$B000..$B002`), with
//! click-free per-channel muting and 60 Hz `ChannelState` telemetry.

use crate::ChannelState;

const GAIN_MAX: i32 = 32_768;
const SLEW_STEP_CYCLES: u32 = 64;
const SLEW_DELTA_Q15: i32 = 1_024; // ~32 steps * 64 cycles = ~2048 CPU cycles (~1.1 ms)

/// DAC scale matching 2A03 mixed scale (`~245` units per VRC6 volume step, where full-scale i16 is 32767).
const VRC6_DAC_UNIT: i32 = 230;

#[derive(Clone, Default)]
struct Vrc6Pulse {
    volume: u8,
    duty: u8,
    constant_mode: bool,
    period: u16,
    enabled: bool,
    timer: u32,
    step: u8,
    triggered_frame: bool,
    prev_active: bool,
    prev_vol: u8,
    gain_q15: i32,
    target_gain_q15: i32,
}

impl Vrc6Pulse {
    fn reset(&mut self, muted: bool) {
        let g = if muted { 0 } else { GAIN_MAX };
        *self = Self {
            gain_q15: g,
            target_gain_q15: g,
            ..Self::default()
        };
    }

    #[inline]
    fn eff_period(&self, freq_ctrl: u8) -> u32 {
        let p = self.period as u32;
        if (freq_ctrl & 0x04) != 0 {
            p >> 8
        } else if (freq_ctrl & 0x02) != 0 {
            p >> 4
        } else {
            p
        }
    }

    #[inline]
    fn raw_dac(&self) -> i32 {
        if !self.enabled {
            return 0;
        }
        if self.constant_mode || self.step <= self.duty {
            self.volume as i32
        } else {
            0
        }
    }

    #[inline]
    fn is_hw_active(&self, freq_ctrl: u8) -> bool {
        if !self.enabled || self.volume == 0 {
            return false;
        }
        if self.constant_mode {
            return false;
        }
        if (freq_ctrl & 0x01) != 0 {
            return false;
        }
        self.eff_period(freq_ctrl) >= 2
    }
}

#[derive(Clone, Default)]
struct Vrc6Saw {
    rate: u8,
    period: u16,
    enabled: bool,
    timer: u32,
    step: u8,
    accum: u8,
    triggered_frame: bool,
    prev_active: bool,
    prev_rate: u8,
    gain_q15: i32,
    target_gain_q15: i32,
}

impl Vrc6Saw {
    fn reset(&mut self, muted: bool) {
        let g = if muted { 0 } else { GAIN_MAX };
        *self = Self {
            gain_q15: g,
            target_gain_q15: g,
            ..Self::default()
        };
    }

    #[inline]
    fn eff_period(&self, freq_ctrl: u8) -> u32 {
        let p = self.period as u32;
        if (freq_ctrl & 0x04) != 0 {
            p >> 8
        } else if (freq_ctrl & 0x02) != 0 {
            p >> 4
        } else {
            p
        }
    }

    #[inline]
    fn raw_dac(&self) -> i32 {
        if !self.enabled {
            0
        } else {
            ((self.accum >> 3) & 0x1F) as i32
        }
    }

    #[inline]
    fn is_hw_active(&self, freq_ctrl: u8) -> bool {
        self.enabled && (freq_ctrl & 0x01) == 0 && self.rate > 0 && self.eff_period(freq_ctrl) >= 2
    }
}

#[derive(Clone, Default)]
pub(crate) struct Vrc6Apu {
    pulse1: Vrc6Pulse,
    pulse2: Vrc6Pulse,
    saw: Vrc6Saw,
    freq_ctrl: u8,
    slew_counter: u32,
}

impl Vrc6Apu {
    pub fn new() -> Self {
        let mut s = Self::default();
        s.reset(0);
        s
    }

    pub fn reset(&mut self, mute_mask: u16) {
        self.freq_ctrl = 0;
        self.slew_counter = SLEW_STEP_CYCLES;
        self.pulse1.reset((mute_mask & (1 << 5)) != 0);
        self.pulse2.reset((mute_mask & (1 << 6)) != 0);
        self.saw.reset((mute_mask & (1 << 7)) != 0);
    }

    pub fn set_mute_mask(&mut self, mute_mask: u16, immediate: bool) {
        let m1 = (mute_mask & (1 << 5)) != 0;
        let m2 = (mute_mask & (1 << 6)) != 0;
        let ms = (mute_mask & (1 << 7)) != 0;
        self.pulse1.target_gain_q15 = if m1 { 0 } else { GAIN_MAX };
        self.pulse2.target_gain_q15 = if m2 { 0 } else { GAIN_MAX };
        self.saw.target_gain_q15 = if ms { 0 } else { GAIN_MAX };
        if immediate {
            self.pulse1.gain_q15 = self.pulse1.target_gain_q15;
            self.pulse2.gain_q15 = self.pulse2.target_gain_q15;
            self.saw.gain_q15 = self.saw.target_gain_q15;
        }
    }

    pub fn write_register(&mut self, addr: u16, val: u8) {
        match addr & 0xF003 {
            0x9000 => {
                self.pulse1.constant_mode = (val & 0x80) != 0;
                self.pulse1.duty = (val >> 4) & 0x07;
                self.pulse1.volume = val & 0x0F;
            }
            0x9001 => {
                self.pulse1.period = (self.pulse1.period & 0x0F00) | (val as u16);
            }
            0x9002 => {
                let was_enabled = self.pulse1.enabled;
                self.pulse1.period = (self.pulse1.period & 0x00FF) | (((val & 0x0F) as u16) << 8);
                self.pulse1.enabled = (val & 0x80) != 0;
                if !self.pulse1.enabled {
                    self.pulse1.step = 0;
                } else if !was_enabled {
                    self.pulse1.triggered_frame = true;
                }
            }
            0x9003 => {
                self.freq_ctrl = val & 0x07;
            }
            0xA000 => {
                self.pulse2.constant_mode = (val & 0x80) != 0;
                self.pulse2.duty = (val >> 4) & 0x07;
                self.pulse2.volume = val & 0x0F;
            }
            0xA001 => {
                self.pulse2.period = (self.pulse2.period & 0x0F00) | (val as u16);
            }
            0xA002 => {
                let was_enabled = self.pulse2.enabled;
                self.pulse2.period = (self.pulse2.period & 0x00FF) | (((val & 0x0F) as u16) << 8);
                self.pulse2.enabled = (val & 0x80) != 0;
                if !self.pulse2.enabled {
                    self.pulse2.step = 0;
                } else if !was_enabled {
                    self.pulse2.triggered_frame = true;
                }
            }
            0xB000 => {
                self.saw.rate = val & 0x3F;
            }
            0xB001 => {
                self.saw.period = (self.saw.period & 0x0F00) | (val as u16);
            }
            0xB002 => {
                let was_enabled = self.saw.enabled;
                self.saw.period = (self.saw.period & 0x00FF) | (((val & 0x0F) as u16) << 8);
                self.saw.enabled = (val & 0x80) != 0;
                if !self.saw.enabled {
                    self.saw.step = 0;
                    self.saw.accum = 0;
                } else if !was_enabled {
                    self.saw.triggered_frame = true;
                }
            }
            _ => {}
        }
    }

    #[inline]
    fn slewing(&self) -> bool {
        self.pulse1.gain_q15 != self.pulse1.target_gain_q15
            || self.pulse2.gain_q15 != self.pulse2.target_gain_q15
            || self.saw.gain_q15 != self.saw.target_gain_q15
    }

    #[inline]
    pub fn next_event_cycles(&self) -> u32 {
        let mut min_c = u32::MAX;
        if (self.freq_ctrl & 0x01) == 0 {
            if self.pulse1.enabled && !self.pulse1.constant_mode && self.pulse1.volume > 0 {
                min_c = min_c.min(self.pulse1.timer + 1);
            }
            if self.pulse2.enabled && !self.pulse2.constant_mode && self.pulse2.volume > 0 {
                min_c = min_c.min(self.pulse2.timer + 1);
            }
            if self.saw.enabled && self.saw.rate > 0 {
                min_c = min_c.min(self.saw.timer + 1);
            }
        }
        if self.slewing() {
            min_c = min_c.min(self.slew_counter.max(1));
        }
        min_c
    }

    #[inline]
    pub fn step_cycles(&mut self, cycles: u32) {
        if (self.freq_ctrl & 0x01) == 0 {
            if self.pulse1.enabled {
                if self.pulse1.timer >= cycles {
                    self.pulse1.timer -= cycles;
                } else {
                    let reload = self.pulse1.eff_period(self.freq_ctrl) + 1;
                    let overshoot = cycles - self.pulse1.timer - 1;
                    let ticks = 1 + overshoot / reload;
                    self.pulse1.step = ((self.pulse1.step as u32 + ticks) & 0x0F) as u8;
                    self.pulse1.timer = reload - 1 - (overshoot % reload);
                }
            }
            if self.pulse2.enabled {
                if self.pulse2.timer >= cycles {
                    self.pulse2.timer -= cycles;
                } else {
                    let reload = self.pulse2.eff_period(self.freq_ctrl) + 1;
                    let overshoot = cycles - self.pulse2.timer - 1;
                    let ticks = 1 + overshoot / reload;
                    self.pulse2.step = ((self.pulse2.step as u32 + ticks) & 0x0F) as u8;
                    self.pulse2.timer = reload - 1 - (overshoot % reload);
                }
            }
            if self.saw.enabled {
                if self.saw.timer >= cycles {
                    self.saw.timer -= cycles;
                } else {
                    let mut rem = cycles;
                    let reload = self.saw.eff_period(self.freq_ctrl);
                    while rem > self.saw.timer {
                        rem -= self.saw.timer + 1;
                        self.saw.timer = reload;
                        self.saw.step += 1;
                        if self.saw.step >= 14 {
                            self.saw.step = 0;
                            self.saw.accum = 0;
                        } else if (self.saw.step & 1) == 0 {
                            self.saw.accum = self.saw.accum.wrapping_add(self.saw.rate);
                        }
                    }
                    self.saw.timer -= rem;
                }
            }
        }

        if self.slewing() {
            if self.slew_counter > cycles {
                self.slew_counter -= cycles;
            } else {
                self.slew_counter = SLEW_STEP_CYCLES;
                slew_toward(&mut self.pulse1.gain_q15, self.pulse1.target_gain_q15);
                slew_toward(&mut self.pulse2.gain_q15, self.pulse2.target_gain_q15);
                slew_toward(&mut self.saw.gain_q15, self.saw.target_gain_q15);
            }
        }
    }

    #[inline]
    pub fn mixed_output(&self) -> i32 {
        let p1 = (self.pulse1.raw_dac() * VRC6_DAC_UNIT * self.pulse1.gain_q15) >> 15;
        let p2 = (self.pulse2.raw_dac() * VRC6_DAC_UNIT * self.pulse2.gain_q15) >> 15;
        let sw = (self.saw.raw_dac() * VRC6_DAC_UNIT * self.saw.gain_q15) >> 15;
        p1 + p2 + sw
    }

    pub fn snapshot_telemetry(
        &mut self,
        cpu_clock_hz: u32,
        mute_mask: u16,
        out_5_to_7: &mut [ChannelState; 3],
    ) {
        let clk = cpu_clock_hz as f32;

        // VRC6 Pulse 1 (bit 5)
        let m1 = u8::from((mute_mask & (1 << 5)) != 0);
        let a1 = self.pulse1.is_hw_active(self.freq_ctrl);
        let onset1 = a1
            && (self.pulse1.triggered_frame
                || !self.pulse1.prev_active
                || self.pulse1.volume > self.pulse1.prev_vol.saturating_add(2));
        out_5_to_7[0] = ChannelState {
            active: u8::from(a1),
            muted: m1,
            onset_trigger: u8::from(onset1),
            duty_or_mode: self.pulse1.duty,
            volume: if a1 && m1 == 0 {
                (self.pulse1.volume as f32) / 15.0
            } else {
                0.0
            },
            freq_hz: if a1 {
                clk / (16.0 * (self.pulse1.eff_period(self.freq_ctrl) as f32 + 1.0))
            } else {
                0.0
            },
        };
        self.pulse1.triggered_frame = false;
        self.pulse1.prev_active = a1;
        self.pulse1.prev_vol = if a1 { self.pulse1.volume } else { 0 };

        // VRC6 Pulse 2 (bit 6)
        let m2 = u8::from((mute_mask & (1 << 6)) != 0);
        let a2 = self.pulse2.is_hw_active(self.freq_ctrl);
        let onset2 = a2
            && (self.pulse2.triggered_frame
                || !self.pulse2.prev_active
                || self.pulse2.volume > self.pulse2.prev_vol.saturating_add(2));
        out_5_to_7[1] = ChannelState {
            active: u8::from(a2),
            muted: m2,
            onset_trigger: u8::from(onset2),
            duty_or_mode: self.pulse2.duty,
            volume: if a2 && m2 == 0 {
                (self.pulse2.volume as f32) / 15.0
            } else {
                0.0
            },
            freq_hz: if a2 {
                clk / (16.0 * (self.pulse2.eff_period(self.freq_ctrl) as f32 + 1.0))
            } else {
                0.0
            },
        };
        self.pulse2.triggered_frame = false;
        self.pulse2.prev_active = a2;
        self.pulse2.prev_vol = if a2 { self.pulse2.volume } else { 0 };

        // VRC6 Sawtooth (bit 7)
        let ms = u8::from((mute_mask & (1 << 7)) != 0);
        let asaw = self.saw.is_hw_active(self.freq_ctrl);
        let onsets = asaw
            && (self.saw.triggered_frame
                || !self.saw.prev_active
                || self.saw.rate > self.saw.prev_rate.saturating_add(4));
        let peak_saw = (((self.saw.rate as u16) * 6) >> 3).min(31) as f32;
        out_5_to_7[2] = ChannelState {
            active: u8::from(asaw),
            muted: ms,
            onset_trigger: u8::from(onsets),
            duty_or_mode: self.saw.rate,
            volume: if asaw && ms == 0 {
                (peak_saw / 31.0).clamp(0.0, 1.0)
            } else {
                0.0
            },
            freq_hz: if asaw {
                clk / (14.0 * (self.saw.eff_period(self.freq_ctrl) as f32 + 1.0))
            } else {
                0.0
            },
        };
        self.saw.triggered_frame = false;
        self.saw.prev_active = asaw;
        self.saw.prev_rate = if asaw { self.saw.rate } else { 0 };
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
