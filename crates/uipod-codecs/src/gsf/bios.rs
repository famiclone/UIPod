//! Game Boy Advance BIOS ROM vectors & High-Level Emulation (HLE) SWI routines.
//!
//! Includes hardware-authentic ARM IRQ dispatch at `0x00000018`, nested-safe
//! `IntrWait` / `VBlankIntrWait`, `CpuSet` / `CpuFastSet`, `LZ77` / `RL` decompression,
//! `Div` / `Sqrt` / `ArcTan2`, `MidiKey2Freq`, and full `SoundDriver*` (`SWI 0x1A..0x29`).

use super::bus::GbaBus;
use super::cpu::{Arm7Tdmi, MODE_SYS};

pub const BIOS_INTR_WAIT_POLL_PC: u32 = 0x0000_0080;
pub const BIOS_SOUND_AFTER_FUNC_PC: u32 = 0x0000_0084;
pub const BIOS_SOUND_DONE_PC: u32 = 0x0000_0088;
pub const BIOS_DEFAULT_MIXER_PC: u32 = 0x0000_0090;

const SWI_INTERNAL_INTR_WAIT_POLL: u8 = 0x7F;
const SWI_INTERNAL_SOUND_AFTER_FUNC: u8 = 0x7D;
const SWI_INTERNAL_SOUND_DONE: u8 = 0x7E;
const SWI_INTERNAL_DEFAULT_MIXER: u8 = 0x7C;

const SOUND_IDENT: u32 = 0x6873_6D53; // "Smsh"
const PCM_DMA_BUF_SIZE: u32 = 1584;

const SAMPLES_PER_VBLANK: [i32; 12] = [96, 132, 176, 224, 264, 304, 352, 448, 528, 608, 672, 704];
const PCM_FREQS: [i32; 12] = [
    5734, 7884, 10512, 13379, 15768, 18157, 21024, 26758, 31536, 36314, 40137, 42048,
];

fn write_bios_u32(bios: &mut [u8; 0x4000], offset: usize, word: u32) {
    bios[offset..offset + 4].copy_from_slice(&word.to_le_bytes());
}

/// Populate the 16 KiB GBA BIOS region with authentic exception vectors and helper stubs.
pub fn install_bios_rom(bios: &mut [u8; 0x4000]) {
    bios.fill(0);

    // 0x00000000: Reset vector -> B 0x00000000 (placeholder; CPU starts at GSF entry_point)
    write_bios_u32(bios, 0x00, 0xEAFF_FFFE);
    // 0x00000008: SWI vector (intercepted in CPU before fetch, BX LR fallback)
    write_bios_u32(bios, 0x08, 0xE12F_FF1E);

    // 0x00000018: Hardware IRQ dispatcher (supports ARM & Thumb [0x03FFFFFC] + NULL fallback)
    write_bios_u32(bios, 0x18, 0xE92D_500F); // STMFD SP!, {R0-R3, R12, LR}
    write_bios_u32(bios, 0x1C, 0xE3A0_0301); // MOV   R0, #0x04000000
    write_bios_u32(bios, 0x20, 0xE510_C004); // LDR   R12, [R0, #-4]       ; [0x03FFFFFC]
    write_bios_u32(bios, 0x24, 0xE35C_0000); // CMP   R12, #0
    write_bios_u32(bios, 0x28, 0x028F_C010); // ADDEQ R12, PC, #0x10       ; -> 0x40 default ack
    write_bios_u32(bios, 0x2C, 0xE28F_E004); // ADD   LR, PC, #4           ; LR = 0x38
    write_bios_u32(bios, 0x30, 0xE12F_FF1C); // BX    R12
    write_bios_u32(bios, 0x34, 0xE1A0_0000); // NOP
    write_bios_u32(bios, 0x38, 0xE8BD_500F); // LDMFD SP!, {R0-R3, R12, LR}
    write_bios_u32(bios, 0x3C, 0xE25E_F004); // SUBS  PC, LR, #4           ; return from IRQ

    // 0x00000040: Default IRQ acknowledge stub when [0x03FFFFFC] == 0
    write_bios_u32(bios, 0x40, 0xE280_0C02); // ADD   R0, R0, #0x200
    write_bios_u32(bios, 0x44, 0xE1D0_10B2); // LDRH  R1, [R0, #2]         ; REG_IF
    write_bios_u32(bios, 0x48, 0xE1C0_10B2); // STRH  R1, [R0, #2]         ; ack REG_IF
    write_bios_u32(bios, 0x4C, 0xE240_0C02); // SUB   R0, R0, #0x200
    write_bios_u32(bios, 0x50, 0xE150_20B8); // LDRH  R2, [R0, #-8]        ; [0x03FFFFF8]
    write_bios_u32(bios, 0x54, 0xE182_2001); // ORR   R2, R2, R1
    write_bios_u32(bios, 0x58, 0xE140_20B8); // STRH  R2, [R0, #-8]
    write_bios_u32(bios, 0x5C, 0xE12F_FF1E); // BX    LR

    // Internal BIOS continuation traps (ARM SWI instructions: 0xEF000000 | (num << 16))
    write_bios_u32(
        bios,
        BIOS_INTR_WAIT_POLL_PC as usize,
        0xEF00_0000 | ((SWI_INTERNAL_INTR_WAIT_POLL as u32) << 16),
    );
    write_bios_u32(
        bios,
        BIOS_SOUND_AFTER_FUNC_PC as usize,
        0xEF00_0000 | ((SWI_INTERNAL_SOUND_AFTER_FUNC as u32) << 16),
    );
    write_bios_u32(
        bios,
        BIOS_SOUND_DONE_PC as usize,
        0xEF00_0000 | ((SWI_INTERNAL_SOUND_DONE as u32) << 16),
    );
    write_bios_u32(
        bios,
        BIOS_DEFAULT_MIXER_PC as usize,
        0xEF00_0000 | ((SWI_INTERNAL_DEFAULT_MIXER as u32) << 16),
    );
    write_bios_u32(bios, (BIOS_DEFAULT_MIXER_PC + 4) as usize, 0xE12F_FF1E); // BX LR
}

