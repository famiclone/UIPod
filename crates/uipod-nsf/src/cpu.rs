//! MOS 6502 / Ricoh 2A03 CPU, NSF 4 KB bankswitch mapper, and cycle-accurate machine bus.
//!
//! Supports all 151 official 6502 opcodes plus common stable undocumented opcodes
//! (`LAX`, `SAX`, `DCP`, `ISB`, `SLO`, `RLA`, `SRE`, `RRA`, `ANC`, `ALR`, `ARR`, `AXS`,
//! multi-byte `NOP`s) and guards `init`/`play` subroutines with cycle budgets.

use crate::apu::Apu2A03;
use crate::blip::BlipBuf;
use crate::exp_mmc5::Mmc5Apu;
use crate::exp_vrc6::Vrc6Apu;
use crate::parser::{ParsedNsf, UNMAPPED_BANK};
use crate::{ApuFrameTelemetry, ChannelState, ChipFlags};

const FLAG_C: u8 = 0x01;
const FLAG_Z: u8 = 0x02;
const FLAG_I: u8 = 0x04;
const FLAG_D: u8 = 0x08;
const FLAG_B: u8 = 0x10;
const FLAG_U: u8 = 0x20;
const FLAG_V: u8 = 0x40;
const FLAG_N: u8 = 0x80;

/// Sentinel address in unmapped `$4100` expansion space (`JMP $4100`).
/// Subroutines (`init_addr` / `play_addr`) push `SENTINEL_PC - 1` onto the stack
/// so executing `RTS` lands cleanly at `SENTINEL_PC` and marks the CPU idle for the frame.
pub(crate) const SENTINEL_PC: u16 = 0x4100;

/// Maximum cycles permitted for `JSR init_addr` before returning control to the frame loop.
const INIT_CYCLE_BUDGET: u32 = 400_000;

#[derive(Clone, Debug)]
pub(crate) struct Cpu6502 {
    pub a: u8,
    pub x: u8,
    pub y: u8,
    pub sp: u8,
    pub pc: u16,
    pub p: u8,
    pub idle: bool,
}

impl Default for Cpu6502 {
    fn default() -> Self {
        Self {
            a: 0,
            x: 0,
            y: 0,
            sp: 0xFD,
            pc: SENTINEL_PC,
            p: FLAG_I | FLAG_U,
            idle: true,
        }
    }
}

impl Cpu6502 {
    #[inline]
    fn set_flag(&mut self, flag: u8, cond: bool) {
        if cond {
            self.p |= flag;
        } else {
            self.p &= !flag;
        }
    }

    #[inline]
    fn update_nz(&mut self, val: u8) {
        self.set_flag(FLAG_Z, val == 0);
        self.set_flag(FLAG_N, (val & 0x80) != 0);
    }

    #[inline]
    fn adc(&mut self, val: u8) {
        // Ricoh 2A03 disables BCD decimal mode in hardware
        let c = u16::from((self.p & FLAG_C) != 0);
        let sum = (self.a as u16) + (val as u16) + c;
        let res = sum as u8;
        let overflow = ((!(self.a ^ val) & (self.a ^ res)) & 0x80) != 0;
        self.set_flag(FLAG_C, sum > 0xFF);
        self.set_flag(FLAG_V, overflow);
        self.a = res;
        self.update_nz(res);
    }

    #[inline]
    fn cmp_reg(&mut self, reg: u8, val: u8) {
        let diff = reg.wrapping_sub(val);
        self.set_flag(FLAG_C, reg >= val);
        self.update_nz(diff);
    }
}

/// Complete pre-allocated NSF hardware machine (CPU + RAM + Mapper + 2A03 + VRC6 + MMC5).
#[derive(Clone)]
pub(crate) struct NsfMachine {
    pub cpu: Cpu6502,
    pub ram: Box<[u8; 2048]>,
    pub prg_ram: Box<[u8; 8192]>,
    pub exram: Box<[u8; 1024]>,
    pub bank_map: [u16; 8],
    pub apu: Apu2A03,
    pub vrc6: Vrc6Apu,
    pub mmc5: Mmc5Apu,
    pub has_vrc6: bool,
    pub has_mmc5: bool,
    pub chip_flags: ChipFlags,
    pub cpu_clock_hz: u32,
    pub frame_cycles: u32,
    pub mute_mask: u16,
    pub has_rendered_pcm: bool,
    last_dac: i32,
}

impl NsfMachine {
    pub fn new(nsf: &ParsedNsf) -> Self {
        let has_vrc6 = nsf.chip_flags.has_vrc6();
        let has_mmc5 = nsf.chip_flags.has_mmc5();
        let mmc5_base_bit = if has_vrc6 { 8 } else { 5 };
        let cpu_clock_hz = nsf.cpu_clock_hz();

        Self {
            cpu: Cpu6502::default(),
            ram: Box::new([0u8; 2048]),
            prg_ram: Box::new([0u8; 8192]),
            exram: Box::new([0u8; 1024]),
            bank_map: nsf.linear_bank_map,
            apu: Apu2A03::new(nsf.is_pal, cpu_clock_hz),
            vrc6: Vrc6Apu::new(),
            mmc5: Mmc5Apu::new(mmc5_base_bit),
            has_vrc6,
            has_mmc5,
            chip_flags: nsf.chip_flags,
            cpu_clock_hz,
            frame_cycles: nsf.frame_cycles(),
            mute_mask: 0,
            has_rendered_pcm: false,
            last_dac: 0,
        }
    }

    pub fn set_mute_mask(&mut self, mask: u16) {
        self.mute_mask = mask;
        let immediate = !self.has_rendered_pcm;
        self.apu.set_mute_mask(mask, immediate);
        if self.has_vrc6 {
            self.vrc6.set_mute_mask(mask, immediate);
        }
        if self.has_mmc5 {
            self.mmc5.set_mute_mask(mask, immediate);
        }
    }

    /// Initialize hardware state and execute `JSR init_addr` for `subtrack_index` (0-based).
    pub fn init_subtrack(&mut self, nsf: &ParsedNsf, subtrack_index: u16) {
        self.ram.fill(0);
        if let Some(ref init_ram) = nsf.initial_prg_ram {
            self.prg_ram.copy_from_slice(init_ram.as_ref());
        } else {
            self.prg_ram.fill(0);
        }
        self.exram.fill(0);
        self.bank_map = nsf.linear_bank_map;
        self.has_rendered_pcm = false;
        self.last_dac = 0;

        self.apu.reset(self.mute_mask);
        if self.has_vrc6 {
            self.vrc6.reset(self.mute_mask);
        }
        if self.has_mmc5 {
            self.mmc5.reset(self.mute_mask, self.cpu_clock_hz);
        }

        // Standard NSF reset sequence: clear $4000..$4013, enable $4015 = $0F, disable frame IRQ ($4017 = $40)
        for reg in 0x4000..=0x4013 {
            self.write_bus(nsf, reg, 0x00);
        }
        self.write_bus(nsf, 0x4015, 0x00);
        self.write_bus(nsf, 0x4015, 0x0F);
        self.write_bus(nsf, 0x4017, 0x40);
        if self.has_mmc5 {
            self.mmc5.write_register(0x5015, 0x03);
        }

        self.cpu = Cpu6502 {
            a: (subtrack_index & 0xFF) as u8,
            x: u8::from(nsf.is_pal),
            y: 0,
            sp: 0xFD,
            pc: SENTINEL_PC,
            p: FLAG_I | FLAG_U,
            idle: true,
        };

        self.call_subroutine(nsf.init_addr);

        // Execute `init_addr` up to `INIT_CYCLE_BUDGET` cycles so decompression / table init finishes cleanly
        let mut init_cycles = 0u32;
        while !self.cpu.idle && init_cycles < INIT_CYCLE_BUDGET {
            let c = self.step_cpu_instruction(nsf);
            self.advance_sound(nsf, 0, c, true, None);
            init_cycles += c;
            if self.cpu.pc == SENTINEL_PC {
                self.cpu.idle = true;
            }
        }
        // Align baseline DAC level if non-muted static level was written during init
        self.last_dac = 0;
    }

