//! Stateful `DualDeckMixer` driving pre-transition `Deck A` tail playback,
//! active Dual-Deck Beatmatch / Vinyl Sync / Filter Wash / Classic Fade mixing,
//! `p = 0.50` midpoint metadata handoff, and post-transition `Deck B` intro drain.

use std::collections::VecDeque;

use crate::cue_scanner::{analyze_pcm_s16, DeckCueAnalysis};
use crate::filter::{DeckChannelStrip, StereoWashDelay};
use crate::planner::{
    evaluate_mix_curves, plan_transition, PlannedTransitionKind, TransitionPlan,
};
use crate::resampler::VinylResampler;
use crate::wsola::WsolaStretcher;
use crate::{CrossfadeMode, DjConfig, DjTelemetry};

const SUB_BLOCK_FRAMES: usize = 128;
const MAX_PLAYHEAD_SPANS: usize = 4096;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MixerPhase {
    /// Normal single-track playback (`0%` CPU overhead).
    Idle,
    /// Capturing `Deck A`'s outgoing tail (`~18s` max).
    CapturingDeckA,
    /// `Deck A` tail captured; capturing `Deck B`'s incoming intro before planning.
    CapturingDeckB,
    /// Playing `Deck A` bit-exact prior to `plan.mix_start_a_frame`.
    PreMixDeckA,
    /// Active dual-deck transition (`0.0 <= p < 1.0`).
    Mixing,
    /// Draining remaining buffered `Deck B` intro frames bit-exact after `p == 1.0`.
    DrainingDeckB,
}

/// Latency-compensated playhead & telemetry snapshot at the speaker-front read pointer.
#[derive(Clone, Copy, Debug, Default)]
pub struct PlayheadSnapshot {
    /// `true` once the audible stream has reached or passed the `p = 0.50` midpoint (`Deck B` foreground).
    pub is_deck_b_foreground: bool,
    /// Audible elapsed position (ms) of the active foreground deck.
    pub audible_elapsed_ms: u32,
    /// Audible remaining time (ms) of the active foreground deck (`0` if total duration was not supplied).
    pub audible_remaining_ms: u32,
    /// Audible elapsed position (ms) of `Deck A` (`Track 1`).
    pub deck_a_elapsed_ms: u32,
    /// Audible remaining time (ms) of `Deck A` (`Track 1`).
    pub deck_a_remaining_ms: u32,
    /// Audible elapsed position (ms) of `Deck B` (`Track 2`).
    pub deck_b_elapsed_ms: u32,
    /// Synchronized DJ telemetry at this exact audible sample.
    pub telemetry: DjTelemetry,
}

#[derive(Clone, Copy, Debug)]
struct RenderedSpan {
    start_out_frame: u64,
    len_frames: u32,
    is_deck_b_foreground: bool,
    a_frame_start: f64,
    a_frame_end: f64,
    b_frame_start: f64,
    b_frame_end: f64,
    telemetry: DjTelemetry,
}

pub struct DualDeckMixer {
    cfg: DjConfig,
    sample_rate: u32,
    phase: MixerPhase,

    // Pre-allocated PCM S16 stereo buffers (`~18s` Deck A + `~15s` Deck B + lossless spill FIFO)
    deck_a_pcm: Vec<i16>,
    deck_b_pcm: Vec<i16>,
    spill_pcm: Vec<i16>,
    spill_cursor: usize,
    deck_a_cursor: usize,
    deck_b_cursor: usize,
    deck_b_eof: bool,

    // Exact source-frame accounting for latency-compensated playhead math
    deck_a_capture_start_frame: u64,
    deck_a_duration_ms: u64,
    deck_b_duration_ms: u64,
    deck_a_spilled_frames: u64,
    deck_a_emitted_spill_frames: u64,
    deck_b_compacted_frames: u64,
    nsf_b_virtual_frames: f64,
    last_tempo_a: f32,
    last_tempo_b: f32,
    total_emitted_frames: u64,
    midpoint_handed_off: bool,
    playhead_spans: VecDeque<RenderedSpan>,

    // Track metadata for Same-Album Gapless check
    album_a: String,
    track_num_a: i32,
    album_b: String,
    track_num_b: i32,
    shuffle_active: bool,
    manual_quick_cut: bool,
    deck_b_is_native_nsf: bool,

    cue_a: DeckCueAnalysis,
    cue_b: DeckCueAnalysis,
    hint_bpm_a: Option<(f32, f32)>,
    hint_bpm_b: Option<(f32, f32)>,
    plan: Option<TransitionPlan>,
    mix_frames_done: usize,
    midpoint_signaled: bool,

    // Per-deck DSP processors (only clocked during `MixerPhase::Mixing`)
    wsola_a: WsolaStretcher,
    wsola_b: WsolaStretcher,
    vinyl_a: VinylResampler,
    vinyl_b: VinylResampler,
    strip_a: DeckChannelStrip,
    strip_b: DeckChannelStrip,
    wash_a: StereoWashDelay,

    // Pre-allocated sub-block scratch buffers (zero heap allocs during `render_block_s16`)
    scratch_in_a: Vec<f32>,
    scratch_in_b: Vec<f32>,
    scratch_out_a: Vec<f32>,
    scratch_out_b: Vec<f32>,

    last_telemetry: DjTelemetry,
    recommended_nsf_tempo_b: f32,
}

impl DualDeckMixer {
    /// Max captured outgoing tail (`18s` @ `48 kHz` = `864_000` stereo frames = `3.45 MiB`).
    pub const MAX_DECK_A_SECONDS: usize = 18;
    /// Max captured incoming intro (`15s` @ `48 kHz` = `720_000` stereo frames = `2.88 MiB`).
    pub const MAX_DECK_B_SECONDS: usize = 15;

