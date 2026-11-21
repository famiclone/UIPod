//! Game Boy Advance Audio Processing Unit (APU):
//! - DirectSound FIFO A & B (8-bit signed DMA/FIFO channels, Timer 0/1 driven)
//! - CGB / DMG 4-Channel PSG (Square 1 w/ Sweep, Square 2, Dual-Bank Wave RAM, LFSR Noise)
//! - 10-bit stereo mixer + fractional resampler + DC blocker -> 44,100 Hz S16LE stereo

pub const GBA_CPU_CLOCK: u64 = 16_777_216;
pub const OUTPUT_SAMPLE_RATE: u32 = 44_100;

/// 512 Hz CGB frame sequencer period in 16.777216 MHz GBA CPU cycles (`16_777_216 / 512 = 32_768`).
const FRAME_SEQ_PERIOD_CYCLES: u32 = 32_768;

const DUTY_TABLE: [[u8; 8]; 4] = [
    [0, 0, 0, 0, 0, 0, 0, 1], // 12.5%
    [1, 0, 0, 0, 0, 0, 0, 1], // 25.0%
    [1, 0, 0, 0, 0, 1, 1, 1], // 50.0%
    [0, 1, 1, 1, 1, 1, 1, 0], // 75.0%
];

#[derive(Clone)]
pub struct DirectSoundFifo {
    buf: [i8; 32],
    head: usize,
    len: usize,
    pub latched: i8,
}

impl Default for DirectSoundFifo {
    fn default() -> Self {
        Self::new()
    }
}

impl DirectSoundFifo {
    pub fn new() -> Self {
        Self {
            buf: [0; 32],
            head: 0,
            len: 0,
            latched: 0,
        }
    }

    #[inline]
    pub fn reset(&mut self) {
        self.buf = [0; 32];
        self.head = 0;
        self.len = 0;
        self.latched = 0;
    }

    #[inline]
    pub fn len(&self) -> usize {
        self.len
    }

    #[inline]
    pub fn push_byte(&mut self, sample: i8) {
        if self.len < 32 {
            let tail = (self.head + self.len) & 31;
            self.buf[tail] = sample;
            self.len += 1;
        }
    }

    #[inline]
    pub fn push_u16(&mut self, val: u16) {
        self.push_byte((val & 0xFF) as u8 as i8);
        self.push_byte((val >> 8) as u8 as i8);
    }

    #[inline]
    pub fn push_u32(&mut self, val: u32) {
        self.push_byte((val & 0xFF) as u8 as i8);
        self.push_byte(((val >> 8) & 0xFF) as u8 as i8);
        self.push_byte(((val >> 16) & 0xFF) as u8 as i8);
        self.push_byte(((val >> 24) & 0xFF) as u8 as i8);
    }

    #[inline]
    pub fn pop_sample(&mut self) -> i8 {
        if self.len > 0 {
            let s = self.buf[self.head];
            self.head = (self.head + 1) & 31;
            self.len -= 1;
            self.latched = s;
        } else {
            self.latched = 0;
        }
        self.latched
    }
}

#[derive(Clone, Default)]
struct SquareChannel {
    has_sweep: bool,
    enabled: bool,
    dac_enabled: bool,

    // Sweep (Channel 1 only)
    sweep_period: u8,
    sweep_negate: bool,
    sweep_shift: u8,
    sweep_timer: u8,
    sweep_enabled: bool,
    shadow_freq: u16,

    // Duty & Length
    duty: u8,
    duty_step: u8,
    length_counter: u16,
    length_enabled: bool,

    // Envelope
    env_init_vol: u8,
    env_add: bool,
    env_period: u8,
    env_timer: u8,
    volume: u8,

    // Frequency timer (in GBA 16.78 MHz cycles: 1 DMG T-cycle = 4 GBA cycles, period = (2048 - freq) * 16 GBA cycles)
    freq: u16,
    timer: i32,

