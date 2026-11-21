//! Pure-Rust ARM7TDMI (`ARMv4T`) CPU core for Game Boy Advance GSF emulation.
//!
//! Implements full 32-bit ARM and 16-bit Thumb instruction sets, banked processor
//! modes (`USR`, `FIQ`, `IRQ`, `SVC`, `ABT`, `UND`, `SYS`), hardware IRQ entry,
//! and GBA BIOS SWI dispatch.

use super::bios::handle_swi;
use super::bus::GbaBus;

pub const MODE_USR: u32 = 0x10;
pub const MODE_FIQ: u32 = 0x11;
pub const MODE_IRQ: u32 = 0x12;
pub const MODE_SVC: u32 = 0x13;
pub const MODE_ABT: u32 = 0x17;
pub const MODE_UND: u32 = 0x1B;
pub const MODE_SYS: u32 = 0x1F;

const FLAG_N: u32 = 1 << 31;
const FLAG_Z: u32 = 1 << 30;
const FLAG_C: u32 = 1 << 29;
const FLAG_V: u32 = 1 << 28;
const FLAG_I: u32 = 1 << 7;
const FLAG_T: u32 = 1 << 5;

pub struct Arm7Tdmi {
    pub regs: [u32; 16], // R0..R14 active; regs[15] kept synced on demand, `pc` is canonical next instruction
    pub pc: u32,
    pub cpsr: u32,

    // Banked registers
    usr_r8_r12: [u32; 5],
    fiq_r8_r12: [u32; 5],

    usr_r13_r14: [u32; 2],
    fiq_r13_r14: [u32; 2],
    irq_r13_r14: [u32; 2],
    svc_r13_r14: [u32; 2],
    abt_r13_r14: [u32; 2],
    und_r13_r14: [u32; 2],

    spsr_fiq: u32,
    spsr_irq: u32,
    spsr_svc: u32,
    spsr_abt: u32,
    spsr_und: u32,
}

impl Default for Arm7Tdmi {
    fn default() -> Self {
        Self::new(0x0800_0000)
    }
}

impl Arm7Tdmi {
    pub fn new(entry_point: u32) -> Self {
        let mut cpu = Self {
            regs: [0; 16],
            pc: 0x0800_0000,
            cpsr: MODE_SYS,
            usr_r8_r12: [0; 5],
            fiq_r8_r12: [0; 5],
            usr_r13_r14: [0x0300_7F00, 0],
            fiq_r13_r14: [0x0300_7F00, 0],
            irq_r13_r14: [0x0300_7FA0, 0],
            svc_r13_r14: [0x0300_7FE0, 0],
            abt_r13_r14: [0x0300_7F00, 0],
            und_r13_r14: [0x0300_7F00, 0],
            spsr_fiq: MODE_SYS,
            spsr_irq: MODE_SYS,
            spsr_svc: MODE_SYS,
            spsr_abt: MODE_SYS,
            spsr_und: MODE_SYS,
        };
        cpu.reset(entry_point);
        cpu
    }

    pub fn reset(&mut self, entry_point: u32) {
        self.regs = [0; 16];
        self.usr_r8_r12 = [0; 5];
        self.fiq_r8_r12 = [0; 5];
        self.usr_r13_r14 = [0x0300_7F00, 0];
        self.fiq_r13_r14 = [0x0300_7F00, 0];
        self.irq_r13_r14 = [0x0300_7FA0, 0];
        self.svc_r13_r14 = [0x0300_7FE0, 0];
        self.abt_r13_r14 = [0x0300_7F00, 0];
        self.und_r13_r14 = [0x0300_7F00, 0];
        self.regs[13] = 0x0300_7F00;
        let start = if entry_point == 0 {
            0x0800_0000
        } else {
            entry_point
        };
        if (start & 1) != 0 {
            self.cpsr = MODE_SYS | FLAG_T;
            self.pc = start & !1;
        } else {
            self.cpsr = MODE_SYS;
            self.pc = start & !3;
        }
    }

    #[inline]
    pub fn is_thumb(&self) -> bool {
        (self.cpsr & FLAG_T) != 0
    }

    #[inline]
    pub fn mode(&self) -> u32 {
        self.cpsr & 0x1F
    }

    #[inline]
    pub fn sp_svc(&self) -> u32 {
        if self.mode() == MODE_SVC {
            self.regs[13]
        } else {
            self.svc_r13_r14[0]
        }
    }

    #[inline]
    pub fn set_sp_svc(&mut self, val: u32) {
        if self.mode() == MODE_SVC {
            self.regs[13] = val;
        } else {
            self.svc_r13_r14[0] = val;
        }
    }

    #[inline]
    pub fn branch_exchange(&mut self, target: u32) {
        if (target & 1) != 0 {
            self.cpsr |= FLAG_T;
            self.pc = target & !1;
        } else {
            self.cpsr &= !FLAG_T;
            self.pc = target & !3;
        }
    }

    fn normalize_mode(raw: u32) -> u32 {
        match raw & 0x1F {
            MODE_USR => MODE_USR,
            MODE_FIQ => MODE_FIQ,
            MODE_IRQ => MODE_IRQ,
            MODE_SVC => MODE_SVC,
            MODE_ABT => MODE_ABT,
            MODE_UND => MODE_UND,
            _ => MODE_SYS,
        }
    }

    pub fn set_cpsr(&mut self, new_cpsr: u32) {
        let old_mode = Self::normalize_mode(self.cpsr);
        let new_mode = Self::normalize_mode(new_cpsr);

        if old_mode != new_mode {
            // Save active banked registers
            if old_mode == MODE_FIQ {
                self.fiq_r8_r12.copy_from_slice(&self.regs[8..13]);
                self.fiq_r13_r14 = [self.regs[13], self.regs[14]];
            } else {
                self.usr_r8_r12.copy_from_slice(&self.regs[8..13]);
                match old_mode {
                    MODE_IRQ => self.irq_r13_r14 = [self.regs[13], self.regs[14]],
                    MODE_SVC => self.svc_r13_r14 = [self.regs[13], self.regs[14]],
                    MODE_ABT => self.abt_r13_r14 = [self.regs[13], self.regs[14]],
                    MODE_UND => self.und_r13_r14 = [self.regs[13], self.regs[14]],
                    _ => self.usr_r13_r14 = [self.regs[13], self.regs[14]],
                }
            }

            // Load new mode's banked registers
            if new_mode == MODE_FIQ {
                self.regs[8..13].copy_from_slice(&self.fiq_r8_r12);
                self.regs[13] = self.fiq_r13_r14[0];
                self.regs[14] = self.fiq_r13_r14[1];
            } else {
                self.regs[8..13].copy_from_slice(&self.usr_r8_r12);
                let pair = match new_mode {
                    MODE_IRQ => self.irq_r13_r14,
                    MODE_SVC => self.svc_r13_r14,
                    MODE_ABT => self.abt_r13_r14,
                    MODE_UND => self.und_r13_r14,
                    _ => self.usr_r13_r14,
                };
                self.regs[13] = pair[0];
                self.regs[14] = pair[1];
            }
        }

        self.cpsr = (new_cpsr & !0x1F) | new_mode;
    }