/// Handle a GBA Software Interrupt (`SWI #num`). Returns approximate cycle cost.
pub fn handle_swi(cpu: &mut Arm7Tdmi, bus: &mut GbaBus, swi_num: u8) -> u32 {
    match swi_num {
        0x00 => {
            // SoftReset
            let flag = bus.read_u8(0x0300_7FFA);
            bus.iwram[0x7E00..0x8000].fill(0);
            cpu.reset(if flag != 0 { 0x0200_0000 } else { 0x0800_0000 });
            64
        }
        0x01 => {
            // RegisterRamReset(flags)
            let flags = cpu.regs[0];
            swi_register_ram_reset(bus, flags);
            64
        }
        0x02 | 0x03 => {
            // Halt / Stop
            bus.halted = true;
            4
        }
        0x04 => {
            // IntrWait(discard, flags)
            let discard = cpu.regs[0] != 0;
            let flags = (cpu.regs[1] & 0xFFFF) as u16;
            enter_intr_wait(cpu, bus, discard, flags);
            8
        }
        0x05 => {
            // VBlankIntrWait() == IntrWait(1, 1)
            cpu.regs[0] = 1;
            cpu.regs[1] = 1;
            enter_intr_wait(cpu, bus, true, 1);
            8
        }
        0x06 => {
            // Div(num, denom)
            let num = cpu.regs[0] as i32;
            let denom = cpu.regs[1] as i32;
            if denom != 0 {
                let q = num.wrapping_div(denom);
                let r = num.wrapping_rem(denom);
                cpu.regs[0] = q as u32;
                cpu.regs[1] = r as u32;
                cpu.regs[3] = q.wrapping_abs() as u32;
            }
            40
        }
        0x07 => {
            // DivArm(denom, num)
            let denom = cpu.regs[0] as i32;
            let num = cpu.regs[1] as i32;
            if denom != 0 {
                let q = num.wrapping_div(denom);
                let r = num.wrapping_rem(denom);
                cpu.regs[0] = q as u32;
                cpu.regs[1] = r as u32;
                cpu.regs[3] = q.wrapping_abs() as u32;
            }
            40
        }
        0x08 => {
            // Sqrt(val)
            let v = cpu.regs[0] as f64;
            cpu.regs[0] = v.sqrt() as u32;
            32
        }
        0x09 => {
            // ArcTan(tan) - 1.14 fixed point in, -0x4000..0x4000 out
            let t = (cpu.regs[0] as i16 as f64) / 16384.0;
            let angle = (t.atan() * (32768.0 / std::f64::consts::PI)).round() as i32;
            cpu.regs[0] = (angle as i16 as i32) as u32;
            32
        }
        0x0A => {
            // ArcTan2(x, y) -> 0..0xFFFF angle
            let x = cpu.regs[0] as i16 as f64;
            let y = cpu.regs[1] as i16 as f64;
            let mut angle = (y.atan2(x) * (32768.0 / std::f64::consts::PI)).round() as i32;
            if angle < 0 {
                angle += 0x10000;
            }
            cpu.regs[0] = (angle & 0xFFFF) as u32;
            40
        }
        0x0B => {
            // CpuSet(src, dst, cnt)
            swi_cpu_set(bus, cpu.regs[0], cpu.regs[1], cpu.regs[2])
        }
        0x0C => {
            // CpuFastSet(src, dst, cnt)
            swi_cpu_fast_set(bus, cpu.regs[0], cpu.regs[1], cpu.regs[2])
        }
        0x0D => {
            // GetBiosChecksum() -> authentic GBA BIOS checksum
            cpu.regs[0] = 0xBAAE_187F;
            16
        }
        0x10 => {
            // BitUnPack(src, dst, info_ptr)
            swi_bit_unpack(bus, cpu.regs[0], cpu.regs[1], cpu.regs[2])
        }
        0x11 | 0x12 => {
            // LZ77UnCompWram / LZ77UnCompVram
            swi_lz77_uncomp(bus, cpu.regs[0], cpu.regs[1])
        }
        0x13 => {
            // HuffUnComp
            swi_huff_uncomp(bus, cpu.regs[0], cpu.regs[1])
        }
        0x14 | 0x15 => {
            // RLUnCompWram / RLUnCompVram
            swi_rl_uncomp(bus, cpu.regs[0], cpu.regs[1])
        }
        0x19 => {
            // SoundBias(level)
            if cpu.regs[0] == 0 {
                bus.apu.soundbias &= !0x03FE;
            } else {
                bus.apu.soundbias = (bus.apu.soundbias & !0x03FE) | 0x0200;
            }
            16
        }
        0x1A => {
            // SoundDriverInit(SoundArea *sa)
            swi_sound_driver_init(bus, cpu.regs[0]);
            64
        }
        0x1B => {
            // SoundDriverMode(u32 mode)
            swi_sound_driver_mode(bus, cpu.regs[0]);
            48
        }
        0x1C => {
            // SoundDriverMain()
            enter_sound_driver_main(cpu, bus);
            32
        }
        0x1D => {
            // SoundDriverVSync()
            swi_sound_driver_vsync(bus);
            24
        }
        0x1E => {
            // SoundChannelClear()
            swi_sound_channel_clear(bus);
            32
        }
        0x1F => {
            // MidiKey2Freq(WaveData *wa, u8 mk, u8 fp)
            let wa = cpu.regs[0];
            let mk = (cpu.regs[1] & 0xFF) as f64;
            let fp = (cpu.regs[2] & 0xFF) as f64;
            let base_freq = read_u32_unaligned_raw(bus, wa.wrapping_add(4)) as f64;
            let exp = ((180.0 - mk) - (fp / 256.0)) / 12.0;
            let res = (base_freq / 2.0_f64.powf(exp)).floor();
            cpu.regs[0] = res.clamp(0.0, u32::MAX as f64) as u32;
            36
        }
        0x28 => {
            // SoundDriverVSyncOff()
            swi_sound_driver_vsync_off(bus);
            16
        }
        0x29 => {
            // SoundDriverVSyncOn()
            swi_sound_driver_vsync_on(bus);
            16
        }

        // Internal BIOS continuation SWIs
        SWI_INTERNAL_INTR_WAIT_POLL => {
            poll_intr_wait(cpu, bus);
            4
        }
        SWI_INTERNAL_SOUND_AFTER_FUNC => {
            continue_sound_driver_after_func(cpu, bus);
            8
        }
        SWI_INTERNAL_SOUND_DONE => {
            finish_sound_driver_main(cpu, bus);
            8
        }
        SWI_INTERNAL_DEFAULT_MIXER => {
            bios_default_sound_mixer(bus, cpu.regs[0]);
            120
        }
        _ => 16,
    }
}

