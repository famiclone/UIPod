//! Game Boy Advance Memory Bus, Timers 0-3, DMA 0-3, LCD Timing, and Interrupts.

use super::apu::GbaApu;

const HDRAW_CYCLES: u32 = 960;
const HBLANK_CYCLES: u32 = 272;
const SCANLINE_CYCLES: u32 = HDRAW_CYCLES + HBLANK_CYCLES; // 1232
const TOTAL_SCANLINES: u16 = 228; // 0..159 visible, 160..227 VBlank

pub const IRQ_VBLANK: u16 = 1 << 0;
pub const IRQ_HBLANK: u16 = 1 << 1;
pub const IRQ_VCOUNT: u16 = 1 << 2;
pub const IRQ_TIMER0: u16 = 1 << 3;
pub const IRQ_DMA0: u16 = 1 << 8;

#[derive(Clone, Copy, Default)]
pub struct GbaTimer {
    pub reload: u16,
    pub counter: u16,
    pub control: u16,
    pub subcycles: u32,
}

impl GbaTimer {
    #[inline]
    pub fn enabled(&self) -> bool {
        (self.control & 0x0080) != 0
    }

    #[inline]
    pub fn cascade(&self) -> bool {
        (self.control & 0x0004) != 0
    }

    #[inline]
    pub fn irq_enabled(&self) -> bool {
        (self.control & 0x0040) != 0
    }

    #[inline]
    pub fn prescaler_shift(&self) -> u32 {
        match self.control & 3 {
            0 => 0,  // 1
            1 => 6,  // 64
            2 => 8,  // 256
            _ => 10, // 1024
        }
    }

    /// Cycles until this non-cascaded enabled timer next overflows (`>= 1`).
    #[inline]
    pub fn cycles_until_overflow(&self) -> Option<u32> {
        if !self.enabled() || self.cascade() {
            return None;
        }
        let shift = self.prescaler_shift();
        let prescale = 1u32 << shift;
        let ticks_needed = 0x10000u32 - (self.counter as u32);
        let total_cycles = (ticks_needed << shift).saturating_sub(self.subcycles);
        Some(total_cycles.max(prescale.saturating_sub(self.subcycles)).max(1))
    }
}

#[derive(Clone, Copy, Default)]
pub struct DmaChannel {
    pub sad: u32,
    pub dad: u32,
    pub cnt_l: u16,
    pub cnt_h: u16,
    pub internal_src: u32,
    pub internal_dst: u32,
    pub internal_count: u32,
}

impl DmaChannel {
    #[inline]
    pub fn enabled(&self) -> bool {
        (self.cnt_h & 0x8000) != 0
    }

    #[inline]
    pub fn timing(&self) -> u16 {
        (self.cnt_h >> 12) & 3
    }

    #[inline]
    pub fn is_32bit(&self) -> bool {
        (self.cnt_h & 0x0400) != 0
    }

    #[inline]
    pub fn repeat(&self) -> bool {
        (self.cnt_h & 0x0200) != 0
    }

    #[inline]
    pub fn irq_enabled(&self) -> bool {
        (self.cnt_h & 0x4000) != 0
    }
}

pub struct GbaBus {
    pub bios: Box<[u8; 0x4000]>,
    pub ewram: Vec<u8>, // 256 KiB
    pub iwram: Vec<u8>, // 32 KiB
    pub pram: Vec<u8>,  // 1 KiB
    pub vram: Vec<u8>,  // 96 KiB
    pub oam: Vec<u8>,   // 1 KiB
    pub rom: Vec<u8>,   // Dynamically sized up to 32 MiB
    pub sram: Vec<u8>,  // 64 KiB

    pub apu: GbaApu,
    pub timers: [GbaTimer; 4],
    pub dma: [DmaChannel; 4],

    // LCD / video timing registers
    pub dispcnt: u16,
    pub dispstat: u16,
    pub vcount: u16,
    line_cycles: u32,
    in_hblank: bool,
    other_io: Vec<u8>,