    #[inline]
    fn get_spsr(&self) -> u32 {
        match Self::normalize_mode(self.cpsr) {
            MODE_FIQ => self.spsr_fiq,
            MODE_IRQ => self.spsr_irq,
            MODE_SVC => self.spsr_svc,
            MODE_ABT => self.spsr_abt,
            MODE_UND => self.spsr_und,
            _ => self.cpsr,
        }
    }

    #[inline]
    fn set_spsr(&mut self, val: u32) {
        match Self::normalize_mode(self.cpsr) {
            MODE_FIQ => self.spsr_fiq = val,
            MODE_IRQ => self.spsr_irq = val,
            MODE_SVC => self.spsr_svc = val,
            MODE_ABT => self.spsr_abt = val,
            MODE_UND => self.spsr_und = val,
            _ => {}
        }
    }

    fn read_user_reg(&self, idx: usize, arm_pc_val: u32) -> u32 {
        match idx {
            0..=7 => self.regs[idx],
            8..=12 => {
                if Self::normalize_mode(self.cpsr) == MODE_FIQ {
                    self.usr_r8_r12[idx - 8]
                } else {
                    self.regs[idx]
                }
            }
            13 | 14 => {
                let m = Self::normalize_mode(self.cpsr);
                if m == MODE_USR || m == MODE_SYS {
                    self.regs[idx]
                } else {
                    self.usr_r13_r14[idx - 13]
                }
            }
            _ => arm_pc_val,
        }
    }

    fn write_user_reg(&mut self, idx: usize, val: u32) {
        match idx {
            0..=7 => self.regs[idx] = val,
            8..=12 => {
                if Self::normalize_mode(self.cpsr) == MODE_FIQ {
                    self.usr_r8_r12[idx - 8] = val;
                } else {
                    self.regs[idx] = val;
                }
            }
            13 | 14 => {
                let m = Self::normalize_mode(self.cpsr);
                if m == MODE_USR || m == MODE_SYS {
                    self.regs[idx] = val;
                } else {
                    self.usr_r13_r14[idx - 13] = val;
                }
            }
            _ => {}
        }
    }

    /// Check and service hardware IRQ if `IME == 1`, `(IE & IF) != 0`, and `CPSR.I == 0`.
    #[inline]
    pub fn check_irq(&mut self, bus: &mut GbaBus) -> u32 {
        if bus.halted && bus.irq_pending() {
            bus.halted = false;
        }
        if (self.cpsr & FLAG_I) == 0 && bus.cpu_should_take_irq() {
            let old_cpsr = self.cpsr;
            let ret_lr = self.pc.wrapping_add(4);
            self.set_cpsr((old_cpsr & !(0x1F | FLAG_T)) | MODE_IRQ | FLAG_I);
            self.spsr_irq = old_cpsr;
            self.regs[14] = ret_lr;
            self.pc = 0x0000_0018;
            return 7;
        }
        0
    }

    /// Execute a single instruction (or enter IRQ). Caller advances `bus` by returned cycles.
    #[inline]
    pub fn step_instruction(&mut self, bus: &mut GbaBus) -> u32 {
        let irq_cyc = self.check_irq(bus);
        if irq_cyc > 0 {
            return irq_cyc;
        }
        if self.is_thumb() {
            let addr = self.pc & !1;
            let instr = bus.read_u16_aligned(addr);
            self.pc = addr.wrapping_add(2);
            self.exec_thumb(bus, addr, instr)
        } else {
            let addr = self.pc & !3;
            let instr = bus.read_u32_aligned(addr);
            self.pc = addr.wrapping_add(4);
            self.exec_arm(bus, addr, instr)
        }
    }

    #[inline]
    fn cond_passed(&self, cond: u32) -> bool {
        if cond == 0xE {
            return true;
        }
        let n = (self.cpsr & FLAG_N) != 0;
        let z = (self.cpsr & FLAG_Z) != 0;
        let c = (self.cpsr & FLAG_C) != 0;
        let v = (self.cpsr & FLAG_V) != 0;
        match cond {
            0x0 => z,
            0x1 => !z,
            0x2 => c,
            0x3 => !c,
            0x4 => n,
            0x5 => !n,
            0x6 => v,
            0x7 => !v,
            0x8 => c && !z,
            0x9 => !c || z,
            0xA => n == v,
            0xB => n != v,
            0xC => !z && (n == v),
            0xD => z || (n != v),
            _ => false,
        }
    }

    #[inline]
    fn set_nz(&mut self, res: u32) {
        self.cpsr &= !(FLAG_N | FLAG_Z);
        if (res & 0x8000_0000) != 0 {
            self.cpsr |= FLAG_N;
        }
        if res == 0 {
            self.cpsr |= FLAG_Z;
        }
    }

    #[inline]
    fn set_nzc(&mut self, res: u32, carry: bool) {
        self.set_nz(res);
        if carry {
            self.cpsr |= FLAG_C;
        } else {
            self.cpsr &= !FLAG_C;
        }
    }

    #[inline]
    fn set_nzcv(&mut self, res: u32, carry: bool, overflow: bool) {
        self.set_nzc(res, carry);
        if overflow {
            self.cpsr |= FLAG_V;
        } else {
            self.cpsr &= !FLAG_V;
        }
    }

    #[inline]
    fn alu_add(a: u32, b: u32) -> (u32, bool, bool) {
        let (res, carry) = a.overflowing_add(b);
        let overflow = ((!(a ^ b) & (a ^ res)) & 0x8000_0000) != 0;
        (res, carry, overflow)
    }

    #[inline]
    fn alu_adc(a: u32, b: u32, c_in: bool) -> (u32, bool, bool) {
        let c = c_in as u64;
        let sum = (a as u64) + (b as u64) + c;
        let res = sum as u32;
        let carry = (sum >> 32) != 0;
        let overflow = ((!(a ^ b) & (a ^ res)) & 0x8000_0000) != 0;
        (res, carry, overflow)
    }

    #[inline]
    fn alu_sub(a: u32, b: u32) -> (u32, bool, bool) {
        let (res, borrow) = a.overflowing_sub(b);
        let carry = !borrow;
        let overflow = (((a ^ b) & (a ^ res)) & 0x8000_0000) != 0;
        (res, carry, overflow)
    }

    #[inline]
    fn alu_sbc(a: u32, b: u32, c_in: bool) -> (u32, bool, bool) {
        let borrow_in = (!c_in) as i64;
        let diff = (a as i64) - (b as i64) - borrow_in;
        let res = diff as u32;
        let carry = diff >= 0;
        let overflow = (((a ^ b) & (a ^ res)) & 0x8000_0000) != 0;
        (res, carry, overflow)
    }