fn read_u32_unaligned_raw(bus: &GbaBus, addr: u32) -> u32 {
    let b0 = bus.read_u8(addr) as u32;
    let b1 = bus.read_u8(addr.wrapping_add(1)) as u32;
    let b2 = bus.read_u8(addr.wrapping_add(2)) as u32;
    let b3 = bus.read_u8(addr.wrapping_add(3)) as u32;
    b0 | (b1 << 8) | (b2 << 16) | (b3 << 24)
}

fn enter_intr_wait(cpu: &mut Arm7Tdmi, bus: &mut GbaBus, discard: bool, flags: u16) {
    let return_pc = cpu.pc;
    let saved_cpsr = cpu.cpsr;

    bus.io_ime = 0;
    if discard {
        let cur = bus.read_u16_aligned(0x03FF_FFF8);
        bus.write_u16_aligned(0x03FF_FFF8, cur & !flags);
    }

    // Push frame {flags, return_pc, saved_cpsr} onto SVC stack so nested SWIs/IntrWaits are safe
    let sp_svc = cpu.sp_svc().wrapping_sub(12);
    cpu.set_sp_svc(sp_svc);
    bus.write_u32_aligned(sp_svc, flags as u32);
    bus.write_u32_aligned(sp_svc.wrapping_add(4), return_pc);
    bus.write_u32_aligned(sp_svc.wrapping_add(8), saved_cpsr);

    // Switch to SYS mode with IRQs enabled (I = 0, T = 0), enable IME, and halt at poll vector
    cpu.set_cpsr((saved_cpsr & 0xF000_0000) | MODE_SYS);
    cpu.pc = BIOS_INTR_WAIT_POLL_PC;
    bus.io_ime = 1;
    bus.halted = true;
}

fn poll_intr_wait(cpu: &mut Arm7Tdmi, bus: &mut GbaBus) {
    let sp_svc = cpu.sp_svc();
    let flags = (bus.read_u32_aligned(sp_svc) & 0xFFFF) as u16;
    let return_pc = bus.read_u32_aligned(sp_svc.wrapping_add(4));
    let saved_cpsr = bus.read_u32_aligned(sp_svc.wrapping_add(8));

    bus.io_ime = 0;
    let intr_check = bus.read_u16_aligned(0x03FF_FFF8);
    if flags == 0 || (intr_check & flags) != 0 {
        bus.write_u16_aligned(0x03FF_FFF8, intr_check & !flags);
        bus.io_ime = 1;
        cpu.set_sp_svc(sp_svc.wrapping_add(12));
        cpu.set_cpsr(saved_cpsr);
        cpu.pc = return_pc;
    } else {
        // Woke up for a different interrupt; stay at poll vector and halt again until `flags` arrive
        cpu.pc = BIOS_INTR_WAIT_POLL_PC;
        bus.io_ime = 1;
        bus.halted = true;
    }
}

fn swi_register_ram_reset(bus: &mut GbaBus, flags: u32) {
    if (flags & 0x01) != 0 {
        bus.ewram.fill(0);
    }
    if (flags & 0x02) != 0 {
        // Preserve 0x03007E00..0x03007FFF (system stacks & IRQ vector) per GBA hardware spec!
        bus.iwram[..0x7E00].fill(0);
    }
    if (flags & 0x04) != 0 {
        bus.pram.fill(0);
    }
    if (flags & 0x08) != 0 {
        bus.vram.fill(0);
    }
    if (flags & 0x10) != 0 {
        bus.oam.fill(0);
    }
    if (flags & 0x20) != 0 {
        bus.apu.reset_registers();
    }
    if (flags & 0x80) != 0 {
        bus.timers = [Default::default(); 4];
        bus.dma = [Default::default(); 4];
        bus.dispcnt = 0x0080;
        bus.dispstat = 0;
        bus.io_ie = 0;
        bus.io_if = 0;
        bus.io_ime = 0;
    }
}

