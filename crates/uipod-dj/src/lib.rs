//! Pure-Rust Dual-Deck Smart DJ Crossfade Engine (`uipod-dj`).
//!
//! Shared identically across:
//! 1. TrimUI Brick / Linux (`aarch64` ALSA)
//! 2. Android Handhelds (`arm64-v8a` SDL2 / AAudio)
//! 3. Web Studio Simulator (`wasm32-unknown-unknown` Web Audio)

pub mod cue_scanner;
pub mod decoder;
pub mod filter;
pub mod manual;
pub mod mixer;
pub mod planner;
pub mod resampler;
pub mod wsola;

pub use cue_scanner::{analyze_pcm_f32, analyze_pcm_s16, DeckCueAnalysis};
pub use filter::{DeckChannelStrip, OnePoleSmoother, StereoWashDelay};
#[cfg(not(target_arch = "wasm32"))]
pub use decoder::{register_opus_decoder_fns, OpusHostFns};
pub use manual::{
    build_in_memory_deck_track, compute_zoomed_24_bars, evaluate_manual_crossfader,
    BassSwapFilter, DeckAtomicControls, DeckBarSnapshot, DeckRamStore, DjMacroFilter,
    LoadedDeckTrack, ManualDjCore, PinnedBeatLoop, PinnedPcmWindow, RAM_CHUNK_FRAMES,
};
#[cfg(not(target_arch = "wasm32"))]
pub use manual::{prepare_streaming_deck_track, DeckRefillWorker, RefillCmd};
pub use mixer::{DualDeckMixer, MixerPhase, PlayheadSnapshot};
pub use planner::{
    evaluate_mix_curves, fold_bpm_to_reference, plan_transition, should_respect_album_gapless,
    MixCurveStep, PlannedTransitionKind, TransitionPlan,
};
pub use resampler::VinylResampler;
pub use wsola::WsolaStretcher;

/// User-selectable crossfade / Auto-DJ mode (`Settings -> Sound -> Crossfade & Auto-DJ`).
#[repr(u8)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum CrossfadeMode {
    #[default]
    Off = 0,
    ClassicFade = 1,
    DjVinylSync = 2,
    DjMasterTempo = 3,
}

impl CrossfadeMode {
    #[inline]
    pub const fn from_u8(val: u8) -> Self {
        match val {
            1 => Self::ClassicFade,
            2 => Self::DjVinylSync,
            3 => Self::DjMasterTempo,
            _ => Self::Off,
        }
    }
}

/// Transition length preset (`4 Bars / 4s`, `8 Bars / 8s`, or `12s` in Classic mode).
#[repr(u8)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum TransitionLength {
    Short4Bars = 0,
    #[default]
    Club8Bars = 1,
    Long12s = 2,
}

impl TransitionLength {
    #[inline]
    pub const fn from_u8(val: u8) -> Self {
        match val {
            0 => Self::Short4Bars,
            2 => Self::Long12s,
            _ => Self::Club8Bars,
        }
    }
}

/// Maximum allowed tempo stretch before falling back to `Filter Wash`.
#[repr(u8)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum MaxTempoStretch {
    Pct6 = 0,
    #[default]
    Pct10 = 1,
    Pct12 = 2,
}

impl MaxTempoStretch {
    #[inline]
    pub const fn from_u8(val: u8) -> Self {
        match val {
            0 => Self::Pct6,
            2 => Self::Pct12,
            _ => Self::Pct10,
        }
    }

    #[inline]
    pub const fn as_fraction(self) -> f32 {
        match self {
            Self::Pct6 => 0.06,
            Self::Pct10 => 0.10,
            Self::Pct12 => 0.12,
        }
    }
}

/// Active transition style reported to Now Playing UI telemetry.
#[repr(u8)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ActiveMixKind {
    #[default]
    None = 0,
    Classic = 1,
    Beatmatch = 2,
    FilterWash = 3,
}

impl ActiveMixKind {
    #[inline]
    pub const fn from_u8(val: u8) -> Self {
        match val {
            1 => Self::Classic,
            2 => Self::Beatmatch,
            3 => Self::FilterWash,
            _ => Self::None,
        }
    }
}

/// Complete user configuration for the DJ engine.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DjConfig {
    pub mode: CrossfadeMode,
    pub length: TransitionLength,
    pub max_stretch: MaxTempoStretch,
    pub keep_album_gapless: bool,
    pub quick_cut_on_next: bool,
}

