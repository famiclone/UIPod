//! Three-tier NSF/NSFe subtrack metadata scanner & runtime 60 Hz loop detector.
//!
//! - **Tier 1:** Authoritative NSFE / NSF2 chunks (`plst`, `tlbl`, `time`, `fade`, `auth`).
//! - **Tier 2:** External `TimingDbLookup` trait keyed by stripped PRG `payload_crc32`.
//! - **Tier 3:** Headless 6502 + APU heuristic pre-scanner classifying `TrackKind::Music`
//!   vs. `TrackKind::Sfx`, paired with a zero-per-frame-allocation runtime loop detector
//!   that triggers a smooth 4000 ms cosine fade-out after 2 full loops.

use crate::cpu::NsfMachine;
use crate::parser::ParsedNsf;
use crate::{
    ApuFrameTelemetry, NsfContainerMeta, NsfError, SubtrackMeta, TrackKind,
};

pub const DEFAULT_MUSIC_DURATION_MS: u32 = 120_000;
pub const DEFAULT_MUSIC_FADE_MS: u32 = 4_000;
pub const LOOP_FADE_MS: u32 = 4_000;

/// Optional per-subtrack metadata record returned by an external timing database (Tier 2).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DbSubtrackTiming {
    pub title: Option<String>,
    pub kind: Option<TrackKind>,
    pub duration_ms: Option<u32>,
    pub fade_ms: Option<u32>,
    pub loop_point_ms: Option<u32>,
}

/// Tier-2 external timing database lookup hook keyed by stripped PRG `payload_crc32`.
pub trait TimingDbLookup {
    fn lookup_subtrack(
        &self,
        payload_crc32: u32,
        subtrack_index: u16,
    ) -> Option<DbSubtrackTiming>;
}

impl TimingDbLookup for () {
    #[inline]
    fn lookup_subtrack(
        &self,
        _payload_crc32: u32,
        _subtrack_index: u16,
    ) -> Option<DbSubtrackTiming> {
        None
    }
}

/// Scan an `.nsf` or `.nsfe` file in memory using Tier 1 (NSFE chunks) + Tier 3 (headless heuristic).
pub fn scan_nsf_container(bytes: &[u8]) -> Result<NsfContainerMeta, NsfError> {
    scan_nsf_container_with_db(bytes, &())
}

/// Scan an `.nsf` or `.nsfe` file using all 3 tiers: NSFE chunks -> `TimingDbLookup` -> Headless heuristic.
pub fn scan_nsf_container_with_db<D: TimingDbLookup + ?Sized>(
    bytes: &[u8],
    db: &D,
) -> Result<NsfContainerMeta, NsfError> {
    let parsed = ParsedNsf::parse(bytes)?;
    scan_parsed_nsf(&parsed, db)
}