fn swi_cpu_set(bus: &mut GbaBus, mut src: u32, mut dst: u32, cnt: u32) -> u32 {
    if src < 0x0000_4000 {
        return 16;
    }
    let count = cnt & 0x001F_FFFF;
    let fill = (cnt & (1 << 24)) != 0;
    let is_32 = (cnt & (1 << 26)) != 0;

    if is_32 {
        src &= !3;
        dst &= !3;
        let fixed_val = bus.read_u32_aligned(src);
        for _ in 0..count {
            let v = if fill {
                fixed_val
            } else {
                let w = bus.read_u32_aligned(src);
                src = src.wrapping_add(4);
                w
            };
            bus.write_u32_aligned(dst, v);
            dst = dst.wrapping_add(4);
        }
    } else {
        src &= !1;
        dst &= !1;
        let fixed_val = bus.read_u16_aligned(src);
        for _ in 0..count {
            let v = if fill {
                fixed_val
            } else {
                let h = bus.read_u16_aligned(src);
                src = src.wrapping_add(2);
                h
            };
            bus.write_u16_aligned(dst, v);
            dst = dst.wrapping_add(2);
        }
    }
    16 + count * 2
}

fn swi_cpu_fast_set(bus: &mut GbaBus, mut src: u32, mut dst: u32, cnt: u32) -> u32 {
    if src < 0x0000_4000 {
        return 16;
    }
    // CpuFastSet rounds word count up to a multiple of 8 words (32 bytes)
    let raw_count = cnt & 0x001F_FFFF;
    let count = (raw_count + 7) & !7;
    let fill = (cnt & (1 << 24)) != 0;
    src &= !3;
    dst &= !3;
    let fixed_val = bus.read_u32_aligned(src);
    for _ in 0..count {
        let v = if fill {
            fixed_val
        } else {
            let w = bus.read_u32_aligned(src);
            src = src.wrapping_add(4);
            w
        };
        bus.write_u32_aligned(dst, v);
        dst = dst.wrapping_add(4);
    }
    16 + count
}

fn swi_bit_unpack(bus: &mut GbaBus, mut src: u32, mut dst: u32, info: u32) -> u32 {
    let src_len = bus.read_u16_aligned(info) as usize;
    let src_bits = bus.read_u8(info.wrapping_add(2)) as u32;
    let dst_bits = bus.read_u8(info.wrapping_add(3)) as u32;
    let data_offset_flag = bus.read_u32_aligned(info.wrapping_add(4));
    let zero_flag = (data_offset_flag & 0x8000_0000) != 0;
    let add_offset = data_offset_flag & 0x7FFF_FFFF;

    if src_bits == 0 || dst_bits == 0 || dst_bits > 32 {
        return 16;
    }
    let src_mask = (1u32 << src_bits) - 1;
    let mut out_word = 0u32;
    let mut out_bit = 0u32;

    for _ in 0..src_len {
        let byte = bus.read_u8(src) as u32;
        src = src.wrapping_add(1);
        let mut in_bit = 0u32;
        while in_bit < 8 {
            let mut unit = (byte >> in_bit) & src_mask;
            if unit != 0 || zero_flag {
                unit = unit.wrapping_add(add_offset);
            }
            out_word |= (unit & ((1u64 << dst_bits) - 1) as u32) << out_bit;
            out_bit += dst_bits;
            if out_bit >= 32 {
                bus.write_u32_aligned(dst, out_word);
                dst = dst.wrapping_add(4);
                out_word = 0;
                out_bit = 0;
            }
            in_bit += src_bits;
        }
    }
    32 + (src_len as u32) * 4
}

fn swi_lz77_uncomp(bus: &mut GbaBus, mut src: u32, dst: u32) -> u32 {
    let header = bus.read_u32_aligned(src);
    if (header & 0xFF) != 0x10 {
        return 16;
    }
    let decomp_len = (header >> 8) as usize;
    if decomp_len == 0 || decomp_len > 0x400000 {
        return 16;
    }
    src = src.wrapping_add(4);

    let mut out = Vec::with_capacity(decomp_len);
    while out.len() < decomp_len {
        let flags = bus.read_u8(src);
        src = src.wrapping_add(1);
        for bit in (0..8).rev() {
            if out.len() >= decomp_len {
                break;
            }
            if ((flags >> bit) & 1) == 0 {
                out.push(bus.read_u8(src));
                src = src.wrapping_add(1);
            } else {
                let b1 = bus.read_u8(src) as usize;
                let b2 = bus.read_u8(src.wrapping_add(1)) as usize;
                src = src.wrapping_add(2);
                let count = (b1 >> 4) + 3;
                let disp = (((b1 & 0x0F) << 8) | b2) + 1;
                for _ in 0..count {
                    if out.len() >= decomp_len {
                        break;
                    }
                    let sample = if disp <= out.len() {
                        out[out.len() - disp]
                    } else {
                        0
                    };
                    out.push(sample);
                }
            }
        }
    }

    write_decomp_block(bus, dst, &out);
    32 + (decomp_len as u32) * 2
}

