//! GSF / MiniGSF (`.gsf`, `.minigsf` + `.gsflib`) decoder & GBA Sound Engine.

pub mod apu;
pub mod bios;
pub mod bus;
pub mod cpu;

use std::path::Path;
use std::sync::Mutex;

use crate::psf::{
    infer_track_and_title_from_path, resolve_sibling_path, PsfFile, PsfTags, PSF_VERSION_GSF,
};
use apu::OUTPUT_SAMPLE_RATE;
use bus::GbaBus;
use cpu::Arm7Tdmi;

const MAX_ROM_SIZE: usize = 32 * 1024 * 1024;
const MAX_LIB_DEPTH: usize = 10;

#[derive(Clone)]
pub struct GsfImage {
    pub entry_point: u32,
    pub rom: Vec<u8>,
    pub ewram_init: Option<Vec<u8>>,
    pub tags: PsfTags,
}

impl GsfImage {
    fn empty() -> Self {
        Self {
            entry_point: 0,
            rom: Vec::new(),
            ewram_init: None,
            tags: PsfTags::default(),
        }
    }

    fn apply_program(&mut self, program: &[u8]) -> Result<(), &'static str> {
        if program.is_empty() {
            return Ok(());
        }
        if program.len() < 12 {
            return Err("GSF program header shorter than 12 bytes");
        }
        let entry = u32::from_le_bytes([program[0], program[1], program[2], program[3]]);
        let offset = u32::from_le_bytes([program[4], program[5], program[6], program[7]]);
        let decl_size =
            u32::from_le_bytes([program[8], program[9], program[10], program[11]]) as usize;
        let payload = &program[12..];
        let copy_len = decl_size.min(payload.len());

        if entry != 0 {
            self.entry_point = entry;
        }

        let region = offset >> 24;
        if region == 0x02 {
            // Multiboot EWRAM (0x02000000..0x0203FFFF)
            let dst_off = (offset as usize) & 0x0003_FFFF;
            let end = (dst_off + copy_len).min(0x40000);
            let actual = end.saturating_sub(dst_off);
            let ewram = self.ewram_init.get_or_insert_with(|| vec![0u8; 0x40000]);
            if actual > 0 {
                ewram[dst_off..dst_off + actual].copy_from_slice(&payload[..actual]);
            }
        } else {
            // Game Pak ROM (0x08000000..0x09FFFFFF, mirrors 0x0A..0x0D, or raw 0x00xxxxxx fallback)
            let dst_off = (offset as usize) & 0x01FF_FFFF;
            if dst_off >= MAX_ROM_SIZE {
                return Err("GSF ROM offset out of range");
            }
            let end = (dst_off + copy_len).min(MAX_ROM_SIZE);
            let actual = end.saturating_sub(dst_off);
            if self.rom.len() < end {
                self.rom.resize(end, 0xFF);
            }
            if actual > 0 {
                self.rom[dst_off..dst_off + actual].copy_from_slice(&payload[..actual]);
            }
        }

        Ok(())
    }

    fn effective_entry_point(&self) -> u32 {
        if self.entry_point != 0 {
            self.entry_point
        } else if self.rom.is_empty() && self.ewram_init.is_some() {
            0x0200_0000
        } else {
            0x0800_0000
        }
    }
}

struct CachedLib {
    key: String,
    image: GsfImage,
}

static GSFLIB_CACHE: Mutex<Option<CachedLib>> = Mutex::new(None);