    #[inline]
    fn barrel_shift_imm(val: u32, shift_type: u32, amount: u32, old_c: bool) -> (u32, bool) {
        match shift_type & 3 {
            0 => {
                // LSL
                if amount == 0 {
                    (val, old_c)
                } else {
                    (val << amount, ((val >> (32 - amount)) & 1) != 0)
                }
            }
            1 => {
                // LSR (#0 means #32)
                if amount == 0 {
                    (0, (val & 0x8000_0000) != 0)
                } else {
                    (val >> amount, ((val >> (amount - 1)) & 1) != 0)
                }
            }
            2 => {
                // ASR (#0 means #32)
                if amount == 0 {
                    let sign = (val & 0x8000_0000) != 0;
                    (if sign { 0xFFFF_FFFF } else { 0 }, sign)
                } else {
                    (
                        ((val as i32) >> amount) as u32,
                        ((val >> (amount - 1)) & 1) != 0,
                    )
                }
            }
            _ => {
                // ROR (#0 means RRX)
                if amount == 0 {
                    let c_out = (val & 1) != 0;
                    let res = ((old_c as u32) << 31) | (val >> 1);
                    (res, c_out)
                } else {
                    (val.rotate_right(amount), ((val >> (amount - 1)) & 1) != 0)
                }
            }
        }
    }

    #[inline]
    fn barrel_shift_reg(val: u32, shift_type: u32, amount: u32, old_c: bool) -> (u32, bool) {
        let amt = amount & 0xFF;
        if amt == 0 {
            return (val, old_c);
        }
        match shift_type & 3 {
            0 => {
                // LSL
                if amt < 32 {
                    (val << amt, ((val >> (32 - amt)) & 1) != 0)
                } else if amt == 32 {
                    (0, (val & 1) != 0)
                } else {
                    (0, false)
                }
            }
            1 => {
                // LSR
                if amt < 32 {
                    (val >> amt, ((val >> (amt - 1)) & 1) != 0)
                } else if amt == 32 {
                    (0, (val & 0x8000_0000) != 0)
                } else {
                    (0, false)
                }
            }
            2 => {
                // ASR
                if amt < 32 {
                    (
                        ((val as i32) >> amt) as u32,
                        ((val >> (amt - 1)) & 1) != 0,
                    )
                } else {
                    let sign = (val & 0x8000_0000) != 0;
                    (if sign { 0xFFFF_FFFF } else { 0 }, sign)
                }
            }
            _ => {
                // ROR
                let rot = amt & 31;
                if rot == 0 {
                    (val, (val & 0x8000_0000) != 0)
                } else {
                    (val.rotate_right(rot), ((val >> (rot - 1)) & 1) != 0)
                }
            }
        }
    }

    #[inline]
    fn read_arm_reg(&self, idx: usize, pc_plus_8: u32) -> u32 {
        if idx == 15 {
            pc_plus_8
        } else {
            self.regs[idx]
        }
    }

    #[inline]
    fn ldr32_rotated(bus: &GbaBus, addr: u32) -> u32 {
        let word = bus.read_u32_aligned(addr);
        let rot = (addr & 3) * 8;
        if rot == 0 {
            word
        } else {
            word.rotate_right(rot)
        }
    }

    #[inline]
    fn ldrh_rotated(bus: &GbaBus, addr: u32) -> u32 {
        let half = bus.read_u16_aligned(addr) as u32;
        if (addr & 1) != 0 {
            half.rotate_right(8)
        } else {
            half
        }
    }

    #[inline]
    fn ldrsh_arm7(bus: &GbaBus, addr: u32) -> u32 {
        if (addr & 1) != 0 {
            // ARM7TDMI quirk: unaligned LDRSH sign-extends the single byte at `addr`!
            (bus.read_u8(addr) as i8 as i32) as u32
        } else {
            (bus.read_u16_aligned(addr) as i16 as i32) as u32
        }
    }

    // ========================================================================
    // 32-bit ARM Instruction Execution
    // ========================================================================

    fn exec_arm(&mut self, bus: &mut GbaBus, instr_addr: u32, instr: u32) -> u32 {
        let cond = instr >> 28;
        if !self.cond_passed(cond) {
            return 1;
        }

        let pc8 = instr_addr.wrapping_add(8);
        let top3 = (instr >> 25) & 7;

        match top3 {
            0b101 => {
                // Branch / Branch with Link (B / BL)
                let link = (instr & (1 << 24)) != 0;
                let offset = (((instr & 0x00FF_FFFF) as i32) << 8 >> 6) as u32;
                if link {
                    self.regs[14] = instr_addr.wrapping_add(4);
                }
                self.pc = pc8.wrapping_add(offset) & !3;
                3
            }
            0b111 => {
                if (instr & (1 << 24)) != 0 {
                    // Software Interrupt (SWI)
                    let swi_num = ((instr >> 16) & 0xFF) as u8;
                    handle_swi(self, bus, swi_num)
                } else {
                    1
                }
            }
            0b100 => {
                // Block Data Transfer (LDM / STM)
                self.exec_arm_block_transfer(bus, instr, pc8)
            }
            0b010 | 0b011 => {
                // Single Data Transfer (LDR / STR / LDRB / STRB)
                if top3 == 0b011 && (instr & 0x10) != 0 {
                    // Architecturally undefined on ARMv4T
                    return 1;
                }
                self.exec_arm_single_transfer(bus, instr, pc8)
            }
            0b000 | 0b001 => {
                // Check special sub-encodings when top3 == 0b000
                if top3 == 0b000 {
                    if (instr & 0x0FFF_FFF0) == 0x012F_FF10 {
                        // Branch & Exchange (BX Rn)
                        let rm = (instr & 0x0F) as usize;
                        let target = self.read_arm_reg(rm, pc8);
                        self.branch_exchange(target);
                        return 3;
                    }

                    if (instr & 0x90) == 0x90 {
                        let sub_op = (instr >> 5) & 3;
                        if sub_op != 0 {
                            // Halfword / Signed Data Transfer (STRH, LDRH, LDRSB, LDRSH)
                            return self.exec_arm_half_signed_transfer(bus, instr, pc8);
                        }
                        if (instr & 0x0FC0_00F0) == 0x0000_0090 {
                            // Multiply / Multiply-Accumulate (MUL / MLA)
                            return self.exec_arm_mul(instr, pc8);
                        }
                        if (instr & 0x0F80_00F0) == 0x0080_0090 {
                            // Long Multiply (UMULL / UMLAL / SMULL / SMLAL)
                            return self.exec_arm_mull(instr, pc8);
                        }
                        if (instr & 0x0FB0_0FF0) == 0x0100_0090 {
                            // Single Data Swap (SWP / SWPB)
                            return self.exec_arm_swp(bus, instr, pc8);
                        }
                    }
                }

                // Check PSR Transfer (MRS / MSR): opcode in 0b1000..0b1011 with S == 0
                let opcode = (instr >> 21) & 0x0F;
                let s_bit = (instr & (1 << 20)) != 0;
                if !s_bit && (0x8..=0xB).contains(&opcode) {
                    return self.exec_arm_psr(instr, pc8);
                }

                self.exec_arm_data_proc(instr, pc8)
            }
            _ => 1,
        }
    }

