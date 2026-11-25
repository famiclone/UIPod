//! Pure-Rust NSF/NSFe chiptune codec, subtrack scanner, and real-time APU telemetry (`uipod-nsf`).
//!
//! Designed for zero-per-frame heap allocation across TrimUI Brick (`aarch64` Linux),
//! Android (`arm64-v8a`), and WebAssembly (`wasm32-unknown-unknown`).

pub mod apu;
pub mod blip;
pub mod cpu;
pub mod exp_mmc5;
pub mod exp_vrc6;
pub mod parser;
pub mod scanner;
pub mod uri;

use std::f64::consts::PI;
use std::fmt;

use blip::BlipBuf;
use cpu::NsfMachine;
use parser::ParsedNsf;
use scanner::{
    find_first_music_subtrack, LoopDetector, DEFAULT_MUSIC_DURATION_MS, DEFAULT_MUSIC_FADE_MS,
    LOOP_FADE_MS,
};

pub use scanner::{
    scan_nsf_container, scan_nsf_container_with_db, DbSubtrackTiming, TimingDbLookup,
};
pub use uri::{format_subtrack_uri, is_nsf_path, parse_subtrack_uri, SubtrackUri};

// ============================================================================
// Public Core Contracts (Phase 1 -> Phase 2 -> Phase 3)
// ============================================================================

#[repr(u8)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TrackKind {
    Music = 0,
    Sfx = 1,
}

/// Metadata for a single toggleable APU channel in the `Now Playing -> Channels` menu.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ChannelDescriptor {
    /// Bit position in `channel_mute_mask: u16` (`0..15`, `1 = muted`, `0 = enabled`).
    pub bit_index: u8,
    /// Compact 2-3 character code (`"P1"`, `"P2"`, `"TR"`, `"NS"`, `"DP"`, `"V1"`, `"V2"`, `"SAW"`).
    pub short_code: &'static str,
    /// Human-readable channel name (`"Pulse 1"`, `"Triangle"`, `"DPCM"`, `"VRC6 Sawtooth"`).
    pub display_name: &'static str,
}

const DESCRIPTORS_2A03: [ChannelDescriptor; 5] = [
    ChannelDescriptor {
        bit_index: 0,
        short_code: "P1",
        display_name: "Pulse 1",
    },
    ChannelDescriptor {
        bit_index: 1,
        short_code: "P2",
        display_name: "Pulse 2",
    },
    ChannelDescriptor {
        bit_index: 2,
        short_code: "TR",
        display_name: "Triangle",
    },
    ChannelDescriptor {
        bit_index: 3,
        short_code: "NS",
        display_name: "Noise",
    },
    ChannelDescriptor {
        bit_index: 4,
        short_code: "DP",
        display_name: "DPCM",
    },
];

const DESCRIPTORS_2A03_VRC6: [ChannelDescriptor; 8] = [
    DESCRIPTORS_2A03[0],
    DESCRIPTORS_2A03[1],
    DESCRIPTORS_2A03[2],
    DESCRIPTORS_2A03[3],
    DESCRIPTORS_2A03[4],
    ChannelDescriptor {
        bit_index: 5,
        short_code: "V1",
        display_name: "VRC6 Pulse 1",
    },
    ChannelDescriptor {
        bit_index: 6,
        short_code: "V2",
        display_name: "VRC6 Pulse 2",
    },
    ChannelDescriptor {
        bit_index: 7,
        short_code: "SAW",
        display_name: "VRC6 Sawtooth",
    },
];

const DESCRIPTORS_2A03_MMC5: [ChannelDescriptor; 8] = [
    DESCRIPTORS_2A03[0],
    DESCRIPTORS_2A03[1],
    DESCRIPTORS_2A03[2],
    DESCRIPTORS_2A03[3],
    DESCRIPTORS_2A03[4],
    ChannelDescriptor {
        bit_index: 5,
        short_code: "M1",
        display_name: "MMC5 Pulse 1",
    },
    ChannelDescriptor {
        bit_index: 6,
        short_code: "M2",
        display_name: "MMC5 Pulse 2",
    },
    ChannelDescriptor {
        bit_index: 7,
        short_code: "PCM",
        display_name: "MMC5 PCM",
    },
];

const DESCRIPTORS_2A03_VRC6_MMC5: [ChannelDescriptor; 11] = [
    DESCRIPTORS_2A03_VRC6[0],
    DESCRIPTORS_2A03_VRC6[1],
    DESCRIPTORS_2A03_VRC6[2],
    DESCRIPTORS_2A03_VRC6[3],
    DESCRIPTORS_2A03_VRC6[4],
    DESCRIPTORS_2A03_VRC6[5],
    DESCRIPTORS_2A03_VRC6[6],
    DESCRIPTORS_2A03_VRC6[7],
    ChannelDescriptor {
        bit_index: 8,
        short_code: "M1",
        display_name: "MMC5 Pulse 1",
    },
    ChannelDescriptor {
        bit_index: 9,
        short_code: "M2",
        display_name: "MMC5 Pulse 2",
    },
    ChannelDescriptor {
        bit_index: 10,
        short_code: "PCM",
        display_name: "MMC5 PCM",
    },
];

/// Bitflags matching NSF header byte `0x7B`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ChipFlags(pub u8);

impl ChipFlags {
    pub const VRC6: u8 = 1 << 0;
    pub const VRC7: u8 = 1 << 1;
    pub const FDS: u8 = 1 << 2;
    pub const MMC5: u8 = 1 << 3;
    pub const N163: u8 = 1 << 4;
    pub const S5B: u8 = 1 << 5;

    #[inline]
    pub const fn has_vrc6(self) -> bool {
        (self.0 & Self::VRC6) != 0
    }

    #[inline]
    pub const fn has_vrc7(self) -> bool {
        (self.0 & Self::VRC7) != 0
    }

    #[inline]
    pub const fn has_fds(self) -> bool {
        (self.0 & Self::FDS) != 0
    }

    #[inline]
    pub const fn has_mmc5(self) -> bool {
        (self.0 & Self::MMC5) != 0
    }

    #[inline]
    pub const fn has_n163(self) -> bool {
        (self.0 & Self::N163) != 0
    }

    #[inline]
    pub const fn has_s5b(self) -> bool {
        (self.0 & Self::S5B) != 0
    }

    /// Primary cartridge badge label (`"2A03"`, `"VRC6"`, `"VRC7"`, `"FDS"`, `"MMC5"`, `"N163"`, `"5B"`).
    pub fn primary_badge_label(self) -> &'static str {
        if self.has_vrc6() {
            "VRC6"
        } else if self.has_vrc7() {
            "VRC7"
        } else if self.has_fds() {
            "FDS"
        } else if self.has_mmc5() {
            "MMC5"
        } else if self.has_n163() {
            "N163"
        } else if self.has_s5b() {
            "5B"
        } else {
            "2A03"
        }
    }

    /// Returns the ordered list of hardware channels present for this file's chips
    /// (5 base 2A03 channels + any active synthesized expansion channels) for UI toggle menus.
    pub fn channel_descriptors(self) -> &'static [ChannelDescriptor] {
        match (self.has_vrc6(), self.has_mmc5()) {
            (true, true) => &DESCRIPTORS_2A03_VRC6_MMC5,
            (true, false) => &DESCRIPTORS_2A03_VRC6,
            (false, true) => &DESCRIPTORS_2A03_MMC5,
            (false, false) => &DESCRIPTORS_2A03,
        }
    }
}