    reg_cnt_l: u16,
    reg_cnt_h: u16,
    reg_cnt_x: u16,
}

impl SquareChannel {
    fn new(has_sweep: bool) -> Self {
        Self {
            has_sweep,
            ..Self::default()
        }
    }

    fn reset(&mut self) {
        let has_sweep = self.has_sweep;
        *self = Self::new(has_sweep);
    }

    fn calc_sweep_freq(&mut self) -> Option<u16> {
        let delta = self.shadow_freq >> self.sweep_shift;
        let new_freq = if self.sweep_negate {
            self.shadow_freq.wrapping_sub(delta)
        } else {
            self.shadow_freq.wrapping_add(delta)
        };
        if new_freq > 2047 {
            self.enabled = false;
            None
        } else {
            Some(new_freq)
        }
    }

    fn write_sweep(&mut self, val: u16) {
        self.reg_cnt_l = val & 0x007F;
        self.sweep_shift = (val & 7) as u8;
        self.sweep_negate = (val & 8) != 0;
        self.sweep_period = ((val >> 4) & 7) as u8;
    }

    fn write_duty_env(&mut self, val: u16) {
        self.reg_cnt_h = val;
        self.length_counter = 64 - (val & 0x3F);
        self.duty = ((val >> 6) & 3) as u8;

        let env = (val >> 8) as u8;
        self.env_period = env & 7;
        self.env_add = (env & 8) != 0;
        self.env_init_vol = (env >> 4) & 0x0F;
        self.dac_enabled = (env & 0xF8) != 0;
        if !self.dac_enabled {
            self.enabled = false;
        }
    }

    fn write_freq_ctrl(&mut self, val: u16) {
        self.reg_cnt_x = val;
        self.freq = val & 0x07FF;
        self.length_enabled = (val & 0x4000) != 0;
        if (val & 0x8000) != 0 {
            self.trigger();
        }
    }

    fn trigger(&mut self) {
        self.enabled = self.dac_enabled;
        if self.length_counter == 0 {
            self.length_counter = 64;
        }
        self.timer = ((2048 - self.freq as i32) * 16).max(16);
        self.volume = self.env_init_vol;
        self.env_timer = if self.env_period == 0 { 8 } else { self.env_period };

        if self.has_sweep {
            self.shadow_freq = self.freq;
            self.sweep_timer = if self.sweep_period == 0 {
                8
            } else {
                self.sweep_period
            };
            self.sweep_enabled = self.sweep_period != 0 || self.sweep_shift != 0;
            if self.sweep_shift != 0 {
                let _ = self.calc_sweep_freq();
            }
        }
    }

    #[inline]
    fn step_cycles(&mut self, cycles: i32) {
        if !self.enabled {
            return;
        }
        self.timer -= cycles;
        let period = ((2048 - self.freq as i32) * 16).max(16);
        while self.timer <= 0 {
            self.timer += period;
            self.duty_step = (self.duty_step + 1) & 7;
        }
    }

    fn clock_length(&mut self) {
        if self.length_enabled && self.length_counter > 0 {
            self.length_counter -= 1;
            if self.length_counter == 0 {
                self.enabled = false;
            }
        }
    }

    fn clock_sweep(&mut self) {
        if !self.has_sweep {
            return;
        }
        if self.sweep_timer > 0 {
            self.sweep_timer -= 1;
        }
        if self.sweep_timer == 0 {
            self.sweep_timer = if self.sweep_period == 0 {
                8
            } else {
                self.sweep_period
            };
            if self.sweep_enabled && self.sweep_period > 0 {
                if let Some(new_freq) = self.calc_sweep_freq() {
                    if self.sweep_shift > 0 {
                        self.freq = new_freq;
                        self.shadow_freq = new_freq;
                        let _ = self.calc_sweep_freq();
                    }
                }
            }
        }
    }