/// Load a `.gsf` / `.minigsf` image from bytes + a library loader closure `load_lib(rel_name) -> Option<(cache_key, bytes)>`.
pub fn load_gsf_image_with<F>(
    main_bytes: &[u8],
    track_hint_path: Option<&str>,
    load_lib: &mut F,
) -> Result<GsfImage, &'static str>
where
    F: FnMut(&str) -> Option<(String, Vec<u8>)>,
{
    fn load_recursive<F>(
        bytes: &[u8],
        cache_key: Option<&str>,
        depth: usize,
        load_lib: &mut F,
    ) -> Result<GsfImage, &'static str>
    where
        F: FnMut(&str) -> Option<(String, Vec<u8>)>,
    {
        if depth > MAX_LIB_DEPTH {
            return Err("GSF _lib recursion depth exceeded");
        }

        if let Some(key) = cache_key {
            if let Ok(guard) = GSFLIB_CACHE.lock() {
                if let Some(cached) = guard.as_ref() {
                    if cached.key == key {
                        return Ok(cached.image.clone());
                    }
                }
            }
        }

        let psf = PsfFile::parse(bytes, Some(PSF_VERSION_GSF))?;
        let mut img = if let Some(ref lib_name) = psf.tags.lib {
            let (sub_key, lib_bytes) = load_lib(lib_name).ok_or("Missing required .gsflib file")?;
            load_recursive(&lib_bytes, Some(&sub_key), depth + 1, load_lib)?
        } else {
            GsfImage::empty()
        };

        let parent_tags = img.tags.clone();
        img.apply_program(&psf.program)?;

        let mut merged_tags = psf.tags;
        merged_tags.merge_fallback_from(&parent_tags);

        // Apply secondary overlay libraries `_lib2`, `_lib3`, ...
        for extra_lib in merged_tags.extra_libs() {
            let (sub_key, extra_bytes) =
                load_lib(&extra_lib).ok_or("Missing secondary _libN .gsflib file")?;
            let extra_img = load_recursive(&extra_bytes, Some(&sub_key), depth + 1, load_lib)?;
            if extra_img.entry_point != 0 {
                img.entry_point = extra_img.entry_point;
            }
            if !extra_img.rom.is_empty() {
                // Re-parse extra PsfFile directly so only its delta slice overwrites img
                let extra_psf = PsfFile::parse(&extra_bytes, Some(PSF_VERSION_GSF))?;
                img.apply_program(&extra_psf.program)?;
            } else if extra_img.ewram_init.is_some() {
                let extra_psf = PsfFile::parse(&extra_bytes, Some(PSF_VERSION_GSF))?;
                img.apply_program(&extra_psf.program)?;
            }
        }

        img.tags = merged_tags;

        // Cache base `.gsflib` images so consecutive `.minigsf` tracks in an album load in <1ms!
        if let Some(key) = cache_key {
            if let Ok(mut guard) = GSFLIB_CACHE.lock() {
                *guard = Some(CachedLib {
                    key: key.to_string(),
                    image: img.clone(),
                });
            }
        }

        Ok(img)
    }

    let mut img = load_recursive(main_bytes, None, 0, load_lib)?;
    if let Some(path) = track_hint_path {
        let (inferred_track, inferred_title) = infer_track_and_title_from_path(path);
        if img.tags.track_num.is_none() {
            img.tags.track_num = inferred_track;
        }
        if img.tags.title.is_none() {
            img.tags.title = inferred_title;
        }
    }
    Ok(img)
}

/// Load a `.gsf` / `.minigsf` image from disk (resolving sibling `.gsflib` files case-insensitively).
pub fn load_gsf_from_disk(main_bytes: &[u8], track_path: &str) -> Result<GsfImage, &'static str> {
    let base_dir = Path::new(track_path)
        .parent()
        .unwrap_or_else(|| Path::new("."));

    load_gsf_image_with(main_bytes, Some(track_path), &mut |lib_rel| {
        let resolved = resolve_sibling_path(base_dir, lib_rel)?;
        let data = std::fs::read(&resolved).ok()?;
        let key = format!(
            "{}:{}",
            resolved.to_string_lossy(),
            data.len()
        );
        Some((key, data))
    })
}

pub struct GsfDecoder {
    image: GsfImage,
    cpu: Arm7Tdmi,
    bus: GbaBus,

    play_frames: u64,
    fade_frames: u64,
    total_frames: u64,
    rendered_frames: u64,
    base_gain: f32,
}