    pub fn new(sample_rate: u32) -> Self {
        let sr = sample_rate.clamp(8_000, 192_000);
        Self {
            cfg: DjConfig::default(),
            sample_rate: sr,
            phase: MixerPhase::Idle,
            deck_a_pcm: Vec::new(),
            deck_b_pcm: Vec::new(),
            spill_pcm: Vec::new(),
            spill_cursor: 0,
            deck_a_cursor: 0,
            deck_b_cursor: 0,
            deck_b_eof: false,
            deck_a_capture_start_frame: 0,
            deck_a_duration_ms: 0,
            deck_b_duration_ms: 0,
            deck_a_spilled_frames: 0,
            deck_a_emitted_spill_frames: 0,
            deck_b_compacted_frames: 0,
            nsf_b_virtual_frames: 0.0,
            last_tempo_a: 1.0,
            last_tempo_b: 1.0,
            total_emitted_frames: 0,
            midpoint_handed_off: false,
            playhead_spans: VecDeque::with_capacity(MAX_PLAYHEAD_SPANS),
            album_a: String::new(),
            track_num_a: 0,
            album_b: String::new(),
            track_num_b: 0,
            shuffle_active: false,
            manual_quick_cut: false,
            deck_b_is_native_nsf: false,
            cue_a: DeckCueAnalysis::default(),
            cue_b: DeckCueAnalysis::default(),
            hint_bpm_a: None,
            hint_bpm_b: None,
            plan: None,
            mix_frames_done: 0,
            midpoint_signaled: false,
            wsola_a: WsolaStretcher::new(sr),
            wsola_b: WsolaStretcher::new(sr),
            vinyl_a: VinylResampler::new(),
            vinyl_b: VinylResampler::new(),
            strip_a: DeckChannelStrip::new(sr),
            strip_b: DeckChannelStrip::new(sr),
            wash_a: StereoWashDelay::new(sr),
            scratch_in_a: vec![0.0; SUB_BLOCK_FRAMES * 8 * 2],
            scratch_in_b: vec![0.0; SUB_BLOCK_FRAMES * 8 * 2],
            scratch_out_a: vec![0.0; SUB_BLOCK_FRAMES * 2],
            scratch_out_b: vec![0.0; SUB_BLOCK_FRAMES * 2],
            last_telemetry: DjTelemetry::default(),
            recommended_nsf_tempo_b: 1.0,
        }
    }

    #[inline]
    pub fn config(&self) -> DjConfig {
        self.cfg
    }

    pub fn set_config(&mut self, cfg: DjConfig) {
        self.cfg = cfg;
        if cfg.mode != CrossfadeMode::Off && self.deck_a_pcm.capacity() == 0 {
            let sr = self.sample_rate as usize;
            self.deck_a_pcm.reserve(sr * Self::MAX_DECK_A_SECONDS * 2);
            self.deck_b_pcm.reserve(sr * Self::MAX_DECK_B_SECONDS * 2);
        }
        // If user disables Crossfade while frames are buffered in `CapturingDeckA`/`CapturingDeckB`,
        // switch cleanly to bit-exact gapless drain instead of dropping buffered audio!
        if cfg.mode == CrossfadeMode::Off
            && matches!(self.phase, MixerPhase::CapturingDeckA | MixerPhase::CapturingDeckB)
        {
            self.flush_to_gapless_drain();
        }
    }

    #[inline]
    pub fn phase(&self) -> MixerPhase {
        self.phase
    }

    #[inline]
    pub fn telemetry(&self) -> DjTelemetry {
        self.last_telemetry
    }

    #[inline]
    pub fn recommended_nsf_tempo_b(&self) -> f32 {
        self.recommended_nsf_tempo_b
    }

    #[inline]
    pub fn total_emitted_frames(&self) -> u64 {
        self.total_emitted_frames
    }

    /// Supply source track timing (`Deck A` capture-start elapsed & full track duration, `Deck B` full duration)
    /// so `PlayheadSnapshot` and chunk timestamps report exact `elapsed_ms` / `remaining_ms`.
    pub fn set_track_timing(
        &mut self,
        deck_a_start_ms: u32,
        deck_a_duration_ms: u32,
        deck_b_duration_ms: u32,
    ) {
        if deck_a_duration_ms > 0 || self.phase == MixerPhase::CapturingDeckA {
            self.deck_a_capture_start_frame =
                ((deck_a_start_ms as u64) * (self.sample_rate as u64)) / 1000;
            self.deck_a_duration_ms = deck_a_duration_ms as u64;
        }
        if deck_b_duration_ms > 0 {
            self.deck_b_duration_ms = deck_b_duration_ms as u64;
        }
    }

    #[inline]
    pub fn set_deck_b_duration_ms(&mut self, deck_b_duration_ms: u32) {
        if deck_b_duration_ms > 0 {
            self.deck_b_duration_ms = deck_b_duration_ms as u64;
        }
    }

    pub fn reset(&mut self) {
        self.phase = MixerPhase::Idle;
        self.deck_a_pcm.clear();
        self.deck_b_pcm.clear();
        self.spill_pcm.clear();
        self.spill_cursor = 0;
        self.deck_a_cursor = 0;
        self.deck_b_cursor = 0;
        self.deck_b_eof = false;
        self.deck_a_capture_start_frame = 0;
        self.deck_a_duration_ms = 0;
        self.deck_b_duration_ms = 0;
        self.deck_a_spilled_frames = 0;
        self.deck_a_emitted_spill_frames = 0;
        self.deck_b_compacted_frames = 0;
        self.nsf_b_virtual_frames = 0.0;
        self.last_tempo_a = 1.0;
        self.last_tempo_b = 1.0;
        self.total_emitted_frames = 0;
        self.midpoint_handed_off = false;
        self.playhead_spans.clear();
        self.manual_quick_cut = false;
        self.deck_b_is_native_nsf = false;
        self.hint_bpm_a = None;
        self.hint_bpm_b = None;
        self.plan = None;
        self.mix_frames_done = 0;
        self.midpoint_signaled = false;
        self.wsola_a.reset();
        self.wsola_b.reset();
        self.vinyl_a.reset();
        self.vinyl_b.reset();
        self.last_telemetry = DjTelemetry::default();
        self.recommended_nsf_tempo_b = 1.0;
    }