impl Default for DjConfig {
    fn default() -> Self {
        Self {
            mode: CrossfadeMode::Off,
            length: TransitionLength::Club8Bars,
            max_stretch: MaxTempoStretch::Pct10,
            keep_album_gapless: true,
            quick_cut_on_next: true,
        }
    }
}

/// Lock-free C-ABI telemetry snapshot rendered as the 4-shade Now Playing pill.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct DjTelemetry {
    /// `1` while `0.0 < p < 1.0`, `0` otherwise.
    pub active: u8,
    /// `ActiveMixKind` (`1 = Classic`, `2 = Beatmatch`, `3 = FilterWash`).
    pub mix_kind: u8,
    /// `1` across the `p = 0.47..=0.53` center Bass Swap downbeat!
    pub bass_swap_flash: u8,
    /// Normalized transition progress `0.0..=1.0`.
    pub progress: f32,
    /// Outgoing `Deck A` BPM (`> 0` when Beatmatched).
    pub bpm_a: f32,
    /// Incoming octave-folded `Deck B` BPM (`> 0` when Beatmatched).
    pub bpm_b: f32,
    /// Instantaneous ramped mix BPM.
    pub current_bpm: f32,
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::f32::consts::PI;

    /// Generate a synthetic stereo `bpm` kick+sine signal (`440 Hz` carrier + periodic `65 Hz` kick impulses).
    fn synth_beat_signal(sr: u32, duration_sec: f32, bpm: f32, freq_hz: f32) -> Vec<f32> {
        let frames = (sr as f32 * duration_sec).round() as usize;
        let beat_period = (sr as f32) * 60.0 / bpm;
        let mut out = vec![0.0f32; frames * 2];
        for i in 0..frames {
            let t = (i as f32) / (sr as f32);
            let sine = 0.35 * (2.0 * PI * freq_hz * t).sin();
            let in_beat = (i as f32) % beat_period;
            let beat_num = ((i as f32) / beat_period).floor() as usize;
            let accent = if beat_num % 4 == 0 { 0.55 } else { 0.38 };
            let kick = if in_beat < (sr as f32) * 0.06 {
                let kt = in_beat / (sr as f32);
                accent * (-kt * 45.0).exp() * (2.0 * PI * 68.0 * kt).sin()
            } else {
                0.0
            };
            let s = (sine + kick).clamp(-0.98, 0.98);
            out[i * 2] = s;
            out[i * 2 + 1] = s;
        }
        out
    }

    /// Estimate dominant carrier frequency between `200..1000 Hz` via Goertzel/DFT magnitude scan.
    fn dominant_freq_hz(stereo: &[f32], sr: u32) -> f32 {
        let start_f = (sr as usize) / 2;
        let len_f = (sr as usize).min((stereo.len() / 2).saturating_sub(start_f));
        let mut best_hz = 0.0f32;
        let mut best_mag = -1.0f32;
        let mut hz = 380.0f32;
        while hz <= 500.0 {
            let w = 2.0 * PI * hz / (sr as f32);
            let mut re = 0.0f32;
            let mut im = 0.0f32;
            for n in 0..len_f {
                let s = stereo[(start_f + n) * 2];
                let ang = w * (n as f32);
                re += s * ang.cos();
                im -= s * ang.sin();
            }
            let mag = re * re + im * im;
            if mag > best_mag {
                best_mag = mag;
                best_hz = hz;
            }
            hz += 0.5;
        }
        best_hz
    }

    #[test]
    fn test_wsola_stretch_plus_and_minus_8_pct_preserves_pitch_and_transients() {
        let sr = 44_100u32;
        let src = synth_beat_signal(sr, 4.0, 120.0, 440.0);
        let in_frames = src.len() / 2;

        let mut wsola = WsolaStretcher::new(sr);

        // +8% faster tempo (`tempo_ratio = 1.08` -> output frames = `in_frames / 1.08`)
        let out_fast = wsola.stretch_offline(&src, 1.08);
        let fast_frames = out_fast.len() / 2;
        let expected_fast = ((in_frames as f64) / 1.08).round() as usize;
        assert_eq!(fast_frames, expected_fast);
        assert!(out_fast.iter().all(|x| x.is_finite() && x.abs() <= 1.25));
        let peak_fast_hz = dominant_freq_hz(&out_fast, sr);
        assert!(
            (peak_fast_hz - 440.0).abs() <= 1.5,
            "Expected 440 Hz pitch preserved at +8%, got {peak_fast_hz} Hz"
        );

        // -8% slower tempo (`tempo_ratio = 0.92` -> output frames = `in_frames / 0.92`)
        let out_slow = wsola.stretch_offline(&src, 0.92);
        let slow_frames = out_slow.len() / 2;
        let expected_slow = ((in_frames as f64) / 0.92).round() as usize;
        assert_eq!(slow_frames, expected_slow);
        assert!(out_slow.iter().all(|x| x.is_finite() && x.abs() <= 1.25));
        let peak_slow_hz = dominant_freq_hz(&out_slow, sr);
        assert!(
            (peak_slow_hz - 440.0).abs() <= 1.5,
            "Expected 440 Hz pitch preserved at -8%, got {peak_slow_hz} Hz"
        );

        // Verify transient sharpness (peak crest preserved, not smeared away)
        let max_amp = out_fast.iter().copied().fold(0.0f32, |a, b| a.max(b.abs()));
        assert!(max_amp > 0.65, "Kick transient peak should remain crisp (got {max_amp})");
    }

    #[test]
    fn test_butterworth_svf_rapid_bass_swap_unity_passband_and_zero_clicks() {
        let sr = 48_000u32;
        let mut strip = DeckChannelStrip::new(sr);
        strip.reset(sr, 1.0, 20.0);

        // Sweep 2 kHz passband sine + 60 Hz sub-bass while rapidly flipping HPF `20 <-> 260 Hz` over 200 ms
        let total_frames = (sr as usize) / 2; // 500 ms
        let mut prev_l = 0.0f32;
        let mut max_step_diff = 0.0f32;
        let mut passband_peak = 0.0f32;

        for i in 0..total_frames {
            let t = (i as f32) / (sr as f32);
            if i == (sr as usize) / 10 {
                strip.set_targets(1.0, 260.0);
            } else if i == (sr as usize) * 3 / 10 {
                strip.set_targets(1.0, 20.0);
            }
            let x = 0.8 * (2.0 * PI * 2000.0 * t).sin();
            let (yl, yr) = strip.process_frame(x, x);
            assert!(yl.is_finite() && yr.is_finite());
            if i > 100 {
                max_step_diff = max_step_diff.max((yl - prev_l).abs());
            }
            if i > (sr as usize) * 4 / 10 {
                passband_peak = passband_peak.max(yl.abs());
            }
            prev_l = yl;
        }

        // Theoretical max sample-to-sample step of `0.8 * sin(2*pi*2000*t)` @ 48 kHz is `0.8 * 2*pi*2000/48000 = 0.2094`
        assert!(
            max_step_diff < 0.25,
            "Zero click/discontinuity violated: max_step_diff = {max_step_diff}"
        );
        assert!(
            (passband_peak - 0.80).abs() < 0.015,
            "Expected unity passband gain ~0.80, got {passband_peak}"
        );
    }

    #[test]
    fn test_cue_scanner_and_planner_beatmatch_octave_fold_wash_and_gapless() {
        let sr = 44_100u32;
        let sig_124 = synth_beat_signal(sr, 12.0, 124.0, 330.0);
        let cue_124 = analyze_pcm_f32(&sig_124, sr);
        assert!(
            (cue_124.bpm - 124.0).abs() < 2.0,
            "Expected ~124 BPM, got {}",
            cue_124.bpm
        );
        assert!(cue_124.confidence >= 0.55, "conf={}", cue_124.confidence);

        let cfg = DjConfig {
            mode: CrossfadeMode::DjMasterTempo,
            length: TransitionLength::Club8Bars,
            max_stretch: MaxTempoStretch::Pct10,
            keep_album_gapless: true,
            quick_cut_on_next: true,
        };

        // 1. `87 BPM` -> `174 BPM` folds 174 * 0.5 = 87.0 BPM -> `1.0x` Beatmatch!
        let cue_87 = DeckCueAnalysis {
            sample_rate: sr,
            total_frames: sr as usize * 16,
            cue_in_frame: 0,
            cue_out_frame: sr as usize * 16,
            bpm: 87.0,
            confidence: 0.85,
            first_downbeat_frame: 0,
            last_downbeat_frame: sr as usize * 15,
        };
        let cue_174 = DeckCueAnalysis {
            bpm: 174.0,
            ..cue_87
        };
        let plan_fold = plan_transition(&cfg, &cue_87, &cue_174, "Album A", 1, "Album B", 1, false, false);
        assert_eq!(plan_fold.kind, PlannedTransitionKind::Beatmatch);
        assert!((plan_fold.folded_bpm_b - 87.0).abs() < 0.01);
        let c_mid = evaluate_mix_curves(&plan_fold, 0.50);
        assert!((c_mid.tempo_a - 1.0).abs() < 0.001);
        assert!((c_mid.tempo_b - 1.0).abs() < 0.001);
        assert!(c_mid.bass_swap_flash);

        // 2. `124 BPM` -> `128 BPM` (`3.2%` delta <= `10%`) -> smooth Beatmatch ramp!
        let cue_a_124 = DeckCueAnalysis { bpm: 124.0, ..cue_87 };
        let cue_b_128 = DeckCueAnalysis { bpm: 128.0, ..cue_87 };
        let plan_bm = plan_transition(&cfg, &cue_a_124, &cue_b_128, "Hits 1", 2, "Hits 2", 5, false, false);
        assert_eq!(plan_bm.kind, PlannedTransitionKind::Beatmatch);
        let s0 = evaluate_mix_curves(&plan_bm, 0.10);
        let s5 = evaluate_mix_curves(&plan_bm, 0.50);
        let s9 = evaluate_mix_curves(&plan_bm, 0.90);
        assert!((s0.current_bpm - 124.0).abs() < 0.01);
        assert!((s5.current_bpm - 126.0).abs() < 0.05);
        assert!((s9.current_bpm - 128.0).abs() < 0.01);
        assert!((s9.tempo_b - 1.0).abs() < 0.0001);

        // 3. `128 BPM` -> `98 BPM` (`30.6%` delta > `10%`) -> triggers `FilterWash` fallback!
        let cue_b_98 = DeckCueAnalysis { bpm: 98.0, ..cue_87 };
        let plan_wash = plan_transition(&cfg, &cue_b_128, &cue_b_98, "Club", 1, "Chill", 1, false, false);
        assert_eq!(plan_wash.kind, PlannedTransitionKind::FilterWash);
        let sw = evaluate_mix_curves(&plan_wash, 0.75);
        assert_eq!(sw.tempo_a, 1.0);
        assert_eq!(sw.tempo_b, 1.0);
        assert!(sw.hpf_a_hz > 300.0);
        assert!(sw.wash_wet_a > 0.20);

        // 4. Consecutive same-album tracks (`Track 3 -> Track 4`, Shuffle OFF) -> `Gapless`!
        let plan_gap = plan_transition(
            &cfg,
            &cue_a_124,
            &cue_b_128,
            "The Dark Side of the Moon",
            3,
            "The Dark Side of the Moon",
            4,
            false,
            false,
        );
        assert_eq!(plan_gap.kind, PlannedTransitionKind::Gapless);
    }

    #[test]
    fn test_dual_deck_mixer_full_transition_and_midpoint_handoff() {
        let sr = 44_100u32;
        let f_a = synth_beat_signal(sr, 12.0, 122.0, 330.0);
        let f_b = synth_beat_signal(sr, 12.0, 126.0, 440.0);
        let s16_a: Vec<i16> = f_a.iter().map(|&x| (x * 24000.0) as i16).collect();
        let s16_b: Vec<i16> = f_b.iter().map(|&x| (x * 24000.0) as i16).collect();

        let mut mixer = DualDeckMixer::new(sr);
        mixer.set_config(DjConfig {
            mode: CrossfadeMode::DjMasterTempo,
            length: TransitionLength::Short4Bars,
            max_stretch: MaxTempoStretch::Pct10,
            keep_album_gapless: true,
            quick_cut_on_next: true,
        });

        mixer.begin_deck_a_capture(sr, "Club Vol 1", 1, false, false);
        mixer.push_deck_a_tail(&s16_a, |_| {});
        mixer.finish_deck_a_and_begin_deck_b("Club Vol 2", 1, false);
        assert!(mixer.push_deck_b_frames(&s16_b, true));

        let mut out = [0i16; 2048];
        let mut midpoint_count = 0usize;
        let mut saw_active_tel = false;
        let mut total_rendered = 0usize;

        while mixer.phase() != MixerPhase::Idle {
            let (n, mid) = mixer.render_s16(&mut out);
            assert!(n > 0);
            total_rendered += n;
            if mid {
                midpoint_count += 1;
            }
            if mixer.telemetry().active != 0 {
                saw_active_tel = true;
            }
        }

        assert_eq!(midpoint_count, 1, "Midpoint handoff must fire exactly once at p=0.50");
        assert!(saw_active_tel, "DJ telemetry should report active Beatmatch during mix");
        assert!(total_rendered > (sr as usize) * 14);
    }

    #[test]
    fn test_burst_writes_zero_drop_and_exact_eof_sample_conservation() {
        let sr = 44_100u32;
        // 24.5 seconds (> 18s MAX_DECK_A_SECONDS) with deterministic per-frame signature
        let frames_a = (sr as usize) * 24 + (sr as usize) / 2;
        let mut pcm_a = Vec::with_capacity(frames_a * 2);
        for i in 0..frames_a {
            let l = ((i % 30_000) as i16).wrapping_sub(15_000);
            let r = (((i * 7 + 13) % 30_000) as i16).wrapping_sub(15_000);
            pcm_a.push(l);
            pcm_a.push(r);
        }

        // Case 1: Faster-than-realtime huge burst writes (65,536 frames/call) + EOF drain with no Deck B
        let mut mixer = DualDeckMixer::new(sr);
        mixer.set_config(DjConfig {
            mode: CrossfadeMode::ClassicFade,
            length: TransitionLength::Long12s,
            max_stretch: MaxTempoStretch::Pct10,
            keep_album_gapless: true,
            quick_cut_on_next: true,
        });
        mixer.begin_deck_a_capture(sr, "Solo", 1, false, false);
        mixer.set_track_timing(155_500, 180_000, 0);

        let mut recovered = Vec::with_capacity(pcm_a.len());
        let mut io_chunk = vec![0i16; 1024 * 2];

        for burst in pcm_a.chunks(65_536 * 2) {
            mixer.push_deck_a_frames(burst);
            // Deliberately pop only 1 small chunk per massive burst to stress internal spill backpressure
            let (spilled, el_ms) = mixer.pop_spill_s16(&mut io_chunk);
            if spilled > 0 {
                assert!(
                    el_ms < 175_000,
                    "Spill timestamp must track actual spilled audio, got {el_ms} ms"
                );
                recovered.extend_from_slice(&io_chunk[..spilled * 2]);
            }
        }

        // Track hits EOF with no next track (or Crossfade switched Off): must drain 100% of tail
        mixer.flush_to_gapless_drain();
        while mixer.phase() != MixerPhase::Idle {
            let (n, _mid) = mixer.render_s16(&mut io_chunk);
            assert!(n > 0);
            recovered.extend_from_slice(&io_chunk[..n * 2]);
        }

        assert_eq!(
            recovered.len(),
            pcm_a.len(),
            "Burst writes + EOF drain must conserve exact frame count (N in == N out)"
        );
        assert_eq!(
            recovered, pcm_a,
            "Every sample from frame 0 to final EOF sample must match bit-for-bit with zero skips"
        );

        // Case 2: Toggling Crossfade = OFF mid-capture drains 100% of captured frames without truncation
        mixer.begin_deck_a_capture(sr, "Live", 1, false, false);
        mixer.push_deck_a_frames(&pcm_a);
        mixer.set_config(DjConfig {
            mode: CrossfadeMode::Off,
            ..mixer.config()
        });
        let mut recovered_off = Vec::with_capacity(pcm_a.len());
        while mixer.phase() != MixerPhase::Idle {
            let (n, _) = mixer.render_s16(&mut io_chunk);
            assert!(n > 0);
            recovered_off.extend_from_slice(&io_chunk[..n * 2]);
        }
        assert_eq!(recovered_off, pcm_a);
    }

    #[test]
    fn test_12s_crossfade_playhead_countdown_never_hits_zero_early_and_mid_handoff() {
        let sr = 44_100u32;
        let f_a = synth_beat_signal(sr, 18.0, 122.0, 330.0);
        let f_b = synth_beat_signal(sr, 16.0, 126.0, 440.0);
        let s16_a: Vec<i16> = f_a.iter().map(|&x| (x * 22_000.0) as i16).collect();
        let s16_b: Vec<i16> = f_b.iter().map(|&x| (x * 22_000.0) as i16).collect();

        for mode in [
            CrossfadeMode::ClassicFade,
            CrossfadeMode::DjVinylSync,
            CrossfadeMode::DjMasterTempo,
        ] {
            let mut mixer = DualDeckMixer::new(sr);
            mixer.set_config(DjConfig {
                mode,
                length: TransitionLength::Long12s,
                max_stretch: MaxTempoStretch::Pct10,
                keep_album_gapless: true,
                quick_cut_on_next: true,
            });

            // Track 1 is 180,000 ms (3:00); tail capture begins at 162,000 ms (18.0s remaining).
            // Track 2 is 210,000 ms (3:30).
            mixer.begin_deck_a_capture(sr, "Set A", 1, false, false);
            mixer.set_track_timing(162_000, 180_000, 210_000);
            mixer.push_deck_a_frames(&s16_a);
            mixer.finish_deck_a_and_begin_deck_b("Set B", 1, false);
            mixer.set_deck_b_duration_ms(210_000);
            assert!(mixer.push_deck_b_frames(&s16_b, true));

            // Simulate faster-than-realtime codec rendering into a 4.0s downstream `pcmbuf` + ALSA FIFO
            let max_downstream_frames = (sr as usize) * 4;
            let mut downstream_queued = 0usize;
            let mut out_chunk = [0i16; 1024 * 2];
            let step_frames = (sr as usize) / 10; // 100 ms hardware sink tick

            let mut prev_a_rem = u32::MAX;
            let mut saw_deck_a_during_mix = false;
            let mut saw_clean_deck_b_handoff = false;
            let mut prev_b_el = 0u32;

            loop {
                while mixer.phase() != MixerPhase::Idle
                    && downstream_queued + 1024 <= max_downstream_frames
                {
                    let (n, _mid, _chunk_el) = mixer.render_s16_timed(&mut out_chunk);
                    if n == 0 {
                        break;
                    }
                    downstream_queued += n;
                }

                if downstream_queued == 0 && mixer.phase() == MixerPhase::Idle {
                    break;
                }

                let snap = mixer.playhead_at_delay_frames(downstream_queued);
                if !snap.is_deck_b_foreground {
                    assert!(
                        snap.audible_remaining_ms >= 4_500,
                        "Mode {mode:?}: Deck A remaining_ms clamped early ({} ms) while Track 1 is foreground!",
                        snap.audible_remaining_ms
                    );
                    assert!(
                        snap.audible_remaining_ms <= prev_a_rem.saturating_add(5),
                        "Mode {mode:?}: Deck A countdown jumped backwards ({} -> {})",
                        prev_a_rem,
                        snap.audible_remaining_ms
                    );
                    prev_a_rem = snap.audible_remaining_ms;
                    if snap.telemetry.active != 0 {
                        saw_deck_a_during_mix = true;
                    }
                } else {
                    if !saw_clean_deck_b_handoff {
                        saw_clean_deck_b_handoff = true;
                        // At p = 0.50 of an 8-bar / 12s crossfade, Deck B's audible elapsed must be ~4.5s..9.0s
                        assert!(
                            (3_500..=10_500).contains(&snap.audible_elapsed_ms),
                            "Mode {mode:?}: Midpoint handoff jumped to wrong Deck B elapsed: {} ms",
                            snap.audible_elapsed_ms
                        );
                        prev_b_el = snap.audible_elapsed_ms;
                    } else {
                        assert!(
                            snap.audible_elapsed_ms + 5 >= prev_b_el,
                            "Mode {mode:?}: Deck B elapsed regressed ({} -> {})",
                            prev_b_el,
                            snap.audible_elapsed_ms
                        );
                        prev_b_el = snap.audible_elapsed_ms;
                    }
                }

                downstream_queued = downstream_queued.saturating_sub(step_frames);
            }

            assert!(saw_deck_a_during_mix, "Mode {mode:?}: Expected active pre-midpoint mix on Deck A");
            assert!(saw_clean_deck_b_handoff, "Mode {mode:?}: Expected clean midpoint handoff to Deck B");
        }
    }

    #[test]
    fn test_lossless_burst_producer_writes_and_exact_eof_tail_drain() {
        let sr = 44_100u32;
        // 28 seconds of uniquely identifiable stereo samples (> 18s MAX_DECK_A_SECONDS cap!)
        let total_frames = (sr as usize) * 28 + 317;
        let mut src = Vec::with_capacity(total_frames * 2);
        for f in 0..total_frames {
            let l = ((f % 30_000) as i16).wrapping_sub(15_000);
            let r = (((f * 7) % 30_000) as i16).wrapping_sub(15_000);
            src.push(l);
            src.push(r);
        }

        let mut mixer = DualDeckMixer::new(sr);
        mixer.set_config(DjConfig {
            mode: CrossfadeMode::DjMasterTempo,
            length: TransitionLength::Club8Bars,
            max_stretch: MaxTempoStretch::Pct10,
            keep_album_gapless: true,
            quick_cut_on_next: true,
        });
        mixer.begin_deck_a_capture(sr, "BurstAlbum", 1, false, false);
        mixer.set_track_timing(0, ((total_frames as u64 * 1000) / sr as u64) as u32, 0);

        // Push irregular faster-than-1x bursts (up to 65,536 frames at once) while consumer pulls slowly
        let burst_sizes = [4096usize, 65_536, 17, 131_072, 1024, 99_991, 513];
        let mut pushed = 0usize;
        let mut bi = 0usize;
        let mut drained = Vec::with_capacity(total_frames * 2);
        let mut pull_buf = [0i16; 512 * 2];

        while pushed < total_frames {
            let chunk = burst_sizes[bi % burst_sizes.len()].min(total_frames - pushed);
            bi += 1;
            mixer.push_deck_a_frames(&src[pushed * 2..(pushed + chunk) * 2]);
            pushed += chunk;

            // Pull only 1 small 512-frame slice per giant producer burst (simulating heavy backpressure)
            let (got, _) = mixer.pop_spill_s16(&mut pull_buf);
            if got > 0 {
                drained.extend_from_slice(&pull_buf[..got * 2]);
            }
        }

        // Simulate EOF with no Deck B -> `flush_to_gapless_drain` must emit 100% of remaining samples!
        mixer.flush_to_gapless_drain();
        while mixer.phase() != MixerPhase::Idle {
            let (got, _, _) = mixer.render_s16_timed(&mut pull_buf);
            if got == 0 {
                break;
            }
            drained.extend_from_slice(&pull_buf[..got * 2]);
        }

        assert_eq!(
            drained.len(),
            src.len(),
            "Expected N={} samples out for N={} samples in (zero drops/truncation)",
            src.len() / 2,
            drained.len() / 2
        );
        assert_eq!(drained, src, "Bit-exact sample integrity violated across spill + EOF tail drain");
    }

    #[test]
    fn test_preroll_and_slow_stream_deck_b_never_freeze_remaining_time_or_starve() {
        let sr = 44_100u32;
        let frames_a = (sr as usize) * 18;
        let s16_a = vec![4000i16; frames_a * 2];

        let mut mixer = DualDeckMixer::new(sr);
        mixer.set_config(DjConfig {
            mode: CrossfadeMode::ClassicFade,
            length: TransitionLength::Short4Bars,
            max_stretch: MaxTempoStretch::Pct10,
            keep_album_gapless: true,
            quick_cut_on_next: true,
        });

        // Codec timestamp slightly overshot by +350 ms at capture start (`162_350 ms` on a `180_000 ms` track)
        mixer.begin_deck_a_capture(sr, "StreamA", 1, false, false);
        mixer.set_track_timing(162_350, 180_000, 200_000);
        mixer.push_deck_a_frames(&s16_a);
        mixer.finish_deck_a_and_begin_deck_b("StreamB", 1, false);

        // Before Deck B arrives, `pop_safe_premix_a_s16` yields safe prefix frames so `pcmbuf` never starves
        let mut pre_buf = [0i16; 4096 * 2];
        let (safe_got, el_ms) = mixer.pop_safe_premix_a_s16(&mut pre_buf);
        assert_eq!(safe_got, 4096);
        assert_eq!(el_ms, 162_000, "Anchored capture start should align exact 18s tail to 162,000 ms");

        // With 4.0s (`176,400` frames) still queued downstream (including ~3.9s of pre-capture audio),
        // playhead lookback must report ~158,093 ms (`> 21,800 ms` remaining), never freezing at 162,000 ms!
        let snap = mixer.playhead_at_delay_frames(sr as usize * 4);
        assert!(!snap.is_deck_b_foreground);
        assert!(
            (21_800..=22_000).contains(&snap.audible_remaining_ms),
            "Expected ~21.9s remaining with 4s downstream pre-roll, got {} ms",
            snap.audible_remaining_ms
        );
    }
}