impl GsfDecoder {
    pub fn from_image(image: GsfImage) -> Self {
        let (play_ms, fade_ms) = image.tags.effective_timing_ms();
        let play_frames = ((play_ms as u64) * (OUTPUT_SAMPLE_RATE as u64)) / 1000;
        let fade_frames = ((fade_ms as u64) * (OUTPUT_SAMPLE_RATE as u64)) / 1000;
        let total_frames = play_frames.saturating_add(fade_frames).max(1);
        let base_gain = image.tags.volume.unwrap_or(1.0);

        let entry = image.effective_entry_point();
        let cpu = Arm7Tdmi::new(entry);
        let bus = GbaBus::new(image.rom.clone(), image.ewram_init.as_deref());

        let mut dec = Self {
            image,
            cpu,
            bus,
            play_frames,
            fade_frames,
            total_frames,
            rendered_frames: 0,
            base_gain,
        };
        dec.skip_leading_silence();
        dec
    }

    pub fn tags(&self) -> &PsfTags {
        &self.image.tags
    }

    pub fn duration_ms(&self) -> u32 {
        self.image.tags.total_duration_ms()
    }

    pub fn elapsed_ms(&self) -> u32 {
        ((self.rendered_frames * 1000) / (OUTPUT_SAMPLE_RATE as u64)) as u32
    }

    fn reset_vm(&mut self) {
        let entry = self.image.effective_entry_point();
        self.cpu = Arm7Tdmi::new(entry);
        self.bus = GbaBus::new(self.image.rom.clone(), self.image.ewram_init.as_deref());
        self.rendered_frames = 0;
        self.skip_leading_silence();
    }

    /// Skip up to 1.5s of initial GBA boot / driver init silence so music begins immediately.
    fn skip_leading_silence(&mut self) {
        let max_skip_frames = (OUTPUT_SAMPLE_RATE as usize) * 3 / 2;
        let mut skipped = 0usize;

        while skipped < max_skip_frames {
            self.step_emulator_slice(true);
            let samples = &self.bus.apu.out_samples;
            if samples.len() >= 2 {
                let mut first_audible_pair = None;
                for pair_idx in 0..(samples.len() / 2) {
                    let l = samples[pair_idx * 2].saturating_abs();
                    let r = samples[pair_idx * 2 + 1].saturating_abs();
                    if l > 8 || r > 8 {
                        first_audible_pair = Some(pair_idx);
                        break;
                    }
                }
                if let Some(idx) = first_audible_pair {
                    self.bus.apu.out_samples.drain(0..idx * 2);
                    return;
                }
                skipped += samples.len() / 2;
                self.bus.apu.out_samples.clear();
            }
        }
    }

    #[inline]
    fn step_emulator_slice(&mut self, emit_audio: bool) {
        if self.bus.halted {
            if self.bus.irq_pending() {
                self.bus.halted = false;
            } else {
                let skip = self.bus.cycles_until_next_event(2048);
                self.bus.advance_cycles(skip, emit_audio, self.base_gain);
                return;
            }
        }

        let mut burst_cycles = 0u32;
        while !self.bus.halted && burst_cycles < 32 {
            burst_cycles += self.cpu.step_instruction(&mut self.bus);
            if self.bus.cpu_should_take_irq() {
                break;
            }
        }
        self.bus
            .advance_cycles(burst_cycles.max(1), emit_audio, self.base_gain);
    }