fn swi_huff_uncomp(bus: &mut GbaBus, src: u32, dst: u32) -> u32 {
    let header = bus.read_u32_aligned(src);
    let bit_size = header & 0x0F;
    if (header & 0xF0) != 0x20 || (bit_size != 4 && bit_size != 8) {
        return 16;
    }
    let decomp_len = (header >> 8) as usize;
    if decomp_len == 0 || decomp_len > 0x400000 {
        return 16;
    }

    let tree_size = ((bus.read_u8(src.wrapping_add(4)) as u32) + 1) * 2;
    let tree_base = src.wrapping_add(5);
    let mut stream_ptr = src.wrapping_add(4 + tree_size);

    let mut out = Vec::with_capacity(decomp_len);
    let mut node_addr = tree_base;
    let mut out_word = 0u32;
    let mut out_bits = 0u32;

    while out.len() < decomp_len {
        let bits = bus.read_u32_aligned(stream_ptr);
        stream_ptr = stream_ptr.wrapping_add(4);
        for i in (0..32).rev() {
            let bit = (bits >> i) & 1;
            let node = bus.read_u8(node_addr);
            let offset = (((node & 0x3F) as u32) + 1) * 2;
            let end_flag = if bit == 0 {
                (node & 0x80) != 0
            } else {
                (node & 0x40) != 0
            };
            let child_addr = (node_addr & !1).wrapping_add(offset).wrapping_add(bit);
            if end_flag {
                let sym = bus.read_u8(child_addr) as u32;
                out_word |= sym << out_bits;
                out_bits += bit_size;
                if out_bits >= 32 {
                    out.extend_from_slice(&out_word.to_le_bytes());
                    out_word = 0;
                    out_bits = 0;
                    if out.len() >= decomp_len {
                        break;
                    }
                }
                node_addr = tree_base;
            } else {
                node_addr = child_addr;
            }
        }
    }
    out.truncate(decomp_len);
    write_decomp_block(bus, dst, &out);
    32 + (decomp_len as u32) * 3
}

fn swi_rl_uncomp(bus: &mut GbaBus, mut src: u32, dst: u32) -> u32 {
    let header = bus.read_u32_aligned(src);
    if (header & 0xF0) != 0x30 {
        return 16;
    }
    let decomp_len = (header >> 8) as usize;
    if decomp_len == 0 || decomp_len > 0x400000 {
        return 16;
    }
    src = src.wrapping_add(4);

    let mut out = Vec::with_capacity(decomp_len);
    while out.len() < decomp_len {
        let flag = bus.read_u8(src);
        src = src.wrapping_add(1);
        if (flag & 0x80) == 0 {
            let count = (flag as usize) + 1;
            for _ in 0..count {
                if out.len() >= decomp_len {
                    break;
                }
                out.push(bus.read_u8(src));
                src = src.wrapping_add(1);
            }
        } else {
            let count = ((flag & 0x7F) as usize) + 3;
            let b = bus.read_u8(src);
            src = src.wrapping_add(1);
            for _ in 0..count {
                if out.len() >= decomp_len {
                    break;
                }
                out.push(b);
            }
        }
    }

    write_decomp_block(bus, dst, &out);
    32 + (decomp_len as u32) * 2
}

fn write_decomp_block(bus: &mut GbaBus, mut dst: u32, data: &[u8]) {
    if (dst >> 24) == 0x06 || (dst >> 24) == 0x05 || (dst >> 24) == 0x07 {
        dst &= !1;
        let mut i = 0;
        while i + 1 < data.len() {
            let h = u16::from_le_bytes([data[i], data[i + 1]]);
            bus.write_u16_aligned(dst, h);
            dst = dst.wrapping_add(2);
            i += 2;
        }
        if i < data.len() {
            bus.write_u16_aligned(dst, data[i] as u16);
        }
    } else {
        for &b in data {
            bus.write_u8(dst, b);
            dst = dst.wrapping_add(1);
        }
    }
}

// ============================================================================
// GBA BIOS SoundDriver (SWI 0x1A..0x1E, 0x28..0x29)
// ============================================================================

fn apply_sound_freq(bus: &mut GbaBus, sa: u32, freq_idx: u8) {
    let idx = (freq_idx.clamp(1, 12) - 1) as usize;
    let spv = SAMPLES_PER_VBLANK[idx];
    let pcm_freq = PCM_FREQS[idx];
    let period = (PCM_DMA_BUF_SIZE as i32) / spv;
    let div_freq = ((1u64 << 32) / (pcm_freq as u64)) as u32;

    bus.write_u8(sa.wrapping_add(0x08), (idx + 1) as u8);
    bus.write_u8(sa.wrapping_add(0x0B), period as u8);
    bus.write_u32_aligned(sa.wrapping_add(0x10), spv as u32);
    bus.write_u32_aligned(sa.wrapping_add(0x14), pcm_freq as u32);
    bus.write_u32_aligned(sa.wrapping_add(0x18), div_freq);

    // Configure Timer 0 reload = 65536 - (280896 / samples_per_vblank)
    let timer_period = (280_896 / spv) as u32;
    let reload = (0x10000u32 - timer_period) as u16;
    bus.write_u16_aligned(0x0400_0100, reload);
}