#[derive(Clone, Debug)]
pub struct NsfContainerMeta {
    pub game_title: String,
    pub artist: String,
    pub copyright: String,
    pub total_subtracks: u16,
    pub starting_subtrack: u16, // 0-based
    pub is_pal: bool,
    pub chip_flags: ChipFlags,
    pub payload_crc32: u32,
    pub subtracks: Vec<SubtrackMeta>,
}

#[derive(Clone, Debug)]
pub struct SubtrackMeta {
    pub index: u16, // 0-based
    pub title: Option<String>,
    pub kind: TrackKind,
    pub duration_ms: u32,
    pub fade_ms: u32,
    pub loop_point_ms: Option<u32>,
}

/// Zero-allocation C-ABI channel state updated at 60/50 Hz for `VizEngine`, UI channel meters,
/// and `.uipod` native plugins.
///
/// ### Mute Semantics Contract
/// - `active` (`0` or `1`) reflects the **hardware oscillator state BEFORE `channel_mute_mask`**
///   so UI channel menus can still indicate incoming note activity on a muted channel.
/// - `muted` is `1` when the channel's bit is set in `channel_mute_mask` (`0` when audible).
/// - `volume` (`0.0..=1.0`) reflects **audible post-mute output level** (`0.0` when `muted == 1`
///   or `active == 0`), so audio visualizers and meters drop to zero immediately on mute.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct ChannelState {
    pub active: u8,
    pub muted: u8,
    pub onset_trigger: u8,
    pub duty_or_mode: u8,
    pub volume: f32,
    pub freq_hz: f32,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct ApuFrameTelemetry {
    pub channel_count: u8,
    pub chip_flags: u8,
    pub mute_mask: u16,
    pub channels: [ChannelState; 8],
}

/// Errors produced when loading or selecting subtracks in an `.nsf`/`.nsfe` container.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NsfError {
    TooShort,
    InvalidMagic,
    InvalidHeader(&'static str),
    MissingChunk(&'static str),
    SubtrackOutOfRange { requested: u16, total: u16 },
}

impl fmt::Display for NsfError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TooShort => write!(f, "NSF/NSFe file is truncated or too short"),
            Self::InvalidMagic => write!(f, "Invalid NSF/NSFe header magic"),
            Self::InvalidHeader(msg) => write!(f, "Invalid NSF/NSFe header: {msg}"),
            Self::MissingChunk(id) => write!(f, "Missing required NSFE chunk: {id}"),
            Self::SubtrackOutOfRange { requested, total } => {
                write!(f, "Subtrack index {requested} out of range (0..{total})")
            }
        }
    }
}

impl std::error::Error for NsfError {}

// ============================================================================
// High-Level Zero-Per-Frame-Allocation NSF Playback Engine (`NsfEngine`)
// ============================================================================

const STEREO_CHUNK_FRAMES: usize = 512;

/// Native pure-Rust NSF/NSFe synthesis and playback engine.
///
/// All RAM, 4 KB ROM banks, `BlipBuf` step buffers, and loop-history buffers are
/// pre-allocated at `NsfEngine::load()` time so the real-time audio render loop
/// performs zero heap allocations.
pub struct NsfEngine {
    parsed: ParsedNsf,
    meta: NsfContainerMeta,
    machine: NsfMachine,
    blip: BlipBuf,
    loop_detector: LoopDetector,
    sample_rate: u32,
    current_subtrack: u16,
    endless_loop: bool,
    frames_elapsed: u64,
    samples_rendered: u64,
    has_explicit_timing: bool,
    play_duration_ms: u32,
    fade_duration_ms: u32,
    loop_point_ms: Option<u32>,
    fade_started_at_ms: Option<u32>,
    finished: bool,
    last_telemetry: ApuFrameTelemetry,
}

impl NsfEngine {
    /// Load an `.nsf` or `.nsfe` image, build container metadata from header/NSFE chunks,
    /// and initialize the starting subtrack at `sample_rate` Hz (e.g. `44100` or `48000`).
    pub fn load(bytes: &[u8], sample_rate: u32) -> Result<Self, NsfError> {
        Self::load_with_meta(bytes, sample_rate, None)
    }

    /// Load an `.nsf` or `.nsfe` image using pre-scanned `NsfContainerMeta` if already available.
    pub fn load_with_meta(
        bytes: &[u8],
        sample_rate: u32,
        pre_scanned: Option<&NsfContainerMeta>,
    ) -> Result<Self, NsfError> {
        let parsed = ParsedNsf::parse(bytes)?;
        let safe_rate = sample_rate.clamp(8_000, 192_000);
        let meta = match pre_scanned {
            Some(m) if m.total_subtracks == parsed.total_subtracks => m.clone(),
            _ => build_fast_container_meta(&parsed),
        };

        let machine = NsfMachine::new(&parsed);
        let blip = BlipBuf::new(parsed.cpu_clock_hz(), safe_rate, 4096);
        let loop_detector = LoopDetector::new();

        let mut engine = Self {
            parsed,
            meta,
            machine,
            blip,
            loop_detector,
            sample_rate: safe_rate,
            current_subtrack: 0,
            endless_loop: false,
            frames_elapsed: 0,
            samples_rendered: 0,
            has_explicit_timing: false,
            play_duration_ms: DEFAULT_MUSIC_DURATION_MS,
            fade_duration_ms: DEFAULT_MUSIC_FADE_MS,
            loop_point_ms: None,
            fade_started_at_ms: None,
            finished: false,
            last_telemetry: ApuFrameTelemetry::default(),
        };

        let start_idx = engine.meta.starting_subtrack;
        engine.select_subtrack(start_idx)?;
        Ok(engine)
    }

    #[inline]
    pub fn container_meta(&self) -> &NsfContainerMeta {
        &self.meta
    }

    #[inline]
    pub fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    #[inline]
    pub fn frame_speed_us(&self) -> u32 {
        self.parsed.frame_speed_us() as u32
    }

    #[inline]
    pub fn current_subtrack(&self) -> u16 {
        self.current_subtrack
    }

    #[inline]
    pub fn channel_mute_mask(&self) -> u16 {
        self.machine.mute_mask
    }