    /// Enter `MixerPhase::Idle` once `DrainingDeckB` completes while preserving `playhead_spans`
    /// so downstream `pcmbuf` / WebAudio buffers still draining the mix report exact playhead & telemetry!
    fn finish_drain_to_idle(&mut self) {
        self.phase = MixerPhase::Idle;
        self.deck_a_pcm.clear();
        self.deck_b_pcm.clear();
        self.spill_pcm.clear();
        self.spill_cursor = 0;
        self.deck_a_cursor = 0;
        self.deck_b_cursor = 0;
        self.deck_b_eof = false;
        self.manual_quick_cut = false;
        self.deck_b_is_native_nsf = false;
        self.hint_bpm_a = None;
        self.hint_bpm_b = None;
        self.plan = None;
        self.mix_frames_done = 0;
        self.midpoint_signaled = false;
        self.wsola_a.reset();
        self.wsola_b.reset();
        self.vinyl_a.reset();
        self.vinyl_b.reset();
        self.last_telemetry = DjTelemetry::default();
        self.recommended_nsf_tempo_b = 1.0;
    }

    /// Convert any buffered `CapturingDeckA` / `CapturingDeckB` audio into a lossless bit-exact
    /// `Gapless` drain so 100% of captured frames reach the output sink without truncation.
    pub fn flush_to_gapless_drain(&mut self) {
        let has_spill = self.spill_cursor * 2 < self.spill_pcm.len();
        let has_a = self.deck_a_cursor * 2 < self.deck_a_pcm.len();
        let has_b = self.deck_b_cursor * 2 < self.deck_b_pcm.len();
        if !has_spill && !has_a && !has_b {
            self.reset();
            return;
        }
        let frames_a = self.deck_a_pcm.len() / 2;
        self.plan = Some(TransitionPlan {
            kind: PlannedTransitionKind::Gapless,
            mode: CrossfadeMode::Off,
            sample_rate: self.sample_rate,
            mix_start_a_frame: frames_a,
            cue_out_a_frame: frames_a,
            start_b_frame: 0,
            mix_output_frames: 0,
            bpm_a: 0.0,
            folded_bpm_b: 0.0,
            raw_bpm_b: 0.0,
            bars: 0,
        });
        self.deck_b_eof = true;
        self.last_telemetry = DjTelemetry::default();
        self.recommended_nsf_tempo_b = 1.0;
        self.phase = if has_spill || has_a {
            MixerPhase::PreMixDeckA
        } else {
            MixerPhase::DrainingDeckB
        };
    }

    pub fn ensure_sample_rate(&mut self, sr: u32) {
        let clamped = sr.clamp(8_000, 192_000);
        if clamped != self.sample_rate {
            self.sample_rate = clamped;
            self.wsola_a = WsolaStretcher::new(clamped);
            self.wsola_b = WsolaStretcher::new(clamped);
            self.strip_a = DeckChannelStrip::new(clamped);
            self.strip_b = DeckChannelStrip::new(clamped);
            self.wash_a = StereoWashDelay::new(clamped);
        }
    }

    /// Begin capturing `Deck A`'s outgoing tail (`~18s` before EOF or `~2s` on manual `Next`).
    pub fn begin_deck_a_capture(
        &mut self,
        sample_rate: u32,
        album_a: &str,
        track_num_a: i32,
        shuffle_active: bool,
        manual_quick_cut: bool,
    ) {
        self.ensure_sample_rate(sample_rate);
        self.deck_a_pcm.clear();
        self.deck_b_pcm.clear();
        self.spill_pcm.clear();
        self.spill_cursor = 0;
        self.deck_a_cursor = 0;
        self.deck_b_cursor = 0;
        self.deck_b_eof = false;
        self.deck_a_capture_start_frame = 0;
        self.deck_a_duration_ms = 0;
        self.deck_b_duration_ms = 0;
        self.deck_a_spilled_frames = 0;
        self.deck_a_emitted_spill_frames = 0;
        self.deck_b_compacted_frames = 0;
        self.nsf_b_virtual_frames = 0.0;
        self.last_tempo_a = 1.0;
        self.last_tempo_b = 1.0;
        self.total_emitted_frames = 0;
        self.midpoint_handed_off = false;
        self.playhead_spans.clear();
        self.album_a.clear();
        self.album_a.push_str(album_a);
        self.track_num_a = track_num_a;
        self.shuffle_active = shuffle_active;
        self.manual_quick_cut = manual_quick_cut;
        self.plan = None;
        self.mix_frames_done = 0;
        self.midpoint_signaled = false;
        self.last_telemetry = DjTelemetry::default();
        self.phase = MixerPhase::CapturingDeckA;
    }

    /// Append decoded `Deck A` stereo S16 frames during `CapturingDeckA` into lossless internal
    /// FIFO (`deck_a_pcm` + `spill_pcm`). Never drops or overwrites unread frames regardless of burst size!
    pub fn push_deck_a_frames(&mut self, stereo_s16: &[i16]) {
        let even_len = stereo_s16.len() & !1;
        if even_len == 0 {
            return;
        }
        let max_samples = (self.sample_rate as usize) * Self::MAX_DECK_A_SECONDS * 2;
        let total = self.deck_a_pcm.len() + even_len;
        if total > max_samples {
            let overflow = total - max_samples;
            // Compact already-popped spill prefix before appending new overflow
            if self.spill_cursor > 0 {
                let drop_s = (self.spill_cursor * 2).min(self.spill_pcm.len());
                self.spill_pcm.drain(..drop_s);
                self.spill_cursor = 0;
            }
            let from_a = overflow.min(self.deck_a_pcm.len()) & !1;
            if from_a > 0 {
                self.spill_pcm.extend_from_slice(&self.deck_a_pcm[..from_a]);
                self.deck_a_pcm.drain(..from_a);
                self.deck_a_spilled_frames += (from_a / 2) as u64;
            }
            let rem_overflow = overflow.saturating_sub(from_a) & !1;
            if rem_overflow > 0 {
                let spill_from_src = rem_overflow.min(even_len);
                self.spill_pcm.extend_from_slice(&stereo_s16[..spill_from_src]);
                self.deck_a_spilled_frames += (spill_from_src / 2) as u64;
                if spill_from_src < even_len {
                    self.deck_a_pcm
                        .extend_from_slice(&stereo_s16[spill_from_src..even_len]);
                }
                return;
            }
        }
        self.deck_a_pcm.extend_from_slice(&stereo_s16[..even_len]);
    }