    /// Seek toward `target_ms` by at most `max_step_ms` milliseconds in one call
    /// so cooperative Rockbox threads can `yield()` between slices on long seeks.
    /// Returns `true` once `rendered_frames` has reached `target_ms`.
    pub fn seek_step_ms(&mut self, target_ms: u32, max_step_ms: u32) -> bool {
        let target_frames = ((target_ms as u64) * (OUTPUT_SAMPLE_RATE as u64)) / 1000;
        if target_frames < self.rendered_frames {
            self.reset_vm();
        }
        self.bus.apu.out_samples.clear();

        let frames_remaining = target_frames.saturating_sub(self.rendered_frames);
        if frames_remaining == 0 {
            return true;
        }

        let max_step_frames =
            (((max_step_ms.max(100) as u64) * (OUTPUT_SAMPLE_RATE as u64)) / 1000).max(1);
        let step_frames = frames_remaining.min(max_step_frames);

        let cycles_to_skip =
            (step_frames as u128 * (apu::GBA_CPU_CLOCK as u128) / (OUTPUT_SAMPLE_RATE as u128))
                as u64;
        let goal_cycles = self.bus.total_cycles.saturating_add(cycles_to_skip);
        while self.bus.total_cycles < goal_cycles {
            self.step_emulator_slice(false);
        }
        self.rendered_frames += step_frames;
        self.rendered_frames >= target_frames
    }

    /// Seek to `target_ms` milliseconds from the start of the track.
    pub fn seek_ms(&mut self, target_ms: u32) {
        while !self.seek_step_ms(target_ms, 60_000) {}
    }