    fn clock_envelope(&mut self) {
        if self.env_period == 0 {
            return;
        }
        if self.env_timer > 0 {
            self.env_timer -= 1;
        }
        if self.env_timer == 0 {
            self.env_timer = self.env_period;
            if self.env_add && self.volume < 15 {
                self.volume += 1;
            } else if !self.env_add && self.volume > 0 {
                self.volume -= 1;
            }
        }
    }

    #[inline]
    fn sample(&self) -> i16 {
        if !self.enabled || !self.dac_enabled {
            return 0;
        }
        let high = DUTY_TABLE[self.duty as usize][self.duty_step as usize];
        if high != 0 {
            self.volume as i16
        } else {
            -(self.volume as i16)
        }
    }
}

#[derive(Clone)]
struct WaveChannel {
    enabled: bool,
    dac_enabled: bool,
    dimension_64: bool,
    bank_select: u8,
    wave_ram: [[u8; 16]; 2],

    length_counter: u16,
    length_enabled: bool,
    vol_code: u8,
    force_75: bool,

    freq: u16,
    timer: i32,
    pos: u8,

    reg_cnt_l: u16,
    reg_cnt_h: u16,
    reg_cnt_x: u16,
}

impl Default for WaveChannel {
    fn default() -> Self {
        Self {
            enabled: false,
            dac_enabled: false,
            dimension_64: false,
            bank_select: 0,
            wave_ram: [[0; 16]; 2],
            length_counter: 0,
            length_enabled: false,
            vol_code: 0,
            force_75: false,
            freq: 0,
            timer: 0,
            pos: 0,
            reg_cnt_l: 0,
            reg_cnt_h: 0,
            reg_cnt_x: 0,
        }
    }
}

impl WaveChannel {
    fn reset(&mut self) {
        let ram = self.wave_ram;
        *self = Self::default();
        self.wave_ram = ram;
    }

    fn write_cnt_l(&mut self, val: u16) {
        self.reg_cnt_l = val & 0x00E0;
        self.dimension_64 = (val & 0x0020) != 0;
        self.bank_select = ((val >> 6) & 1) as u8;
        self.dac_enabled = (val & 0x0080) != 0;
        if !self.dac_enabled {
            self.enabled = false;
        }
    }

    fn write_cnt_h(&mut self, val: u16) {
        self.reg_cnt_h = val;
        self.length_counter = 256 - (val & 0x00FF);
        self.vol_code = ((val >> 13) & 3) as u8;
        self.force_75 = (val & 0x8000) != 0;
    }

    fn write_cnt_x(&mut self, val: u16) {
        self.reg_cnt_x = val;
        self.freq = val & 0x07FF;
        self.length_enabled = (val & 0x4000) != 0;
        if (val & 0x8000) != 0 {
            self.enabled = self.dac_enabled;
            if self.length_counter == 0 {
                self.length_counter = 256;
            }
            self.timer = ((2048 - self.freq as i32) * 8).max(8);
            self.pos = 0;
        }
    }

    /// CPU accesses the NON-selected Wave RAM bank (`bank_select ^ 1`).
    #[inline]
    fn read_wave_byte(&self, offset: usize) -> u8 {
        let cpu_bank = (self.bank_select ^ 1) as usize;
        self.wave_ram[cpu_bank][offset & 15]
    }

    #[inline]
    fn write_wave_byte(&mut self, offset: usize, val: u8) {
        let cpu_bank = (self.bank_select ^ 1) as usize;
        self.wave_ram[cpu_bank][offset & 15] = val;
    }

    #[inline]
    fn step_cycles(&mut self, cycles: i32) {
        if !self.enabled {
            return;
        }
        self.timer -= cycles;
        let period = ((2048 - self.freq as i32) * 8).max(8);
        while self.timer <= 0 {
            self.timer += period;
            self.pos = (self.pos + 1) & 31;
            if self.pos == 0 && self.dimension_64 {
                self.bank_select ^= 1;
            }
        }
    }