    // Interrupt & power control
    pub io_ie: u16,
    pub io_if: u16,
    pub io_ime: u16,
    pub waitcnt: u16,
    pub postflg: u8,
    pub halted: bool,

    pub total_cycles: u64,
}

impl GbaBus {
    pub fn new(rom: Vec<u8>, ewram_init: Option<&[u8]>) -> Self {
        let mut ewram = vec![0u8; 0x40000];
        if let Some(init) = ewram_init {
            let copy_len = init.len().min(ewram.len());
            ewram[..copy_len].copy_from_slice(&init[..copy_len]);
        }
        let bios_vec = vec![0u8; 0x4000].into_boxed_slice();
        let bios: Box<[u8; 0x4000]> = bios_vec.try_into().expect("bios 16KiB");
        let mut bus = Self {
            bios,
            ewram,
            iwram: vec![0u8; 0x8000],
            pram: vec![0u8; 0x400],
            vram: vec![0u8; 0x18000],
            oam: vec![0u8; 0x400],
            rom,
            sram: vec![0xFFu8; 0x10000],
            apu: GbaApu::new(),
            timers: [GbaTimer::default(); 4],
            dma: [DmaChannel::default(); 4],
            dispcnt: 0x0080,
            dispstat: 0,
            vcount: 0,
            line_cycles: 0,
            in_hblank: false,
            other_io: vec![0u8; 0x400],
            io_ie: 0,
            io_if: 0,
            io_ime: 0,
            waitcnt: 0,
            postflg: 1,
            halted: false,
            total_cycles: 0,
        };
        super::bios::install_bios_rom(&mut bus.bios);
        bus
    }

    #[inline]
    pub fn irq_pending(&self) -> bool {
        (self.io_ie & self.io_if) != 0
    }

    #[inline]
    pub fn cpu_should_take_irq(&self) -> bool {
        (self.io_ime & 1) != 0 && (self.io_ie & self.io_if) != 0
    }

    #[inline]
    pub fn read_u8(&self, addr: u32) -> u8 {
        match addr >> 24 {
            0x00 => {
                let off = addr as usize;
                if off < 0x4000 {
                    self.bios[off]
                } else {
                    0
                }
            }
            0x02 => self.ewram[(addr as usize) & 0x3FFFF],
            0x03 => self.iwram[(addr as usize) & 0x7FFF],
            0x04 => self.read_io_u8(addr & 0x00FF_FFFF),
            0x05 => self.pram[(addr as usize) & 0x3FF],
            0x06 => {
                let mut off = (addr as usize) & 0x1FFFF;
                if off >= 0x18000 {
                    off -= 0x8000;
                }
                self.vram[off]
            }
            0x07 => self.oam[(addr as usize) & 0x3FF],
            0x08..=0x0D => {
                let off = (addr as usize) & 0x01FF_FFFF;
                if off < self.rom.len() {
                    self.rom[off]
                } else {
                    let open = ((addr >> 1) & 0xFFFF) as u16;
                    if (addr & 1) == 0 {
                        (open & 0xFF) as u8
                    } else {
                        (open >> 8) as u8
                    }
                }
            }
            0x0E | 0x0F => self.sram[(addr as usize) & 0xFFFF],
            _ => 0,
        }
    }