    /// Pop up to `out_s16.len() / 2` older spilled `Deck A` stereo frames in exact sample order,
    /// returning `(frames_popped, chunk_elapsed_ms)`.
    pub fn pop_spill_s16(&mut self, out_s16: &mut [i16]) -> (usize, u32) {
        let cap_frames = out_s16.len() / 2;
        let avail_frames = (self.spill_pcm.len() / 2).saturating_sub(self.spill_cursor);
        let n = cap_frames.min(avail_frames);
        if n == 0 {
            let el_ms = self.frames_to_ms(
                (self.deck_a_capture_start_frame + self.deck_a_emitted_spill_frames) as f64,
            );
            return (0, el_ms);
        }
        let src_off = self.spill_cursor * 2;
        out_s16[..n * 2].copy_from_slice(&self.spill_pcm[src_off..src_off + n * 2]);
        self.spill_cursor += n;
        if self.spill_cursor * 2 >= self.spill_pcm.len() {
            self.spill_pcm.clear();
            self.spill_cursor = 0;
        }
        let a0 = (self.deck_a_capture_start_frame + self.deck_a_emitted_spill_frames) as f64;
        self.deck_a_emitted_spill_frames += n as u64;
        let a1 = (self.deck_a_capture_start_frame + self.deck_a_emitted_spill_frames) as f64;
        self.record_span(n, false, a0, a1, 0.0, 0.0, DjTelemetry::default());
        (n, self.frames_to_ms(a1))
    }

    /// Append decoded `Deck A` stereo S16 frames during `CapturingDeckA`, invoking `spill_cb`
    /// with any overflow older than `MAX_DECK_A_SECONDS`.
    pub fn push_deck_a_tail(&mut self, stereo_s16: &[i16], mut spill_cb: impl FnMut(&[i16])) {
        self.push_deck_a_frames(stereo_s16);
        if self.spill_cursor * 2 < self.spill_pcm.len() {
            let start = self.spill_cursor * 2;
            let avail_frames = (self.spill_pcm.len() - start) / 2;
            spill_cb(&self.spill_pcm[start..]);
            let a0 = (self.deck_a_capture_start_frame + self.deck_a_emitted_spill_frames) as f64;
            self.deck_a_emitted_spill_frames += avail_frames as u64;
            let a1 = (self.deck_a_capture_start_frame + self.deck_a_emitted_spill_frames) as f64;
            self.spill_pcm.clear();
            self.spill_cursor = 0;
            self.record_span(
                avail_frames,
                false,
                a0,
                a1,
                0.0,
                0.0,
                DjTelemetry::default(),
            );
        }
    }

    /// Supply cached full-track `(bpm, confidence)` hints from `uipod-track-intel` so tracks
    /// with ambient/drumless outros or intros still beatmatch accurately.
    pub fn set_deck_bpm_hints(
        &mut self,
        deck_a: Option<(f32, f32)>,
        deck_b: Option<(f32, f32)>,
    ) {
        if deck_a.is_some() {
            self.hint_bpm_a = deck_a;
        }
        if deck_b.is_some() {
            self.hint_bpm_b = deck_b;
        }
    }

    fn apply_bpm_hint(cue: &mut DeckCueAnalysis, hint: Option<(f32, f32)>) {
        if let Some((hbpm, hconf)) = hint {
            if (60.0..=200.0).contains(&hbpm) && hconf >= 0.55 {
                if cue.confidence < 0.62 || (cue.bpm - hbpm).abs() > 4.0 {
                    cue.bpm = hbpm;
                    cue.confidence = cue.confidence.max(hconf * 0.95);
                }
            }
        }
    }

    /// Finish `Deck A` capture, run `cue_scanner` on `Deck A`'s tail, and move to `CapturingDeckB`.
    pub fn finish_deck_a_and_begin_deck_b(
        &mut self,
        album_b: &str,
        track_num_b: i32,
        deck_b_is_native_nsf: bool,
    ) {
        self.album_b.clear();
        self.album_b.push_str(album_b);
        self.track_num_b = track_num_b;
        self.deck_b_is_native_nsf = deck_b_is_native_nsf;
        self.cue_a = analyze_pcm_s16(&self.deck_a_pcm, self.sample_rate);
        Self::apply_bpm_hint(&mut self.cue_a, self.hint_bpm_a);
        // Ensure `deck_a_duration_ms` is at least the exact total decoded frame length of Deck A
        let total_a_frames = self.deck_a_capture_start_frame
            + self.deck_a_spilled_frames
            + (self.deck_a_pcm.len() / 2) as u64;
        let true_a_ms = (total_a_frames * 1000) / (self.sample_rate as u64);
        if self.deck_a_duration_ms < true_a_ms {
            self.deck_a_duration_ms = true_a_ms;
        }
        self.phase = MixerPhase::CapturingDeckB;
    }

    /// Append incoming `Deck B` stereo S16 frames. Once enough intro is buffered (or `is_eof`),
    /// computes the `TransitionPlan` and enters `PreMixDeckA` / `Mixing`.
    /// Returns `true` once the transition plan is ready to render!
    pub fn push_deck_b_frames(&mut self, stereo_s16: &[i16], is_eof: bool) -> bool {
        let even_len = stereo_s16.len() & !1;
        if even_len > 0 {
            self.deck_b_pcm.extend_from_slice(&stereo_s16[..even_len]);
        }
        if is_eof {
            self.deck_b_eof = true;
        }

        if self.phase == MixerPhase::CapturingDeckB {
            let sr = self.sample_rate as usize;
            let has_bpm_hint_b = self
                .hint_bpm_b
                .is_some_and(|(bpm, conf)| (60.0..=200.0).contains(&bpm) && conf >= 0.55);
            let min_frames = if self.manual_quick_cut {
                (sr * 35) / 100 // 350 ms for instant manual Quick Cut
            } else if self.cfg.mode == CrossfadeMode::ClassicFade
                || self.cfg.mode == CrossfadeMode::Off
            {
                (sr * 15) / 10 // 1.5s to detect leading digital silence
            } else if has_bpm_hint_b {
                (sr * 32) / 10 // 3.2s (~1-2 bars to lock downbeat phase with cached BPM)
            } else {
                (sr * 55) / 10 // 5.5s (~2.5-3 bars for cold onset BPM + downbeat scan)
            };
            if self.deck_b_pcm.len() >= min_frames * 2 || self.deck_b_eof {
                self.commit_transition_plan();
                return true;
            }
            return false;
        }
        true
    }