    fn clock_length(&mut self) {
        if self.length_enabled && self.length_counter > 0 {
            self.length_counter -= 1;
            if self.length_counter == 0 {
                self.enabled = false;
            }
        }
    }

    #[inline]
    fn sample(&self) -> i16 {
        if !self.enabled || !self.dac_enabled {
            return 0;
        }
        let byte = self.wave_ram[self.bank_select as usize][(self.pos >> 1) as usize];
        let nibble = if (self.pos & 1) == 0 {
            byte >> 4
        } else {
            byte & 0x0F
        };
        let centered = (nibble as i16) * 2 - 15; // -15..+15
        if self.force_75 {
            (centered * 3) >> 2
        } else {
            match self.vol_code {
                0 => 0,
                1 => centered,
                2 => centered >> 1,
                _ => centered >> 2,
            }
        }
    }
}

#[derive(Clone, Default)]
struct NoiseChannel {
    enabled: bool,
    dac_enabled: bool,
    length_counter: u16,
    length_enabled: bool,

    env_init_vol: u8,
    env_add: bool,
    env_period: u8,
    env_timer: u8,
    volume: u8,

    div_code: u8,
    width_7: bool,
    shift_clock: u8,
    lfsr: u16,
    timer: i32,

    reg_cnt_l: u16,
    reg_cnt_h: u16,
}

impl NoiseChannel {
    fn reset(&mut self) {
        *self = Self::default();
        self.lfsr = 0x7FFF;
    }

    fn period_cycles(&self) -> i32 {
        let base = match self.div_code {
            0 => 8,
            n => (n as i32) * 16,
        };
        // 1 DMG T-cycle = 4 GBA cycles
        (base << (self.shift_clock.min(14) as u32)) * 4
    }

    fn write_cnt_l(&mut self, val: u16) {
        self.reg_cnt_l = val;
        self.length_counter = 64 - (val & 0x3F);
        let env = (val >> 8) as u8;
        self.env_period = env & 7;
        self.env_add = (env & 8) != 0;
        self.env_init_vol = (env >> 4) & 0x0F;
        self.dac_enabled = (env & 0xF8) != 0;
        if !self.dac_enabled {
            self.enabled = false;
        }
    }

    fn write_cnt_h(&mut self, val: u16) {
        self.reg_cnt_h = val;
        self.div_code = (val & 7) as u8;
        self.width_7 = (val & 8) != 0;
        self.shift_clock = ((val >> 4) & 0x0F) as u8;
        self.length_enabled = (val & 0x4000) != 0;
        if (val & 0x8000) != 0 {
            self.enabled = self.dac_enabled;
            if self.length_counter == 0 {
                self.length_counter = 64;
            }
            self.lfsr = 0x7FFF;
            self.volume = self.env_init_vol;
            self.env_timer = if self.env_period == 0 { 8 } else { self.env_period };
            self.timer = self.period_cycles().max(16);
        }
    }

    #[inline]
    fn step_cycles(&mut self, cycles: i32) {
        if !self.enabled || self.shift_clock >= 14 {
            return;
        }
        self.timer -= cycles;
        let period = self.period_cycles().max(16);
        while self.timer <= 0 {
            self.timer += period;
            let xor = (self.lfsr & 1) ^ ((self.lfsr >> 1) & 1);
            self.lfsr = (self.lfsr >> 1) | (xor << 14);
            if self.width_7 {
                self.lfsr = (self.lfsr & !0x0040) | (xor << 6);
            }
        }
    }

    fn clock_length(&mut self) {
        if self.length_enabled && self.length_counter > 0 {
            self.length_counter -= 1;
            if self.length_counter == 0 {
                self.enabled = false;
            }
        }
    }

    fn clock_envelope(&mut self) {
        if self.env_period == 0 {
            return;
        }
        if self.env_timer > 0 {
            self.env_timer -= 1;
        }
        if self.env_timer == 0 {
            self.env_timer = self.env_period;
            if self.env_add && self.volume < 15 {
                self.volume += 1;
            } else if !self.env_add && self.volume > 0 {
                self.volume -= 1;
            }
        }
    }