pub(crate) fn scan_parsed_nsf<D: TimingDbLookup + ?Sized>(
    parsed: &ParsedNsf,
    db: &D,
) -> Result<NsfContainerMeta, NsfError> {
    let total = parsed.total_subtracks;
    let mut subtracks = Vec::with_capacity(total as usize);
    let mut machine: Option<NsfMachine> = None;

    let frame_us = parsed.frame_speed_us().max(1_000) as u64;
    // ~8.0 simulated seconds (~480 frames @ 60 Hz NTSC, ~400 frames @ 50 Hz PAL)
    let scan_frames = ((8_000_000u64 / frame_us) as usize).clamp(240, 600);
    // Require ~300 ms of trailing silence before the 8s boundary to count as permanently silent
    let silence_tail_guard = ((300_000u64 / frame_us) as usize).clamp(10, 30);

    for idx in 0..total {
        let i_usize = idx as usize;

        // --- Tier 1: NSFE / NSF2 Chunks ---
        let mut title = parsed.tlbl.get(i_usize).and_then(|t| t.clone());
        let mut kind = parsed.plst.as_ref().map(|plst| {
            if plst.contains(&idx) {
                TrackKind::Music
            } else {
                TrackKind::Sfx
            }
        });
        let mut duration_ms = parsed.time_ms.get(i_usize).and_then(|&d| d);
        let mut fade_ms = parsed.fade_ms.get(i_usize).and_then(|&f| f);
        let mut loop_point_ms: Option<u32> = None;

        // --- Tier 2: External Timing DB by stripped PRG CRC32 ---
        if let Some(hit) = db.lookup_subtrack(parsed.payload_crc32, idx) {
            if title.is_none() {
                title = hit.title;
            }
            if kind.is_none() {
                kind = hit.kind;
            }
            if duration_ms.is_none() {
                duration_ms = hit.duration_ms;
            }
            if fade_ms.is_none() {
                fade_ms = hit.fade_ms;
            }
            if loop_point_ms.is_none() {
                loop_point_ms = hit.loop_point_ms;
            }
        }

        // If NSFE `time` was given without `plst`, infer short non-fading cues (< 4.0s) as SFX
        if kind.is_none() {
            if let Some(d) = duration_ms {
                if d < 4_000 && fade_ms.unwrap_or(0) == 0 {
                    kind = Some(TrackKind::Sfx);
                }
            }
        }

        // --- Tier 3: Headless Heuristic Pre-Scanner ---
        if kind.is_none() || (kind == Some(TrackKind::Sfx) && duration_ms.is_none()) {
            let m = machine.get_or_insert_with(|| NsfMachine::new(parsed));
            let (heur_kind, heur_dur_ms, heur_fade_ms) =
                classify_subtrack_headless(m, parsed, idx, scan_frames, silence_tail_guard, frame_us);
            if kind.is_none() {
                kind = Some(heur_kind);
            }
            if duration_ms.is_none() {
                duration_ms = Some(heur_dur_ms);
            }
            if fade_ms.is_none() {
                fade_ms = Some(heur_fade_ms);
            }
        }

        let resolved_kind = kind.unwrap_or(TrackKind::Music);
        let resolved_duration_ms = duration_ms.unwrap_or(match resolved_kind {
            TrackKind::Music => DEFAULT_MUSIC_DURATION_MS,
            TrackKind::Sfx => 2_000,
        });
        let resolved_fade_ms = fade_ms.unwrap_or(match resolved_kind {
            TrackKind::Music => DEFAULT_MUSIC_FADE_MS,
            TrackKind::Sfx => 0,
        });

        subtracks.push(SubtrackMeta {
            index: idx,
            title,
            kind: resolved_kind,
            duration_ms: resolved_duration_ms,
            fade_ms: resolved_fade_ms,
            loop_point_ms,
        });
    }

    Ok(NsfContainerMeta {
        game_title: parsed.game_title.clone(),
        artist: parsed.artist.clone(),
        copyright: parsed.copyright.clone(),
        total_subtracks: total,
        starting_subtrack: parsed.starting_subtrack,
        is_pal: parsed.is_pal,
        chip_flags: parsed.chip_flags,
        payload_crc32: parsed.payload_crc32,
        subtracks,
    })
}

pub(crate) fn find_first_music_subtrack(parsed: &ParsedNsf) -> u16 {
    if let Some(ref plst) = parsed.plst {
        if let Some(&first) = plst.first() {
            if first < parsed.total_subtracks {
                return first;
            }
        }
    }
    if parsed.total_subtracks <= 1 {
        return 0;
    }

    let frame_us = parsed.frame_speed_us().max(1_000) as u64;
    let scan_frames = ((8_000_000u64 / frame_us) as usize).clamp(240, 600);
    let silence_tail_guard = ((300_000u64 / frame_us) as usize).clamp(10, 30);
    let mut machine = NsfMachine::new(parsed);

    // Check `starting_subtrack` first if > 0? No, canonical index order `0..total` matches
    // `expand_nsf_music_uris` so bare `Game.nsf` always starts on the first Music track.
    for idx in 0..parsed.total_subtracks {
        let i_usize = idx as usize;
        if let Some(Some(d)) = parsed.time_ms.get(i_usize) {
            let fade = parsed.fade_ms.get(i_usize).and_then(|&f| f).unwrap_or(0);
            if *d >= 4_000 || fade > 0 {
                return idx;
            }
            continue;
        }
        let (kind, _, _) = classify_subtrack_headless(
            &mut machine,
            parsed,
            idx,
            scan_frames,
            silence_tail_guard,
            frame_us,
        );
        if kind == TrackKind::Music {
            return idx;
        }
    }
    parsed.starting_subtrack
}