    fn commit_transition_plan(&mut self) {
        self.cue_b = analyze_pcm_s16(&self.deck_b_pcm, self.sample_rate);
        Self::apply_bpm_hint(&mut self.cue_b, self.hint_bpm_b);
        // If `Deck B` is still streaming (`!deck_b_eof`), expose its full track length to `plan_transition`
        // so early plan commit (`1.5s..5.5s` intro lookahead) never truncates `8s`/`12s` fade lengths!
        if !self.deck_b_eof {
            let sr = self.sample_rate as usize;
            let expected_b_frames = if self.deck_b_duration_ms > 0 {
                ((self.deck_b_duration_ms as usize) * sr) / 1000
            } else {
                sr * 30
            };
            if self.cue_b.total_frames < expected_b_frames {
                self.cue_b.total_frames = expected_b_frames;
            }
        }
        let plan = plan_transition(
            &self.cfg,
            &self.cue_a,
            &self.cue_b,
            &self.album_a,
            self.track_num_a,
            &self.album_b,
            self.track_num_b,
            self.shuffle_active,
            self.manual_quick_cut,
        );

        self.deck_a_cursor = 0;
        self.deck_b_cursor = plan.start_b_frame.min(self.deck_b_pcm.len() / 2);
        self.nsf_b_virtual_frames = self.deck_b_cursor as f64;
        self.mix_frames_done = 0;
        self.midpoint_signaled = false;
        self.midpoint_handed_off = false;
        self.last_tempo_a = 1.0;
        self.last_tempo_b = 1.0;

        // Reset per-deck DSP engines cleanly for the upcoming transition
        self.wsola_a.reset();
        self.wsola_b.reset();
        self.vinyl_a.reset();
        self.vinyl_b.reset();

        let step0 = evaluate_mix_curves(&plan, 0.0);
        self.strip_a
            .reset(self.sample_rate, step0.gain_a, step0.hpf_a_hz);
        self.strip_b
            .reset(self.sample_rate, step0.gain_b, step0.hpf_b_hz);
        self.wash_a.clear();
        self.wash_a.configure(self.sample_rate, plan.bpm_a);

        let has_spill = self.spill_cursor * 2 < self.spill_pcm.len();
        self.phase = if plan.kind == PlannedTransitionKind::Gapless {
            self.deck_b_cursor = 0;
            self.nsf_b_virtual_frames = 0.0;
            MixerPhase::PreMixDeckA
        } else if has_spill || plan.mix_start_a_frame > 0 {
            MixerPhase::PreMixDeckA
        } else {
            MixerPhase::Mixing
        };
        self.plan = Some(plan);
    }

    #[inline]
    fn frames_to_ms(&self, frames: f64) -> u32 {
        if frames <= 0.0 {
            0
        } else {
            ((frames * 1000.0) / (self.sample_rate.max(1) as f64)).round() as u32
        }
    }

    #[inline]
    fn current_mixer_frame_a(&self) -> f64 {
        let base_a = (self.deck_a_capture_start_frame + self.deck_a_spilled_frames) as f64;
        let dsp_unplayed = if self.phase == MixerPhase::Mixing {
            match self.plan.map(|p| p.mode).unwrap_or(CrossfadeMode::Off) {
                CrossfadeMode::DjVinylSync => self.vinyl_a.unplayed_input_frames(),
                CrossfadeMode::DjMasterTempo => self.wsola_a.unplayed_input_frames(self.last_tempo_a),
                _ => 0.0,
            }
        } else {
            0.0
        };
        base_a + (self.deck_a_cursor as f64 - dsp_unplayed).max(0.0)
    }

    #[inline]
    fn current_mixer_frame_b(&self) -> f64 {
        if self.deck_b_is_native_nsf && self.phase == MixerPhase::Mixing {
            return self.nsf_b_virtual_frames.max(0.0);
        }
        let raw_b = (self.deck_b_compacted_frames + self.deck_b_cursor as u64) as f64;
        let dsp_unplayed = if self.phase == MixerPhase::Mixing && !self.deck_b_is_native_nsf {
            match self.plan.map(|p| p.mode).unwrap_or(CrossfadeMode::Off) {
                CrossfadeMode::DjVinylSync => self.vinyl_b.unplayed_input_frames(),
                CrossfadeMode::DjMasterTempo => self.wsola_b.unplayed_input_frames(self.last_tempo_b),
                _ => 0.0,
            }
        } else {
            0.0
        };
        (raw_b - dsp_unplayed).max(0.0)
    }

    /// Total `Deck B` source frames consumed from Track 2 so far.
    #[inline]
    pub fn deck_b_consumed_frames(&self) -> u64 {
        self.current_mixer_frame_b().round().max(0.0) as u64
    }

    fn record_span(
        &mut self,
        len_frames: usize,
        is_deck_b_foreground: bool,
        a_frame_start: f64,
        a_frame_end: f64,
        b_frame_start: f64,
        b_frame_end: f64,
        telemetry: DjTelemetry,
    ) {
        if len_frames == 0 {
            return;
        }
        let start_out = self.total_emitted_frames;
        self.total_emitted_frames += len_frames as u64;
        if let Some(back) = self.playhead_spans.back_mut() {
            // Coalesce contiguous spans with identical foreground deck & inactive telemetry (e.g. PreMix/Drain)
            if back.start_out_frame + (back.len_frames as u64) == start_out
                && back.is_deck_b_foreground == is_deck_b_foreground
                && back.telemetry.active == 0
                && telemetry.active == 0
                && (back.a_frame_end - a_frame_start).abs() < 1.5
                && (back.b_frame_end - b_frame_start).abs() < 1.5
            {
                back.len_frames = back.len_frames.saturating_add(len_frames as u32);
                back.a_frame_end = a_frame_end;
                back.b_frame_end = b_frame_end;
                return;
            }
        }
        if self.playhead_spans.len() >= MAX_PLAYHEAD_SPANS {
            self.playhead_spans.pop_front();
        }
        self.playhead_spans.push_back(RenderedSpan {
            start_out_frame: start_out,
            len_frames: len_frames as u32,
            is_deck_b_foreground,
            a_frame_start,
            a_frame_end,
            b_frame_start,
            b_frame_end,
            telemetry,
        });
    }