    #[inline]
    fn sample(&self) -> i16 {
        if !self.enabled || !self.dac_enabled {
            return 0;
        }
        if (self.lfsr & 1) == 0 {
            self.volume as i16
        } else {
            -(self.volume as i16)
        }
    }
}

pub struct GbaApu {
    pub fifo_a: DirectSoundFifo,
    pub fifo_b: DirectSoundFifo,

    ch1: SquareChannel,
    ch2: SquareChannel,
    ch3: WaveChannel,
    ch4: NoiseChannel,

    pub soundcnt_l: u16,
    pub soundcnt_h: u16,
    pub soundcnt_x: u16,
    pub soundbias: u16,

    frame_seq_cycles: u32,
    frame_seq_step: u8,

    // Box-filter / accumulator anti-aliasing resampler from 16.78 MHz clock -> 44,100 Hz
    sample_phase: u64,
    accum_l: i64,
    accum_r: i64,
    accum_weight: i64,

    // Gentle high-pass DC blocker (~12 Hz @ 44.1 kHz)
    dc_prev_in_l: f32,
    dc_prev_out_l: f32,
    dc_prev_in_r: f32,
    dc_prev_out_r: f32,

    pub out_samples: Vec<i16>,
}

impl Default for GbaApu {
    fn default() -> Self {
        Self::new()
    }
}

impl GbaApu {
    pub fn new() -> Self {
        let mut ch4 = NoiseChannel::default();
        ch4.lfsr = 0x7FFF;
        Self {
            fifo_a: DirectSoundFifo::new(),
            fifo_b: DirectSoundFifo::new(),
            ch1: SquareChannel::new(true),
            ch2: SquareChannel::new(false),
            ch3: WaveChannel::default(),
            ch4,
            soundcnt_l: 0,
            soundcnt_h: 0x0B0E,
            soundcnt_x: 0x0080,
            soundbias: 0x0200,
            frame_seq_cycles: 0,
            frame_seq_step: 0,
            sample_phase: 0,
            accum_l: 0,
            accum_r: 0,
            accum_weight: 0,
            dc_prev_in_l: 0.0,
            dc_prev_out_l: 0.0,
            dc_prev_in_r: 0.0,
            dc_prev_out_r: 0.0,
            out_samples: Vec::with_capacity(4096),
        }
    }

    pub fn reset_registers(&mut self) {
        self.fifo_a.reset();
        self.fifo_b.reset();
        self.ch1.reset();
        self.ch2.reset();
        self.ch3.reset();
        self.ch4.reset();
        self.soundcnt_l = 0;
        self.soundcnt_h = 0;
        self.soundcnt_x = 0;
        self.soundbias = 0x0200;
    }

    #[inline]
    pub fn master_enabled(&self) -> bool {
        (self.soundcnt_x & 0x0080) != 0
    }

    #[inline]
    pub fn dsa_timer(&self) -> usize {
        if (self.soundcnt_h & 0x0400) != 0 {
            1
        } else {
            0
        }
    }

    #[inline]
    pub fn dsb_timer(&self) -> usize {
        if (self.soundcnt_h & 0x4000) != 0 {
            1
        } else {
            0
        }
    }