    #[inline]
    pub fn channel_descriptors(&self) -> &'static [ChannelDescriptor] {
        self.parsed.chip_flags.channel_descriptors()
    }

    /// Find the first `TrackKind::Music` subtrack (`0..total_subtracks`), stopping at the first hit.
    pub fn first_music_subtrack(&self) -> u16 {
        find_first_music_subtrack(&self.parsed)
    }

    /// Update the live per-channel mute bitmask (`1 = muted`, `0 = enabled`).
    ///
    /// Silences selected channels smoothly at the voice mixer stage without DC pops while
    /// keeping all 6502 instructions, APU timers, length counters, sweeps, and DPCM DMA
    /// running identically in lockstep.
    pub fn set_channel_mute_mask(&mut self, mask: u16) {
        self.machine.set_mute_mask(mask);
        self.last_telemetry.mute_mask = mask;
        for i in 0..8 {
            let muted = (mask & (1 << i)) != 0;
            self.last_telemetry.channels[i].muted = u8::from(muted);
            if muted {
                self.last_telemetry.channels[i].volume = 0.0;
            }
        }
    }

    #[inline]
    pub fn endless_loop(&self) -> bool {
        self.endless_loop
    }

    /// Enable or disable endless looping (disables automatic 2-loop / duration fade-out and termination).
    pub fn set_endless_loop(&mut self, enabled: bool) {
        self.endless_loop = enabled;
        if enabled {
            self.fade_started_at_ms = None;
            self.finished = false;
        }
    }

    /// Native DJ Beatmatch tempo hook: scales the emulated 60/50 Hz `play_addr` tick rate
    /// (`rate * tempo_ratio`) directly while keeping 1.789773 MHz APU pitch oscillators
    /// 100% pitch-exact at zero WSOLA CPU cost!
    pub fn set_tempo_ratio(&mut self, tempo_ratio: f32) {
        let r = (tempo_ratio as f64).clamp(0.5, 2.0);
        let base = self.parsed.frame_cycles() as f64;
        self.machine.frame_cycles = (base / r).round().max(256.0) as u32;
    }

    /// Reset RAM/APU, initialize bank registers, apply `channel_mute_mask`, and execute `JSR init_addr`.
    pub fn select_subtrack(&mut self, index: u16) -> Result<(), NsfError> {
        if index >= self.parsed.total_subtracks {
            return Err(NsfError::SubtrackOutOfRange {
                requested: index,
                total: self.parsed.total_subtracks,
            });
        }

        self.current_subtrack = index;
        self.blip.clear();
        self.loop_detector.reset();
        self.frames_elapsed = 0;
        self.samples_rendered = 0;
        self.fade_started_at_ms = None;
        self.finished = false;

        let idx_usize = index as usize;
        let explicit_time = self.parsed.time_ms.get(idx_usize).and_then(|&t| t);
        if let Some(sub) = self.meta.subtracks.get(idx_usize) {
            self.play_duration_ms = sub.duration_ms;
            self.fade_duration_ms = sub.fade_ms;
            self.loop_point_ms = sub.loop_point_ms;
            self.has_explicit_timing = explicit_time.is_some() || sub.kind == TrackKind::Sfx;
        } else {
            self.play_duration_ms = DEFAULT_MUSIC_DURATION_MS;
            self.fade_duration_ms = DEFAULT_MUSIC_FADE_MS;
            self.loop_point_ms = None;
            self.has_explicit_timing = explicit_time.is_some();
        }

        self.machine.init_subtrack(&self.parsed, index);
        self.last_telemetry = ApuFrameTelemetry {
            channel_count: self.channel_descriptors().len().min(8) as u8,
            chip_flags: self.parsed.chip_flags.0,
            mute_mask: self.machine.mute_mask,
            channels: [ChannelState::default(); 8],
        };
        for i in 0..8 {
            self.last_telemetry.channels[i].muted =
                u8::from((self.machine.mute_mask & (1 << i)) != 0);
        }

        Ok(())
    }

    #[inline]
    pub fn elapsed_ms(&self) -> u32 {
        let frame_ms = self.frames_to_ms(self.frames_elapsed);
        if self.samples_rendered > 0 {
            ((self.samples_rendered * 1_000) / (self.sample_rate as u64)) as u32
        } else {
            frame_ms
        }
    }

    #[inline]
    pub fn duration_ms(&self) -> u32 {
        self.play_duration_ms
    }

    #[inline]
    pub fn fade_ms(&self) -> u32 {
        self.fade_duration_ms
    }

    #[inline]
    pub fn total_duration_ms(&self) -> u32 {
        self.play_duration_ms.saturating_add(self.fade_duration_ms)
    }

    #[inline]
    pub fn loop_point_ms(&self) -> Option<u32> {
        self.loop_point_ms
    }

    #[inline]
    pub fn is_finished(&self) -> bool {
        self.finished && !self.endless_loop
    }

    #[inline]
    pub fn telemetry(&self) -> ApuFrameTelemetry {
        self.last_telemetry
    }

    /// Fast-forward (or re-init and fast-forward) headless 6502 + APU cycles to `target_ms` without PCM synthesis.
    pub fn seek_ms(&mut self, target_ms: u32) {
        let _ = self.seek_step_ms(target_ms, u32::MAX);
    }

    /// Incremental headless seek step advancing at most `max_step_ms` per call.
    /// Returns `true` once `target_ms` has been reached.
    pub fn seek_step_ms(&mut self, target_ms: u32, max_step_ms: u32) -> bool {
        let cur_ms = self.frames_to_ms(self.frames_elapsed);
        if target_ms < cur_ms {
            let sub = self.current_subtrack;
            let _ = self.select_subtrack(sub);
        } else {
            self.blip.clear();
            self.machine.has_rendered_pcm = false;
        }

        if let Some(fade_start) = self.fade_started_at_ms {
            if target_ms < fade_start {
                self.fade_started_at_ms = None;
                self.finished = false;
            } else if target_ms < fade_start.saturating_add(self.fade_duration_ms) {
                self.finished = false;
            }
        } else {
            self.finished = false;
        }

        let start_step_ms = self.frames_to_ms(self.frames_elapsed);
        let budget_end_ms = start_step_ms.saturating_add(max_step_ms.max(17)).min(target_ms);

        while self.frames_to_ms(self.frames_elapsed) < budget_end_ms {
            let _ = self.step_frame_internal(true);
            if self.is_finished() {
                self.samples_rendered =
                    ((self.frames_to_ms(self.frames_elapsed) as u64) * (self.sample_rate as u64))
                        / 1_000;
                return true;
            }
        }

        let new_ms = self.frames_to_ms(self.frames_elapsed);
        self.samples_rendered = ((new_ms as u64) * (self.sample_rate as u64)) / 1_000;
        new_ms >= target_ms
    }

    /// Step exactly one 60/50 Hz hardware frame in headless mode (without PCM synthesis)
    /// and return the resulting `ApuFrameTelemetry`.
    pub fn step_frame_headless(&mut self) -> ApuFrameTelemetry {
        let tel = self.step_frame_internal(true);
        let ms = self.frames_to_ms(self.frames_elapsed);
        self.samples_rendered = ((ms as u64) * (self.sample_rate as u64)) / 1_000;
        tel
    }

    /// Render mono 16-bit PCM samples into `out`. Returns number of samples written (`0` on EOF).
    pub fn render_mono_s16(&mut self, out: &mut [i16]) -> usize {
        if out.is_empty() || self.is_finished() {
            return 0;
        }

        let mut written = 0usize;
        while written < out.len() {
            if self.blip.samples_avail() == 0 {
                if self.is_finished() {
                    break;
                }
                let _ = self.step_frame_internal(false);
                if self.blip.samples_avail() == 0 && self.is_finished() {
                    break;
                }
            }

            let gain_q15 = self.current_fade_gain_q15();
            let n = self.blip.read_samples(&mut out[written..], gain_q15);
            if n == 0 {
                break;
            }
            written += n;
            self.samples_rendered += n as u64;

            if !self.endless_loop && gain_q15 == 0 && self.fade_started_at_ms.is_some() {
                self.finished = true;
                self.blip.clear();
                break;
            }
        }

        written
    }

    /// Render interleaved stereo `[L, R, L, R, ...]` 16-bit PCM frames into `out_interleaved`.
    /// Returns the number of stereo frames written (`0` on EOF).
    pub fn render_stereo_s16(&mut self, out_interleaved: &mut [i16]) -> usize {
        let max_frames = out_interleaved.len() / 2;
        if max_frames == 0 || self.is_finished() {
            return 0;
        }

        let mut mono_buf = [0i16; STEREO_CHUNK_FRAMES];
        let mut frames_done = 0usize;
        while frames_done < max_frames {
            let chunk = (max_frames - frames_done).min(STEREO_CHUNK_FRAMES);
            let got = self.render_mono_s16(&mut mono_buf[..chunk]);
            if got == 0 {
                break;
            }
            for (i, &s) in mono_buf[..got].iter().enumerate() {
                let dst = (frames_done + i) * 2;
                out_interleaved[dst] = s;
                out_interleaved[dst + 1] = s;
            }
            frames_done += got;
        }
        frames_done
    }

    #[inline]
    fn frames_to_ms(&self, frames: u64) -> u32 {
        let us = self.parsed.frame_speed_us() as u64;
        ((frames * us) / 1_000).min(u32::MAX as u64) as u32
    }

    fn step_frame_internal(&mut self, headless: bool) -> ApuFrameTelemetry {
        let blip_opt = if headless { None } else { Some(&mut self.blip) };
        let tel = self.machine.run_frame(&self.parsed, headless, blip_opt);
        self.frames_elapsed += 1;
        self.last_telemetry = tel;

        let cur_ms = self.frames_to_ms(self.frames_elapsed);

        if !self.has_explicit_timing && self.loop_detector.observe_frame(&tel) {
            if let Some(lp_frame) = self.loop_detector.loop_point_frame() {
                let lp_ms = self.frames_to_ms(lp_frame as u64);
                self.loop_point_ms = Some(lp_ms);
            }
            self.play_duration_ms = cur_ms;
            self.fade_duration_ms = LOOP_FADE_MS;
            if let Some(sub) = self.meta.subtracks.get_mut(self.current_subtrack as usize) {
                sub.duration_ms = cur_ms;
                sub.fade_ms = LOOP_FADE_MS;
                sub.loop_point_ms = self.loop_point_ms;
            }
            if !self.endless_loop && self.fade_started_at_ms.is_none() {
                self.fade_started_at_ms = Some(cur_ms);
            }
        }

        if !self.endless_loop {
            if self.fade_started_at_ms.is_none() && cur_ms >= self.play_duration_ms {
                if self.fade_duration_ms == 0 {
                    self.finished = true;
                } else {
                    self.fade_started_at_ms = Some(self.play_duration_ms);
                }
            }
            if let Some(start_ms) = self.fade_started_at_ms {
                if cur_ms >= start_ms.saturating_add(self.fade_duration_ms) {
                    self.finished = true;
                }
            }
        }

        tel
    }

    fn current_fade_gain_q15(&self) -> i32 {
        if self.endless_loop {
            return 32_768;
        }
        let Some(start_ms) = self.fade_started_at_ms else {
            return 32_768;
        };
        if self.fade_duration_ms == 0 {
            return 0;
        }
        let now_ms = self.elapsed_ms();
        if now_ms <= start_ms {
            return 32_768;
        }
        let into_fade = now_ms - start_ms;
        if into_fade >= self.fade_duration_ms {
            return 0;
        }
        let progress = (into_fade as f64) / (self.fade_duration_ms as f64);
        let gain = 0.5 * (1.0 + (PI * progress).cos());
        ((gain * 32_768.0).round() as i32).clamp(0, 32_768)
    }
}