    #[inline]
    pub fn read_u16_aligned(&self, addr: u32) -> u16 {
        let a = addr & !1;
        match a >> 24 {
            0x00 => {
                let off = a as usize;
                if off + 1 < 0x4000 {
                    u16::from_le_bytes([self.bios[off], self.bios[off + 1]])
                } else {
                    0
                }
            }
            0x02 => {
                let off = (a as usize) & 0x3FFFE;
                u16::from_le_bytes([self.ewram[off], self.ewram[off + 1]])
            }
            0x03 => {
                let off = (a as usize) & 0x7FFE;
                u16::from_le_bytes([self.iwram[off], self.iwram[off + 1]])
            }
            0x04 => self.read_io_u16(a & 0x00FF_FFFE),
            0x05 => {
                let off = (a as usize) & 0x3FE;
                u16::from_le_bytes([self.pram[off], self.pram[off + 1]])
            }
            0x06 => {
                let mut off = (a as usize) & 0x1FFFE;
                if off >= 0x18000 {
                    off -= 0x8000;
                }
                u16::from_le_bytes([self.vram[off], self.vram[off + 1]])
            }
            0x07 => {
                let off = (a as usize) & 0x3FE;
                u16::from_le_bytes([self.oam[off], self.oam[off + 1]])
            }
            0x08..=0x0D => {
                let off = (a as usize) & 0x01FF_FFFE;
                if off + 1 < self.rom.len() {
                    u16::from_le_bytes([self.rom[off], self.rom[off + 1]])
                } else if off < self.rom.len() {
                    self.rom[off] as u16
                } else {
                    ((a >> 1) & 0xFFFF) as u16
                }
            }
            0x0E | 0x0F => {
                let b = self.sram[(addr as usize) & 0xFFFF] as u16;
                b | (b << 8)
            }
            _ => 0,
        }
    }

    #[inline]
    pub fn read_u32_aligned(&self, addr: u32) -> u32 {
        let a = addr & !3;
        match a >> 24 {
            0x00 => {
                let off = a as usize;
                if off + 3 < 0x4000 {
                    u32::from_le_bytes([
                        self.bios[off],
                        self.bios[off + 1],
                        self.bios[off + 2],
                        self.bios[off + 3],
                    ])
                } else {
                    0
                }
            }
            0x02 => {
                let off = (a as usize) & 0x3FFFC;
                u32::from_le_bytes([
                    self.ewram[off],
                    self.ewram[off + 1],
                    self.ewram[off + 2],
                    self.ewram[off + 3],
                ])
            }
            0x03 => {
                let off = (a as usize) & 0x7FFC;
                u32::from_le_bytes([
                    self.iwram[off],
                    self.iwram[off + 1],
                    self.iwram[off + 2],
                    self.iwram[off + 3],
                ])
            }
            0x08..=0x0D => {
                let off = (a as usize) & 0x01FF_FFFC;
                if off + 3 < self.rom.len() {
                    u32::from_le_bytes([
                        self.rom[off],
                        self.rom[off + 1],
                        self.rom[off + 2],
                        self.rom[off + 3],
                    ])
                } else {
                    let lo = self.read_u16_aligned(a) as u32;
                    let hi = self.read_u16_aligned(a + 2) as u32;
                    lo | (hi << 16)
                }
            }
            0x0E | 0x0F => {
                let b = self.sram[(addr as usize) & 0xFFFF] as u32;
                b * 0x0101_0101
            }
            _ => {
                let lo = self.read_u16_aligned(a) as u32;
                let hi = self.read_u16_aligned(a + 2) as u32;
                lo | (hi << 16)
            }
        }
    }

    #[inline]
    pub fn write_u8(&mut self, addr: u32, val: u8) {
        match addr >> 24 {
            0x02 => self.ewram[(addr as usize) & 0x3FFFF] = val,
            0x03 => self.iwram[(addr as usize) & 0x7FFF] = val,
            0x04 => self.write_io_u8(addr & 0x00FF_FFFF, val),
            0x05 => {
                let off = (addr as usize) & 0x3FE;
                self.pram[off] = val;
                self.pram[off + 1] = val;
            }
            0x06 => {
                let mut off = (addr as usize) & 0x1FFFE;
                if off >= 0x18000 {
                    off -= 0x8000;
                }
                self.vram[off] = val;
                self.vram[off + 1] = val;
            }
            0x0E | 0x0F => self.sram[(addr as usize) & 0xFFFF] = val,
            _ => {}
        }
    }