    pub fn read_u16(&self, offset: u32) -> u16 {
        match offset {
            0x060 => self.ch1.reg_cnt_l & 0x007F,
            0x062 => self.ch1.reg_cnt_h & 0xFFC0,
            0x064 => self.ch1.reg_cnt_x & 0x4000,
            0x068 => self.ch2.reg_cnt_h & 0xFFC0,
            0x06C => self.ch2.reg_cnt_x & 0x4000,
            0x070 => self.ch3.reg_cnt_l & 0x00E0,
            0x072 => self.ch3.reg_cnt_h & 0xE000,
            0x074 => self.ch3.reg_cnt_x & 0x4000,
            0x078 => self.ch4.reg_cnt_l & 0xFF00,
            0x07C => self.ch4.reg_cnt_h & 0x40FF,
            0x080 => self.soundcnt_l & 0xFF77,
            0x082 => self.soundcnt_h & 0x770F,
            0x084 => {
                let mut st = self.soundcnt_x & 0x0080;
                if self.ch1.enabled {
                    st |= 0x0001;
                }
                if self.ch2.enabled {
                    st |= 0x0002;
                }
                if self.ch3.enabled {
                    st |= 0x0004;
                }
                if self.ch4.enabled {
                    st |= 0x0008;
                }
                st
            }
            0x088 => self.soundbias,
            0x090..=0x09E => {
                let base = (offset - 0x090) as usize;
                let lo = self.ch3.read_wave_byte(base) as u16;
                let hi = self.ch3.read_wave_byte(base + 1) as u16;
                lo | (hi << 8)
            }
            _ => 0,
        }
    }

    pub fn write_u16(&mut self, offset: u32, val: u16) {
        match offset {
            0x082 => {
                if (val & 0x0800) != 0 {
                    self.fifo_a.reset();
                }
                if (val & 0x8000) != 0 {
                    self.fifo_b.reset();
                }
                self.soundcnt_h = val & 0x770F;
            }
            0x084 => {
                let was_on = self.master_enabled();
                let now_on = (val & 0x0080) != 0;
                self.soundcnt_x = val & 0x0080;
                if was_on && !now_on {
                    self.ch1.reset();
                    self.ch2.reset();
                    self.ch3.reset();
                    self.ch4.reset();
                    self.soundcnt_l = 0;
                }
            }
            0x088 => {
                self.soundbias = val & 0xC3FE;
            }
            0x090..=0x09E => {
                let base = (offset - 0x090) as usize;
                self.ch3.write_wave_byte(base, (val & 0xFF) as u8);
                self.ch3.write_wave_byte(base + 1, (val >> 8) as u8);
            }
            0x0A0 | 0x0A2 => {
                self.fifo_a.push_u16(val);
            }
            0x0A4 | 0x0A6 => {
                self.fifo_b.push_u16(val);
            }
            _ => {
                if !self.master_enabled() {
                    return;
                }
                match offset {
                    0x060 => self.ch1.write_sweep(val),
                    0x062 => self.ch1.write_duty_env(val),
                    0x064 => self.ch1.write_freq_ctrl(val),
                    0x068 => self.ch2.write_duty_env(val),
                    0x06C => self.ch2.write_freq_ctrl(val),
                    0x070 => self.ch3.write_cnt_l(val),
                    0x072 => self.ch3.write_cnt_h(val),
                    0x074 => self.ch3.write_cnt_x(val),
                    0x078 => self.ch4.write_cnt_l(val),
                    0x07C => self.ch4.write_cnt_h(val),
                    0x080 => self.soundcnt_l = val & 0xFF77,
                    _ => {}
                }
            }
        }
    }

    pub fn write_u8(&mut self, offset: u32, val: u8) {
        match offset {
            0x082 => {
                let next = (self.soundcnt_h & 0xFF00) | (val as u16);
                self.write_u16(0x082, next);
            }
            0x083 => {
                let next = (self.soundcnt_h & 0x00FF) | ((val as u16) << 8);
                self.write_u16(0x082, next);
            }
            0x084 => {
                self.write_u16(0x084, val as u16);
            }
            0x090..=0x09F => {
                self.ch3.write_wave_byte((offset - 0x090) as usize, val);
            }
            0x0A0..=0x0A3 => {
                self.fifo_a.push_byte(val as i8);
            }
            0x0A4..=0x0A7 => {
                self.fifo_b.push_byte(val as i8);
            }
            _ => {
                let aligned = offset & !1;
                let cur = self.read_u16(aligned);
                let next = if (offset & 1) == 0 {
                    (cur & 0xFF00) | (val as u16)
                } else {
                    (cur & 0x00FF) | ((val as u16) << 8)
                };
                self.write_u16(aligned, next);
            }
        }
    }