    #[inline]
    fn call_subroutine(&mut self, target_addr: u16) {
        let ret = SENTINEL_PC.wrapping_sub(1);
        self.push_u8((ret >> 8) as u8);
        self.push_u8((ret & 0xFF) as u8);
        self.cpu.pc = target_addr;
        self.cpu.idle = false;
    }

    /// Run one 60 Hz / 50 Hz frame (`frame_cycles` CPU clocks), either headless or synthesizing into `BlipBuf`.
    pub fn run_frame(
        &mut self,
        nsf: &ParsedNsf,
        headless: bool,
        mut blip: Option<&mut BlipBuf>,
    ) -> ApuFrameTelemetry {
        if !headless && blip.is_some() {
            self.has_rendered_pcm = true;
        }

        if self.cpu.idle && nsf.play_addr != 0 {
            self.call_subroutine(nsf.play_addr);
        }

        let total_cycles = self.frame_cycles;
        let mut elapsed_cycles = 0u32;

        while !self.cpu.idle && elapsed_cycles < total_cycles {
            // Check hardware IRQ (DPCM IRQ or APU Frame IRQ)
            if self.apu.irq_asserted() && (self.cpu.p & FLAG_I) == 0 {
                let irq_cycles = self.service_irq(nsf);
                self.advance_sound(
                    nsf,
                    elapsed_cycles,
                    irq_cycles,
                    headless,
                    blip.as_deref_mut(),
                );
                elapsed_cycles += irq_cycles;
                if elapsed_cycles >= total_cycles {
                    break;
                }
            }

            let step = self.step_cpu_instruction(nsf);
            self.advance_sound(nsf, elapsed_cycles, step, headless, blip.as_deref_mut());
            elapsed_cycles += step;

            if self.cpu.pc == SENTINEL_PC {
                self.cpu.idle = true;
            }
        }

        // Guard runaway infinite loop in `play_addr`: terminate current frame cleanly if play_addr != 0
        if !self.cpu.idle && nsf.play_addr != 0 {
            self.cpu.idle = true;
            self.cpu.sp = 0xFD;
        }

        if elapsed_cycles < total_cycles {
            let rem = total_cycles - elapsed_cycles;
            self.advance_sound(nsf, elapsed_cycles, rem, headless, blip.as_deref_mut());
        }

        if let Some(b) = blip {
            b.end_frame(total_cycles);
        }

        self.build_telemetry()
    }

    fn advance_sound(
        &mut self,
        nsf: &ParsedNsf,
        start_cycle: u32,
        cycles: u32,
        headless: bool,
        mut blip: Option<&mut BlipBuf>,
    ) {
        if cycles == 0 {
            return;
        }
        let rom_banks = &nsf.rom_banks;
        let bank_map = self.bank_map;

        if headless || blip.is_none() {
            self.apu.step_cycles(cycles, true, |addr| {
                read_rom_mapped(rom_banks, &bank_map, addr)
            });
            if self.has_vrc6 {
                self.vrc6.step_cycles(cycles);
            }
            if self.has_mmc5 {
                self.mmc5.step_cycles(cycles, true);
            }
            return;
        }

        let b = blip.as_mut().unwrap();
        let mut cur = start_cycle;
        let mut rem = cycles;

        while rem > 0 {
            let mut step = rem.min(self.apu.next_event_cycles(false));
            if self.has_vrc6 {
                step = step.min(self.vrc6.next_event_cycles());
            }
            if self.has_mmc5 {
                step = step.min(self.mmc5.next_event_cycles());
            }
            step = step.max(1).min(rem);

            self.apu.step_cycles(step, false, |addr| {
                read_rom_mapped(rom_banks, &bank_map, addr)
            });
            if self.has_vrc6 {
                self.vrc6.step_cycles(step);
            }
            if self.has_mmc5 {
                self.mmc5.step_cycles(step, false);
            }

            cur += step;
            rem -= step;

            let mut mixed = self.apu.mixed_output();
            if self.has_vrc6 {
                mixed += self.vrc6.mixed_output();
            }
            if self.has_mmc5 {
                mixed += self.mmc5.mixed_output();
            }

            if mixed != self.last_dac {
                b.add_delta(cur, mixed - self.last_dac);
                self.last_dac = mixed;
            }
        }
    }

    fn build_telemetry(&mut self) -> ApuFrameTelemetry {
        let mut channels = [ChannelState::default(); 8];
        let mut base_5 = [ChannelState::default(); 5];
        self.apu.snapshot_telemetry(self.mute_mask, &mut base_5);
        channels[..5].copy_from_slice(&base_5);

        let mut channel_count = 5u8;
        if self.has_vrc6 {
            let mut exp_3 = [ChannelState::default(); 3];
            self.vrc6
                .snapshot_telemetry(self.cpu_clock_hz, self.mute_mask, &mut exp_3);
            channels[5..8].copy_from_slice(&exp_3);
            channel_count = 8;
        } else if self.has_mmc5 {
            let mut exp_3 = [ChannelState::default(); 3];
            self.mmc5
                .snapshot_telemetry(self.cpu_clock_hz, self.mute_mask, &mut exp_3);
            channels[5..8].copy_from_slice(&exp_3);
            channel_count = 8;
        }

        ApuFrameTelemetry {
            channel_count,
            chip_flags: self.chip_flags.0,
            mute_mask: self.mute_mask,
            channels,
        }
    }

    // ========================================================================
    // Memory Bus Read / Write
    // ========================================================================

    #[inline]
    pub fn read_bus(&mut self, nsf: &ParsedNsf, addr: u16) -> u8 {
        match addr {
            0x0000..=0x1FFF => self.ram[(addr & 0x07FF) as usize],
            0x4015 => self.apu.read_status(),
            0x4100 => 0x4C, // JMP $4100 sentinel
            0x4101 => 0x00,
            0x4102 => 0x41,
            0x5000..=0x5206 if self.has_mmc5 => self.mmc5.read_register(addr).unwrap_or(0),
            0x5C00..=0x5FF7 => self.exram[(addr - 0x5C00) as usize],
            0x5FF8..=0x5FFF => {
                let b = self.bank_map[(addr - 0x5FF8) as usize];
                if b == UNMAPPED_BANK {
                    0
                } else {
                    b as u8
                }
            }
            0x6000..=0x7FFF => self.prg_ram[(addr - 0x6000) as usize],
            0x8000..=0xFFFF => {
                let v = read_rom_mapped(&nsf.rom_banks, &self.bank_map, addr);
                if self.has_mmc5 {
                    self.mmc5.notify_rom_read(addr, v);
                }
                v
            }
            _ => 0,
        }
    }

    #[inline]
    pub fn write_bus(&mut self, nsf: &ParsedNsf, addr: u16, val: u8) {
        match addr {
            0x0000..=0x1FFF => {
                self.ram[(addr & 0x07FF) as usize] = val;
            }
            0x4000..=0x4017 => {
                let rom_banks = &nsf.rom_banks;
                let bank_map = self.bank_map;
                self.apu.write_register(addr, val, |dma_addr| {
                    read_rom_mapped(rom_banks, &bank_map, dma_addr)
                });
            }
            0x5000..=0x5206 if self.has_mmc5 => {
                self.mmc5.write_register(addr, val);
            }
            0x5C00..=0x5FF7 => {
                self.exram[(addr - 0x5C00) as usize] = val;
            }
            0x5FF8..=0x5FFF => {
                let slot = (addr - 0x5FF8) as usize;
                let bank_idx = val as usize;
                self.bank_map[slot] = if bank_idx < nsf.rom_banks.len() {
                    val as u16
                } else {
                    UNMAPPED_BANK
                };
            }
            0x6000..=0x7FFF => {
                self.prg_ram[(addr - 0x6000) as usize] = val;
            }
            0x9000..=0xBFFF if self.has_vrc6 => {
                self.vrc6.write_register(addr, val);
            }
            _ => {}
        }
    }