    fn exec_arm_psr(&mut self, instr: u32, pc8: u32) -> u32 {
        let use_spsr = (instr & (1 << 22)) != 0;
        if (instr & 0x0FBF_0FFF) == 0x010F_0000 {
            // MRS Rd, CPSR/SPSR
            let rd = ((instr >> 12) & 0x0F) as usize;
            let val = if use_spsr { self.get_spsr() } else { self.cpsr };
            if rd != 15 {
                self.regs[rd] = val;
            }
            return 1;
        }

        // MSR CPSR/SPSR_<fields>, Rm / #imm
        let is_imm = (instr & (1 << 25)) != 0;
        let operand = if is_imm {
            let imm8 = instr & 0xFF;
            let rot = ((instr >> 8) & 0x0F) * 2;
            imm8.rotate_right(rot)
        } else {
            let rm = (instr & 0x0F) as usize;
            self.read_arm_reg(rm, pc8)
        };

        let mut mask = 0u32;
        if (instr & (1 << 19)) != 0 {
            mask |= 0xFF00_0000;
        }
        if (instr & (1 << 18)) != 0 {
            mask |= 0x00FF_0000;
        }
        if (instr & (1 << 17)) != 0 {
            mask |= 0x0000_FF00;
        }
        if (instr & (1 << 16)) != 0 && (use_spsr || self.mode() != MODE_USR) {
            mask |= 0x0000_00FF;
        }

        if use_spsr {
            let cur = self.get_spsr();
            self.set_spsr((cur & !mask) | (operand & mask));
        } else {
            // On MSR to CPSR, software cannot directly set T bit via MSR (only BX/exception return)
            mask &= !FLAG_T;
            let cur = self.cpsr;
            self.set_cpsr((cur & !mask) | (operand & mask));
        }
        1
    }

    fn exec_arm_data_proc(&mut self, instr: u32, pc8: u32) -> u32 {
        let is_imm = (instr & (1 << 25)) != 0;
        let opcode = (instr >> 21) & 0x0F;
        let s_bit = (instr & (1 << 20)) != 0;
        let rn = ((instr >> 16) & 0x0F) as usize;
        let rd = ((instr >> 12) & 0x0F) as usize;
        let old_c = (self.cpsr & FLAG_C) != 0;

        let mut rn_pc = pc8;
        let (op2, shifter_c) = if is_imm {
            let imm8 = instr & 0xFF;
            let rot = ((instr >> 8) & 0x0F) * 2;
            if rot == 0 {
                (imm8, old_c)
            } else {
                let v = imm8.rotate_right(rot);
                (v, (v & 0x8000_0000) != 0)
            }
        } else {
            let rm = (instr & 0x0F) as usize;
            let shift_type = (instr >> 5) & 3;
            if (instr & 0x10) == 0 {
                let amount = (instr >> 7) & 0x1F;
                let rm_val = self.read_arm_reg(rm, pc8);
                Self::barrel_shift_imm(rm_val, shift_type, amount, old_c)
            } else {
                // Register-specified shift adds +1 cycle -> R15 reads as PC + 12!
                rn_pc = pc8.wrapping_add(4);
                let rs = ((instr >> 8) & 0x0F) as usize;
                let rs_val = self.read_arm_reg(rs, pc8);
                let rm_val = self.read_arm_reg(rm, rn_pc);
                Self::barrel_shift_reg(rm_val, shift_type, rs_val, old_c)
            }
        };

        let op1 = self.read_arm_reg(rn, rn_pc);

        let (res, write_rd, cyc) = match opcode {
            0x0 => {
                // AND
                let r = op1 & op2;
                if s_bit && rd != 15 {
                    self.set_nzc(r, shifter_c);
                }
                (r, true, 1)
            }
            0x1 => {
                // EOR
                let r = op1 ^ op2;
                if s_bit && rd != 15 {
                    self.set_nzc(r, shifter_c);
                }
                (r, true, 1)
            }
            0x2 => {
                // SUB
                let (r, c, v) = Self::alu_sub(op1, op2);
                if s_bit && rd != 15 {
                    self.set_nzcv(r, c, v);
                }
                (r, true, 1)
            }
            0x3 => {
                // RSB
                let (r, c, v) = Self::alu_sub(op2, op1);
                if s_bit && rd != 15 {
                    self.set_nzcv(r, c, v);
                }
                (r, true, 1)
            }
            0x4 => {
                // ADD
                let (r, c, v) = Self::alu_add(op1, op2);
                if s_bit && rd != 15 {
                    self.set_nzcv(r, c, v);
                }
                (r, true, 1)
            }
            0x5 => {
                // ADC
                let (r, c, v) = Self::alu_adc(op1, op2, old_c);
                if s_bit && rd != 15 {
                    self.set_nzcv(r, c, v);
                }
                (r, true, 1)
            }
            0x6 => {
                // SBC
                let (r, c, v) = Self::alu_sbc(op1, op2, old_c);
                if s_bit && rd != 15 {
                    self.set_nzcv(r, c, v);
                }
                (r, true, 1)
            }
            0x7 => {
                // RSC
                let (r, c, v) = Self::alu_sbc(op2, op1, old_c);
                if s_bit && rd != 15 {
                    self.set_nzcv(r, c, v);
                }
                (r, true, 1)
            }
            0x8 => {
                // TST
                let r = op1 & op2;
                self.set_nzc(r, shifter_c);
                (r, false, 1)
            }
            0x9 => {
                // TEQ
                let r = op1 ^ op2;
                self.set_nzc(r, shifter_c);
                (r, false, 1)
            }
            0xA => {
                // CMP
                let (r, c, v) = Self::alu_sub(op1, op2);
                self.set_nzcv(r, c, v);
                (r, false, 1)
            }
            0xB => {
                // CMN
                let (r, c, v) = Self::alu_add(op1, op2);
                self.set_nzcv(r, c, v);
                (r, false, 1)
            }
            0xC => {
                // ORR
                let r = op1 | op2;
                if s_bit && rd != 15 {
                    self.set_nzc(r, shifter_c);
                }
                (r, true, 1)
            }
            0xD => {
                // MOV
                let r = op2;
                if s_bit && rd != 15 {
                    self.set_nzc(r, shifter_c);
                }
                (r, true, 1)
            }
            0xE => {
                // BIC
                let r = op1 & !op2;
                if s_bit && rd != 15 {
                    self.set_nzc(r, shifter_c);
                }
                (r, true, 1)
            }
            _ => {
                // MVN
                let r = !op2;
                if s_bit && rd != 15 {
                    self.set_nzc(r, shifter_c);
                }
                (r, true, 1)
            }
        };

        if rd == 15 {
            if s_bit {
                let spsr = self.get_spsr();
                self.set_cpsr(spsr);
            }
            if write_rd {
                self.pc = if self.is_thumb() { res & !1 } else { res & !3 };
                return cyc + 2;
            }
        } else if write_rd {
            self.regs[rd] = res;
        }
        cyc
    }