    /// Render up to `out_interleaved.len() / 2` stereo S16 frames at 44,100 Hz.
    /// Returns the number of stereo frames written (`0` = end of track).
    pub fn render_stereo_s16(&mut self, out_interleaved: &mut [i16], loop_forever: bool) -> usize {
        let max_frames = out_interleaved.len() / 2;
        if max_frames == 0 {
            return 0;
        }
        if !loop_forever && self.rendered_frames >= self.total_frames {
            return 0;
        }

        let target_frames = if loop_forever {
            max_frames
        } else {
            max_frames.min((self.total_frames - self.rendered_frames) as usize)
        };

        while self.bus.apu.out_samples.len() < target_frames * 2 {
            self.step_emulator_slice(true);
        }

        for f in 0..target_frames {
            let mut l = self.bus.apu.out_samples[f * 2];
            let mut r = self.bus.apu.out_samples[f * 2 + 1];

            if !loop_forever && self.rendered_frames >= self.play_frames && self.fade_frames > 0 {
                let fade_pos = self.rendered_frames - self.play_frames;
                let rem = self.fade_frames.saturating_sub(fade_pos) as f32;
                let lin = (rem / (self.fade_frames as f32)).clamp(0.0, 1.0);
                let gain = lin * lin;
                l = ((l as f32) * gain).round() as i16;
                r = ((r as f32) * gain).round() as i16;
            }

            out_interleaved[f * 2] = l;
            out_interleaved[f * 2 + 1] = r;
            self.rendered_frames += 1;
        }

        self.bus.apu.out_samples.drain(0..target_frames * 2);
        target_frames
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use flate2::write::ZlibEncoder;
    use flate2::Compression;
    use std::io::Write;

    fn build_psf(version: u8, program: &[u8], tags: &str) -> Vec<u8> {
        let mut comp = Vec::new();
        if !program.is_empty() {
            let mut enc = ZlibEncoder::new(&mut comp, Compression::default());
            enc.write_all(program).unwrap();
            enc.finish().unwrap();
        }
        let crc = crc32fast::hash(&comp);
        let mut out = Vec::new();
        out.extend_from_slice(b"PSF");
        out.push(version);
        out.extend_from_slice(&0u32.to_le_bytes());
        out.extend_from_slice(&(comp.len() as u32).to_le_bytes());
        out.extend_from_slice(&crc.to_le_bytes());
        out.extend_from_slice(&comp);
        if !tags.is_empty() {
            out.extend_from_slice(b"[TAG]");
            out.extend_from_slice(tags.as_bytes());
        }
        out
    }

    fn build_gsf_program(entry: u32, offset: u32, payload: &[u8]) -> Vec<u8> {
        let mut p = Vec::with_capacity(12 + payload.len());
        p.extend_from_slice(&entry.to_le_bytes());
        p.extend_from_slice(&offset.to_le_bytes());
        p.extend_from_slice(&(payload.len() as u32).to_le_bytes());
        p.extend_from_slice(payload);
        p
    }

    #[test]
    fn test_psf_tags_and_filename_inference() {
        let prog = build_gsf_program(0x0800_0000, 0x0800_0100, &[0x01]);
        let gsf = build_psf(
            PSF_VERSION_GSF,
            &prog,
            "title=BGM #01\ngame=Ace Combat Advance\nartist=Namco\nyear=2005\nlength=1:32.5\nfade=8\n_lib=AGB-BAEE-USA.gsflib\n",
        );

        let tags = PsfFile::parse_tags_from_slice(&gsf, Some(PSF_VERSION_GSF)).unwrap();
        assert_eq!(tags.title.as_deref(), Some("BGM #01"));
        assert_eq!(tags.game.as_deref(), Some("Ace Combat Advance"));
        assert_eq!(tags.artist.as_deref(), Some("Namco"));
        assert_eq!(tags.year, Some(2005));
        assert_eq!(tags.length_ms, Some(92_500));
        assert_eq!(tags.fade_ms, Some(8_000));
        assert_eq!(tags.total_duration_ms(), 100_500);
        assert_eq!(tags.lib.as_deref(), Some("AGB-BAEE-USA.gsflib"));

        let (num, title) = infer_track_and_title_from_path(
            "/mnt/SDCARD/Music/Ace Combat Advance (Namco)[GBA]/01 BGM #01.minigsf",
        );
        assert_eq!(num, Some(1));
        assert_eq!(title.as_deref(), Some("BGM #01"));
    }

    #[test]
    fn test_midi_key_2_freq_accuracy() {
        let mut bus = GbaBus::new(vec![0; 256], None);
        let mut cpu = Arm7Tdmi::new(0x0800_0000);
        // WaveData at 0x02000000 with base freq = 13379 << 10 = 13,700,096 (MIDI key 60 = 13379 Hz)
        bus.write_u32_aligned(0x0200_0004, 13379 << 10);
        cpu.regs[0] = 0x0200_0000;
        cpu.regs[1] = 60;
        cpu.regs[2] = 0;
        bios::handle_swi(&mut cpu, &mut bus, 0x1F);
        assert_eq!(cpu.regs[0], 13379);

        // One octave higher (MIDI key 72) -> 2 * 13379 = 26758
        cpu.regs[0] = 0x0200_0000;
        cpu.regs[1] = 72;
        cpu.regs[2] = 0;
        bios::handle_swi(&mut cpu, &mut bus, 0x1F);
        assert_eq!(cpu.regs[0], 26758);
    }

    #[test]
    fn test_minigsf_plus_gsflib_bios_sounddriver_synthesis() {
        // Build a synthetic `.gsflib` containing real ARM7TDMI GBA code that:
        // 1) Calls SWI 0x1A SoundDriverInit(sa = 0x02000000)
        // 2) Configures virtual channel 0 with a looping sine/square WaveData in ROM,
        //    reading the MIDI key patched at 0x08000100 by the `.minigsf`!
        // 3) Installs a VBlank IRQ handler calling SWI 0x1D (SoundDriverVSync) + SWI 0x1C (SoundDriverMain)
        // 4) Loops on SWI 0x05 (VBlankIntrWait)!
        let mut rom = vec![0u8; 0x300];
        let put_arm = |buf: &mut [u8], off: usize, word: u32| {
            buf[off..off + 4].copy_from_slice(&word.to_le_bytes());
        };

        // Main entry at 0x08000000:
        put_arm(&mut rom, 0x00, 0xE3A0_0402); // MOV R0, #0x02000000 (SoundArea in EWRAM)
        put_arm(&mut rom, 0x04, 0xEF1A_0000); // SWI 0x1A (SoundDriverInit)
        // Set up WaveData pointer (0x08000200) & MIDI key from [0x08000100] -> MidiKey2Freq
        put_arm(&mut rom, 0x08, 0xE3A0_4408); // MOV R4, #0x08000000
        put_arm(&mut rom, 0x0C, 0xE284_0C02); // ADD R0, R4, #0x200   (WaveData = 0x08000200)
        put_arm(&mut rom, 0x10, 0xE5D4_1100); // LDRB R1, [R4, #0x100] (patched MIDI note!)
        put_arm(&mut rom, 0x14, 0xE3A0_2000); // MOV R2, #0
        put_arm(&mut rom, 0x18, 0xEF1F_0000); // SWI 0x1F (MidiKey2Freq -> R0 = fr)
        // Populate SoundArea->vchn[0] at 0x02000050
        put_arm(&mut rom, 0x1C, 0xE3A0_5402); // MOV R5, #0x02000000
        put_arm(&mut rom, 0x20, 0xE285_5050); // ADD R5, R5, #0x50
        put_arm(&mut rom, 0x24, 0xE585_0020); // STR R0, [R5, #0x20]  (fr)
        put_arm(&mut rom, 0x28, 0xE284_0C02); // ADD R0, R4, #0x200
        put_arm(&mut rom, 0x2C, 0xE585_0024); // STR R0, [R5, #0x24]  (wp = 0x08000200)
        put_arm(&mut rom, 0x30, 0xE3A0_00C0); // MOV R0, #192
        put_arm(&mut rom, 0x34, 0xE5C5_0002); // STRB R0, [R5, #2]    (RightVol)
        put_arm(&mut rom, 0x38, 0xE5C5_0003); // STRB R0, [R5, #3]    (LeftVol)
        put_arm(&mut rom, 0x3C, 0xE3A0_00FF); // MOV R0, #255
        put_arm(&mut rom, 0x40, 0xE5C5_0004); // STRB R0, [R5, #4]    (Attack = 255)
        put_arm(&mut rom, 0x44, 0xE5C5_0005); // STRB R0, [R5, #5]    (Decay = 255)
        put_arm(&mut rom, 0x48, 0xE5C5_0006); // STRB R0, [R5, #6]    (Sustain = 255)
        put_arm(&mut rom, 0x4C, 0xE3A0_0080); // MOV R0, #0x80        (Status = Key-On!)
        put_arm(&mut rom, 0x50, 0xE5C5_0000); // STRB R0, [R5, #0]
        // Install VBlank IRQ handler (0x08000080) at [0x03FFFFFC] and enable VBlank IRQ
        put_arm(&mut rom, 0x54, 0xE284_0080); // ADD R0, R4, #0x80    (handler = 0x08000080)
        put_arm(&mut rom, 0x58, 0xE3A0_6301); // MOV R6, #0x04000000
        put_arm(&mut rom, 0x5C, 0xE506_0004); // STR R0, [R6, #-4]    ([0x03FFFFFC])
        put_arm(&mut rom, 0x60, 0xE3A0_0008); // MOV R0, #8           (DISPSTAT VBlank IRQ enable)
        put_arm(&mut rom, 0x64, 0xE1C6_00B4); // STRH R0, [R6, #4]
        put_arm(&mut rom, 0x68, 0xE3A0_0001); // MOV R0, #1           (IE = IRQ_VBLANK, IME = 1)
        put_arm(&mut rom, 0x6C, 0xE286_7C02); // ADD R7, R6, #0x200
        put_arm(&mut rom, 0x70, 0xE1C7_00B0); // STRH R0, [R7, #0]    (REG_IE)
        put_arm(&mut rom, 0x74, 0xE1C7_00B8); // STRH R0, [R7, #8]    (REG_IME)
        // Loop forever on VBlankIntrWait (SWI 0x05):
        put_arm(&mut rom, 0x78, 0xEF05_0000); // SWI 0x05 (VBlankIntrWait)
        put_arm(&mut rom, 0x7C, 0xEAFF_FFFD); // B 0x78

        // VBlank IRQ handler at 0x08000080:
        put_arm(&mut rom, 0x80, 0xE92D_4010); // STMFD SP!, {R4, LR}
        put_arm(&mut rom, 0x84, 0xEF1D_0000); // SWI 0x1D (SoundDriverVSync)
        put_arm(&mut rom, 0x88, 0xEF1C_0000); // SWI 0x1C (SoundDriverMain)
        put_arm(&mut rom, 0x8C, 0xE3A0_0301); // MOV R0, #0x04000000
        put_arm(&mut rom, 0x90, 0xE3A0_1001); // MOV R1, #1
        put_arm(&mut rom, 0x94, 0xE280_2C02); // ADD R2, R0, #0x200
        put_arm(&mut rom, 0x98, 0xE1C2_10B2); // STRH R1, [R2, #2]    (ack REG_IF)
        put_arm(&mut rom, 0x9C, 0xE150_30B8); // LDRH R3, [R0, #-8]   ([0x03FFFFF8])
        put_arm(&mut rom, 0xA0, 0xE183_3001); // ORR R3, R3, R1
        put_arm(&mut rom, 0xA4, 0xE140_30B8); // STRH R3, [R0, #-8]
        put_arm(&mut rom, 0xA8, 0xE8BD_4010); // LDMFD SP!, {R4, LR}
        put_arm(&mut rom, 0xAC, 0xE12F_FF1E); // BX LR

        // Default note at 0x08000100 = 60 (overridden by .minigsf!)
        rom[0x100] = 60;

        // WaveData at 0x08000200: 32-sample looping waveform
        put_arm(&mut rom, 0x200, 0x4000_0000); // type=0, stat=0x4000 (loop!)
        put_arm(&mut rom, 0x204, 13379 << 10); // base freq at MIDI 60
        put_arm(&mut rom, 0x208, 0);           // loopStart = 0
        put_arm(&mut rom, 0x20C, 32);          // size = 32 samples
        for i in 0..32usize {
            let phase = (i as f32) * std::f32::consts::TAU / 32.0;
            rom[0x210 + i] = (phase.sin() * 100.0).round() as i8 as u8;
        }

        let gsflib_prog = build_gsf_program(0x0800_0000, 0x0800_0000, &rom);
        let gsflib_bytes = build_psf(
            PSF_VERSION_GSF,
            &gsflib_prog,
            "game=Ace Combat Advance\nartist=Namco\n",
        );

        // `.minigsf` patches just 1 byte at 0x08000100 (MIDI note 72) and references `_lib=test.gsflib`
        let minigsf_prog = build_gsf_program(0, 0x0800_0100, &[72]);
        let minigsf_bytes = build_psf(
            PSF_VERSION_GSF,
            &minigsf_prog,
            "title=BGM #01\nlength=0:01\nfade=0:00.2\n_lib=test.gsflib\n",
        );

        let img = load_gsf_image_with(
            &minigsf_bytes,
            Some("01 BGM #01.minigsf"),
            &mut |lib_name| {
                assert_eq!(lib_name, "test.gsflib");
                Some(("mem:test.gsflib".to_string(), gsflib_bytes.clone()))
            },
        )
        .expect("Failed to load minigsf + gsflib");

        assert_eq!(img.tags.title.as_deref(), Some("BGM #01"));
        assert_eq!(img.tags.game.as_deref(), Some("Ace Combat Advance"));
        assert_eq!(img.tags.artist.as_deref(), Some("Namco"));
        assert_eq!(img.tags.track_num, Some(1));
        assert_eq!(img.rom[0x100], 72);

        let mut dec = GsfDecoder::from_image(img);
        let mut pcm = vec![0i16; 4096 * 2];
        let frames = dec.render_stereo_s16(&mut pcm, false);
        assert_eq!(frames, 4096);

        let peak = pcm.iter().map(|s| s.saturating_abs()).max().unwrap_or(0);
        assert!(
            peak > 2000,
            "Expected strong synthesized GBA DirectSound waveform, got peak={peak}"
        );

        // Test backward & forward seeking
        dec.seek_ms(500);
        assert!((dec.elapsed_ms() as i32 - 500).abs() <= 5);
        dec.seek_ms(100);
        assert!((dec.elapsed_ms() as i32 - 100).abs() <= 5);
    }
}