/// Fast zero-emulation header & NSFE chunk metadata inspector.
///
/// Accepts either a 128-byte `NESM\x1A` header slice or a full `.nsf`/`.nsfe` buffer and builds
/// `NsfContainerMeta` in microseconds without allocating ROM banks or running 6502 CPU frames.
pub fn inspect_nsf_header_fast(bytes: &[u8]) -> Result<NsfContainerMeta, NsfError> {
    let parsed = ParsedNsf::parse_header_fast(bytes)?;
    Ok(build_fast_container_meta(&parsed))
}

/// Fast non-scanning container metadata builder used when opening playback directly via `NsfEngine::load`.
fn build_fast_container_meta(parsed: &ParsedNsf) -> NsfContainerMeta {
    let total = parsed.total_subtracks;
    let mut subtracks = Vec::with_capacity(total as usize);
    for idx in 0..total {
        let i_usize = idx as usize;
        let title = parsed.tlbl.get(i_usize).and_then(|t| t.clone());
        let kind = parsed
            .plst
            .as_ref()
            .map(|plst| {
                if plst.contains(&idx) {
                    TrackKind::Music
                } else {
                    TrackKind::Sfx
                }
            })
            .unwrap_or(TrackKind::Music);
        let duration_ms = parsed
            .time_ms
            .get(i_usize)
            .and_then(|&d| d)
            .unwrap_or(match kind {
                TrackKind::Music => DEFAULT_MUSIC_DURATION_MS,
                TrackKind::Sfx => 2_000,
            });
        let fade_ms = parsed
            .fade_ms
            .get(i_usize)
            .and_then(|&f| f)
            .unwrap_or(match kind {
                TrackKind::Music => DEFAULT_MUSIC_FADE_MS,
                TrackKind::Sfx => 0,
            });
        subtracks.push(SubtrackMeta {
            index: idx,
            title,
            kind,
            duration_ms,
            fade_ms,
            loop_point_ms: None,
        });
    }

    NsfContainerMeta {
        game_title: parsed.game_title.clone(),
        artist: parsed.artist.clone(),
        copyright: parsed.copyright.clone(),
        total_subtracks: total,
        starting_subtrack: parsed.starting_subtrack,
        is_pal: parsed.is_pal,
        chip_flags: parsed.chip_flags,
        payload_crc32: parsed.payload_crc32,
        subtracks,
    }
}