    fn exec_arm_mul(&mut self, instr: u32, pc8: u32) -> u32 {
        let accumulate = (instr & (1 << 21)) != 0;
        let s_bit = (instr & (1 << 20)) != 0;
        let rd = ((instr >> 16) & 0x0F) as usize;
        let rn = ((instr >> 12) & 0x0F) as usize;
        let rs = ((instr >> 8) & 0x0F) as usize;
        let rm = (instr & 0x0F) as usize;

        let mut res = self
            .read_arm_reg(rm, pc8)
            .wrapping_mul(self.read_arm_reg(rs, pc8));
        if accumulate {
            res = res.wrapping_add(self.read_arm_reg(rn, pc8));
        }
        if rd != 15 {
            self.regs[rd] = res;
        }
        if s_bit {
            self.set_nz(res);
        }
        3
    }

    fn exec_arm_mull(&mut self, instr: u32, pc8: u32) -> u32 {
        let signed = (instr & (1 << 22)) != 0;
        let accumulate = (instr & (1 << 21)) != 0;
        let s_bit = (instr & (1 << 20)) != 0;
        let rd_hi = ((instr >> 16) & 0x0F) as usize;
        let rd_lo = ((instr >> 12) & 0x0F) as usize;
        let rs = ((instr >> 8) & 0x0F) as usize;
        let rm = (instr & 0x0F) as usize;

        let vm = self.read_arm_reg(rm, pc8);
        let vs = self.read_arm_reg(rs, pc8);

        let prod: u64 = if signed {
            ((vm as i32 as i64).wrapping_mul(vs as i32 as i64)) as u64
        } else {
            (vm as u64).wrapping_mul(vs as u64)
        };

        let total = if accumulate {
            let cur = ((self.read_arm_reg(rd_hi, pc8) as u64) << 32)
                | (self.read_arm_reg(rd_lo, pc8) as u64);
            prod.wrapping_add(cur)
        } else {
            prod
        };

        if rd_lo != 15 {
            self.regs[rd_lo] = total as u32;
        }
        if rd_hi != 15 {
            self.regs[rd_hi] = (total >> 32) as u32;
        }
        if s_bit {
            self.cpsr &= !(FLAG_N | FLAG_Z);
            if (total & (1u64 << 63)) != 0 {
                self.cpsr |= FLAG_N;
            }
            if total == 0 {
                self.cpsr |= FLAG_Z;
            }
        }
        4
    }

    fn exec_arm_swp(&mut self, bus: &mut GbaBus, instr: u32, pc8: u32) -> u32 {
        let byte = (instr & (1 << 22)) != 0;
        let rn = ((instr >> 16) & 0x0F) as usize;
        let rd = ((instr >> 12) & 0x0F) as usize;
        let rm = (instr & 0x0F) as usize;

        let addr = self.read_arm_reg(rn, pc8);
        let src_val = self.read_arm_reg(rm, pc8);
        if byte {
            let old = bus.read_u8(addr) as u32;
            bus.write_u8(addr, (src_val & 0xFF) as u8);
            if rd != 15 {
                self.regs[rd] = old;
            }
        } else {
            let old = Self::ldr32_rotated(bus, addr);
            bus.write_u32_aligned(addr, src_val);
            if rd != 15 {
                self.regs[rd] = old;
            }
        }
        4
    }

    fn exec_arm_single_transfer(&mut self, bus: &mut GbaBus, instr: u32, pc8: u32) -> u32 {
        let is_reg_offset = (instr & (1 << 25)) != 0;
        let pre_index = (instr & (1 << 24)) != 0;
        let add_up = (instr & (1 << 23)) != 0;
        let is_byte = (instr & (1 << 22)) != 0;
        let writeback = (instr & (1 << 21)) != 0;
        let is_load = (instr & (1 << 20)) != 0;
        let rn = ((instr >> 16) & 0x0F) as usize;
        let rd = ((instr >> 12) & 0x0F) as usize;

        let offset = if !is_reg_offset {
            instr & 0x0FFF
        } else {
            let rm = (instr & 0x0F) as usize;
            let shift_type = (instr >> 5) & 3;
            let amount = (instr >> 7) & 0x1F;
            let rm_val = self.read_arm_reg(rm, pc8);
            Self::barrel_shift_imm(rm_val, shift_type, amount, (self.cpsr & FLAG_C) != 0).0
        };

        let base = self.read_arm_reg(rn, pc8);
        let modified = if add_up {
            base.wrapping_add(offset)
        } else {
            base.wrapping_sub(offset)
        };
        let addr = if pre_index { modified } else { base };

        if is_load {
            let val = if is_byte {
                bus.read_u8(addr) as u32
            } else {
                Self::ldr32_rotated(bus, addr)
            };
            if (!pre_index || writeback) && rn != 15 && rn != rd {
                self.regs[rn] = modified;
            }
            if rd == 15 {
                self.pc = val & !3;
                return 5;
            }
            self.regs[rd] = val;
            3
        } else {
            // STR / STRB: when Rd == R15 on ARM7TDMI, stores PC + 12
            let val = if rd == 15 {
                pc8.wrapping_add(4)
            } else {
                self.regs[rd]
            };
            if is_byte {
                bus.write_u8(addr, (val & 0xFF) as u8);
            } else {
                bus.write_u32_aligned(addr, val);
            }
            if (!pre_index || writeback) && rn != 15 {
                self.regs[rn] = modified;
            }
            2
        }
    }