fn classify_subtrack_headless(
    machine: &mut NsfMachine,
    parsed: &ParsedNsf,
    subtrack_idx: u16,
    scan_frames: usize,
    silence_tail_guard: usize,
    frame_us: u64,
) -> (TrackKind, u32, u32) {
    machine.set_mute_mask(0);
    machine.init_subtrack(parsed, subtrack_idx);

    let mut ever_active = false;
    let mut last_active_frame = 0usize;
    let mut pitch_events = 0u32;
    let mut prev_active = [false; 8];
    let mut prev_freq_bin = [0u32; 8];

    // Early-exit frame thresholds scaled to NTSC/PAL frame rate
    let dead_silent_frames = ((1_500_000u64 / frame_us) as usize).clamp(60, 120);
    let sfx_silence_confirm_frames = (silence_tail_guard * 3).clamp(36, 90);

    for f in 0..scan_frames {
        let tel = machine.run_frame(parsed, true, None);
        let n_ch = (tel.channel_count as usize).min(8);
        let mut frame_active = false;

        for c in 0..n_ch {
            let ch = &tel.channels[c];
            let is_act = ch.active != 0;
            if is_act {
                frame_active = true;
                // Quantize frequency into ~2 Hz / semitone-ish bins to detect genuine note changes
                let bin = (ch.freq_hz * 0.5).round() as u32;
                let pitch_jumped = prev_active[c] && bin.abs_diff(prev_freq_bin[c]) >= 2;
                if !prev_active[c] || ch.onset_trigger != 0 || pitch_jumped {
                    pitch_events = pitch_events.saturating_add(1);
                    prev_freq_bin[c] = bin;
                }
            }
            prev_active[c] = is_act;
        }

        if frame_active {
            ever_active = true;
            last_active_frame = f;

            let active_end_ms = (((f + 1) as u64 * frame_us) / 1_000) as u32;
            // Once a subtrack has reached >= 4.0s with >= 4 note events (exact mathematical
            // complement of the SFX condition below), OR >= 2.5s of active melodic playback
            // with >= 12 note events, it is provably `TrackKind::Music`!
            if (active_end_ms >= 4_000 && pitch_events >= 4)
                || (active_end_ms >= 2_500 && pitch_events >= 12)
            {
                return (
                    TrackKind::Music,
                    DEFAULT_MUSIC_DURATION_MS,
                    DEFAULT_MUSIC_FADE_MS,
                );
            }
        } else if !ever_active {
            if f >= dead_silent_frames {
                return (TrackKind::Sfx, 500, 0);
            }
        } else if f.saturating_sub(last_active_frame) >= sfx_silence_confirm_frames {
            let active_end_ms = (((last_active_frame + 1) as u64 * frame_us) / 1_000) as u32;
            if active_end_ms < 4_000 || pitch_events <= 3 {
                let natural_dur = (active_end_ms + 200).max(400);
                return (TrackKind::Sfx, natural_dur, 0);
            }
        }
    }

    if !ever_active {
        return (TrackKind::Sfx, 500, 0);
    }

    let fell_permanently_silent = last_active_frame + silence_tail_guard < scan_frames;
    let active_end_ms = (((last_active_frame + 1) as u64 * frame_us) / 1_000) as u32;

    if fell_permanently_silent && (active_end_ms < 4_000 || pitch_events <= 3) {
        let natural_dur = (active_end_ms + 200).max(400);
        (TrackKind::Sfx, natural_dur, 0)
    } else {
        (
            TrackKind::Music,
            DEFAULT_MUSIC_DURATION_MS,
            DEFAULT_MUSIC_FADE_MS,
        )
    }
}

// ============================================================================
// Zero-Per-Frame-Allocation Runtime 60 Hz Loop Detector
// ============================================================================

const MAX_HISTORY_FRAMES: usize = 10_800; // 180 seconds @ 60 Hz, pre-allocated once
const WINDOW_FRAMES: usize = 48; // ~0.8s phrase window
const MIN_LOOP_FRAMES: usize = 90; // >= 1.5s loop period
const CONFIRM_LOOP3_FRAMES: usize = 8; // Confirm start of 3rd iteration before fading (handles A-A-B form!)

#[derive(Clone)]
pub(crate) struct LoopDetector {
    frame_sigs: Vec<u64>,
    phrase_hashes: Vec<u64>,
    cursor: usize,
    cand_loop_start: usize,
    cand_loop_len: usize,
    cand_mismatches: usize,
    has_candidate: bool,
    triggered: bool,
    loop_point_frame: Option<usize>,
}

impl LoopDetector {
    pub fn new() -> Self {
        Self {
            frame_sigs: vec![0u64; MAX_HISTORY_FRAMES],
            phrase_hashes: vec![0u64; MAX_HISTORY_FRAMES],
            cursor: 0,
            cand_loop_start: 0,
            cand_loop_len: 0,
            cand_mismatches: 0,
            has_candidate: false,
            triggered: false,
            loop_point_frame: None,
        }
    }

