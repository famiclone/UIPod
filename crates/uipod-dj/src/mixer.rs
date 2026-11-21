//! Stateful `DualDeckMixer` driving pre-transition `Deck A` tail playback,
//! active Dual-Deck Beatmatch / Vinyl Sync / Filter Wash / Classic Fade mixing,
//! `p = 0.50` midpoint metadata handoff, and post-transition `Deck B` intro drain.

use crate::cue_scanner::{analyze_pcm_s16, DeckCueAnalysis};
use crate::filter::{DeckChannelStrip, StereoWashDelay};
use crate::planner::{
    evaluate_mix_curves, plan_transition, PlannedTransitionKind, TransitionPlan,
};
use crate::resampler::VinylResampler;
use crate::wsola::WsolaStretcher;
use crate::{CrossfadeMode, DjConfig, DjTelemetry};

const SUB_BLOCK_FRAMES: usize = 128;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MixerPhase {
    /// Normal single-track playback (`0%` CPU overhead).
    Idle,
    /// Capturing `Deck A`'s outgoing tail (`~18s` max).
    CapturingDeckA,
    /// `Deck A` tail captured; capturing `Deck B`'s incoming intro (`~15s` max) before planning.
    CapturingDeckB,
    /// Playing `Deck A` bit-exact prior to `plan.mix_start_a_frame`.
    PreMixDeckA,
    /// Active dual-deck transition (`0.0 <= p < 1.0`).
    Mixing,
    /// Draining remaining buffered `Deck B` intro frames bit-exact after `p == 1.0`.
    DrainingDeckB,
}

pub struct DualDeckMixer {
    cfg: DjConfig,
    sample_rate: u32,
    phase: MixerPhase,

    // Pre-allocated PCM S16 stereo buffers (`~18s` Deck A + `~15s` Deck B)
    deck_a_pcm: Vec<i16>,
    deck_b_pcm: Vec<i16>,
    deck_a_cursor: usize,
    deck_b_cursor: usize,
    deck_b_eof: bool,

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
            deck_a_cursor: 0,
            deck_b_cursor: 0,
            deck_b_eof: false,
            album_a: String::new(),
            track_num_a: 0,
            album_b: String::new(),
            track_num_b: 0,
            shuffle_active: false,
            manual_quick_cut: false,
            deck_b_is_native_nsf: false,
            cue_a: DeckCueAnalysis::default(),
            cue_b: DeckCueAnalysis::default(),
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