    /// Compute the exact speaker-front playhead & telemetry given `downstream_frames`
    /// of rendered audio still queued between `DualDeckMixer` and the hardware DAC.
    pub fn playhead_at_delay_frames(&self, downstream_frames: usize) -> PlayheadSnapshot {
        let target_out = self
            .total_emitted_frames
            .saturating_sub(downstream_frames as u64);

        let (is_b_fg, a_frame, b_frame, tel) = if let Some(first) = self.playhead_spans.front() {
            if target_out <= first.start_out_frame {
                (
                    first.is_deck_b_foreground,
                    first.a_frame_start,
                    first.b_frame_start,
                    first.telemetry,
                )
            } else {
                let mut found = None;
                for sp in self.playhead_spans.iter().rev() {
                    let end_out = sp.start_out_frame + sp.len_frames as u64;
                    if target_out >= sp.start_out_frame && target_out <= end_out {
                        let frac = if sp.len_frames > 0 {
                            ((target_out - sp.start_out_frame) as f64) / (sp.len_frames as f64)
                        } else {
                            1.0
                        };
                        let af = sp.a_frame_start + (sp.a_frame_end - sp.a_frame_start) * frac;
                        let bf = sp.b_frame_start + (sp.b_frame_end - sp.b_frame_start) * frac;
                        found = Some((sp.is_deck_b_foreground, af, bf, sp.telemetry));
                        break;
                    }
                }
                if let Some(res) = found {
                    res
                } else if let Some(last) = self.playhead_spans.back() {
                    (
                        last.is_deck_b_foreground,
                        last.a_frame_end,
                        last.b_frame_end,
                        last.telemetry,
                    )
                } else {
                    (
                        self.midpoint_handed_off,
                        self.current_mixer_frame_a(),
                        self.current_mixer_frame_b(),
                        self.last_telemetry,
                    )
                }
            }
        } else {
            (
                self.midpoint_handed_off,
                self.current_mixer_frame_a(),
                self.current_mixer_frame_b(),
                self.last_telemetry,
            )
        };

        let a_el = self.frames_to_ms(a_frame);
        let b_el = self.frames_to_ms(b_frame);
        let a_rem = if self.deck_a_duration_ms > 0 {
            (self.deck_a_duration_ms.saturating_sub(a_el as u64)) as u32
        } else {
            0
        };
        let b_rem = if self.deck_b_duration_ms > 0 {
            (self.deck_b_duration_ms.saturating_sub(b_el as u64)) as u32
        } else {
            0
        };

        let (fg_el, fg_rem) = if is_b_fg {
            (b_el, b_rem)
        } else {
            (a_el, a_rem)
        };

        PlayheadSnapshot {
            is_deck_b_foreground: is_b_fg,
            audible_elapsed_ms: fg_el,
            audible_remaining_ms: fg_rem,
            deck_a_elapsed_ms: a_el,
            deck_a_remaining_ms: a_rem,
            deck_b_elapsed_ms: b_el,
            telemetry: tel,
        }
    }

    #[inline]
    pub fn take_midpoint_reached(&mut self) -> bool {
        if self.midpoint_signaled {
            self.midpoint_signaled = false;
            true
        } else {
            false
        }
    }

    /// Pull up to `max_frames` stereo S16 frames from the active transition pipeline.
    /// Returns `(frames_written, midpoint_crossed_in_this_call)`.
    pub fn render_s16(&mut self, out_s16: &mut [i16]) -> (usize, bool) {
        let (written, crossed, _) = self.render_s16_timed(out_s16);
        (written, crossed)
    }