fn swi_sound_driver_init(bus: &mut GbaBus, sa: u32) {
    if sa < 0x0200_0000 {
        return;
    }
    // Store SoundArea pointer at [0x03007FF0]
    bus.write_u32_aligned(0x0300_7FF0, sa);

    // Clear channels & PCM ring buffer
    for off in (0..0x350 + PCM_DMA_BUF_SIZE * 2).step_by(4) {
        bus.write_u32_aligned(sa.wrapping_add(off), 0);
    }

    bus.write_u8(sa.wrapping_add(0x04), 1); // pcmDmaCounter
    bus.write_u8(sa.wrapping_add(0x05), 0); // reverb
    bus.write_u8(sa.wrapping_add(0x06), 8); // maxChans default = 8
    bus.write_u8(sa.wrapping_add(0x07), 15); // masterVol default = 15
    bus.write_u8(sa.wrapping_add(0x09), 0); // mode
    bus.write_u32_aligned(sa.wrapping_add(0x24), BIOS_DEFAULT_MIXER_PC); // default intp mixer

    // Enable Master Sound (`SOUNDCNT_X` = 0x80), DirectSound A Left + B Right 100% on Timer 0 (`0x9A0C` -> DSA Left 0x0200, DSB Right 0x1000, resets 0x8800, 100% 0x000C)
    bus.write_u16_aligned(0x0400_0084, 0x0080);
    bus.write_u16_aligned(0x0400_0082, 0x9A0E);
    bus.write_u16_aligned(0x0400_0088, 0x0200);

    apply_sound_freq(bus, sa, 4); // Default freq 4 = 13379 Hz

    // Configure DMA1 -> FIFO_A (Left pcmBuffer) & DMA2 -> FIFO_B (Right pcmBuffer)
    let left_buf = sa.wrapping_add(0x350);
    let right_buf = left_buf.wrapping_add(PCM_DMA_BUF_SIZE);

    bus.write_u16_aligned(0x0400_00C6, 0x0400);
    bus.write_u32_aligned(0x0400_00BC, left_buf);
    bus.write_u32_aligned(0x0400_00C0, 0x0400_00A0);
    bus.write_u16_aligned(0x0400_00C4, 4);
    bus.write_u16_aligned(0x0400_00C6, 0xB640);

    bus.write_u16_aligned(0x0400_00D2, 0x0400);
    bus.write_u32_aligned(0x0400_00C8, right_buf);
    bus.write_u32_aligned(0x0400_00CC, 0x0400_00A4);
    bus.write_u16_aligned(0x0400_00D0, 4);
    bus.write_u16_aligned(0x0400_00D2, 0xB640);

    // Start Timer 0 (`TM0CNT_H = 0x0080`)
    bus.write_u16_aligned(0x0400_0102, 0x0000);
    bus.write_u16_aligned(0x0400_0102, 0x0080);

    bus.write_u32_aligned(sa, SOUND_IDENT);
}

fn swi_sound_driver_mode(bus: &mut GbaBus, mode: u32) {
    let sa = bus.read_u32_aligned(0x0300_7FF0);
    if sa < 0x0200_0000 || bus.read_u32_aligned(sa) != SOUND_IDENT {
        return;
    }

    if (mode & 0x0000_0080) != 0 {
        bus.write_u8(sa.wrapping_add(0x05), (mode & 0x7F) as u8);
    }
    let max_ch = ((mode >> 8) & 0x0F) as u8;
    if (1..=12).contains(&max_ch) {
        bus.write_u8(sa.wrapping_add(0x06), max_ch);
    }
    let mas_vol = ((mode >> 12) & 0x0F) as u8;
    if mas_vol != 0 {
        bus.write_u8(sa.wrapping_add(0x07), mas_vol);
    }
    let freq_idx = ((mode >> 16) & 0x0F) as u8;
    if (1..=12).contains(&freq_idx) {
        apply_sound_freq(bus, sa, freq_idx);
        // Restart Timer 0 with updated reload
        bus.write_u16_aligned(0x0400_0102, 0x0000);
        bus.write_u16_aligned(0x0400_0102, 0x0080);
    }
    let dac_bits = ((mode >> 20) & 0x0F) as u16;
    if (8..=11).contains(&dac_bits) {
        let bias_mode = (11 - dac_bits) << 14;
        let cur_bias = bus.read_u16_aligned(0x0400_0088);
        bus.write_u16_aligned(0x0400_0088, (cur_bias & 0x3FFF) | bias_mode);
    }
}

fn swi_sound_driver_vsync(bus: &mut GbaBus) {
    let sa = bus.read_u32_aligned(0x0300_7FF0);
    if sa < 0x0200_0000 {
        return;
    }
    let ident = bus.read_u32_aligned(sa);
    if ident.wrapping_sub(SOUND_IDENT) > 1 {
        return;
    }

    let counter = (bus.read_u8(sa.wrapping_add(0x04)) as i8).wrapping_sub(1);
    bus.write_u8(sa.wrapping_add(0x04), counter as u8);
    if counter > 0 {
        return;
    }

    let period = bus.read_u8(sa.wrapping_add(0x0B)).max(1);
    bus.write_u8(sa.wrapping_add(0x04), period);

    // Stop and restart DMA1 & DMA2 so internal_src reloads SAD (start of pcmBuffer)
    bus.write_u16_aligned(0x0400_00C6, 0x0400);
    bus.write_u16_aligned(0x0400_00D2, 0x0400);
    bus.write_u16_aligned(0x0400_00C6, 0xB640);
    bus.write_u16_aligned(0x0400_00D2, 0xB640);
}

fn swi_sound_driver_vsync_off(bus: &mut GbaBus) {
    let sa = bus.read_u32_aligned(0x0300_7FF0);
    if sa >= 0x0200_0000 && bus.read_u32_aligned(sa).wrapping_sub(SOUND_IDENT) <= 1 {
        bus.write_u32_aligned(sa, SOUND_IDENT + 2);
        bus.write_u16_aligned(0x0400_0102, 0x0000);
        bus.write_u16_aligned(0x0400_00C6, 0x0400);
        bus.write_u16_aligned(0x0400_00D2, 0x0400);
        let sh = bus.read_u16_aligned(0x0400_0082);
        bus.write_u16_aligned(0x0400_0082, sh | 0x8800);
    }
}