    #[inline]
    pub fn write_u16_aligned(&mut self, addr: u32, val: u16) {
        let a = addr & !1;
        match a >> 24 {
            0x02 => {
                let off = (a as usize) & 0x3FFFE;
                let bytes = val.to_le_bytes();
                self.ewram[off] = bytes[0];
                self.ewram[off + 1] = bytes[1];
            }
            0x03 => {
                let off = (a as usize) & 0x7FFE;
                let bytes = val.to_le_bytes();
                self.iwram[off] = bytes[0];
                self.iwram[off + 1] = bytes[1];
            }
            0x04 => self.write_io_u16(a & 0x00FF_FFFE, val),
            0x05 => {
                let off = (a as usize) & 0x3FE;
                let bytes = val.to_le_bytes();
                self.pram[off] = bytes[0];
                self.pram[off + 1] = bytes[1];
            }
            0x06 => {
                let mut off = (a as usize) & 0x1FFFE;
                if off >= 0x18000 {
                    off -= 0x8000;
                }
                let bytes = val.to_le_bytes();
                self.vram[off] = bytes[0];
                self.vram[off + 1] = bytes[1];
            }
            0x07 => {
                let off = (a as usize) & 0x3FE;
                let bytes = val.to_le_bytes();
                self.oam[off] = bytes[0];
                self.oam[off + 1] = bytes[1];
            }
            0x0E | 0x0F => {
                self.sram[(addr as usize) & 0xFFFF] = (val & 0xFF) as u8;
            }
            _ => {}
        }
    }

    #[inline]
    pub fn write_u32_aligned(&mut self, addr: u32, val: u32) {
        let a = addr & !3;
        match a >> 24 {
            0x02 => {
                let off = (a as usize) & 0x3FFFC;
                self.ewram[off..off + 4].copy_from_slice(&val.to_le_bytes());
            }
            0x03 => {
                let off = (a as usize) & 0x7FFC;
                self.iwram[off..off + 4].copy_from_slice(&val.to_le_bytes());
            }
            0x04 => {
                let io_off = a & 0x00FF_FFFC;
                if io_off == 0x0A0 {
                    self.apu.fifo_a.push_u32(val);
                } else if io_off == 0x0A4 {
                    self.apu.fifo_b.push_u32(val);
                } else {
                    self.write_io_u16(io_off, (val & 0xFFFF) as u16);
                    self.write_io_u16(io_off + 2, (val >> 16) as u16);
                }
            }
            0x05 => {
                let off = (a as usize) & 0x3FC;
                self.pram[off..off + 4].copy_from_slice(&val.to_le_bytes());
            }
            0x06 => {
                let mut off = (a as usize) & 0x1FFFC;
                if off >= 0x18000 {
                    off -= 0x8000;
                }
                self.vram[off..off + 4].copy_from_slice(&val.to_le_bytes());
            }
            0x07 => {
                let off = (a as usize) & 0x3FC;
                self.oam[off..off + 4].copy_from_slice(&val.to_le_bytes());
            }
            0x0E | 0x0F => {
                self.sram[(addr as usize) & 0xFFFF] = (val & 0xFF) as u8;
            }
            _ => {}
        }
    }

    fn read_io_u8(&self, offset: u32) -> u8 {
        if offset == 0x300 {
            return self.postflg;
        }
        if offset == 0x301 {
            return 0;
        }
        let val16 = self.read_io_u16(offset & !1);
        if (offset & 1) == 0 {
            (val16 & 0xFF) as u8
        } else {
            (val16 >> 8) as u8
        }
    }