    #[inline]
    fn current_mix_10bit(&self) -> (i32, i32) {
        if !self.master_enabled() {
            return (0, 0);
        }

        let mut left: i32 = 0;
        let mut right: i32 = 0;

        // DirectSound A (-128..127 -> *4 for 100% [-512..508], *2 for 50% [-256..254])
        let dsa_shift = if (self.soundcnt_h & 0x0004) != 0 { 2 } else { 1 };
        let dsa = (self.fifo_a.latched as i32) << dsa_shift;
        if (self.soundcnt_h & 0x0200) != 0 {
            left += dsa;
        }
        if (self.soundcnt_h & 0x0100) != 0 {
            right += dsa;
        }

        // DirectSound B
        let dsb_shift = if (self.soundcnt_h & 0x0008) != 0 { 2 } else { 1 };
        let dsb = (self.fifo_b.latched as i32) << dsb_shift;
        if (self.soundcnt_h & 0x2000) != 0 {
            left += dsb;
        }
        if (self.soundcnt_h & 0x1000) != 0 {
            right += dsb;
        }

        // CGB 4-channel PSG
        if (self.soundcnt_l & 0xFF00) != 0 {
            let s1 = self.ch1.sample() as i32;
            let s2 = self.ch2.sample() as i32;
            let s3 = self.ch3.sample() as i32;
            let s4 = self.ch4.sample() as i32;

            let mut psg_r = 0i32;
            let mut psg_l = 0i32;
            if (self.soundcnt_l & 0x0100) != 0 {
                psg_r += s1;
            }
            if (self.soundcnt_l & 0x0200) != 0 {
                psg_r += s2;
            }
            if (self.soundcnt_l & 0x0400) != 0 {
                psg_r += s3;
            }
            if (self.soundcnt_l & 0x0800) != 0 {
                psg_r += s4;
            }
            if (self.soundcnt_l & 0x1000) != 0 {
                psg_l += s1;
            }
            if (self.soundcnt_l & 0x2000) != 0 {
                psg_l += s2;
            }
            if (self.soundcnt_l & 0x4000) != 0 {
                psg_l += s3;
            }
            if (self.soundcnt_l & 0x8000) != 0 {
                psg_l += s4;
            }

            let vol_r = ((self.soundcnt_l & 7) + 1) as i32;
            let vol_l = (((self.soundcnt_l >> 4) & 7) + 1) as i32;
            psg_r *= vol_r;
            psg_l *= vol_l;

            // CGB ratio: 0 = 25%, 1 = 50%, 2 = 100%
            match self.soundcnt_h & 3 {
                0 => {
                    psg_r >>= 2;
                    psg_l >>= 2;
                }
                1 => {
                    psg_r >>= 1;
                    psg_l >>= 1;
                }
                2 => {}
                _ => {
                    psg_r = 0;
                    psg_l = 0;
                }
            }

            left += psg_l;
            right += psg_r;
        }

        (left.clamp(-512, 511), right.clamp(-512, 511))
    }