    // ========================================================================
    // 6502 Stack & Addressing Helpers
    // ========================================================================

    #[inline]
    fn push_u8(&mut self, val: u8) {
        self.ram[0x0100 | (self.cpu.sp as usize)] = val;
        self.cpu.sp = self.cpu.sp.wrapping_sub(1);
    }

    #[inline]
    fn pop_u8(&mut self) -> u8 {
        self.cpu.sp = self.cpu.sp.wrapping_add(1);
        self.ram[0x0100 | (self.cpu.sp as usize)]
    }

    #[inline]
    fn fetch_u8(&mut self, nsf: &ParsedNsf) -> u8 {
        let pc = self.cpu.pc;
        self.cpu.pc = pc.wrapping_add(1);
        self.read_bus(nsf, pc)
    }

    #[inline]
    fn fetch_u16(&mut self, nsf: &ParsedNsf) -> u16 {
        let lo = self.fetch_u8(nsf) as u16;
        let hi = self.fetch_u8(nsf) as u16;
        lo | (hi << 8)
    }

    #[inline]
    fn addr_zp(&mut self, nsf: &ParsedNsf) -> u16 {
        self.fetch_u8(nsf) as u16
    }

    #[inline]
    fn addr_zpx(&mut self, nsf: &ParsedNsf) -> u16 {
        self.fetch_u8(nsf).wrapping_add(self.cpu.x) as u16
    }

    #[inline]
    fn addr_zpy(&mut self, nsf: &ParsedNsf) -> u16 {
        self.fetch_u8(nsf).wrapping_add(self.cpu.y) as u16
    }

    #[inline]
    fn addr_abs(&mut self, nsf: &ParsedNsf) -> u16 {
        self.fetch_u16(nsf)
    }

    #[inline]
    fn addr_absx(&mut self, nsf: &ParsedNsf) -> (u16, u32) {
        let base = self.fetch_u16(nsf);
        let addr = base.wrapping_add(self.cpu.x as u16);
        let crossed = u32::from((base & 0xFF00) != (addr & 0xFF00));
        (addr, crossed)
    }

    #[inline]
    fn addr_absy(&mut self, nsf: &ParsedNsf) -> (u16, u32) {
        let base = self.fetch_u16(nsf);
        let addr = base.wrapping_add(self.cpu.y as u16);
        let crossed = u32::from((base & 0xFF00) != (addr & 0xFF00));
        (addr, crossed)
    }

    #[inline]
    fn addr_indx(&mut self, nsf: &ParsedNsf) -> u16 {
        let zp = self.fetch_u8(nsf).wrapping_add(self.cpu.x);
        let lo = self.ram[zp as usize] as u16;
        let hi = self.ram[zp.wrapping_add(1) as usize] as u16;
        lo | (hi << 8)
    }

    #[inline]
    fn addr_indy(&mut self, nsf: &ParsedNsf) -> (u16, u32) {
        let zp = self.fetch_u8(nsf);
        let lo = self.ram[zp as usize] as u16;
        let hi = self.ram[zp.wrapping_add(1) as usize] as u16;
        let base = lo | (hi << 8);
        let addr = base.wrapping_add(self.cpu.y as u16);
        let crossed = u32::from((base & 0xFF00) != (addr & 0xFF00));
        (addr, crossed)
    }

    #[inline]
    fn branch(&mut self, nsf: &ParsedNsf, take: bool) -> u32 {
        let offset = self.fetch_u8(nsf) as i8;
        if !take {
            2
        } else {
            let old_pc = self.cpu.pc;
            let new_pc = old_pc.wrapping_add(offset as i16 as u16);
            self.cpu.pc = new_pc;
            if (old_pc & 0xFF00) != (new_pc & 0xFF00) {
                4
            } else {
                3
            }
        }
    }

    fn service_irq(&mut self, nsf: &ParsedNsf) -> u32 {
        let pc = self.cpu.pc;
        self.push_u8((pc >> 8) as u8);
        self.push_u8((pc & 0xFF) as u8);
        self.push_u8((self.cpu.p | FLAG_U) & !FLAG_B);
        self.cpu.p |= FLAG_I;
        let lo = self.read_bus(nsf, 0xFFFE) as u16;
        let hi = self.read_bus(nsf, 0xFFFF) as u16;
        self.cpu.pc = lo | (hi << 8);
        7
    }

    // ========================================================================
    // Read-Modify-Write Helpers (Official & Undocumented)
    // ========================================================================

    #[inline]
    fn op_asl_mem(&mut self, nsf: &ParsedNsf, addr: u16) {
        let v = self.read_bus(nsf, addr);
        self.cpu.set_flag(FLAG_C, (v & 0x80) != 0);
        let r = v << 1;
        self.write_bus(nsf, addr, r);
        self.cpu.update_nz(r);
    }

    #[inline]
    fn op_lsr_mem(&mut self, nsf: &ParsedNsf, addr: u16) {
        let v = self.read_bus(nsf, addr);
        self.cpu.set_flag(FLAG_C, (v & 0x01) != 0);
        let r = v >> 1;
        self.write_bus(nsf, addr, r);
        self.cpu.update_nz(r);
    }

    #[inline]
    fn op_rol_mem(&mut self, nsf: &ParsedNsf, addr: u16) {
        let v = self.read_bus(nsf, addr);
        let c = u8::from((self.cpu.p & FLAG_C) != 0);
        self.cpu.set_flag(FLAG_C, (v & 0x80) != 0);
        let r = (v << 1) | c;
        self.write_bus(nsf, addr, r);
        self.cpu.update_nz(r);
    }

    #[inline]
    fn op_ror_mem(&mut self, nsf: &ParsedNsf, addr: u16) {
        let v = self.read_bus(nsf, addr);
        let c = if (self.cpu.p & FLAG_C) != 0 { 0x80 } else { 0 };
        self.cpu.set_flag(FLAG_C, (v & 0x01) != 0);
        let r = (v >> 1) | c;
        self.write_bus(nsf, addr, r);
        self.cpu.update_nz(r);
    }

    #[inline]
    fn op_slo(&mut self, nsf: &ParsedNsf, addr: u16) {
        let v = self.read_bus(nsf, addr);
        self.cpu.set_flag(FLAG_C, (v & 0x80) != 0);
        let r = v << 1;
        self.write_bus(nsf, addr, r);
        self.cpu.a |= r;
        self.cpu.update_nz(self.cpu.a);
    }

    #[inline]
    fn op_rla(&mut self, nsf: &ParsedNsf, addr: u16) {
        let v = self.read_bus(nsf, addr);
        let c = u8::from((self.cpu.p & FLAG_C) != 0);
        self.cpu.set_flag(FLAG_C, (v & 0x80) != 0);
        let r = (v << 1) | c;
        self.write_bus(nsf, addr, r);
        self.cpu.a &= r;
        self.cpu.update_nz(self.cpu.a);
    }

    #[inline]
    fn op_sre(&mut self, nsf: &ParsedNsf, addr: u16) {
        let v = self.read_bus(nsf, addr);
        self.cpu.set_flag(FLAG_C, (v & 0x01) != 0);
        let r = v >> 1;
        self.write_bus(nsf, addr, r);
        self.cpu.a ^= r;
        self.cpu.update_nz(self.cpu.a);
    }