fn swi_sound_driver_vsync_on(bus: &mut GbaBus) {
    let sa = bus.read_u32_aligned(0x0300_7FF0);
    if sa >= 0x0200_0000 && bus.read_u32_aligned(sa) == SOUND_IDENT + 2 {
        bus.write_u8(sa.wrapping_add(0x04), 1);
        bus.write_u32_aligned(sa, SOUND_IDENT);
        bus.write_u16_aligned(0x0400_0102, 0x0080);
    }
}

fn swi_sound_channel_clear(bus: &mut GbaBus) {
    let sa = bus.read_u32_aligned(0x0300_7FF0);
    if sa < 0x0200_0000 {
        return;
    }
    for i in 0..12u32 {
        let ch_addr = sa.wrapping_add(0x50 + i * 0x40);
        bus.write_u8(ch_addr, 0);
    }
    let left_buf = sa.wrapping_add(0x350);
    for off in (0..PCM_DMA_BUF_SIZE * 2).step_by(4) {
        bus.write_u32_aligned(left_buf.wrapping_add(off), 0);
    }
}

fn enter_sound_driver_main(cpu: &mut Arm7Tdmi, bus: &mut GbaBus) {
    let sa = bus.read_u32_aligned(0x0300_7FF0);
    if sa < 0x0200_0000 || bus.read_u32_aligned(sa) != SOUND_IDENT {
        return;
    }
    bus.write_u32_aligned(sa, SOUND_IDENT + 1);

    let func = bus.read_u32_aligned(sa.wrapping_add(0x20));
    let intp = bus.read_u32_aligned(sa.wrapping_add(0x24));

    let return_pc = cpu.pc;
    let saved_cpsr = cpu.cpsr;
    let sp_svc = cpu.sp_svc().wrapping_sub(16);
    cpu.set_sp_svc(sp_svc);
    bus.write_u32_aligned(sp_svc, sa);
    bus.write_u32_aligned(sp_svc.wrapping_add(4), intp);
    bus.write_u32_aligned(sp_svc.wrapping_add(8), return_pc);
    bus.write_u32_aligned(sp_svc.wrapping_add(12), saved_cpsr);

    if func != 0 {
        cpu.regs[14] = BIOS_SOUND_AFTER_FUNC_PC;
        cpu.branch_exchange(func);
    } else {
        continue_sound_driver_after_func(cpu, bus);
    }
}

fn continue_sound_driver_after_func(cpu: &mut Arm7Tdmi, bus: &mut GbaBus) {
    let sp_svc = cpu.sp_svc();
    let sa = bus.read_u32_aligned(sp_svc);
    let intp = bus.read_u32_aligned(sp_svc.wrapping_add(4));

    if intp != 0 {
        cpu.regs[0] = sa;
        cpu.regs[14] = BIOS_SOUND_DONE_PC;
        cpu.branch_exchange(intp);
    } else {
        finish_sound_driver_main(cpu, bus);
    }
}

fn finish_sound_driver_main(cpu: &mut Arm7Tdmi, bus: &mut GbaBus) {
    let sp_svc = cpu.sp_svc();
    let sa = bus.read_u32_aligned(sp_svc);
    let return_pc = bus.read_u32_aligned(sp_svc.wrapping_add(8));
    let saved_cpsr = bus.read_u32_aligned(sp_svc.wrapping_add(12));
    cpu.set_sp_svc(sp_svc.wrapping_add(16));

    if sa >= 0x0200_0000 {
        bus.write_u32_aligned(sa, SOUND_IDENT);
    }
    cpu.set_cpsr(saved_cpsr);
    cpu.pc = return_pc;
}