    /// Pull up to `max_frames` stereo S16 frames from the active transition pipeline,
    /// splitting cleanly at the `p = 0.50` midpoint so pre-midpoint (`Deck A`) and post-midpoint (`Deck B`)
    /// frames never share a chunk, and returning `(frames_written, midpoint_crossed, chunk_elapsed_ms)`.
    pub fn render_s16_timed(&mut self, out_s16: &mut [i16]) -> (usize, bool, u32) {
        let cap_frames = out_s16.len() / 2;
        if cap_frames == 0 {
            return (0, false, 0);
        }

        let mut written = 0usize;
        let mut crossed_midpoint = false;

        while written < cap_frames {
            // Drain any unread lossless `Deck A` spill frames before `PreMixDeckA`
            if self.phase != MixerPhase::Idle
                && self.phase != MixerPhase::CapturingDeckA
                && self.spill_cursor * 2 < self.spill_pcm.len()
            {
                let dst_off = written * 2;
                let (n, _) = self.pop_spill_s16(&mut out_s16[dst_off..]);
                written += n;
                if written >= cap_frames {
                    break;
                }
            }

            match self.phase {
                MixerPhase::Idle | MixerPhase::CapturingDeckA | MixerPhase::CapturingDeckB => {
                    break;
                }
                MixerPhase::PreMixDeckA => {
                    let Some(plan) = self.plan else {
                        self.reset();
                        break;
                    };
                    let end_a = if plan.kind == PlannedTransitionKind::Gapless {
                        self.deck_a_pcm.len() / 2
                    } else {
                        plan.mix_start_a_frame.min(self.deck_a_pcm.len() / 2)
                    };

                    if self.deck_a_cursor < end_a {
                        let a0 = self.current_mixer_frame_a();
                        let b0 = self.current_mixer_frame_b();
                        let avail = end_a - self.deck_a_cursor;
                        let n = avail.min(cap_frames - written);
                        let src_off = self.deck_a_cursor * 2;
                        let dst_off = written * 2;
                        out_s16[dst_off..dst_off + n * 2]
                            .copy_from_slice(&self.deck_a_pcm[src_off..src_off + n * 2]);
                        self.deck_a_cursor += n;
                        written += n;
                        let a1 = self.current_mixer_frame_a();
                        self.record_span(n, false, a0, a1, b0, b0, DjTelemetry::default());
                    } else if plan.kind == PlannedTransitionKind::Gapless {
                        self.last_telemetry = DjTelemetry::default();
                        if self.deck_b_cursor < self.deck_b_pcm.len() / 2 {
                            crossed_midpoint = true;
                            self.midpoint_signaled = true;
                            self.midpoint_handed_off = true;
                            self.phase = MixerPhase::DrainingDeckB;
                            if written > 0 {
                                break;
                            }
                        } else {
                            self.finish_drain_to_idle();
                            break;
                        }
                    } else {
                        self.phase = MixerPhase::Mixing;
                    }
                }
                MixerPhase::Mixing => {
                    let Some(plan) = self.plan else {
                        self.phase = MixerPhase::DrainingDeckB;
                        continue;
                    };
                    let total_mix = plan.mix_output_frames.max(1);
                    if self.mix_frames_done >= total_mix {
                        self.last_telemetry = DjTelemetry::default();
                        self.recommended_nsf_tempo_b = 1.0;
                        self.phase = MixerPhase::DrainingDeckB;
                        if !self.midpoint_handed_off {
                            crossed_midpoint = true;
                            self.midpoint_signaled = true;
                            self.midpoint_handed_off = true;
                            break;
                        }
                        continue;
                    }

                    let rem_mix = total_mix - self.mix_frames_done;
                    let blk = (cap_frames - written).min(SUB_BLOCK_FRAMES).min(rem_mix);

                    // If Deck B hasn't supplied enough live samples yet and hasn't hit EOF, pause pull
                    let total_b_frames = self.deck_b_pcm.len() / 2;
                    if !self.deck_b_eof && self.deck_b_cursor + blk * 4 > total_b_frames {
                        break;
                    }

                    let prev_p = (self.mix_frames_done as f32) / (total_mix as f32);
                    let mid_p = ((self.mix_frames_done + blk / 2) as f32) / (total_mix as f32);
                    let next_p = ((self.mix_frames_done + blk) as f32) / (total_mix as f32);

                    let curve = evaluate_mix_curves(&plan, mid_p);
                    self.last_tempo_a = curve.tempo_a;
                    self.last_tempo_b = if self.deck_b_is_native_nsf {
                        1.0
                    } else {
                        curve.tempo_b
                    };
                    self.recommended_nsf_tempo_b = if self.deck_b_is_native_nsf {
                        curve.tempo_b
                    } else {
                        1.0
                    };

                    let a0 = self.current_mixer_frame_a();
                    let b0 = self.current_mixer_frame_b();

                    // Allow Deck A to drain all the way to `deck_a_pcm.len() / 2` under `gain_a`
                    // so quiet acoustic/reverb tails past `cue_out_a_frame` are never chopped!
                    let end_a = self.deck_a_pcm.len() / 2;
                    let got_a = Self::pull_deck_subblock(
                        &self.deck_a_pcm,
                        &mut self.deck_a_cursor,
                        end_a,
                        true,
                        plan.mode,
                        false,
                        curve.tempo_a,
                        &mut self.wsola_a,
                        &mut self.vinyl_a,
                        &mut self.scratch_in_a,
                        &mut self.scratch_out_a[..blk * 2],
                    );

                    let eff_tempo_b = if self.deck_b_is_native_nsf {
                        1.0 // Already scaled natively inside `NsfEngine`!
                    } else {
                        curve.tempo_b
                    };
                    let got_b = Self::pull_deck_subblock(
                        &self.deck_b_pcm,
                        &mut self.deck_b_cursor,
                        total_b_frames,
                        self.deck_b_eof,
                        plan.mode,
                        self.deck_b_is_native_nsf,
                        eff_tempo_b,
                        &mut self.wsola_b,
                        &mut self.vinyl_b,
                        &mut self.scratch_in_b,
                        &mut self.scratch_out_b[..blk * 2],
                    );

                    if self.deck_b_is_native_nsf {
                        self.nsf_b_virtual_frames += (got_b as f64) * (curve.tempo_b as f64);
                    }
                    let _ = got_a;

                    self.strip_a.set_targets(curve.gain_a, curve.hpf_a_hz);
                    self.strip_b.set_targets(curve.gain_b, curve.hpf_b_hz);
                    self.wash_a.set_wet_send(curve.wash_wet_a);

                    let dst_base = written * 2;
                    for i in 0..blk {
                        let (al, ar) = self.strip_a.process_frame(
                            self.scratch_out_a[i * 2],
                            self.scratch_out_a[i * 2 + 1],
                        );
                        let (wl, wr) = if plan.kind == PlannedTransitionKind::FilterWash {
                            self.wash_a.step(
                                self.scratch_out_a[i * 2],
                                self.scratch_out_a[i * 2 + 1],
                            )
                        } else {
                            (0.0, 0.0)
                        };
                        let (bl, br) = self.strip_b.process_frame(
                            self.scratch_out_b[i * 2],
                            self.scratch_out_b[i * 2 + 1],
                        );

                        // Soft-clipping safety limiter above `0.92` so loud mastered peaks never clip
                        let sum_l = Self::soft_clip(al + wl + bl);
                        let sum_r = Self::soft_clip(ar + wr + br);
                        out_s16[dst_base + i * 2] =
                            (sum_l * 32767.0).round().clamp(-32768.0, 32767.0) as i16;
                        out_s16[dst_base + i * 2 + 1] =
                            (sum_r * 32767.0).round().clamp(-32768.0, 32767.0) as i16;
                    }

                    self.mix_frames_done += blk;
                    written += blk;

                    let tel = DjTelemetry {
                        active: 1,
                        mix_kind: plan.kind.active_mix_kind() as u8,
                        bass_swap_flash: u8::from(curve.bass_swap_flash),
                        progress: next_p.clamp(0.0, 1.0),
                        bpm_a: plan.bpm_a,
                        bpm_b: plan.folded_bpm_b,
                        current_bpm: curve.current_bpm,
                    };
                    self.last_telemetry = tel;

                    let a1 = self.current_mixer_frame_a();
                    let b1 = self.current_mixer_frame_b();
                    let span_is_b = self.midpoint_handed_off;
                    self.record_span(blk, span_is_b, a0, a1, b0, b1, tel);

                    // Compact already-consumed Deck B prefix periodically so memory stays bounded
                    if self.deck_b_cursor >= 65_536 {
                        let drop_frames = self.deck_b_cursor;
                        self.deck_b_pcm.drain(..drop_frames * 2);
                        self.deck_b_compacted_frames += drop_frames as u64;
                        self.deck_b_cursor = 0;
                    }

                    if prev_p < 0.50 && next_p >= 0.50 && !self.midpoint_handed_off {
                        crossed_midpoint = true;
                        self.midpoint_signaled = true;
                        self.midpoint_handed_off = true;
                        // Split chunk right at `p = 0.50` so Deck B's first post-midpoint chunk
                        // receives the new `pos_key` and Deck B's exact `elapsed_ms`!
                        break;
                    }
                }
                MixerPhase::DrainingDeckB => {
                    let total_b = self.deck_b_pcm.len() / 2;
                    if self.deck_b_cursor < total_b {
                        let a0 = self.current_mixer_frame_a();
                        let b0 = self.current_mixer_frame_b();
                        let avail = total_b - self.deck_b_cursor;
                        let n = avail.min(cap_frames - written);
                        let src_off = self.deck_b_cursor * 2;
                        let dst_off = written * 2;
                        out_s16[dst_off..dst_off + n * 2]
                            .copy_from_slice(&self.deck_b_pcm[src_off..src_off + n * 2]);
                        self.deck_b_cursor += n;
                        if self.deck_b_is_native_nsf {
                            self.nsf_b_virtual_frames += n as f64;
                        }
                        written += n;
                        let b1 = self.current_mixer_frame_b();
                        self.record_span(n, true, a0, a0, b0, b1, DjTelemetry::default());
                    } else {
                        // All buffered Deck B intro frames drained -> back to zero-overhead `Idle`
                        // while preserving `playhead_spans` for downstream sink latency lookback!
                        let final_b_ms = self.frames_to_ms(self.current_mixer_frame_b());
                        self.finish_drain_to_idle();
                        if written == 0 {
                            return (0, crossed_midpoint, final_b_ms);
                        }
                        break;
                    }
                }
            }
        }

        let snap = self.playhead_at_delay_frames(0);
        let chunk_el = if crossed_midpoint {
            // The chunk ending at `crossed_midpoint` is Deck A's final pre-midpoint chunk
            snap.deck_a_elapsed_ms
        } else {
            snap.audible_elapsed_ms
        };
        (written, crossed_midpoint, chunk_el)
    }