    fn exec_arm_half_signed_transfer(&mut self, bus: &mut GbaBus, instr: u32, pc8: u32) -> u32 {
        let pre_index = (instr & (1 << 24)) != 0;
        let add_up = (instr & (1 << 23)) != 0;
        let is_imm = (instr & (1 << 22)) != 0;
        let writeback = (instr & (1 << 21)) != 0;
        let is_load = (instr & (1 << 20)) != 0;
        let rn = ((instr >> 16) & 0x0F) as usize;
        let rd = ((instr >> 12) & 0x0F) as usize;
        let sh_op = (instr >> 5) & 3;

        let offset = if is_imm {
            ((instr >> 4) & 0xF0) | (instr & 0x0F)
        } else {
            let rm = (instr & 0x0F) as usize;
            self.read_arm_reg(rm, pc8)
        };

        let base = self.read_arm_reg(rn, pc8);
        let modified = if add_up {
            base.wrapping_add(offset)
        } else {
            base.wrapping_sub(offset)
        };
        let addr = if pre_index { modified } else { base };

        if is_load {
            let val = match sh_op {
                1 => Self::ldrh_rotated(bus, addr),
                2 => (bus.read_u8(addr) as i8 as i32) as u32,
                _ => Self::ldrsh_arm7(bus, addr),
            };
            if (!pre_index || writeback) && rn != 15 && rn != rd {
                self.regs[rn] = modified;
            }
            if rd == 15 {
                self.pc = val & !3;
                return 5;
            }
            self.regs[rd] = val;
            3
        } else {
            let val = if rd == 15 {
                pc8.wrapping_add(4)
            } else {
                self.regs[rd]
            };
            bus.write_u16_aligned(addr, (val & 0xFFFF) as u16);
            if (!pre_index || writeback) && rn != 15 {
                self.regs[rn] = modified;
            }
            2
        }
    }

    fn exec_arm_block_transfer(&mut self, bus: &mut GbaBus, instr: u32, pc8: u32) -> u32 {
        let pre_index = (instr & (1 << 24)) != 0;
        let add_up = (instr & (1 << 23)) != 0;
        let s_bit = (instr & (1 << 22)) != 0;
        let writeback = (instr & (1 << 21)) != 0;
        let is_load = (instr & (1 << 20)) != 0;
        let rn = ((instr >> 16) & 0x0F) as usize;
        let rlist = instr & 0xFFFF;

        let base = self.read_arm_reg(rn, pc8);
        if rlist == 0 {
            // ARM7TDMI empty rlist quirk: transfers R15 and offsets Rn by 0x40
            let wb = if add_up {
                base.wrapping_add(0x40)
            } else {
                base.wrapping_sub(0x40)
            };
            let addr = match (add_up, pre_index) {
                (true, false) => base,
                (true, true) => base.wrapping_add(4),
                (false, false) => base.wrapping_sub(0x3C),
                (false, true) => base.wrapping_sub(0x40),
            };
            if is_load {
                self.pc = bus.read_u32_aligned(addr) & !3;
            } else {
                bus.write_u32_aligned(addr, pc8.wrapping_add(4));
            }
            if writeback && rn != 15 {
                self.regs[rn] = wb;
            }
            return 4;
        }

        let count = rlist.count_ones();
        let bytes = count * 4;
        let wb_addr = if add_up {
            base.wrapping_add(bytes)
        } else {
            base.wrapping_sub(bytes)
        };
        let mut cur_addr = match (add_up, pre_index) {
            (true, false) => base,                        // IA
            (true, true) => base.wrapping_add(4),         // IB
            (false, false) => wb_addr.wrapping_add(4),    // DA
            (false, true) => wb_addr,                     // DB
        };

        let r15_in_list = (rlist & 0x8000) != 0;
        let user_bank = s_bit && (!is_load || !r15_in_list);

        if is_load {
            if writeback && rn != 15 && (rlist & (1 << rn)) == 0 {
                self.regs[rn] = wb_addr;
            }
            for reg in 0..16usize {
                if (rlist & (1 << reg)) == 0 {
                    continue;
                }
                let val = bus.read_u32_aligned(cur_addr);
                cur_addr = cur_addr.wrapping_add(4);
                if reg == 15 {
                    if s_bit {
                        let spsr = self.get_spsr();
                        self.set_cpsr(spsr);
                    }
                    self.pc = if self.is_thumb() { val & !1 } else { val & !3 };
                } else if user_bank {
                    self.write_user_reg(reg, val);
                } else {
                    self.regs[reg] = val;
                }
            }
            2 + count + if r15_in_list { 2 } else { 0 }
        } else {
            let lowest_reg = rlist.trailing_zeros() as usize;
            let pc12 = pc8.wrapping_add(4);
            for reg in 0..16usize {
                if (rlist & (1 << reg)) == 0 {
                    continue;
                }
                let val = if reg == 15 {
                    pc12
                } else if user_bank {
                    self.read_user_reg(reg, pc12)
                } else if writeback && reg == rn && reg != lowest_reg {
                    wb_addr
                } else {
                    self.regs[reg]
                };
                bus.write_u32_aligned(cur_addr, val);
                cur_addr = cur_addr.wrapping_add(4);
            }
            if writeback && rn != 15 {
                self.regs[rn] = wb_addr;
            }
            1 + count
        }
    }

    // ========================================================================
    // 16-bit Thumb Instruction Execution
    // ========================================================================