/// Fallback GBA BIOS virtual DirectSound mixer (used when a game leaves `sa->intp` at BIOS default).
fn bios_default_sound_mixer(bus: &mut GbaBus, sa: u32) {
    if sa < 0x0200_0000 {
        return;
    }
    let dma_counter = bus.read_u8(sa.wrapping_add(0x04));
    let reverb = bus.read_u8(sa.wrapping_add(0x05)) & 0x7F;
    let max_chans = bus.read_u8(sa.wrapping_add(0x06)).clamp(1, 12) as u32;
    let master_vol = (bus.read_u8(sa.wrapping_add(0x07)) & 0x0F) as i32;
    let period = bus.read_u8(sa.wrapping_add(0x0B)).max(1);
    let spv = bus.read_u32_aligned(sa.wrapping_add(0x10)).clamp(16, 1584) as usize;
    let div_freq = bus.read_u32_aligned(sa.wrapping_add(0x18)) as u64;

    let slice_idx = if dma_counter > 1 && period >= dma_counter {
        (period - dma_counter + 1) as usize
    } else {
        0
    };
    let slice_off = (slice_idx * spv) % (PCM_DMA_BUF_SIZE as usize);
    let left_base = sa.wrapping_add(0x350);
    let right_base = left_base.wrapping_add(PCM_DMA_BUF_SIZE);

    let mut mix_l = vec![0i32; spv];
    let mut mix_r = vec![0i32; spv];

    if reverb > 0 {
        let prev_slice = if slice_idx == 0 {
            period.saturating_sub(1) as usize
        } else {
            slice_idx - 1
        };
        let prev_off = (prev_slice * spv) % (PCM_DMA_BUF_SIZE as usize);
        for s in 0..spv {
            let pl = bus.read_u8(left_base.wrapping_add((prev_off + s) as u32)) as i8 as i32;
            let pr = bus.read_u8(right_base.wrapping_add((prev_off + s) as u32)) as i8 as i32;
            let rev = ((pl + pr) * (reverb as i32)) >> 8;
            mix_l[s] = rev;
            mix_r[s] = rev;
        }
    }

    for ch in 0..max_chans {
        let ch_addr = sa.wrapping_add(0x50 + ch * 0x40);
        let mut status = bus.read_u8(ch_addr);
        if (status & 0xC7) == 0 {
            continue;
        }
        let r_vol = bus.read_u8(ch_addr.wrapping_add(0x02)) as i32;
        let l_vol = bus.read_u8(ch_addr.wrapping_add(0x03)) as i32;
        let atk = bus.read_u8(ch_addr.wrapping_add(0x04));
        let dec = bus.read_u8(ch_addr.wrapping_add(0x05));
        let sus = bus.read_u8(ch_addr.wrapping_add(0x06));
        let rel = bus.read_u8(ch_addr.wrapping_add(0x07));
        let mut env = bus.read_u8(ch_addr.wrapping_add(0x09));
        let fr = bus.read_u32_aligned(ch_addr.wrapping_add(0x20)) as u64;
        let wp = bus.read_u32_aligned(ch_addr.wrapping_add(0x24));
        if wp < 0x0200_0000 {
            bus.write_u8(ch_addr, 0);
            continue;
        }

        let wave_stat = bus.read_u16_aligned(wp.wrapping_add(0x02));
        let loop_start = bus.read_u32_aligned(wp.wrapping_add(0x08));
        let wave_size = bus.read_u32_aligned(wp.wrapping_add(0x0C));
        let is_loop = (wave_stat & 0xC000) != 0 || (status & 0x10) != 0;

        let mut cur_ptr = bus.read_u32_aligned(ch_addr.wrapping_add(0x28));
        let mut rem_count = bus.read_u32_aligned(ch_addr.wrapping_add(0x2C));
        let mut fw = bus.read_u32_aligned(ch_addr.wrapping_add(0x1C));

        if (status & 0x80) != 0 {
            // Key-on initialization
            status = if is_loop { 0x13 } else { 0x03 };
            env = 0;
            fw = 0;
            cur_ptr = wp.wrapping_add(0x10);
            rem_count = wave_size;
        }

        if (status & 0x40) != 0 {
            // Release phase
            env = ((env as u32 * rel as u32) >> 8) as u8;
            if env == 0 {
                bus.write_u8(ch_addr, 0);
                continue;
            }
        } else {
            match status & 0x07 {
                3 => {
                    let next_env = (env as u16) + (atk as u16);
                    if next_env >= 255 {
                        env = 255;
                        status = (status & !0x07) | 2;
                    } else {
                        env = next_env as u8;
                    }
                }
                2 => {
                    let decayed = ((env as u32 * dec as u32) >> 8) as u8;
                    if decayed <= sus {
                        env = sus;
                        status = (status & !0x07) | 1;
                    } else {
                        env = decayed;
                    }
                }
                _ => {}
            }
        }

        if rem_count == 0 {
            bus.write_u8(ch_addr, 0);
            continue;
        }

        let step_fp23 = ((fr.saturating_mul(div_freq)) >> 9) as u32;
        let gain_l = (l_vol * (env as i32) * (master_vol + 1)) >> 12;
        let gain_r = (r_vol * (env as i32) * (master_vol + 1)) >> 12;

        let loop_ptr = wp.wrapping_add(0x10).wrapping_add(loop_start);
        let loop_len = wave_size.saturating_sub(loop_start).max(1);

        let mut stopped = false;
        for s in 0..spv {
            let s0 = bus.read_u8(cur_ptr) as i8 as i32;
            let s1 = if rem_count > 1 {
                bus.read_u8(cur_ptr.wrapping_add(1)) as i8 as i32
            } else if is_loop {
                bus.read_u8(loop_ptr) as i8 as i32
            } else {
                0
            };
            let frac = (fw >> 15) as i32; // 8-bit linear interpolation
            let interp = s0 + (((s1 - s0) * frac) >> 8);
            mix_l[s] += (interp * gain_l) >> 8;
            mix_r[s] += (interp * gain_r) >> 8;

            fw = fw.wrapping_add(step_fp23);
            let adv = fw >> 23;
            fw &= 0x007F_FFFF;
            if adv > 0 {
                if adv < rem_count {
                    cur_ptr = cur_ptr.wrapping_add(adv);
                    rem_count -= adv;
                } else if is_loop {
                    let over = (adv - rem_count) % loop_len;
                    cur_ptr = loop_ptr.wrapping_add(over);
                    rem_count = loop_len - over;
                } else {
                    stopped = true;
                    break;
                }
            }
        }

        if stopped {
            bus.write_u8(ch_addr, 0);
        } else {
            bus.write_u8(ch_addr, status);
            bus.write_u8(ch_addr.wrapping_add(0x09), env);
            bus.write_u32_aligned(ch_addr.wrapping_add(0x1C), fw);
            bus.write_u32_aligned(ch_addr.wrapping_add(0x28), cur_ptr);
            bus.write_u32_aligned(ch_addr.wrapping_add(0x2C), rem_count);
        }
    }

    for s in 0..spv {
        let sl = mix_l[s].clamp(-128, 127) as i8 as u8;
        let sr = mix_r[s].clamp(-128, 127) as i8 as u8;
        bus.write_u8(left_base.wrapping_add((slice_off + s) as u32), sl);
        bus.write_u8(right_base.wrapping_add((slice_off + s) as u32), sr);
    }
}