    #[inline]
    fn op_rra(&mut self, nsf: &ParsedNsf, addr: u16) {
        let v = self.read_bus(nsf, addr);
        let c = if (self.cpu.p & FLAG_C) != 0 { 0x80 } else { 0 };
        self.cpu.set_flag(FLAG_C, (v & 0x01) != 0);
        let r = (v >> 1) | c;
        self.write_bus(nsf, addr, r);
        self.cpu.adc(r);
    }

    #[inline]
    fn op_dcp(&mut self, nsf: &ParsedNsf, addr: u16) {
        let v = self.read_bus(nsf, addr).wrapping_sub(1);
        self.write_bus(nsf, addr, v);
        self.cpu.cmp_reg(self.cpu.a, v);
    }

    #[inline]
    fn op_isb(&mut self, nsf: &ParsedNsf, addr: u16) {
        let v = self.read_bus(nsf, addr).wrapping_add(1);
        self.write_bus(nsf, addr, v);
        self.cpu.adc(v ^ 0xFF);
    }

    // ========================================================================
    // Full 256-Opcode Instruction Decoder
    // ========================================================================

    pub fn step_cpu_instruction(&mut self, nsf: &ParsedNsf) -> u32 {
        let op = self.fetch_u8(nsf);
        match op {
            // --- LDA ---
            0xA9 => {
                let v = self.fetch_u8(nsf);
                self.cpu.a = v;
                self.cpu.update_nz(v);
                2
            }
            0xA5 => {
                let a = self.addr_zp(nsf);
                let v = self.read_bus(nsf, a);
                self.cpu.a = v;
                self.cpu.update_nz(v);
                3
            }
            0xB5 => {
                let a = self.addr_zpx(nsf);
                let v = self.read_bus(nsf, a);
                self.cpu.a = v;
                self.cpu.update_nz(v);
                4
            }
            0xAD => {
                let a = self.addr_abs(nsf);
                let v = self.read_bus(nsf, a);
                self.cpu.a = v;
                self.cpu.update_nz(v);
                4
            }
            0xBD => {
                let (a, c) = self.addr_absx(nsf);
                let v = self.read_bus(nsf, a);
                self.cpu.a = v;
                self.cpu.update_nz(v);
                4 + c
            }
            0xB9 => {
                let (a, c) = self.addr_absy(nsf);
                let v = self.read_bus(nsf, a);
                self.cpu.a = v;
                self.cpu.update_nz(v);
                4 + c
            }
            0xA1 => {
                let a = self.addr_indx(nsf);
                let v = self.read_bus(nsf, a);
                self.cpu.a = v;
                self.cpu.update_nz(v);
                6
            }
            0xB1 => {
                let (a, c) = self.addr_indy(nsf);
                let v = self.read_bus(nsf, a);
                self.cpu.a = v;
                self.cpu.update_nz(v);
                5 + c
            }

            // --- LDX ---
            0xA2 => {
                let v = self.fetch_u8(nsf);
                self.cpu.x = v;
                self.cpu.update_nz(v);
                2
            }
            0xA6 => {
                let a = self.addr_zp(nsf);
                let v = self.read_bus(nsf, a);
                self.cpu.x = v;
                self.cpu.update_nz(v);
                3
            }
            0xB6 => {
                let a = self.addr_zpy(nsf);
                let v = self.read_bus(nsf, a);
                self.cpu.x = v;
                self.cpu.update_nz(v);
                4
            }
            0xAE => {
                let a = self.addr_abs(nsf);
                let v = self.read_bus(nsf, a);
                self.cpu.x = v;
                self.cpu.update_nz(v);
                4
            }
            0xBE => {
                let (a, c) = self.addr_absy(nsf);
                let v = self.read_bus(nsf, a);
                self.cpu.x = v;
                self.cpu.update_nz(v);
                4 + c
            }

            // --- LDY ---
            0xA0 => {
                let v = self.fetch_u8(nsf);
                self.cpu.y = v;
                self.cpu.update_nz(v);
                2
            }
            0xA4 => {
                let a = self.addr_zp(nsf);
                let v = self.read_bus(nsf, a);
                self.cpu.y = v;
                self.cpu.update_nz(v);
                3
            }
            0xB4 => {
                let a = self.addr_zpx(nsf);
                let v = self.read_bus(nsf, a);
                self.cpu.y = v;
                self.cpu.update_nz(v);
                4
            }
            0xAC => {
                let a = self.addr_abs(nsf);
                let v = self.read_bus(nsf, a);
                self.cpu.y = v;
                self.cpu.update_nz(v);
                4
            }
            0xBC => {
                let (a, c) = self.addr_absx(nsf);
                let v = self.read_bus(nsf, a);
                self.cpu.y = v;
                self.cpu.update_nz(v);
                4 + c
            }

            // --- STA / STX / STY ---
            0x85 => {
                let a = self.addr_zp(nsf);
                self.write_bus(nsf, a, self.cpu.a);
                3
            }
            0x95 => {
                let a = self.addr_zpx(nsf);
                self.write_bus(nsf, a, self.cpu.a);
                4
            }
            0x8D => {
                let a = self.addr_abs(nsf);
                self.write_bus(nsf, a, self.cpu.a);
                4
            }
            0x9D => {
                let (a, _) = self.addr_absx(nsf);
                self.write_bus(nsf, a, self.cpu.a);
                5
            }
            0x99 => {
                let (a, _) = self.addr_absy(nsf);
                self.write_bus(nsf, a, self.cpu.a);
                5
            }
            0x81 => {
                let a = self.addr_indx(nsf);
                self.write_bus(nsf, a, self.cpu.a);
                6
            }
            0x91 => {
                let (a, _) = self.addr_indy(nsf);
                self.write_bus(nsf, a, self.cpu.a);
                6
            }
            0x86 => {
                let a = self.addr_zp(nsf);
                self.write_bus(nsf, a, self.cpu.x);
                3
            }
            0x96 => {
                let a = self.addr_zpy(nsf);
                self.write_bus(nsf, a, self.cpu.x);
                4
            }
            0x8E => {
                let a = self.addr_abs(nsf);
                self.write_bus(nsf, a, self.cpu.x);
                4
            }
            0x84 => {
                let a = self.addr_zp(nsf);
                self.write_bus(nsf, a, self.cpu.y);
                3
            }
            0x94 => {
                let a = self.addr_zpx(nsf);
                self.write_bus(nsf, a, self.cpu.y);
                4
            }
            0x8C => {
                let a = self.addr_abs(nsf);
                self.write_bus(nsf, a, self.cpu.y);
                4
            }

            // --- Register Transfers ---
            0xAA => {
                self.cpu.x = self.cpu.a;
                self.cpu.update_nz(self.cpu.x);
                2
            }
            0xA8 => {
                self.cpu.y = self.cpu.a;
                self.cpu.update_nz(self.cpu.y);
                2
            }
            0x8A => {
                self.cpu.a = self.cpu.x;
                self.cpu.update_nz(self.cpu.a);
                2
            }
            0x98 => {
                self.cpu.a = self.cpu.y;
                self.cpu.update_nz(self.cpu.a);
                2
            }
            0xBA => {
                self.cpu.x = self.cpu.sp;
                self.cpu.update_nz(self.cpu.x);
                2
            }
            0x9A => {
                self.cpu.sp = self.cpu.x;
                2
            }

            // --- Stack ---
            0x48 => {
                self.push_u8(self.cpu.a);
                3
            }
            0x08 => {
                self.push_u8(self.cpu.p | FLAG_B | FLAG_U);
                3
            }
            0x68 => {
                let v = self.pop_u8();
                self.cpu.a = v;
                self.cpu.update_nz(v);
                4
            }
            0x28 => {
                let v = self.pop_u8();
                self.cpu.p = (v | FLAG_U) & !FLAG_B;
                4
            }

            // --- ORA ---
            0x09 => {
                let v = self.fetch_u8(nsf);
                self.cpu.a |= v;
                self.cpu.update_nz(self.cpu.a);
                2
            }
            0x05 => {
                let a = self.addr_zp(nsf);
                self.cpu.a |= self.read_bus(nsf, a);
                self.cpu.update_nz(self.cpu.a);
                3
            }
            0x15 => {
                let a = self.addr_zpx(nsf);
                self.cpu.a |= self.read_bus(nsf, a);
                self.cpu.update_nz(self.cpu.a);
                4
            }
            0x0D => {
                let a = self.addr_abs(nsf);
                self.cpu.a |= self.read_bus(nsf, a);
                self.cpu.update_nz(self.cpu.a);
                4
            }
            0x1D => {
                let (a, c) = self.addr_absx(nsf);
                self.cpu.a |= self.read_bus(nsf, a);
                self.cpu.update_nz(self.cpu.a);
                4 + c
            }
            0x19 => {
                let (a, c) = self.addr_absy(nsf);
                self.cpu.a |= self.read_bus(nsf, a);
                self.cpu.update_nz(self.cpu.a);
                4 + c
            }
            0x01 => {
                let a = self.addr_indx(nsf);
                self.cpu.a |= self.read_bus(nsf, a);
                self.cpu.update_nz(self.cpu.a);
                6
            }
            0x11 => {
                let (a, c) = self.addr_indy(nsf);
                self.cpu.a |= self.read_bus(nsf, a);
                self.cpu.update_nz(self.cpu.a);
                5 + c
            }

            // --- AND ---
            0x29 => {
                let v = self.fetch_u8(nsf);
                self.cpu.a &= v;
                self.cpu.update_nz(self.cpu.a);
                2
            }
            0x25 => {
                let a = self.addr_zp(nsf);
                self.cpu.a &= self.read_bus(nsf, a);
                self.cpu.update_nz(self.cpu.a);
                3
            }
            0x35 => {
                let a = self.addr_zpx(nsf);
                self.cpu.a &= self.read_bus(nsf, a);
                self.cpu.update_nz(self.cpu.a);
                4
            }
            0x2D => {
                let a = self.addr_abs(nsf);
                self.cpu.a &= self.read_bus(nsf, a);
                self.cpu.update_nz(self.cpu.a);
                4
            }
            0x3D => {
                let (a, c) = self.addr_absx(nsf);
                self.cpu.a &= self.read_bus(nsf, a);
                self.cpu.update_nz(self.cpu.a);
                4 + c
            }
            0x39 => {
                let (a, c) = self.addr_absy(nsf);
                self.cpu.a &= self.read_bus(nsf, a);
                self.cpu.update_nz(self.cpu.a);
                4 + c
            }
            0x21 => {
                let a = self.addr_indx(nsf);
                self.cpu.a &= self.read_bus(nsf, a);
                self.cpu.update_nz(self.cpu.a);
                6
            }
            0x31 => {
                let (a, c) = self.addr_indy(nsf);
                self.cpu.a &= self.read_bus(nsf, a);
                self.cpu.update_nz(self.cpu.a);
                5 + c
            }

            // --- EOR ---
            0x49 => {
                let v = self.fetch_u8(nsf);
                self.cpu.a ^= v;
                self.cpu.update_nz(self.cpu.a);
                2
            }
            0x45 => {
                let a = self.addr_zp(nsf);
                self.cpu.a ^= self.read_bus(nsf, a);
                self.cpu.update_nz(self.cpu.a);
                3
            }
            0x55 => {
                let a = self.addr_zpx(nsf);
                self.cpu.a ^= self.read_bus(nsf, a);
                self.cpu.update_nz(self.cpu.a);
                4
            }
            0x4D => {
                let a = self.addr_abs(nsf);
                self.cpu.a ^= self.read_bus(nsf, a);
                self.cpu.update_nz(self.cpu.a);
                4
            }
            0x5D => {
                let (a, c) = self.addr_absx(nsf);
                self.cpu.a ^= self.read_bus(nsf, a);
                self.cpu.update_nz(self.cpu.a);
                4 + c
            }
            0x59 => {
                let (a, c) = self.addr_absy(nsf);
                self.cpu.a ^= self.read_bus(nsf, a);
                self.cpu.update_nz(self.cpu.a);
                4 + c
            }
            0x41 => {
                let a = self.addr_indx(nsf);
                self.cpu.a ^= self.read_bus(nsf, a);
                self.cpu.update_nz(self.cpu.a);
                6
            }
            0x51 => {
                let (a, c) = self.addr_indy(nsf);
                self.cpu.a ^= self.read_bus(nsf, a);
                self.cpu.update_nz(self.cpu.a);
                5 + c
            }

            // --- BIT ---
            0x24 => {
                let a = self.addr_zp(nsf);
                let v = self.read_bus(nsf, a);
                self.cpu.set_flag(FLAG_Z, (self.cpu.a & v) == 0);
                self.cpu.set_flag(FLAG_V, (v & 0x40) != 0);
                self.cpu.set_flag(FLAG_N, (v & 0x80) != 0);
                3
            }
            0x2C => {
                let a = self.addr_abs(nsf);
                let v = self.read_bus(nsf, a);
                self.cpu.set_flag(FLAG_Z, (self.cpu.a & v) == 0);
                self.cpu.set_flag(FLAG_V, (v & 0x40) != 0);
                self.cpu.set_flag(FLAG_N, (v & 0x80) != 0);
                4
            }

            // --- ADC ---
            0x69 => {
                let v = self.fetch_u8(nsf);
                self.cpu.adc(v);
                2
            }
            0x65 => {
                let a = self.addr_zp(nsf);
                let v = self.read_bus(nsf, a);
                self.cpu.adc(v);
                3
            }
            0x75 => {
                let a = self.addr_zpx(nsf);
                let v = self.read_bus(nsf, a);
                self.cpu.adc(v);
                4
            }
            0x6D => {
                let a = self.addr_abs(nsf);
                let v = self.read_bus(nsf, a);
                self.cpu.adc(v);
                4
            }
            0x7D => {
                let (a, c) = self.addr_absx(nsf);
                let v = self.read_bus(nsf, a);
                self.cpu.adc(v);
                4 + c
            }
            0x79 => {
                let (a, c) = self.addr_absy(nsf);
                let v = self.read_bus(nsf, a);
                self.cpu.adc(v);
                4 + c
            }
            0x61 => {
                let a = self.addr_indx(nsf);
                let v = self.read_bus(nsf, a);
                self.cpu.adc(v);
                6
            }
            0x71 => {
                let (a, c) = self.addr_indy(nsf);
                let v = self.read_bus(nsf, a);
                self.cpu.adc(v);
                5 + c
            }

            // --- SBC (including unofficial $EB) ---
            0xE9 | 0xEB => {
                let v = self.fetch_u8(nsf);
                self.cpu.adc(v ^ 0xFF);
                2
            }
            0xE5 => {
                let a = self.addr_zp(nsf);
                let v = self.read_bus(nsf, a);
                self.cpu.adc(v ^ 0xFF);
                3
            }
            0xF5 => {
                let a = self.addr_zpx(nsf);
                let v = self.read_bus(nsf, a);
                self.cpu.adc(v ^ 0xFF);
                4
            }
            0xED => {
                let a = self.addr_abs(nsf);
                let v = self.read_bus(nsf, a);
                self.cpu.adc(v ^ 0xFF);
                4
            }
            0xFD => {
                let (a, c) = self.addr_absx(nsf);
                let v = self.read_bus(nsf, a);
                self.cpu.adc(v ^ 0xFF);
                4 + c
            }
            0xF9 => {
                let (a, c) = self.addr_absy(nsf);
                let v = self.read_bus(nsf, a);
                self.cpu.adc(v ^ 0xFF);
                4 + c
            }
            0xE1 => {
                let a = self.addr_indx(nsf);
                let v = self.read_bus(nsf, a);
                self.cpu.adc(v ^ 0xFF);
                6
            }
            0xF1 => {
                let (a, c) = self.addr_indy(nsf);
                let v = self.read_bus(nsf, a);
                self.cpu.adc(v ^ 0xFF);
                5 + c
            }

            // --- CMP / CPX / CPY ---
            0xC9 => {
                let v = self.fetch_u8(nsf);
                self.cpu.cmp_reg(self.cpu.a, v);
                2
            }
            0xC5 => {
                let a = self.addr_zp(nsf);
                let v = self.read_bus(nsf, a);
                self.cpu.cmp_reg(self.cpu.a, v);
                3
            }
            0xD5 => {
                let a = self.addr_zpx(nsf);
                let v = self.read_bus(nsf, a);
                self.cpu.cmp_reg(self.cpu.a, v);
                4
            }
            0xCD => {
                let a = self.addr_abs(nsf);
                let v = self.read_bus(nsf, a);
                self.cpu.cmp_reg(self.cpu.a, v);
                4
            }
            0xDD => {
                let (a, c) = self.addr_absx(nsf);
                let v = self.read_bus(nsf, a);
                self.cpu.cmp_reg(self.cpu.a, v);
                4 + c
            }
            0xD9 => {
                let (a, c) = self.addr_absy(nsf);
                let v = self.read_bus(nsf, a);
                self.cpu.cmp_reg(self.cpu.a, v);
                4 + c
            }
            0xC1 => {
                let a = self.addr_indx(nsf);
                let v = self.read_bus(nsf, a);
                self.cpu.cmp_reg(self.cpu.a, v);
                6
            }
            0xD1 => {
                let (a, c) = self.addr_indy(nsf);
                let v = self.read_bus(nsf, a);
                self.cpu.cmp_reg(self.cpu.a, v);
                5 + c
            }
            0xE0 => {
                let v = self.fetch_u8(nsf);
                self.cpu.cmp_reg(self.cpu.x, v);
                2
            }
            0xE4 => {
                let a = self.addr_zp(nsf);
                let v = self.read_bus(nsf, a);
                self.cpu.cmp_reg(self.cpu.x, v);
                3
            }
            0xEC => {
                let a = self.addr_abs(nsf);
                let v = self.read_bus(nsf, a);
                self.cpu.cmp_reg(self.cpu.x, v);
                4
            }
            0xC0 => {
                let v = self.fetch_u8(nsf);
                self.cpu.cmp_reg(self.cpu.y, v);
                2
            }
            0xC4 => {
                let a = self.addr_zp(nsf);
                let v = self.read_bus(nsf, a);
                self.cpu.cmp_reg(self.cpu.y, v);
                3
            }
            0xCC => {
                let a = self.addr_abs(nsf);
                let v = self.read_bus(nsf, a);
                self.cpu.cmp_reg(self.cpu.y, v);
                4
            }

            // --- INC / DEC / INX / INY / DEX / DEY ---
            0xE6 => {
                let a = self.addr_zp(nsf);
                let v = self.read_bus(nsf, a).wrapping_add(1);
                self.write_bus(nsf, a, v);
                self.cpu.update_nz(v);
                5
            }
            0xF6 => {
                let a = self.addr_zpx(nsf);
                let v = self.read_bus(nsf, a).wrapping_add(1);
                self.write_bus(nsf, a, v);
                self.cpu.update_nz(v);
                6
            }
            0xEE => {
                let a = self.addr_abs(nsf);
                let v = self.read_bus(nsf, a).wrapping_add(1);
                self.write_bus(nsf, a, v);
                self.cpu.update_nz(v);
                6
            }
            0xFE => {
                let (a, _) = self.addr_absx(nsf);
                let v = self.read_bus(nsf, a).wrapping_add(1);
                self.write_bus(nsf, a, v);
                self.cpu.update_nz(v);
                7
            }
            0xC6 => {
                let a = self.addr_zp(nsf);
                let v = self.read_bus(nsf, a).wrapping_sub(1);
                self.write_bus(nsf, a, v);
                self.cpu.update_nz(v);
                5
            }
            0xD6 => {
                let a = self.addr_zpx(nsf);
                let v = self.read_bus(nsf, a).wrapping_sub(1);
                self.write_bus(nsf, a, v);
                self.cpu.update_nz(v);
                6
            }
            0xCE => {
                let a = self.addr_abs(nsf);
                let v = self.read_bus(nsf, a).wrapping_sub(1);
                self.write_bus(nsf, a, v);
                self.cpu.update_nz(v);
                6
            }
            0xDE => {
                let (a, _) = self.addr_absx(nsf);
                let v = self.read_bus(nsf, a).wrapping_sub(1);
                self.write_bus(nsf, a, v);
                self.cpu.update_nz(v);
                7
            }
            0xE8 => {
                self.cpu.x = self.cpu.x.wrapping_add(1);
                self.cpu.update_nz(self.cpu.x);
                2
            }
            0xC8 => {
                self.cpu.y = self.cpu.y.wrapping_add(1);
                self.cpu.update_nz(self.cpu.y);
                2
            }
            0xCA => {
                self.cpu.x = self.cpu.x.wrapping_sub(1);
                self.cpu.update_nz(self.cpu.x);
                2
            }
            0x88 => {
                self.cpu.y = self.cpu.y.wrapping_sub(1);
                self.cpu.update_nz(self.cpu.y);
                2
            }

            // --- Shifts & Rotates ---
            0x0A => {
                self.cpu.set_flag(FLAG_C, (self.cpu.a & 0x80) != 0);
                self.cpu.a <<= 1;
                self.cpu.update_nz(self.cpu.a);
                2
            }
            0x06 => {
                let a = self.addr_zp(nsf);
                self.op_asl_mem(nsf, a);
                5
            }
            0x16 => {
                let a = self.addr_zpx(nsf);
                self.op_asl_mem(nsf, a);
                6
            }
            0x0E => {
                let a = self.addr_abs(nsf);
                self.op_asl_mem(nsf, a);
                6
            }
            0x1E => {
                let (a, _) = self.addr_absx(nsf);
                self.op_asl_mem(nsf, a);
                7
            }

            0x4A => {
                self.cpu.set_flag(FLAG_C, (self.cpu.a & 0x01) != 0);
                self.cpu.a >>= 1;
                self.cpu.update_nz(self.cpu.a);
                2
            }
            0x46 => {
                let a = self.addr_zp(nsf);
                self.op_lsr_mem(nsf, a);
                5
            }
            0x56 => {
                let a = self.addr_zpx(nsf);
                self.op_lsr_mem(nsf, a);
                6
            }
            0x4E => {
                let a = self.addr_abs(nsf);
                self.op_lsr_mem(nsf, a);
                6
            }
            0x5E => {
                let (a, _) = self.addr_absx(nsf);
                self.op_lsr_mem(nsf, a);
                7
            }

            0x2A => {
                let c = u8::from((self.cpu.p & FLAG_C) != 0);
                self.cpu.set_flag(FLAG_C, (self.cpu.a & 0x80) != 0);
                self.cpu.a = (self.cpu.a << 1) | c;
                self.cpu.update_nz(self.cpu.a);
                2
            }
            0x26 => {
                let a = self.addr_zp(nsf);
                self.op_rol_mem(nsf, a);
                5
            }
            0x36 => {
                let a = self.addr_zpx(nsf);
                self.op_rol_mem(nsf, a);
                6
            }
            0x2E => {
                let a = self.addr_abs(nsf);
                self.op_rol_mem(nsf, a);
                6
            }
            0x3E => {
                let (a, _) = self.addr_absx(nsf);
                self.op_rol_mem(nsf, a);
                7
            }

            0x6A => {
                let c = if (self.cpu.p & FLAG_C) != 0 { 0x80 } else { 0 };
                self.cpu.set_flag(FLAG_C, (self.cpu.a & 0x01) != 0);
                self.cpu.a = (self.cpu.a >> 1) | c;
                self.cpu.update_nz(self.cpu.a);
                2
            }
            0x66 => {
                let a = self.addr_zp(nsf);
                self.op_ror_mem(nsf, a);
                5
            }
            0x76 => {
                let a = self.addr_zpx(nsf);
                self.op_ror_mem(nsf, a);
                6
            }
            0x6E => {
                let a = self.addr_abs(nsf);
                self.op_ror_mem(nsf, a);
                6
            }
            0x7E => {
                let (a, _) = self.addr_absx(nsf);
                self.op_ror_mem(nsf, a);
                7
            }

            // --- Jumps & Subroutines ---
            0x4C => {
                let target = self.fetch_u16(nsf);
                self.cpu.pc = target;
                3
            }
            0x6C => {
                // NMOS 6502 page-wrap bug on indirect JMP ($xxFF)
                let ptr = self.fetch_u16(nsf);
                let lo = self.read_bus(nsf, ptr) as u16;
                let hi_addr = (ptr & 0xFF00) | ((ptr.wrapping_add(1)) & 0x00FF);
                let hi = self.read_bus(nsf, hi_addr) as u16;
                self.cpu.pc = lo | (hi << 8);
                5
            }
            0x20 => {
                let target = self.fetch_u16(nsf);
                let ret = self.cpu.pc.wrapping_sub(1);
                self.push_u8((ret >> 8) as u8);
                self.push_u8((ret & 0xFF) as u8);
                self.cpu.pc = target;
                6
            }
            0x60 => {
                let lo = self.pop_u8() as u16;
                let hi = self.pop_u8() as u16;
                self.cpu.pc = (lo | (hi << 8)).wrapping_add(1);
                6
            }
            0x40 => {
                let p = self.pop_u8();
                let lo = self.pop_u8() as u16;
                let hi = self.pop_u8() as u16;
                self.cpu.p = (p | FLAG_U) & !FLAG_B;
                self.cpu.pc = lo | (hi << 8);
                6
            }
            0x00 => {
                let ret = self.cpu.pc.wrapping_add(1);
                self.push_u8((ret >> 8) as u8);
                self.push_u8((ret & 0xFF) as u8);
                self.push_u8(self.cpu.p | FLAG_B | FLAG_U);
                self.cpu.p |= FLAG_I;
                let lo = self.read_bus(nsf, 0xFFFE) as u16;
                let hi = self.read_bus(nsf, 0xFFFF) as u16;
                let vec_target = lo | (hi << 8);
                self.cpu.pc = if vec_target == 0 { SENTINEL_PC } else { vec_target };
                7
            }

            // --- Branches ---
            0x10 => self.branch(nsf, (self.cpu.p & FLAG_N) == 0),
            0x30 => self.branch(nsf, (self.cpu.p & FLAG_N) != 0),
            0x50 => self.branch(nsf, (self.cpu.p & FLAG_V) == 0),
            0x70 => self.branch(nsf, (self.cpu.p & FLAG_V) != 0),
            0x90 => self.branch(nsf, (self.cpu.p & FLAG_C) == 0),
            0xB0 => self.branch(nsf, (self.cpu.p & FLAG_C) != 0),
            0xD0 => self.branch(nsf, (self.cpu.p & FLAG_Z) == 0),
            0xF0 => self.branch(nsf, (self.cpu.p & FLAG_Z) != 0),

            // --- Flag Instructions ---
            0x18 => {
                self.cpu.p &= !FLAG_C;
                2
            }
            0x38 => {
                self.cpu.p |= FLAG_C;
                2
            }
            0x58 => {
                self.cpu.p &= !FLAG_I;
                2
            }
            0x78 => {
                self.cpu.p |= FLAG_I;
                2
            }
            0xB8 => {
                self.cpu.p &= !FLAG_V;
                2
            }
            0xD8 => {
                self.cpu.p &= !FLAG_D;
                2
            }
            0xF8 => {
                self.cpu.p |= FLAG_D;
                2
            }

            // ================================================================
            // Stable Undocumented Opcodes (LAX, SAX, DCP, ISB, SLO, RLA, SRE, RRA)
            // ================================================================

            // --- LAX (LDA + LDX) ---
            0xA7 => {
                let a = self.addr_zp(nsf);
                let v = self.read_bus(nsf, a);
                self.cpu.a = v;
                self.cpu.x = v;
                self.cpu.update_nz(v);
                3
            }
            0xB7 => {
                let a = self.addr_zpy(nsf);
                let v = self.read_bus(nsf, a);
                self.cpu.a = v;
                self.cpu.x = v;
                self.cpu.update_nz(v);
                4
            }
            0xAF => {
                let a = self.addr_abs(nsf);
                let v = self.read_bus(nsf, a);
                self.cpu.a = v;
                self.cpu.x = v;
                self.cpu.update_nz(v);
                4
            }
            0xBF => {
                let (a, c) = self.addr_absy(nsf);
                let v = self.read_bus(nsf, a);
                self.cpu.a = v;
                self.cpu.x = v;
                self.cpu.update_nz(v);
                4 + c
            }
            0xA3 => {
                let a = self.addr_indx(nsf);
                let v = self.read_bus(nsf, a);
                self.cpu.a = v;
                self.cpu.x = v;
                self.cpu.update_nz(v);
                6
            }
            0xB3 => {
                let (a, c) = self.addr_indy(nsf);
                let v = self.read_bus(nsf, a);
                self.cpu.a = v;
                self.cpu.x = v;
                self.cpu.update_nz(v);
                5 + c
            }
            0xAB => {
                let v = self.fetch_u8(nsf);
                let r = (self.cpu.a | 0xEE) & v;
                self.cpu.a = r;
                self.cpu.x = r;
                self.cpu.update_nz(r);
                2
            }

            // --- SAX (Store A & X) ---
            0x87 => {
                let a = self.addr_zp(nsf);
                self.write_bus(nsf, a, self.cpu.a & self.cpu.x);
                3
            }
            0x97 => {
                let a = self.addr_zpy(nsf);
                self.write_bus(nsf, a, self.cpu.a & self.cpu.x);
                4
            }
            0x8F => {
                let a = self.addr_abs(nsf);
                self.write_bus(nsf, a, self.cpu.a & self.cpu.x);
                4
            }
            0x83 => {
                let a = self.addr_indx(nsf);
                self.write_bus(nsf, a, self.cpu.a & self.cpu.x);
                6
            }

            // --- DCP (DEC + CMP) ---
            0xC7 => {
                let a = self.addr_zp(nsf);
                self.op_dcp(nsf, a);
                5
            }
            0xD7 => {
                let a = self.addr_zpx(nsf);
                self.op_dcp(nsf, a);
                6
            }
            0xCF => {
                let a = self.addr_abs(nsf);
                self.op_dcp(nsf, a);
                6
            }
            0xDF => {
                let (a, _) = self.addr_absx(nsf);
                self.op_dcp(nsf, a);
                7
            }
            0xDB => {
                let (a, _) = self.addr_absy(nsf);
                self.op_dcp(nsf, a);
                7
            }
            0xC3 => {
                let a = self.addr_indx(nsf);
                self.op_dcp(nsf, a);
                8
            }
            0xD3 => {
                let (a, _) = self.addr_indy(nsf);
                self.op_dcp(nsf, a);
                8
            }

            // --- ISB / ISC (INC + SBC) ---
            0xE7 => {
                let a = self.addr_zp(nsf);
                self.op_isb(nsf, a);
                5
            }
            0xF7 => {
                let a = self.addr_zpx(nsf);
                self.op_isb(nsf, a);
                6
            }
            0xEF => {
                let a = self.addr_abs(nsf);
                self.op_isb(nsf, a);
                6
            }
            0xFF => {
                let (a, _) = self.addr_absx(nsf);
                self.op_isb(nsf, a);
                7
            }
            0xFB => {
                let (a, _) = self.addr_absy(nsf);
                self.op_isb(nsf, a);
                7
            }
            0xE3 => {
                let a = self.addr_indx(nsf);
                self.op_isb(nsf, a);
                8
            }
            0xF3 => {
                let (a, _) = self.addr_indy(nsf);
                self.op_isb(nsf, a);
                8
            }

            // --- SLO (ASL + ORA) ---
            0x07 => {
                let a = self.addr_zp(nsf);
                self.op_slo(nsf, a);
                5
            }
            0x17 => {
                let a = self.addr_zpx(nsf);
                self.op_slo(nsf, a);
                6
            }
            0x0F => {
                let a = self.addr_abs(nsf);
                self.op_slo(nsf, a);
                6
            }
            0x1F => {
                let (a, _) = self.addr_absx(nsf);
                self.op_slo(nsf, a);
                7
            }
            0x1B => {
                let (a, _) = self.addr_absy(nsf);
                self.op_slo(nsf, a);
                7
            }
            0x03 => {
                let a = self.addr_indx(nsf);
                self.op_slo(nsf, a);
                8
            }
            0x13 => {
                let (a, _) = self.addr_indy(nsf);
                self.op_slo(nsf, a);
                8
            }

            // --- RLA (ROL + AND) ---
            0x27 => {
                let a = self.addr_zp(nsf);
                self.op_rla(nsf, a);
                5
            }
            0x37 => {
                let a = self.addr_zpx(nsf);
                self.op_rla(nsf, a);
                6
            }
            0x2F => {
                let a = self.addr_abs(nsf);
                self.op_rla(nsf, a);
                6
            }
            0x3F => {
                let (a, _) = self.addr_absx(nsf);
                self.op_rla(nsf, a);
                7
            }
            0x3B => {
                let (a, _) = self.addr_absy(nsf);
                self.op_rla(nsf, a);
                7
            }
            0x23 => {
                let a = self.addr_indx(nsf);
                self.op_rla(nsf, a);
                8
            }
            0x33 => {
                let (a, _) = self.addr_indy(nsf);
                self.op_rla(nsf, a);
                8
            }

            // --- SRE (LSR + EOR) ---
            0x47 => {
                let a = self.addr_zp(nsf);
                self.op_sre(nsf, a);
                5
            }
            0x57 => {
                let a = self.addr_zpx(nsf);
                self.op_sre(nsf, a);
                6
            }
            0x4F => {
                let a = self.addr_abs(nsf);
                self.op_sre(nsf, a);
                6
            }
            0x5F => {
                let (a, _) = self.addr_absx(nsf);
                self.op_sre(nsf, a);
                7
            }
            0x5B => {
                let (a, _) = self.addr_absy(nsf);
                self.op_sre(nsf, a);
                7
            }
            0x43 => {
                let a = self.addr_indx(nsf);
                self.op_sre(nsf, a);
                8
            }
            0x53 => {
                let (a, _) = self.addr_indy(nsf);
                self.op_sre(nsf, a);
                8
            }

            // --- RRA (ROR + ADC) ---
            0x67 => {
                let a = self.addr_zp(nsf);
                self.op_rra(nsf, a);
                5
            }
            0x77 => {
                let a = self.addr_zpx(nsf);
                self.op_rra(nsf, a);
                6
            }
            0x6F => {
                let a = self.addr_abs(nsf);
                self.op_rra(nsf, a);
                6
            }
            0x7F => {
                let (a, _) = self.addr_absx(nsf);
                self.op_rra(nsf, a);
                7
            }
            0x7B => {
                let (a, _) = self.addr_absy(nsf);
                self.op_rra(nsf, a);
                7
            }
            0x63 => {
                let a = self.addr_indx(nsf);
                self.op_rra(nsf, a);
                8
            }
            0x73 => {
                let (a, _) = self.addr_indy(nsf);
                self.op_rra(nsf, a);
                8
            }

            // --- ANC / ALR / ARR / AXS ---
            0x0B | 0x2B => {
                let v = self.fetch_u8(nsf);
                self.cpu.a &= v;
                self.cpu.update_nz(self.cpu.a);
                self.cpu.set_flag(FLAG_C, (self.cpu.a & 0x80) != 0);
                2
            }
            0x4B => {
                let v = self.fetch_u8(nsf);
                self.cpu.a &= v;
                self.cpu.set_flag(FLAG_C, (self.cpu.a & 0x01) != 0);
                self.cpu.a >>= 1;
                self.cpu.update_nz(self.cpu.a);
                2
            }
            0x6B => {
                let v = self.fetch_u8(nsf);
                let c = if (self.cpu.p & FLAG_C) != 0 { 0x80 } else { 0 };
                self.cpu.a = ((self.cpu.a & v) >> 1) | c;
                self.cpu.update_nz(self.cpu.a);
                let b6 = (self.cpu.a >> 6) & 1;
                let b5 = (self.cpu.a >> 5) & 1;
                self.cpu.set_flag(FLAG_C, b6 != 0);
                self.cpu.set_flag(FLAG_V, (b6 ^ b5) != 0);
                2
            }
            0xCB => {
                let v = self.fetch_u8(nsf);
                let ax = self.cpu.a & self.cpu.x;
                self.cpu.set_flag(FLAG_C, ax >= v);
                self.cpu.x = ax.wrapping_sub(v);
                self.cpu.update_nz(self.cpu.x);
                2
            }

            // --- Official & Undocumented NOPs ---
            0xEA | 0x1A | 0x3A | 0x5A | 0x7A | 0xDA | 0xFA => 2,
            0x80 | 0x82 | 0x89 | 0xC2 | 0xE2 => {
                let _ = self.fetch_u8(nsf);
                2
            }
            0x04 | 0x44 | 0x64 => {
                let _ = self.fetch_u8(nsf);
                3
            }
            0x14 | 0x34 | 0x54 | 0x74 | 0xD4 | 0xF4 => {
                let _ = self.fetch_u8(nsf);
                4
            }
            0x0C => {
                let _ = self.fetch_u16(nsf);
                4
            }
            0x1C | 0x3C | 0x5C | 0x7C | 0xDC | 0xFC => {
                let (_, c) = self.addr_absx(nsf);
                4 + c
            }

            // --- KIL / JAM or rare unstable opcodes: retire frame cleanly ---
            _ => {
                self.cpu.idle = true;
                2
            }
        }
    }
}

#[inline]
pub(crate) fn read_rom_mapped(
    rom_banks: &[[u8; 4096]],
    bank_map: &[u16; 8],
    addr: u16,
) -> u8 {
    if addr < 0x8000 {
        return 0;
    }
    let slot = ((addr - 0x8000) >> 12) as usize;
    let b = bank_map[slot];
    if b == UNMAPPED_BANK {
        0
    } else if let Some(bank) = rom_banks.get(b as usize) {
        bank[(addr & 0x0FFF) as usize]
    } else {
        0
    }
}