    fn exec_thumb(&mut self, bus: &mut GbaBus, instr_addr: u32, instr: u16) -> u32 {
        let pc4 = instr_addr.wrapping_add(4);

        match instr >> 13 {
            0b000 => {
                if (instr & 0x1800) == 0x1800 {
                    // Format 2: ADD / SUB (register or 3-bit immediate)
                    let is_imm = (instr & 0x0400) != 0;
                    let is_sub = (instr & 0x0200) != 0;
                    let rn_imm = ((instr >> 6) & 7) as usize;
                    let rs = ((instr >> 3) & 7) as usize;
                    let rd = (instr & 7) as usize;

                    let op1 = self.regs[rs];
                    let op2 = if is_imm {
                        rn_imm as u32
                    } else {
                        self.regs[rn_imm]
                    };
                    let (res, c, v) = if is_sub {
                        Self::alu_sub(op1, op2)
                    } else {
                        Self::alu_add(op1, op2)
                    };
                    self.regs[rd] = res;
                    self.set_nzcv(res, c, v);
                } else {
                    // Format 1: Move shifted register (LSL, LSR, ASR)
                    let shift_type = ((instr >> 11) & 3) as u32;
                    let amount = ((instr >> 6) & 0x1F) as u32;
                    let rs = ((instr >> 3) & 7) as usize;
                    let rd = (instr & 7) as usize;
                    let old_c = (self.cpsr & FLAG_C) != 0;
                    let (res, c) = Self::barrel_shift_imm(self.regs[rs], shift_type, amount, old_c);
                    self.regs[rd] = res;
                    self.set_nzc(res, c);
                }
                1
            }
            0b001 => {
                // Format 3: MOV / CMP / ADD / SUB immediate
                let op = (instr >> 11) & 3;
                let rd = ((instr >> 8) & 7) as usize;
                let imm8 = (instr & 0xFF) as u32;
                match op {
                    0 => {
                        self.regs[rd] = imm8;
                        self.set_nz(imm8);
                    }
                    1 => {
                        let (r, c, v) = Self::alu_sub(self.regs[rd], imm8);
                        self.set_nzcv(r, c, v);
                    }
                    2 => {
                        let (r, c, v) = Self::alu_add(self.regs[rd], imm8);
                        self.regs[rd] = r;
                        self.set_nzcv(r, c, v);
                    }
                    _ => {
                        let (r, c, v) = Self::alu_sub(self.regs[rd], imm8);
                        self.regs[rd] = r;
                        self.set_nzcv(r, c, v);
                    }
                }
                1
            }
            0b010 => {
                match (instr >> 10) & 7 {
                    0b000 => {
                        // Format 4: ALU operations
                        self.exec_thumb_alu(instr)
                    }
                    0b001 => {
                        // Format 5: Hi register operations / BX
                        self.exec_thumb_hireg(instr, pc4)
                    }
                    0b010 | 0b011 => {
                        // Format 6: PC-relative load `LDR Rd, [PC, #imm]`
                        let rd = ((instr >> 8) & 7) as usize;
                        let imm = ((instr & 0xFF) as u32) << 2;
                        let addr = (pc4 & !3).wrapping_add(imm);
                        self.regs[rd] = bus.read_u32_aligned(addr);
                        3
                    }
                    _ => {
                        // Formats 7 & 8: Load/store with register offset
                        let ro = ((instr >> 6) & 7) as usize;
                        let rb = ((instr >> 3) & 7) as usize;
                        let rd = (instr & 7) as usize;
                        let addr = self.regs[rb].wrapping_add(self.regs[ro]);

                        if (instr & 0x0200) == 0 {
                            match (instr >> 10) & 3 {
                                0 => {
                                    bus.write_u32_aligned(addr, self.regs[rd]);
                                    2
                                }
                                1 => {
                                    bus.write_u8(addr, (self.regs[rd] & 0xFF) as u8);
                                    2
                                }
                                2 => {
                                    self.regs[rd] = Self::ldr32_rotated(bus, addr);
                                    3
                                }
                                _ => {
                                    self.regs[rd] = bus.read_u8(addr) as u32;
                                    3
                                }
                            }
                        } else {
                            match (instr >> 10) & 3 {
                                0 => {
                                    bus.write_u16_aligned(addr, (self.regs[rd] & 0xFFFF) as u16);
                                    2
                                }
                                1 => {
                                    self.regs[rd] = (bus.read_u8(addr) as i8 as i32) as u32;
                                    3
                                }
                                2 => {
                                    self.regs[rd] = Self::ldrh_rotated(bus, addr);
                                    3
                                }
                                _ => {
                                    self.regs[rd] = Self::ldrsh_arm7(bus, addr);
                                    3
                                }
                            }
                        }
                    }
                }
            }
            0b011 => {
                // Format 9: Load/store word or byte with 5-bit immediate offset
                let is_byte = (instr & 0x1000) != 0;
                let is_load = (instr & 0x0800) != 0;
                let imm5 = ((instr >> 6) & 0x1F) as u32;
                let rb = ((instr >> 3) & 7) as usize;
                let rd = (instr & 7) as usize;
                let offset = if is_byte { imm5 } else { imm5 << 2 };
                let addr = self.regs[rb].wrapping_add(offset);

                if is_load {
                    self.regs[rd] = if is_byte {
                        bus.read_u8(addr) as u32
                    } else {
                        Self::ldr32_rotated(bus, addr)
                    };
                    3
                } else {
                    if is_byte {
                        bus.write_u8(addr, (self.regs[rd] & 0xFF) as u8);
                    } else {
                        bus.write_u32_aligned(addr, self.regs[rd]);
                    }
                    2
                }
            }
            0b100 => {
                if (instr & 0x1000) == 0 {
                    // Format 10: Load/store halfword with immediate offset
                    let is_load = (instr & 0x0800) != 0;
                    let imm = (((instr >> 6) & 0x1F) as u32) << 1;
                    let rb = ((instr >> 3) & 7) as usize;
                    let rd = (instr & 7) as usize;
                    let addr = self.regs[rb].wrapping_add(imm);
                    if is_load {
                        self.regs[rd] = Self::ldrh_rotated(bus, addr);
                        3
                    } else {
                        bus.write_u16_aligned(addr, (self.regs[rd] & 0xFFFF) as u16);
                        2
                    }
                } else {
                    // Format 11: SP-relative load/store
                    let is_load = (instr & 0x0800) != 0;
                    let rd = ((instr >> 8) & 7) as usize;
                    let imm = ((instr & 0xFF) as u32) << 2;
                    let addr = self.regs[13].wrapping_add(imm);
                    if is_load {
                        self.regs[rd] = Self::ldr32_rotated(bus, addr);
                        3
                    } else {
                        bus.write_u32_aligned(addr, self.regs[rd]);
                        2
                    }
                }
            }
            0b101 => {
                if (instr & 0x1000) == 0 {
                    // Format 12: Load address (`ADD Rd, PC/SP, #imm`)
                    let use_sp = (instr & 0x0800) != 0;
                    let rd = ((instr >> 8) & 7) as usize;
                    let imm = ((instr & 0xFF) as u32) << 2;
                    let base = if use_sp { self.regs[13] } else { pc4 & !3 };
                    self.regs[rd] = base.wrapping_add(imm);
                    1
                } else if (instr & 0x0F00) == 0x0000 {
                    // Format 13: Add offset to SP (`ADD SP, #+-imm`)
                    let imm = ((instr & 0x7F) as u32) << 2;
                    if (instr & 0x0080) != 0 {
                        self.regs[13] = self.regs[13].wrapping_sub(imm);
                    } else {
                        self.regs[13] = self.regs[13].wrapping_add(imm);
                    }
                    1
                } else if (instr & 0x0600) == 0x0400 {
                    // Format 14: PUSH / POP registers
                    let is_pop = (instr & 0x0800) != 0;
                    let pclr = (instr & 0x0100) != 0;
                    let rlist = instr & 0x00FF;
                    let count = rlist.count_ones() + (pclr as u32);
                    if is_pop {
                        let mut sp = self.regs[13];
                        for reg in 0..8usize {
                            if (rlist & (1 << reg)) != 0 {
                                self.regs[reg] = bus.read_u32_aligned(sp);
                                sp = sp.wrapping_add(4);
                            }
                        }
                        if pclr {
                            let new_pc = bus.read_u32_aligned(sp);
                            sp = sp.wrapping_add(4);
                            self.pc = new_pc & !1;
                        }
                        self.regs[13] = sp;
                        2 + count + if pclr { 2 } else { 0 }
                    } else {
                        let mut sp = self.regs[13].wrapping_sub(count * 4);
                        self.regs[13] = sp;
                        for reg in 0..8usize {
                            if (rlist & (1 << reg)) != 0 {
                                bus.write_u32_aligned(sp, self.regs[reg]);
                                sp = sp.wrapping_add(4);
                            }
                        }
                        if pclr {
                            bus.write_u32_aligned(sp, self.regs[14]);
                        }
                        1 + count
                    }
                } else {
                    1
                }
            }
            0b110 => {
                if (instr & 0x1000) == 0 {
                    // Format 15: Multiple load/store (`LDMIA Rb!, {..}` / `STMIA Rb!, {..}`)
                    let is_load = (instr & 0x0800) != 0;
                    let rb = ((instr >> 8) & 7) as usize;
                    let rlist = instr & 0x00FF;
                    let base = self.regs[rb];

                    if rlist == 0 {
                        let wb = base.wrapping_add(0x40);
                        if is_load {
                            self.pc = bus.read_u32_aligned(base) & !1;
                        } else {
                            bus.write_u32_aligned(base, pc4.wrapping_add(2));
                        }
                        self.regs[rb] = wb;
                        return 4;
                    }

                    let count = rlist.count_ones();
                    let wb = base.wrapping_add(count * 4);
                    let mut addr = base;
                    if is_load {
                        for reg in 0..8usize {
                            if (rlist & (1 << reg)) != 0 {
                                self.regs[reg] = bus.read_u32_aligned(addr);
                                addr = addr.wrapping_add(4);
                            }
                        }
                        if (rlist & (1 << rb)) == 0 {
                            self.regs[rb] = wb;
                        }
                        2 + count
                    } else {
                        let lowest = rlist.trailing_zeros() as usize;
                        for reg in 0..8usize {
                            if (rlist & (1 << reg)) != 0 {
                                let val = if reg == rb && reg != lowest {
                                    wb
                                } else {
                                    self.regs[reg]
                                };
                                bus.write_u32_aligned(addr, val);
                                addr = addr.wrapping_add(4);
                            }
                        }
                        self.regs[rb] = wb;
                        1 + count
                    }
                } else {
                    // Format 16 & 17: Conditional branch (`Bcond`) and `SWI #imm8`
                    let cond = ((instr >> 8) & 0x0F) as u32;
                    if cond == 0xF {
                        let swi_num = (instr & 0xFF) as u8;
                        return handle_swi(self, bus, swi_num);
                    }
                    if cond < 0xE && self.cond_passed(cond) {
                        let offset = (((instr & 0xFF) as i8 as i32) << 1) as u32;
                        self.pc = pc4.wrapping_add(offset) & !1;
                        return 3;
                    }
                    1
                }
            }
            _ => {
                // 0b111: Format 18 (`B`) & Format 19 (`BL` high/low halfwords)
                match (instr >> 11) & 3 {
                    0 => {
                        // Unconditional branch `B label`
                        let offset = (((instr & 0x07FF) as i32) << 21 >> 20) as u32;
                        self.pc = pc4.wrapping_add(offset) & !1;
                        3
                    }
                    2 => {
                        // Long branch w/ link prefix (`H == 0`): `LR = PC + 4 + (imm11 << 12)`
                        let hi_off = (((instr & 0x07FF) as i32) << 21 >> 9) as u32;
                        self.regs[14] = pc4.wrapping_add(hi_off);
                        1
                    }
                    3 => {
                        // Long branch w/ link suffix (`H == 1`)
                        let lo_off = ((instr & 0x07FF) as u32) << 1;
                        let target = self.regs[14].wrapping_add(lo_off) & !1;
                        self.regs[14] = instr_addr.wrapping_add(2) | 1;
                        self.pc = target;
                        3
                    }
                    _ => 1,
                }
            }
        }
    }