    #[inline]
    fn soft_clip(x: f32) -> f32 {
        if x.abs() <= 0.92 {
            x
        } else {
            let sign = x.signum();
            let over = (x.abs() - 0.92).min(4.0);
            sign * (0.92 + 0.08 * (1.0 - (-over * 12.5).exp()))
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn pull_deck_subblock(
        pcm_s16: &[i16],
        cursor: &mut usize,
        end_frame: usize,
        is_eof: bool,
        mode: CrossfadeMode,
        force_direct: bool,
        tempo_ratio: f32,
        wsola: &mut WsolaStretcher,
        vinyl: &mut VinylResampler,
        scratch_in: &mut [f32],
        out_f32: &mut [f32],
    ) -> usize {
        let want_frames = out_f32.len() / 2;
        out_f32.fill(0.0);
        if *cursor >= end_frame
            && wsola.available_output_frames() == 0
            && wsola.unplayed_input_frames(tempo_ratio) < 0.5
            && vinyl.unplayed_input_frames() < 0.5
        {
            return 0;
        }

        let inv = 1.0 / 32768.0f32;

        if force_direct
            || mode == CrossfadeMode::ClassicFade
            || mode == CrossfadeMode::Off
            || ((tempo_ratio - 1.0).abs() < 0.0005
                && mode == CrossfadeMode::DjMasterTempo
                && wsola.available_output_frames() == 0
                && wsola.unplayed_input_frames(1.0) < 0.5)
        {
            let avail = end_frame.saturating_sub(*cursor);
            let n = avail.min(want_frames);
            for i in 0..n {
                let base = (*cursor + i) * 2;
                out_f32[i * 2] = (pcm_s16[base] as f32) * inv;
                out_f32[i * 2 + 1] = (pcm_s16[base + 1] as f32) * inv;
            }
            *cursor += n;
            return n;
        }

        match mode {
            CrossfadeMode::DjVinylSync => {
                let max_in = (scratch_in.len() / 2).min(end_frame.saturating_sub(*cursor));
                for i in 0..max_in {
                    let base = (*cursor + i) * 2;
                    scratch_in[i * 2] = (pcm_s16[base] as f32) * inv;
                    scratch_in[i * 2 + 1] = (pcm_s16[base + 1] as f32) * inv;
                }
                let slice_reaches_eof = is_eof && (*cursor + max_in >= end_frame);
                let (c_in, c_out) = vinyl.process_with_eof(
                    &scratch_in[..max_in * 2],
                    tempo_ratio,
                    slice_reaches_eof,
                    out_f32,
                );
                *cursor += c_in;
                c_out
            }
            CrossfadeMode::DjMasterTempo => {
                let mut produced = 0usize;
                for _ in 0..4 {
                    if produced >= want_frames {
                        break;
                    }
                    let max_in = (scratch_in.len() / 2).min(end_frame.saturating_sub(*cursor));
                    for i in 0..max_in {
                        let base = (*cursor + i) * 2;
                        scratch_in[i * 2] = (pcm_s16[base] as f32) * inv;
                        scratch_in[i * 2 + 1] = (pcm_s16[base + 1] as f32) * inv;
                    }
                    let slice_reaches_eof = is_eof && (*cursor + max_in >= end_frame);
                    let (c_in, c_out) = wsola.process_with_eof(
                        &scratch_in[..max_in * 2],
                        tempo_ratio,
                        slice_reaches_eof,
                        &mut out_f32[produced * 2..],
                    );
                    *cursor += c_in;
                    produced += c_out;
                    if c_in == 0 && c_out == 0 {
                        break;
                    }
                }
                produced
            }
            _ => 0,
        }
    }
}