// ============================================================================
// Comprehensive Unit Tests (Phase 1 Verification Suite)
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    /// Helper to wrap raw 6502 PRG bytes into a valid 128-byte NESM `.nsf` container.
    #[allow(clippy::too_many_arguments)]
    fn build_test_nsf(
        total_songs: u8,
        start_song_1based: u8,
        load_addr: u16,
        init_addr: u16,
        play_addr: u16,
        chip_flags: u8,
        banks: [u8; 8],
        prg: &[u8],
    ) -> Vec<u8> {
        let mut out = vec![0u8; 0x80 + prg.len()];
        out[0x00..0x05].copy_from_slice(b"NESM\x1A");
        out[0x05] = 0x01;
        out[0x06] = total_songs;
        out[0x07] = start_song_1based;
        out[0x08..0x0A].copy_from_slice(&load_addr.to_le_bytes());
        out[0x0A..0x0C].copy_from_slice(&init_addr.to_le_bytes());
        out[0x0C..0x0E].copy_from_slice(&play_addr.to_le_bytes());
        out[0x0E..0x1A].copy_from_slice(b"UIPod Synth\0");
        out[0x2E..0x39].copy_from_slice(b"Test Coder\0");
        out[0x4E..0x56].copy_from_slice(b"2026 UI\0");
        out[0x6E..0x70].copy_from_slice(&16_666u16.to_le_bytes());
        out[0x70..0x78].copy_from_slice(&banks);
        out[0x78..0x7A].copy_from_slice(&20_000u16.to_le_bytes());
        out[0x7A] = 0x00; // NTSC
        out[0x7B] = chip_flags;
        out[0x80..].copy_from_slice(prg);
        out
    }

    /// Build a multi-subtrack 6502 test ROM loaded at `$8000`:
    /// - Subtrack 0 (`Music`): Loops an 8-note melody on Pulse 1 (changing pitch every 16 frames,
    ///   full loop = 128 frames ≈ 2.13s) + continuous Triangle bassline (with periodic kick sweep!).
    /// - Subtrack 1 (`Sfx`): Plays a 2-note coin ping for 20 frames (~0.33s), then silences `$4015 = 0`.
    /// - Subtrack 2 (`Sfx`): Plays a single decaying chord for 90 frames (~1.5s, 1 pitch event), then silences.
    fn build_music_and_sfx_nsf() -> Vec<u8> {
        let mut prg = vec![0u8; 0x0200];
        // $8000: INIT (A = subtrack index)
        // STA $00 (subtrack), LDA #0, STA $01 (frame_lo), STA $02 (note_idx), LDA #$0F, STA $4015, RTS
        let init_code: &[u8] = &[
            0x85, 0x00, // STA $00
            0xA9, 0x00, // LDA #$00
            0x85, 0x01, // STA $01 (tick)
            0x85, 0x02, // STA $02 (step)
            0xA9, 0x0F, // LDA #$0F
            0x8D, 0x15, 0x40, // STA $4015
            0x60, // RTS
        ];
        prg[0x0000..init_code.len()].copy_from_slice(init_code);

        // $8020: PLAY
        // Dispatch based on $00 (0 = Music, 1 = Coin SFX, 2 = Single Ping SFX)
        let play_code: &[u8] = &[
            0xA5, 0x00, // $8020: LDA $00
            0xF0, 0x08, // $8022: BEQ music ($802C)
            0xC9, 0x01, // $8024: CMP #$01
            0xF0, 0x4C, // $8026: BEQ sfx_coin ($8074)
            0x4C, 0xB0, 0x80, // $8028: JMP sfx_ping ($80B0)
            0xEA, // $802B: NOP (pad to $802C)
            // --- $802C: music ---
            // Check if (tick & 0x0F) == 0 -> advance note on Pulse 1 & kick Triangle
            0xA5, 0x01, // $802C: LDA $01
            0x29, 0x0F, // $802E: AND #$0F
            0xD0, 0x2B, // $8030: BNE $805D (glide/sustain)
            // Trigger new Pulse 1 note from table at $8100 + X
            0xA5, 0x02, // $8032: LDA $02
            0x29, 0x07, // $8034: AND #$07 (8-note loop -> 8 * 16 = 128 frames ≈ 2.13s!)
            0xAA,       // $8036: TAX
            0xA9, 0xBF, // $8037: LDA #$BF (50% duty, constant vol 15, halt length)
            0x8D, 0x00, 0x40, // $8039: STA $4000
            0xBD, 0x00, 0x81, // $803C: LDA $8100,X
            0x8D, 0x02, 0x40, // $803F: STA $4002
            0xA9, 0x08, // $8042: LDA #$08 (high=0, len load=1)
            0x8D, 0x03, 0x40, // $8044: STA $4003
            // Start Triangle kick at high pitch ($60 ≈ 577 Hz)
            0xA9, 0xFF, // $8047: LDA #$FF (control+linear=127)
            0x8D, 0x08, 0x40, // $8049: STA $4008
            0xA9, 0x60, // $804C: LDA #$60
            0x85, 0x03, // $804E: STA $03 (tri period)
            0x8D, 0x0A, 0x40, // $8050: STA $400A
            0xA9, 0x08, // $8053: LDA #$08
            0x8D, 0x0B, 0x40, // $8055: STA $400B
            0xE6, 0x02, // $8058: INC $02
            0x4C, 0x6C, 0x80, // $805A: JMP $806C
            // --- $805D: sustain / downward Triangle kick sweep on frames 1..4 ---
            0xA5, 0x03, // $805D: LDA $03
            0xC9, 0xD0, // $805F: CMP #$D0
            0xB0, 0x09, // $8061: BCS $806C
            0x18,       // $8063: CLC
            0x69, 0x28, // $8064: ADC #$28 (rapidly increase period -> drop pitch for kick sweep!)
            0x85, 0x03, // $8066: STA $03
            0x8D, 0x0A, 0x40, // $8068: STA $400A
            0xEA,       // $806B: NOP
            // --- $806C: increment tick & return ---
            0xE6, 0x01, // $806C: INC $01
            0xD0, 0x02, // $806E: BNE +2
            0xE6, 0x04, // $8070: INC $04
            0x60, // $8072: RTS
        ];
        prg[0x0020..0x0020 + play_code.len()].copy_from_slice(play_code);
        assert!(0x0020 + play_code.len() <= 0x0074);

        // $8074: sfx_coin (plays 2 notes over 20 frames, then silences $4015 = 0)
        let sfx_coin: &[u8] = &[
            0xA5, 0x01, // LDA $01
            0xD0, 0x10, // BNE check_step2
            // Frame 0: Note B5 (period ~112)
            0xA9, 0x9F, 0x8D, 0x00, 0x40, // STA $4000
            0xA9, 0x70, 0x8D, 0x02, 0x40, // STA $4002
            0xA9, 0x08, 0x8D, 0x03, 0x40, // STA $4003
            0xE6, 0x01, 0x60, // INC $01; RTS
            // check_step2:
            0xC9, 0x06, // CMP #$06
            0xD0, 0x08, // BNE check_end
            0xA9, 0x54, 0x8D, 0x02, 0x40, // E6 note (period 84)
            0xE6, 0x01, 0x60,
            // check_end:
            0xC9, 0x14, // CMP #20
            0x90, 0x06, // BCC inc_ret
            0xA9, 0x00, 0x8D, 0x15, 0x40, // STA $4015 (permanent silence!)
            0x60,
            0xE6, 0x01, 0x60,
        ];
        prg[0x0074..0x0074 + sfx_coin.len()].copy_from_slice(sfx_coin);
        assert!(0x0074 + sfx_coin.len() <= 0x00B0);

        // $80B0: sfx_ping (1 note for 60 frames = 1.0s, then silence)
        let sfx_ping: &[u8] = &[
            0xA5, 0x01,
            0xD0, 0x10,
            0xA9, 0xBF, 0x8D, 0x00, 0x40,
            0xA9, 0xFD, 0x8D, 0x02, 0x40, // 253 -> ~440.4 Hz A4!
            0xA9, 0x08, 0x8D, 0x03, 0x40,
            0xE6, 0x01, 0x60,
            0xC9, 0x3C, // 60 frames
            0x90, 0x06,
            0xA9, 0x00, 0x8D, 0x15, 0x40,
            0x60,
            0xE6, 0x01, 0x60,
        ];
        prg[0x00B0..0x00B0 + sfx_ping.len()].copy_from_slice(sfx_ping);

        // $8100: 8-note melody period table
        let notes: [u8; 8] = [253, 225, 201, 190, 169, 201, 225, 240];
        prg[0x0100..0x0108].copy_from_slice(&notes);

        build_test_nsf(3, 1, 0x8000, 0x8000, 0x8020, 0, [0; 8], &prg)
    }

    fn append_nsfe_chunk(buf: &mut Vec<u8>, id: &[u8; 4], data: &[u8]) {
        buf.extend_from_slice(&(data.len() as u32).to_le_bytes());
        buf.extend_from_slice(id);
        buf.extend_from_slice(data);
    }

    #[test]
    fn c_abi_telemetry_layout_is_compact_and_deterministic() {
        assert_eq!(std::mem::size_of::<ChannelState>(), 12);
        assert_eq!(std::mem::align_of::<ChannelState>(), 4);
        assert_eq!(std::mem::size_of::<ApuFrameTelemetry>(), 4 + 8 * 12);
    }

    #[test]
    fn chip_flags_badges_and_channel_descriptors_match_spec() {
        let pure_2a03 = ChipFlags(0);
        assert_eq!(pure_2a03.primary_badge_label(), "2A03");
        let d_2a03 = pure_2a03.channel_descriptors();
        assert_eq!(d_2a03.len(), 5);
        assert_eq!(d_2a03[0].short_code, "P1");
        assert_eq!(d_2a03[2].display_name, "Triangle");
        assert_eq!(d_2a03[4].bit_index, 4);

        let vrc6 = ChipFlags(ChipFlags::VRC6);
        assert_eq!(vrc6.primary_badge_label(), "VRC6");
        let d_vrc6 = vrc6.channel_descriptors();
        assert_eq!(d_vrc6.len(), 8);
        assert_eq!(d_vrc6[5].short_code, "V1");
        assert_eq!(d_vrc6[6].short_code, "V2");
        assert_eq!(d_vrc6[7].short_code, "SAW");
        assert_eq!(d_vrc6[7].display_name, "VRC6 Sawtooth");

        let mmc5 = ChipFlags(ChipFlags::MMC5);
        assert_eq!(mmc5.primary_badge_label(), "MMC5");
        let d_mmc5 = mmc5.channel_descriptors();
        assert_eq!(d_mmc5.len(), 8);
        assert_eq!(d_mmc5[5].display_name, "MMC5 Pulse 1");
        assert_eq!(d_mmc5[6].display_name, "MMC5 Pulse 2");
        assert_eq!(d_mmc5[7].display_name, "MMC5 PCM");

        assert_eq!(ChipFlags(ChipFlags::FDS).primary_badge_label(), "FDS");
        assert_eq!(ChipFlags(ChipFlags::N163).primary_badge_label(), "N163");
        assert_eq!(ChipFlags(ChipFlags::S5B).primary_badge_label(), "5B");
    }

    #[test]
    fn tier3_scanner_classifies_music_vs_sfx_accurately() {
        let nsf = build_music_and_sfx_nsf();
        let meta = scan_nsf_container(&nsf).expect("scan_nsf_container succeeded");
        assert_eq!(meta.total_subtracks, 3);
        assert_eq!(meta.starting_subtrack, 0);
        assert_eq!(meta.game_title, "UIPod Synth");
        assert_eq!(meta.subtracks[0].kind, TrackKind::Music);
        assert_eq!(meta.subtracks[1].kind, TrackKind::Sfx);
        assert!(meta.subtracks[1].duration_ms < 1_000);
        assert_eq!(meta.subtracks[1].fade_ms, 0);
        assert_eq!(meta.subtracks[2].kind, TrackKind::Sfx);
        assert!(meta.subtracks[2].duration_ms < 2_000);
    }

    #[test]
    fn nsfe_chunks_and_tier2_crc32_db_lookup_work() {
        let prg = vec![0x60u8; 64]; // simple RTS stub
        let expected_crc = crc32fast::hash(&prg);

        let mut nsfe = Vec::from(&b"NSFE"[..]);
        // INFO: load=$8000, init=$8000, play=$8000, NTSC, pure 2A03, 4 songs, start=1 (0-based!)
        let info = [0x00, 0x80, 0x00, 0x80, 0x00, 0x80, 0x00, 0x00, 4, 1];
        append_nsfe_chunk(&mut nsfe, b"INFO", &info);
        append_nsfe_chunk(&mut nsfe, b"DATA", &prg);
        // plst includes only subtracks 0 and 2 -> 0 & 2 are Music, 1 & 3 default to Sfx!
        append_nsfe_chunk(&mut nsfe, b"plst", &[0, 2]);
        append_nsfe_chunk(
            &mut nsfe,
            b"tlbl",
            b"Title Theme\0Jump SFX\0Boss Battle\0\0",
        );
        let mut times = Vec::new();
        times.extend_from_slice(&95_000i32.to_le_bytes());
        times.extend_from_slice(&600i32.to_le_bytes());
        times.extend_from_slice(&(-1i32).to_le_bytes()); // unspecified -> Tier 2 DB fills this!
        times.extend_from_slice(&450i32.to_le_bytes());
        append_nsfe_chunk(&mut nsfe, b"time", &times);

        let mut fades = Vec::new();
        fades.extend_from_slice(&3_500i32.to_le_bytes());
        fades.extend_from_slice(&0i32.to_le_bytes());
        fades.extend_from_slice(&(-1i32).to_le_bytes());
        fades.extend_from_slice(&0i32.to_le_bytes());
        append_nsfe_chunk(&mut nsfe, b"fade", &fades);
        append_nsfe_chunk(
            &mut nsfe,
            b"auth",
            b"Castlevania III\x00Konami Kukeiha Club\x001989 Konami\x00RipperX\x00",
        );
        append_nsfe_chunk(&mut nsfe, b"NEND", &[]);

        struct MockDb(HashMap<(u32, u16), DbSubtrackTiming>);
        impl TimingDbLookup for MockDb {
            fn lookup_subtrack(&self, crc: u32, idx: u16) -> Option<DbSubtrackTiming> {
                self.0.get(&(crc, idx)).cloned()
            }
        }
        let mut map = HashMap::new();
        map.insert(
            (expected_crc, 2),
            DbSubtrackTiming {
                title: None,
                kind: Some(TrackKind::Music),
                duration_ms: Some(110_000),
                fade_ms: Some(5_000),
                loop_point_ms: Some(12_000),
            },
        );
        let db = MockDb(map);

        let meta = scan_nsf_container_with_db(&nsfe, &db).expect("NSFE scan succeeded");
        assert_eq!(meta.payload_crc32, expected_crc);
        assert_eq!(meta.game_title, "Castlevania III");
        assert_eq!(meta.artist, "Konami Kukeiha Club");
        assert_eq!(meta.starting_subtrack, 1);
        assert_eq!(meta.subtracks[0].kind, TrackKind::Music);
        assert_eq!(meta.subtracks[0].title.as_deref(), Some("Title Theme"));
        assert_eq!(meta.subtracks[0].duration_ms, 95_000);
        assert_eq!(meta.subtracks[0].fade_ms, 3_500);

        assert_eq!(meta.subtracks[1].kind, TrackKind::Sfx);
        assert_eq!(meta.subtracks[1].title.as_deref(), Some("Jump SFX"));

        assert_eq!(meta.subtracks[2].kind, TrackKind::Music);
        assert_eq!(meta.subtracks[2].duration_ms, 110_000);
        assert_eq!(meta.subtracks[2].fade_ms, 5_000);
        assert_eq!(meta.subtracks[2].loop_point_ms, Some(12_000));

        assert_eq!(meta.subtracks[3].kind, TrackKind::Sfx);
        assert_eq!(meta.subtracks[3].title, None);
    }

    #[test]
    fn channel_mute_mask_silences_all_or_isolates_individual_channels_without_stalling() {
        let nsf = build_music_and_sfx_nsf();

        // 1. Muting all bits (0xFFFF) produces exact 0 PCM while CPU/APU advance identically
        let mut muted_engine = NsfEngine::load(&nsf, 48_000).unwrap();
        muted_engine.set_channel_mute_mask(0xFFFF);
        let mut silent_pcm = vec![0i16; 4800];
        let n_silent = muted_engine.render_mono_s16(&mut silent_pcm);
        assert_eq!(n_silent, 4800);
        assert!(
            silent_pcm.iter().all(|&s| s == 0),
            "0xFFFF mute mask must produce completely silent PCM"
        );
        // Verify CPU/music driver did NOT stall: zero-page note counter $02 advanced!
        assert!(muted_engine.machine.ram[0x02] > 0);
        // Verify telemetry marks muted=1, volume=0.0, while active=1 still reflects hardware notes!
        let tel_muted = muted_engine.telemetry();
        assert_eq!(tel_muted.mute_mask, 0xFFFF);
        assert_eq!(tel_muted.channels[0].active, 1);
        assert_eq!(tel_muted.channels[0].muted, 1);
        assert_eq!(tel_muted.channels[0].volume, 0.0);

        // 2. Soloing Pulse 1 vs Soloing Triangle produces distinct audible waveforms
        let mut p1_solo = NsfEngine::load(&nsf, 48_000).unwrap();
        p1_solo.set_channel_mute_mask(!0x0001); // only bit 0 (Pulse 1) enabled
        let mut p1_pcm = vec![0i16; 4800];
        p1_solo.render_mono_s16(&mut p1_pcm);
        let p1_peak = p1_pcm.iter().map(|s| s.unsigned_abs()).max().unwrap_or(0);
        assert!(p1_peak > 1000, "Pulse 1 solo should be audible, got {p1_peak}");

        let mut tri_solo = NsfEngine::load(&nsf, 48_000).unwrap();
        tri_solo.set_channel_mute_mask(!0x0004); // only bit 2 (Triangle) enabled
        let mut tri_pcm = vec![0i16; 4800];
        tri_solo.render_mono_s16(&mut tri_pcm);
        let tri_peak = tri_pcm.iter().map(|s| s.unsigned_abs()).max().unwrap_or(0);
        assert!(tri_peak > 1000, "Triangle solo should be audible, got {tri_peak}");
        assert_ne!(p1_pcm, tri_pcm, "Pulse 1 solo and Triangle solo waveforms must differ");

        // Verify CPU RAM state after identical sample counts matches 1:1 regardless of mute mask!
        assert_eq!(
            muted_engine.machine.ram[0x01],
            tri_solo.machine.ram[0x01],
            "Muting must never desynchronize 6502/APU timing"
        );
    }

    #[test]
    fn apu_frame_telemetry_reports_accurate_freq_volume_duty_and_onsets() {
        let nsf = build_music_and_sfx_nsf();
        let mut engine = NsfEngine::load(&nsf, 44_100).unwrap();

        // Subtrack 0 Frame 0: Pulse 1 plays period 253 (~440.4 Hz), duty 2 (50%), vol 15/15 (1.0)
        let tel0 = engine.step_frame_headless();
        assert_eq!(tel0.channel_count, 5);
        let p1 = tel0.channels[0];
        assert_eq!(p1.active, 1);
        assert_eq!(p1.muted, 0);
        assert_eq!(p1.onset_trigger, 1);
        assert_eq!(p1.duty_or_mode, 2);
        assert!((p1.volume - 1.0).abs() < 1e-4);
        assert!(
            (p1.freq_hz - 440.4).abs() < 1.0,
            "expected ~440.4 Hz, got {}",
            p1.freq_hz
        );

        // Frame 1: Pulse 1 sustains without re-triggering onset; Triangle sweeps downward (kick!)
        let tel1 = engine.step_frame_headless();
        assert_eq!(tel1.channels[0].onset_trigger, 0);
        assert_eq!(tel1.channels[2].active, 1);
        assert_eq!(
            tel1.channels[2].onset_trigger, 1,
            "Triangle downward pitch sweep should fire kick onset_trigger"
        );
    }

    #[test]
    fn seek_ms_and_runtime_loop_detector_work_accurately() {
        let nsf = build_music_and_sfx_nsf();

        // Reference engine stepped frame-by-frame to 2500 ms
        let mut ref_eng = NsfEngine::load(&nsf, 48_000).unwrap();
        while ref_eng.elapsed_ms() < 2_500 {
            ref_eng.step_frame_headless();
        }

        // Seek engine jumps straight to 2500 ms, then seeks backward to 1000 ms, then forward to 2500 ms
        let mut seek_eng = NsfEngine::load(&nsf, 48_000).unwrap();
        seek_eng.seek_ms(2_500);
        assert_eq!(seek_eng.elapsed_ms(), ref_eng.elapsed_ms());
        assert_eq!(seek_eng.machine.ram[0x01], ref_eng.machine.ram[0x01]);
        assert_eq!(seek_eng.machine.ram[0x02], ref_eng.machine.ram[0x02]);

        seek_eng.seek_ms(1_000);
        assert!(seek_eng.elapsed_ms() >= 1_000 && seek_eng.elapsed_ms() < 1_030);
        seek_eng.seek_ms(2_500);
        assert_eq!(seek_eng.machine.ram[0x02], ref_eng.machine.ram[0x02]);

        // Runtime 2-loop detector: Subtrack 0 loops every 128 frames (~2.13s).
        // After 2 full loops (~4.3s), fade-out begins and finishes 4000 ms later (~8.4s)!
        let mut loop_eng = NsfEngine::load(&nsf, 44_100).unwrap();
        let mut buf = [0i16; 2048];
        let mut safety_iters = 0;
        while !loop_eng.is_finished() && safety_iters < 600 {
            loop_eng.render_mono_s16(&mut buf);
            safety_iters += 1;
        }
        assert!(loop_eng.is_finished(), "Engine should auto-fade and finish after 2 loops");
        assert!(loop_eng.loop_point_ms().is_some());
        assert!(
            loop_eng.elapsed_ms() >= 7_500 && loop_eng.elapsed_ms() <= 11_000,
            "Expected ~8.4s total play+fade for 2.13s loop, got {} ms",
            loop_eng.elapsed_ms()
        );

        // With `set_endless_loop(true)`, playback never terminates past the 2-loop mark
        let mut endless = NsfEngine::load(&nsf, 44_100).unwrap();
        endless.set_endless_loop(true);
        endless.seek_ms(15_000);
        assert!(!endless.is_finished());
        let rendered = endless.render_stereo_s16(&mut buf);
        assert_eq!(rendered, buf.len() / 2);
    }

    #[test]
    fn vrc6_and_mmc5_expansion_synthesis_and_bankswitching_work() {
        // 1. VRC6 + 4KB Bankswitching test:
        // Bank 0 at $8000 switches Bank 1 into $9000 ($5FF9 = 1), reads note period from $9010,
        // and drives VRC6 Pulse 1 ($9000..$9002) and VRC6 Sawtooth ($B000..$B002)!
        let mut prg = vec![0u8; 8192]; // two 4KB banks
        let vrc6_init: &[u8] = &[
            0xA9, 0x01, 0x8D, 0xF9, 0x5F, // LDA #1; STA $5FF9 (map bank 1 at $9000..$9FFF)
            0xAD, 0x10, 0x90, // LDA $9010 (reads 0x7E from bank 1!)
            0x8D, 0x01, 0x90, // STA $9001 (VRC6 P1 period lo)
            0xA9, 0x7F, 0x8D, 0x00, 0x90, // STA $9000 (50% duty, vol 15)
            0xA9, 0x80, 0x8D, 0x02, 0x90, // STA $9002 (enable VRC6 P1)
            0xA9, 0x20, 0x8D, 0x00, 0xB0, // STA $B000 (VRC6 Saw rate = 32)
            0xA9, 0xAA, 0x8D, 0x01, 0xB0, // STA $B001
            0xA9, 0x80, 0x8D, 0x02, 0xB0, // STA $B002 (enable VRC6 Saw)
            0x60, // RTS
        ];
        prg[0..vrc6_init.len()].copy_from_slice(vrc6_init);
        prg[0x0040] = 0x60; // PLAY = RTS
        prg[0x1010] = 0x7E; // Bank 1 offset $0010

        let vrc6_nsf = build_test_nsf(
            1,
            1,
            0x8000,
            0x8000,
            0x8040,
            ChipFlags::VRC6,
            [0, 0, 0, 0, 0, 0, 0, 0],
            &prg,
        );
        let mut vrc6_eng = NsfEngine::load(&vrc6_nsf, 48_000).unwrap();
        assert_eq!(vrc6_eng.channel_descriptors().len(), 8);
        let mut pcm = [0i16; 1600];
        vrc6_eng.render_mono_s16(&mut pcm);
        let peak = pcm.iter().map(|s| s.unsigned_abs()).max().unwrap_or(0);
        assert!(peak > 1500, "VRC6 P1 + Saw should produce strong output, got {peak}");

        let tel = vrc6_eng.telemetry();
        assert_eq!(tel.channel_count, 8);
        assert_eq!(tel.channels[5].active, 1); // VRC6 P1
        assert_eq!(tel.channels[7].active, 1); // VRC6 Saw
        assert!(tel.channels[5].freq_hz > 800.0);

        // 2. MMC5 Pulse + Hardware 8x8 Multiplier ($5205 * $5206) + Undocumented LAX/SAX test:
        let mut mmc5_prg = vec![0u8; 256];
        let mmc5_init: &[u8] = &[
            0xA9, 0x0C, 0x8D, 0x05, 0x52, // $5205 = 12
            0xA9, 0x15, 0x8D, 0x06, 0x52, // $5206 = 21 -> product = 252 ($00FC)
            0xAF, 0x05, 0x52, // LAX $5205 (undocumented LAX abs -> A = X = 252!)
            0x87, 0x10, // SAX $10 (undocumented SAX zp -> $10 = 252)
            0xA9, 0x03, 0x8D, 0x15, 0x50, // STA $5015 (enable MMC5 pulses)
            0xA9, 0xBF, 0x8D, 0x00, 0x50, // STA $5000 (MMC5 P1 vol 15)
            0xA5, 0x10, 0x8D, 0x02, 0x50, // STA $5002 (period 252)
            0xA9, 0x08, 0x8D, 0x03, 0x50, // STA $5003 (trigger MMC5 P1)
            0x60,
        ];
        mmc5_prg[0..mmc5_init.len()].copy_from_slice(mmc5_init);
        mmc5_prg[0x40] = 0x60; // PLAY = RTS ("infinite loop" test at $8050 below too)
        // Infinite loop guard test at $8050: JMP $8050
        mmc5_prg[0x50..0x53].copy_from_slice(&[0x4C, 0x50, 0x80]);

        let mmc5_nsf = build_test_nsf(
            1,
            1,
            0x8000,
            0x8000,
            0x8050, // Intentionally point PLAY at infinite loop `JMP $8050` to test cycle guard!
            ChipFlags::MMC5,
            [0; 8],
            &mmc5_prg,
        );
        let mut mmc5_eng = NsfEngine::load(&mmc5_nsf, 48_000).unwrap();
        let mut mmc5_pcm = [0i16; 1600];
        let n = mmc5_eng.render_mono_s16(&mut mmc5_pcm);
        assert_eq!(n, 1600, "Cycle budget must prevent infinite-loop PLAY from hanging");
        let mmc5_peak = mmc5_pcm.iter().map(|s| s.unsigned_abs()).max().unwrap_or(0);
        assert!(mmc5_peak > 1000, "MMC5 Pulse 1 should synthesize audio, got {mmc5_peak}");
        let mtel = mmc5_eng.telemetry();
        assert_eq!(mtel.channel_count, 8);
        assert_eq!(mtel.channels[5].active, 1);
        assert!((mtel.channels[5].freq_hz - 442.1).abs() < 2.0);
    }

    #[test]
    fn dpcm_dma_noise_undocumented_opcodes_and_pop_free_live_mute_work() {
        // Build a ROM mapped at $C000..$FFFF so $C000 DPCM DMA sample table is directly accessible!
        let mut prg = vec![0xAAu8; 0x1000]; // Alternating 10101010 DPCM delta bits at $C040+
        let init: &[u8] = &[
            // Exercise DCP, ISB, SLO, RLA, SRE, RRA undocumented opcodes in zero-page
            0xA9, 0x05, 0x85, 0x20, // $20 = 5
            0xC7, 0x20, // DCP $20 -> $20 = 4, C = 1
            0x07, 0x20, // SLO $20 -> $20 = 8, A |= 8
            0x27, 0x20, // RLA $20 -> $20 = 16
            0x47, 0x20, // SRE $20 -> $20 = 8
            0x67, 0x20, // RRA $20 -> $20 = 4
            0xE7, 0x20, // ISB $20 -> $20 = 5
            // Enable Noise (looping mode=1 metallic, vol=12) + DPCM looping sample from $C040 (addr reg = 1)
            0xA9, 0x3C, 0x8D, 0x0C, 0x40, // STA $400C (halt+const vol 12)
            0xA9, 0x84, 0x8D, 0x0E, 0x40, // STA $400E (mode=1, period idx=4)
            0xA9, 0x08, 0x8D, 0x0F, 0x40, // STA $400F (trigger Noise)
            0xA9, 0x40, 0x8D, 0x11, 0x40, // STA $4011 (DPCM resting level 64)
            0xA9, 0x4F, 0x8D, 0x10, 0x40, // STA $4010 (loop=1, rate=15 fastest)
            0xA9, 0x01, 0x8D, 0x12, 0x40, // STA $4012 ($C040)
            0xA9, 0x04, 0x8D, 0x13, 0x40, // STA $4013 (65 bytes)
            0xA9, 0x18, 0x8D, 0x15, 0x40, // STA $4015 (enable Noise + DPCM)
            0x60, // RTS
        ];
        prg[0..init.len()].copy_from_slice(init);
        assert!(init.len() <= 0x60);
        prg[0x60] = 0x60; // PLAY = RTS

        let nsf = build_test_nsf(1, 1, 0xC000, 0xC000, 0xC060, 0, [0; 8], &prg);
        let mut eng = NsfEngine::load(&nsf, 48_000).unwrap();
        assert_eq!(eng.machine.ram[0x20], 5, "Undocumented RMW opcodes must compute exact RAM state");

        let mut before_mute = [0i16; 1600];
        eng.render_mono_s16(&mut before_mute);
        let tel = eng.telemetry();
        assert_eq!(tel.channels[3].active, 1); // Noise active
        assert_eq!(tel.channels[3].duty_or_mode, 1); // Metallic short-LFSR mode
        assert_eq!(tel.channels[4].active, 1); // DPCM DMA active

        // Live toggle all channels to muted mid-playback while DPCM sits around level 64:
        // Verify smooth ramp (no sudden >12000 sample-to-sample DC pop!)
        let last_sample = *before_mute.last().unwrap();
        eng.set_channel_mute_mask(0xFFFF);
        let mut transition = [0i16; 1600];
        eng.render_mono_s16(&mut transition);
        let mut prev = last_sample;
        let mut max_step_delta = 0i32;
        for &s in &transition {
            max_step_delta = max_step_delta.max((s as i32 - prev as i32).abs());
            prev = s;
        }
        assert!(
            max_step_delta < 8_000,
            "Live DPCM/Noise muting must ramp smoothly without full-scale DC pop (got delta {max_step_delta})"
        );
    }
}