    fn exec_thumb_alu(&mut self, instr: u16) -> u32 {
        let op = (instr >> 6) & 0x0F;
        let rs = ((instr >> 3) & 7) as usize;
        let rd = (instr & 7) as usize;
        let a = self.regs[rd];
        let b = self.regs[rs];
        let old_c = (self.cpsr & FLAG_C) != 0;

        match op {
            0x0 => {
                let r = a & b;
                self.regs[rd] = r;
                self.set_nz(r);
            }
            0x1 => {
                let r = a ^ b;
                self.regs[rd] = r;
                self.set_nz(r);
            }
            0x2 => {
                let (r, c) = Self::barrel_shift_reg(a, 0, b, old_c);
                self.regs[rd] = r;
                self.set_nzc(r, c);
            }
            0x3 => {
                let (r, c) = Self::barrel_shift_reg(a, 1, b, old_c);
                self.regs[rd] = r;
                self.set_nzc(r, c);
            }
            0x4 => {
                let (r, c) = Self::barrel_shift_reg(a, 2, b, old_c);
                self.regs[rd] = r;
                self.set_nzc(r, c);
            }
            0x5 => {
                let (r, c, v) = Self::alu_adc(a, b, old_c);
                self.regs[rd] = r;
                self.set_nzcv(r, c, v);
            }
            0x6 => {
                let (r, c, v) = Self::alu_sbc(a, b, old_c);
                self.regs[rd] = r;
                self.set_nzcv(r, c, v);
            }
            0x7 => {
                let (r, c) = Self::barrel_shift_reg(a, 3, b, old_c);
                self.regs[rd] = r;
                self.set_nzc(r, c);
            }
            0x8 => {
                self.set_nz(a & b);
            }
            0x9 => {
                let (r, c, v) = Self::alu_sub(0, b);
                self.regs[rd] = r;
                self.set_nzcv(r, c, v);
            }
            0xA => {
                let (r, c, v) = Self::alu_sub(a, b);
                self.set_nzcv(r, c, v);
            }
            0xB => {
                let (r, c, v) = Self::alu_add(a, b);
                self.set_nzcv(r, c, v);
            }
            0xC => {
                let r = a | b;
                self.regs[rd] = r;
                self.set_nz(r);
            }
            0xD => {
                let r = a.wrapping_mul(b);
                self.regs[rd] = r;
                self.set_nz(r);
                return 3;
            }
            0xE => {
                let r = a & !b;
                self.regs[rd] = r;
                self.set_nz(r);
            }
            _ => {
                let r = !b;
                self.regs[rd] = r;
                self.set_nz(r);
            }
        }
        1
    }

    fn exec_thumb_hireg(&mut self, instr: u16, pc4: u32) -> u32 {
        let op = (instr >> 8) & 3;
        let h1 = ((instr >> 7) & 1) as usize;
        let h2 = ((instr >> 6) & 1) as usize;
        let rs = (((instr >> 3) & 7) as usize) | (h2 << 3);
        let rd = ((instr & 7) as usize) | (h1 << 3);

        let src_val = if rs == 15 { pc4 } else { self.regs[rs] };
        let dst_val = if rd == 15 { pc4 } else { self.regs[rd] };

        match op {
            0 => {
                let res = dst_val.wrapping_add(src_val);
                if rd == 15 {
                    self.pc = res & !1;
                    return 3;
                }
                self.regs[rd] = res;
                1
            }
            1 => {
                let (r, c, v) = Self::alu_sub(dst_val, src_val);
                self.set_nzcv(r, c, v);
                1
            }
            2 => {
                if rd == 15 {
                    self.pc = src_val & !1;
                    return 3;
                }
                self.regs[rd] = src_val;
                1
            }
            _ => {
                self.branch_exchange(src_val);
                3
            }
        }
    }
}