    pub fn reset(&mut self) {
        self.phase = MixerPhase::Idle;
        self.deck_a_pcm.clear();
        self.deck_b_pcm.clear();
        self.deck_a_cursor = 0;
        self.deck_b_cursor = 0;
        self.deck_b_eof = false;
        self.manual_quick_cut = false;
        self.deck_b_is_native_nsf = false;
        self.plan = None;
        self.mix_frames_done = 0;
        self.midpoint_signaled = false;
        self.last_telemetry = DjTelemetry::default();
        self.recommended_nsf_tempo_b = 1.0;
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
        self.deck_a_cursor = 0;
        self.deck_b_cursor = 0;
        self.deck_b_eof = false;
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

    /// Append decoded `Deck A` stereo S16 frames during `CapturingDeckA`.
    /// If `deck_a_pcm` exceeds `MAX_DECK_A_SECONDS`, oldest frames are spilled out into `spill_out`
    /// so `Deck A` never drops a single sample!
    pub fn push_deck_a_tail(&mut self, stereo_s16: &[i16], mut spill_cb: impl FnMut(&[i16])) {
        let max_samples = (self.sample_rate as usize) * Self::MAX_DECK_A_SECONDS * 2;
        if self.deck_a_pcm.len() + stereo_s16.len() > max_samples {
            let overflow = (self.deck_a_pcm.len() + stereo_s16.len()) - max_samples;
            let spill = overflow.min(self.deck_a_pcm.len()) & !1;
            if spill > 0 {
                spill_cb(&self.deck_a_pcm[..spill]);
                self.deck_a_pcm.drain(..spill);
            }
        }
        self.deck_a_pcm.extend_from_slice(stereo_s16);
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
        self.phase = MixerPhase::CapturingDeckB;
    }

    /// Append incoming `Deck B` stereo S16 frames. Once enough intro is buffered (`~13s` or `eof`),
    /// automatically computes the `TransitionPlan` and enters `PreMixDeckA` / `Mixing`.
    /// Returns `true` once the transition plan is ready to render!
    pub fn push_deck_b_frames(&mut self, stereo_s16: &[i16], is_eof: bool) -> bool {
        if !stereo_s16.is_empty() {
            self.deck_b_pcm.extend_from_slice(stereo_s16);
        }
        if is_eof {
            self.deck_b_eof = true;
        }

        if self.phase == MixerPhase::CapturingDeckB {
            let target_sec = if self.manual_quick_cut { 3 } else { 13 };
            let min_samples = (self.sample_rate as usize) * target_sec * 2;
            if self.deck_b_pcm.len() >= min_samples || self.deck_b_eof {
                self.commit_transition_plan();
                return true;
            }
            return false;
        }
        true
    }

    fn commit_transition_plan(&mut self) {
        self.cue_b = analyze_pcm_s16(&self.deck_b_pcm, self.sample_rate);
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
        self.mix_frames_done = 0;
        self.midpoint_signaled = false;

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

        self.phase = if plan.kind == PlannedTransitionKind::Gapless {
            self.deck_b_cursor = 0;
            MixerPhase::PreMixDeckA
        } else if plan.mix_start_a_frame > 0 {
            MixerPhase::PreMixDeckA
        } else {
            MixerPhase::Mixing
        };
        self.plan = Some(plan);
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
        let cap_frames = out_s16.len() / 2;
        if cap_frames == 0 {
            return (0, false);
        }

        let mut written = 0usize;
        let mut crossed_midpoint = false;

        while written < cap_frames {
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
                        let avail = end_a - self.deck_a_cursor;
                        let n = avail.min(cap_frames - written);
                        let src_off = self.deck_a_cursor * 2;
                        let dst_off = written * 2;
                        out_s16[dst_off..dst_off + n * 2]
                            .copy_from_slice(&self.deck_a_pcm[src_off..src_off + n * 2]);
                        self.deck_a_cursor += n;
                        written += n;
                    } else if plan.kind == PlannedTransitionKind::Gapless {
                        crossed_midpoint = true;
                        self.last_telemetry = DjTelemetry::default();
                        self.phase = MixerPhase::DrainingDeckB;
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
                        if !self.midpoint_signaled {
                            crossed_midpoint = true;
                            self.midpoint_signaled = true;
                        }
                        self.last_telemetry = DjTelemetry::default();
                        self.recommended_nsf_tempo_b = 1.0;
                        self.phase = MixerPhase::DrainingDeckB;
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

                    if prev_p < 0.50 && next_p >= 0.50 && !self.midpoint_signaled {
                        crossed_midpoint = true;
                        self.midpoint_signaled = true;
                    }

                    let curve = evaluate_mix_curves(&plan, mid_p);
                    self.recommended_nsf_tempo_b = if self.deck_b_is_native_nsf {
                        curve.tempo_b
                    } else {
                        1.0
                    };

                    // Render sub-block for Deck A & Deck B
                    let end_a = plan.cue_out_a_frame.min(self.deck_a_pcm.len() / 2);
                    let got_a = Self::pull_deck_subblock(
                        &self.deck_a_pcm,
                        &mut self.deck_a_cursor,
                        end_a,
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
                        plan.mode,
                        self.deck_b_is_native_nsf,
                        eff_tempo_b,
                        &mut self.wsola_b,
                        &mut self.vinyl_b,
                        &mut self.scratch_in_b,
                        &mut self.scratch_out_b[..blk * 2],
                    );

                    let _ = (got_a, got_b);

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

                        // Soft-clipping safety limiter above `0.98` so loud mastered peaks never clip
                        let sum_l = Self::soft_clip(al + wl + bl);
                        let sum_r = Self::soft_clip(ar + wr + br);
                        out_s16[dst_base + i * 2] =
                            (sum_l * 32767.0).round().clamp(-32768.0, 32767.0) as i16;
                        out_s16[dst_base + i * 2 + 1] =
                            (sum_r * 32767.0).round().clamp(-32768.0, 32767.0) as i16;
                    }

                    self.mix_frames_done += blk;
                    written += blk;

                    self.last_telemetry = DjTelemetry {
                        active: 1,
                        mix_kind: plan.kind.active_mix_kind() as u8,
                        bass_swap_flash: u8::from(curve.bass_swap_flash),
                        progress: next_p.clamp(0.0, 1.0),
                        bpm_a: plan.bpm_a,
                        bpm_b: plan.folded_bpm_b,
                        current_bpm: curve.current_bpm,
                    };

                    // Compact already-consumed Deck B prefix periodically so memory stays bounded
                    if self.deck_b_cursor >= 65_536 {
                        let drop_samples = self.deck_b_cursor * 2;
                        self.deck_b_pcm.drain(..drop_samples);
                        self.deck_b_cursor = 0;
                    }
                }
                MixerPhase::DrainingDeckB => {
                    let total_b = self.deck_b_pcm.len() / 2;
                    if self.deck_b_cursor < total_b {
                        let avail = total_b - self.deck_b_cursor;
                        let n = avail.min(cap_frames - written);
                        let src_off = self.deck_b_cursor * 2;
                        let dst_off = written * 2;
                        out_s16[dst_off..dst_off + n * 2]
                            .copy_from_slice(&self.deck_b_pcm[src_off..src_off + n * 2]);
                        self.deck_b_cursor += n;
                        written += n;
                    } else {
                        // All buffered Deck B intro frames drained -> back to zero-overhead `Idle`!
                        self.reset();
                        break;
                    }
                }
            }
        }

        (written, crossed_midpoint)
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
        if *cursor >= end_frame && wsola.available_output_frames() == 0 {
            return 0;
        }

        let inv = 1.0 / 32768.0f32;

        if force_direct
            || mode == CrossfadeMode::ClassicFade
            || mode == CrossfadeMode::Off
            || ((tempo_ratio - 1.0).abs() < 0.0005
                && mode == CrossfadeMode::DjMasterTempo
                && wsola.available_output_frames() == 0)
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
                let (c_in, c_out) =
                    vinyl.process(&scratch_in[..max_in * 2], tempo_ratio, out_f32);
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
                    let (c_in, c_out) = wsola.process(
                        &scratch_in[..max_in * 2],
                        tempo_ratio,
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