    fn read_io_u16(&self, offset: u32) -> u16 {
        match offset {
            0x000 => self.dispcnt,
            0x004 => {
                let mut st = self.dispstat & 0xFFF8;
                if (160..227).contains(&self.vcount) {
                    st |= 1;
                }
                if self.in_hblank {
                    st |= 2;
                }
                let lyc = (self.dispstat >> 8) & 0xFF;
                if self.vcount == lyc {
                    st |= 4;
                }
                st
            }
            0x006 => self.vcount,
            0x060..=0x0A6 => self.apu.read_u16(offset),
            0x0B8 => 0,
            0x0BA => self.dma[0].cnt_h & 0xF7E0,
            0x0C4 => 0,
            0x0C6 => self.dma[1].cnt_h & 0xF7E0,
            0x0D0 => 0,
            0x0D2 => self.dma[2].cnt_h & 0xF7E0,
            0x0DC => 0,
            0x0DE => self.dma[3].cnt_h & 0xFFE0,
            0x100 => self.timers[0].counter,
            0x102 => self.timers[0].control,
            0x104 => self.timers[1].counter,
            0x106 => self.timers[1].control,
            0x108 => self.timers[2].counter,
            0x10A => self.timers[2].control,
            0x10C => self.timers[3].counter,
            0x10E => self.timers[3].control,
            0x130 => 0x03FF, // KEYINPUT: all 10 GBA buttons released!
            0x134 => 0x8000, // RCNT
            0x200 => self.io_ie,
            0x202 => self.io_if,
            0x204 => self.waitcnt,
            0x208 => self.io_ime,
            0x300 => self.postflg as u16,
            _ if offset + 1 < 0x400 => {
                let o = offset as usize;
                u16::from_le_bytes([self.other_io[o], self.other_io[o + 1]])
            }
            _ => 0,
        }
    }

    fn write_io_u8(&mut self, offset: u32, val: u8) {
        match offset {
            0x060..=0x0A7 => self.apu.write_u8(offset, val),
            0x200 => self.io_ie = (self.io_ie & 0xFF00) | (val as u16),
            0x201 => self.io_ie = (self.io_ie & 0x00FF) | ((val as u16) << 8),
            // Write-1-to-clear on REG_IF must ONLY clear bits written in that byte!
            0x202 => self.io_if &= !(val as u16),
            0x203 => self.io_if &= !((val as u16) << 8),
            0x208 => self.io_ime = (val & 1) as u16,
            0x209 => {}
            0x300 => self.postflg = val & 1,
            0x301 => {
                // HALTCNT: bit 7 == 0 -> Halt until (IE & IF) != 0
                self.halted = true;
            }
            _ => {
                let aligned = offset & !1;
                let cur = self.read_io_u16(aligned);
                let next = if (offset & 1) == 0 {
                    (cur & 0xFF00) | (val as u16)
                } else {
                    (cur & 0x00FF) | ((val as u16) << 8)
                };
                self.write_io_u16(aligned, next);
            }
        }
    }