    pub fn reset(&mut self) {
        self.cursor = 0;
        self.cand_loop_start = 0;
        self.cand_loop_len = 0;
        self.cand_mismatches = 0;
        self.has_candidate = false;
        self.triggered = false;
        self.loop_point_frame = None;
    }

    #[inline]
    pub fn loop_point_frame(&self) -> Option<usize> {
        self.loop_point_frame
    }

    /// Observe one 60/50 Hz frame's APU telemetry (independent of user channel mute mask!).
    /// Returns `true` on the frame where 2 full loops have completed and fade-out should start.
    pub fn observe_frame(&mut self, tel: &ApuFrameTelemetry) -> bool {
        if self.triggered || self.cursor >= MAX_HISTORY_FRAMES {
            return false;
        }

        let f = self.cursor;
        let sig = compute_frame_signature(tel);
        self.frame_sigs[f] = sig;
        self.cursor += 1;

        if f + 1 < WINDOW_FRAMES {
            self.phrase_hashes[f] = 0;
            return false;
        }

        // Compute 48-frame phrase hash if phrase has musical motion & activity
        let start_w = f + 1 - WINDOW_FRAMES;
        let mut h = 0xcbf2_9ce4_8422_2325_u64;
        let mut active_cnt = 0usize;
        let mut changes = 0usize;
        let mut prev_s = self.frame_sigs[start_w];
        for i in start_w..=f {
            let s = self.frame_sigs[i];
            if s != 0 {
                active_cnt += 1;
            }
            if s != prev_s {
                changes += 1;
                prev_s = s;
            }
            h ^= s.wrapping_add(0x9e37_79b9_7f4a_7c15);
            h = h.wrapping_mul(0x1000_0000_01b3);
        }

        // Ignore dead silence or flat unchanging drones when anchoring a phrase
        let valid_phrase = active_cnt >= WINDOW_FRAMES / 3 && changes >= 1;
        self.phrase_hashes[f] = if valid_phrase { h } else { 0 };

        if self.has_candidate {
            let ref_idx = f.saturating_sub(self.cand_loop_len);
            if self.frame_sigs[f] != self.frame_sigs[ref_idx] {
                self.cand_mismatches += 1;
            }
            let max_allowed = (self.cand_loop_len / 24).max(2);
            if self.cand_mismatches > max_allowed {
                // Diverged (e.g. A-A-B song entered Section B); discard candidate and keep scanning
                self.has_candidate = false;
                self.cand_mismatches = 0;
            } else {
                let target_end =
                    self.cand_loop_start + 2 * self.cand_loop_len + CONFIRM_LOOP3_FRAMES;
                if f >= target_end {
                    self.triggered = true;
                    self.loop_point_frame = Some(self.cand_loop_start);
                    return true;
                }
                return false;
            }
        }

        // Search for matching phrase anchor `s` at least `MIN_LOOP_FRAMES` earlier
        if valid_phrase && f >= WINDOW_FRAMES - 1 + MIN_LOOP_FRAMES {
            let max_s = f - MIN_LOOP_FRAMES;
            for s in (WINDOW_FRAMES - 1)..=max_s {
                if self.phrase_hashes[s] == h {
                    let loop_len = f - s;
                    let loop_start = s + 1 - WINDOW_FRAMES;
                    if loop_start + 2 * loop_len + CONFIRM_LOOP3_FRAMES < MAX_HISTORY_FRAMES {
                        self.has_candidate = true;
                        self.cand_loop_start = loop_start;
                        self.cand_loop_len = loop_len;
                        self.cand_mismatches = 0;
                        break;
                    }
                }
            }
        }

        false
    }
}

/// Computes a mute-invariant 64-bit musical state fingerprint of all hardware channels.
fn compute_frame_signature(tel: &ApuFrameTelemetry) -> u64 {
    let n_ch = (tel.channel_count as usize).min(8);
    let mut any_active = false;
    let mut hash = 0x811c_9dc5_u64;

    for c in 0..n_ch {
        let ch = &tel.channels[c];
        let act = ch.active != 0;
        if act {
            any_active = true;
        }
        let freq_q = if act {
            (ch.freq_hz * 0.5).round() as u64
        } else {
            0
        };
        let word = (u64::from(act) << 24)
            | ((ch.duty_or_mode as u64) << 16)
            | (freq_q & 0xFFFF);
        hash ^= word.wrapping_mul(0x9e37_79b9_7f4a_7c15_u64.rotate_left((c * 7) as u32));
        hash = hash.rotate_left(11).wrapping_mul(0x0100_0193);
    }

    if any_active {
        hash | 1
    } else {
        0
    }
}