    fn step_cgb(&mut self, mut cycles: u32) {
        if !self.master_enabled() {
            return;
        }
        while cycles > 0 {
            let until_seq = FRAME_SEQ_PERIOD_CYCLES - self.frame_seq_cycles;
            let chunk = cycles.min(until_seq);
            let ci = chunk as i32;
            self.ch1.step_cycles(ci);
            self.ch2.step_cycles(ci);
            self.ch3.step_cycles(ci);
            self.ch4.step_cycles(ci);

            self.frame_seq_cycles += chunk;
            cycles -= chunk;
            if self.frame_seq_cycles >= FRAME_SEQ_PERIOD_CYCLES {
                self.frame_seq_cycles -= FRAME_SEQ_PERIOD_CYCLES;
                match self.frame_seq_step {
                    0 | 4 => {
                        self.ch1.clock_length();
                        self.ch2.clock_length();
                        self.ch3.clock_length();
                        self.ch4.clock_length();
                    }
                    2 | 6 => {
                        self.ch1.clock_length();
                        self.ch2.clock_length();
                        self.ch3.clock_length();
                        self.ch4.clock_length();
                        self.ch1.clock_sweep();
                    }
                    7 => {
                        self.ch1.clock_envelope();
                        self.ch2.clock_envelope();
                        self.ch4.clock_envelope();
                    }
                    _ => {}
                }
                self.frame_seq_step = (self.frame_seq_step + 1) & 7;
            }
        }
    }

    /// Advance APU by `cycles` GBA CPU cycles. When `emit_audio` is false (fast-forward seek),
    /// skips output resampling while maintaining exact CGB & FIFO state.
    pub fn advance(&mut self, cycles: u32, emit_audio: bool, gain_scale: f32) {
        if cycles == 0 {
            return;
        }

        if !emit_audio {
            self.step_cgb(cycles);
            return;
        }

        let mut rem = cycles;
        let rate = OUTPUT_SAMPLE_RATE as u64;
        let cgb_active = self.master_enabled()
            && (self.soundcnt_l & 0xFF00) != 0
            && (self.ch1.enabled || self.ch2.enabled || self.ch3.enabled || self.ch4.enabled);

        while rem > 0 {
            // Only subdivide into 128-cycle micro-slices when CGB square/wave/noise channels are active;
            // pure DirectSound FIFO samples only change on Timer 0/1 overflows (already sub-stepped by GbaBus)!
            let max_step = if cgb_active { rem.min(128) } else { rem };
            if cgb_active {
                self.step_cgb(max_step);
            }
            let (l10, r10) = self.current_mix_10bit();

            let mut phase_rem = (max_step as u64) * rate;
            while phase_rem > 0 {
                let until_sample = GBA_CPU_CLOCK - self.sample_phase;
                if phase_rem < until_sample {
                    self.accum_l += (l10 as i64) * (phase_rem as i64);
                    self.accum_r += (r10 as i64) * (phase_rem as i64);
                    self.accum_weight += phase_rem as i64;
                    self.sample_phase += phase_rem;
                    phase_rem = 0;
                } else {
                    self.accum_l += (l10 as i64) * (until_sample as i64);
                    self.accum_r += (r10 as i64) * (until_sample as i64);
                    self.accum_weight += until_sample as i64;

                    let denom = self.accum_weight.max(1) as f32;
                    // 10-bit [-512..511] * 64.0 => 16-bit [-32768..32704]
                    let raw_l = (self.accum_l as f32 / denom) * 64.0 * gain_scale;
                    let raw_r = (self.accum_r as f32 / denom) * 64.0 * gain_scale;

                    // 1-pole DC blocker (R = 0.9982 -> ~12.6 Hz cutoff at 44.1 kHz)
                    let out_l = raw_l - self.dc_prev_in_l + 0.9982 * self.dc_prev_out_l;
                    self.dc_prev_in_l = raw_l;
                    self.dc_prev_out_l = out_l;

                    let out_r = raw_r - self.dc_prev_in_r + 0.9982 * self.dc_prev_out_r;
                    self.dc_prev_in_r = raw_r;
                    self.dc_prev_out_r = out_r;

                    self.out_samples
                        .push(out_l.clamp(-32768.0, 32767.0) as i16);
                    self.out_samples
                        .push(out_r.clamp(-32768.0, 32767.0) as i16);

                    phase_rem -= until_sample;
                    self.accum_l = 0;
                    self.accum_r = 0;
                    self.accum_weight = 0;
                    self.sample_phase = 0;
                }
            }

            rem -= max_step;
        }
    }
}