    fn write_io_u16(&mut self, offset: u32, val: u16) {
        match offset {
            0x000 => self.dispcnt = val,
            0x004 => self.dispstat = (self.dispstat & 0x0007) | (val & 0xFFF8),
            0x006 => {} // VCOUNT is read-only
            0x060..=0x0A6 => self.apu.write_u16(offset, val),

            // DMA 0..3 registers
            0x0B0 => self.dma[0].sad = (self.dma[0].sad & 0xFFFF_0000) | (val as u32),
            0x0B2 => self.dma[0].sad = (self.dma[0].sad & 0x0000_FFFF) | (((val & 0x0FFF) as u32) << 16),
            0x0B4 => self.dma[0].dad = (self.dma[0].dad & 0xFFFF_0000) | (val as u32),
            0x0B6 => self.dma[0].dad = (self.dma[0].dad & 0x0000_FFFF) | (((val & 0x0FFF) as u32) << 16),
            0x0B8 => self.dma[0].cnt_l = val & 0x3FFF,
            0x0BA => self.write_dma_cnt_h(0, val),

            0x0BC => self.dma[1].sad = (self.dma[1].sad & 0xFFFF_0000) | (val as u32),
            0x0BE => self.dma[1].sad = (self.dma[1].sad & 0x0000_FFFF) | (((val & 0x0FFF) as u32) << 16),
            0x0C0 => self.dma[1].dad = (self.dma[1].dad & 0xFFFF_0000) | (val as u32),
            0x0C2 => self.dma[1].dad = (self.dma[1].dad & 0x0000_FFFF) | (((val & 0x0FFF) as u32) << 16),
            0x0C4 => self.dma[1].cnt_l = val & 0x3FFF,
            0x0C6 => self.write_dma_cnt_h(1, val),

            0x0C8 => self.dma[2].sad = (self.dma[2].sad & 0xFFFF_0000) | (val as u32),
            0x0CA => self.dma[2].sad = (self.dma[2].sad & 0x0000_FFFF) | (((val & 0x0FFF) as u32) << 16),
            0x0CC => self.dma[2].dad = (self.dma[2].dad & 0xFFFF_0000) | (val as u32),
            0x0CE => self.dma[2].dad = (self.dma[2].dad & 0x0000_FFFF) | (((val & 0x0FFF) as u32) << 16),
            0x0D0 => self.dma[2].cnt_l = val & 0x3FFF,
            0x0D2 => self.write_dma_cnt_h(2, val),

            0x0D4 => self.dma[3].sad = (self.dma[3].sad & 0xFFFF_0000) | (val as u32),
            0x0D6 => self.dma[3].sad = (self.dma[3].sad & 0x0000_FFFF) | (((val & 0x0FFF) as u32) << 16),
            0x0D8 => self.dma[3].dad = (self.dma[3].dad & 0xFFFF_0000) | (val as u32),
            0x0DA => self.dma[3].dad = (self.dma[3].dad & 0x0000_FFFF) | (((val & 0x0FFF) as u32) << 16),
            0x0DC => self.dma[3].cnt_l = val,
            0x0DE => self.write_dma_cnt_h(3, val),

            // Timers 0..3
            0x100 => self.timers[0].reload = val,
            0x102 => self.write_timer_cnt_h(0, val),
            0x104 => self.timers[1].reload = val,
            0x106 => self.write_timer_cnt_h(1, val),
            0x108 => self.timers[2].reload = val,
            0x10A => self.write_timer_cnt_h(2, val),
            0x10C => self.timers[3].reload = val,
            0x10E => self.write_timer_cnt_h(3, val),

            0x200 => self.io_ie = val,
            0x202 => self.io_if &= !val, // Write 1 to clear interrupt flags
            0x204 => self.waitcnt = val,
            0x208 => self.io_ime = val & 1,
            0x300 => {
                self.postflg = (val & 1) as u8;
                if (val & 0xFF00) == 0 && offset == 0x300 {
                    // 16-bit write to 0x300 covers 0x301 (HALTCNT) only if intended;
                    // GBA games write HALTCNT via 8-bit STRB to 0x04000301.
                }
            }
            _ if offset + 1 < 0x400 => {
                let o = offset as usize;
                let bytes = val.to_le_bytes();
                self.other_io[o] = bytes[0];
                self.other_io[o + 1] = bytes[1];
            }
            _ => {}
        }
    }

    fn write_timer_cnt_h(&mut self, idx: usize, val: u16) {
        let was_enabled = self.timers[idx].enabled();
        self.timers[idx].control = val & 0x00C7;
        if !was_enabled && self.timers[idx].enabled() {
            self.timers[idx].counter = self.timers[idx].reload;
            self.timers[idx].subcycles = 0;
        }
    }

    fn write_dma_cnt_h(&mut self, idx: usize, val: u16) {
        let was_enabled = self.dma[idx].enabled();
        self.dma[idx].cnt_h = val;
        if !was_enabled && self.dma[idx].enabled() {
            let max_cnt = if idx == 3 { 0x10000 } else { 0x4000 };
            let raw_cnt = self.dma[idx].cnt_l as u32;
            self.dma[idx].internal_src = self.dma[idx].sad;
            self.dma[idx].internal_dst = self.dma[idx].dad;
            self.dma[idx].internal_count = if raw_cnt == 0 { max_cnt } else { raw_cnt };

            if self.dma[idx].timing() == 0 {
                self.execute_dma(idx, false);
            }
        }
    }

    fn execute_dma(&mut self, idx: usize, is_sound_fifo: bool) {
        let ch = self.dma[idx];
        if !ch.enabled() {
            return;
        }

        let is_32 = is_sound_fifo || ch.is_32bit();
        let step: i32 = if is_32 { 4 } else { 2 };

        let count = if is_sound_fifo && ch.repeat() {
            4u32
        } else {
            let max_cnt = if idx == 3 { 0x10000 } else { 0x4000 };
            if ch.internal_count == 0 {
                max_cnt
            } else {
                ch.internal_count
            }
        };

        let dst_ctrl = (ch.cnt_h >> 5) & 3;
        let src_ctrl = (ch.cnt_h >> 7) & 3;

        let src_delta: i32 = match src_ctrl {
            0 | 3 => step,
            1 => -step,
            _ => 0,
        };
        let dst_delta: i32 = if is_sound_fifo {
            0
        } else {
            match dst_ctrl {
                0 | 3 => step,
                1 => -step,
                _ => 0,
            }
        };

        let mut src = ch.internal_src;
        let mut dst = ch.internal_dst;

        if is_32 {
            src &= !3;
            dst &= !3;
            for _ in 0..count {
                let w = self.read_u32_aligned(src);
                self.write_u32_aligned(dst, w);
                src = src.wrapping_add(src_delta as u32);
                dst = dst.wrapping_add(dst_delta as u32);
            }
        } else {
            src &= !1;
            dst &= !1;
            for _ in 0..count {
                let h = self.read_u16_aligned(src);
                self.write_u16_aligned(dst, h);
                src = src.wrapping_add(src_delta as u32);
                dst = dst.wrapping_add(dst_delta as u32);
            }
        }

        self.dma[idx].internal_src = src;
        self.dma[idx].internal_dst = dst;

        if ch.irq_enabled() {
            self.io_if |= IRQ_DMA0 << idx;
        }

        if ch.repeat() && ch.timing() != 0 {
            if !is_sound_fifo {
                let max_cnt = if idx == 3 { 0x10000 } else { 0x4000 };
                let raw_cnt = self.dma[idx].cnt_l as u32;
                self.dma[idx].internal_count = if raw_cnt == 0 { max_cnt } else { raw_cnt };
                if dst_ctrl == 3 {
                    self.dma[idx].internal_dst = self.dma[idx].dad;
                }
            }
        } else {
            self.dma[idx].cnt_h &= !0x8000;
        }
    }

    fn trigger_sound_fifo_dma(&mut self, fifo_addr: u32) {
        for idx in [1usize, 2usize] {
            if self.dma[idx].enabled()
                && self.dma[idx].timing() == 3
                && (self.dma[idx].internal_dst & 0x0FFF_FFFC) == fifo_addr
            {
                self.execute_dma(idx, true);
            }
        }
    }

    fn on_timer_overflow(&mut self, idx: usize) {
        if self.timers[idx].irq_enabled() {
            self.io_if |= IRQ_TIMER0 << idx;
        }

        // DirectSound FIFO A & B are clocked only by Timer 0 or Timer 1
        if idx <= 1 && self.apu.master_enabled() {
            if self.apu.dsa_timer() == idx {
                self.apu.fifo_a.pop_sample();
                if self.apu.fifo_a.len() <= 16 {
                    self.trigger_sound_fifo_dma(0x0400_00A0);
                }
            }
            if self.apu.dsb_timer() == idx {
                self.apu.fifo_b.pop_sample();
                if self.apu.fifo_b.len() <= 16 {
                    self.trigger_sound_fifo_dma(0x0400_00A4);
                }
            }
        }

        // Cascade into Timer idx + 1 if configured for count-up
        if idx + 1 < 4 && self.timers[idx + 1].enabled() && self.timers[idx + 1].cascade() {
            let (next_val, overflowed) = self.timers[idx + 1].counter.overflowing_add(1);
            if overflowed {
                self.timers[idx + 1].counter = self.timers[idx + 1].reload;
                self.on_timer_overflow(idx + 1);
            } else {
                self.timers[idx + 1].counter = next_val;
            }
        }
    }

    fn step_timers(&mut self, cycles: u32) {
        for idx in 0..4 {
            if !self.timers[idx].enabled() || self.timers[idx].cascade() {
                continue;
            }
            let shift = self.timers[idx].prescaler_shift();
            let mask = (1u32 << shift) - 1;
            let total = self.timers[idx].subcycles + cycles;
            let mut ticks = total >> shift;
            self.timers[idx].subcycles = total & mask;

            while ticks > 0 {
                let rem_to_ovf = 0x10000u32 - (self.timers[idx].counter as u32);
                if ticks >= rem_to_ovf {
                    ticks -= rem_to_ovf;
                    self.timers[idx].counter = self.timers[idx].reload;
                    self.on_timer_overflow(idx);
                } else {
                    self.timers[idx].counter =
                        self.timers[idx].counter.wrapping_add(ticks as u16);
                    ticks = 0;
                }
            }
        }
    }

    fn step_lcd(&mut self, cycles: u32) {
        self.line_cycles += cycles;
        while self.line_cycles >= HDRAW_CYCLES {
            if !self.in_hblank {
                self.in_hblank = true;
                if (self.dispstat & 0x0010) != 0 {
                    self.io_if |= IRQ_HBLANK;
                }
                if self.vcount < 160 {
                    for idx in 0..4 {
                        if self.dma[idx].enabled() && self.dma[idx].timing() == 2 {
                            self.execute_dma(idx, false);
                        }
                    }
                }
            }

            if self.line_cycles < SCANLINE_CYCLES {
                break;
            }

            self.line_cycles -= SCANLINE_CYCLES;
            self.in_hblank = false;
            self.vcount += 1;
            if self.vcount >= TOTAL_SCANLINES {
                self.vcount = 0;
            }

            // VCount match IRQ
            let lyc = (self.dispstat >> 8) & 0xFF;
            if self.vcount == lyc && (self.dispstat & 0x0020) != 0 {
                self.io_if |= IRQ_VCOUNT;
            }

            // VBlank start (scanline 160)
            if self.vcount == 160 {
                if (self.dispstat & 0x0008) != 0 {
                    self.io_if |= IRQ_VBLANK;
                }
                for idx in 0..4 {
                    if self.dma[idx].enabled() && self.dma[idx].timing() == 1 {
                        self.execute_dma(idx, false);
                    }
                }
            }
        }
    }

    #[inline]
    fn cycles_until_lcd_boundary(&self) -> u32 {
        if !self.in_hblank {
            (HDRAW_CYCLES - self.line_cycles).max(1)
        } else {
            (SCANLINE_CYCLES - self.line_cycles).max(1)
        }
    }

    /// Cycles we can safely fast-skip while CPU is `halted` (until next LCD boundary or active Timer overflow).
    pub fn cycles_until_next_event(&self, max_cycles: u32) -> u32 {
        let mut step = max_cycles.min(self.cycles_until_lcd_boundary());
        for t in &self.timers {
            if let Some(ovf) = t.cycles_until_overflow() {
                step = step.min(ovf);
            }
        }
        step.max(1)
    }

    /// Advance hardware (APU, DirectSound Timer 0/1 sub-steps, Timers 2/3, LCD/DMA) by `cycles`.
    pub fn advance_cycles(&mut self, mut cycles: u32, emit_audio: bool, gain_scale: f32) {
        while cycles > 0 {
            let mut sub = cycles.min(self.cycles_until_lcd_boundary());
            // Sub-step precisely to Timer 0 & Timer 1 overflows so every DirectSound FIFO
            // sample is integrated at its exact cycle timestamp!
            if let Some(t0) = self.timers[0].cycles_until_overflow() {
                sub = sub.min(t0);
            }
            if let Some(t1) = self.timers[1].cycles_until_overflow() {
                sub = sub.min(t1);
            }
            sub = sub.max(1);

            self.apu.advance(sub, emit_audio, gain_scale);
            self.step_timers(sub);
            self.step_lcd(sub);
            self.total_cycles += sub as u64;
            cycles -= sub;

            if self.halted && self.irq_pending() {
                self.halted = false;
                // Stop fast-skipping immediately so the CPU services the waking interrupt!
                break;
            }
        }
    }
}
