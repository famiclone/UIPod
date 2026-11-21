//! Polyphonic SoundFont 2 wavetable synthesizer with GM/GS/XG controllers and stereo reverb.

use std::f32::consts::TAU;
use std::sync::Arc;

use super::sf2::{timecents_to_seconds, LoopMode, ResolvedRegion, SoundFont2};
use super::smf::{MidiEventKind, ParsedMidi, MIDI_SAMPLE_RATE};
use super::{current_soundfont, current_soundfont_generation};

const MAX_VOICES: usize = 96;
const CTRL_BLOCK: usize = 64;
const MASTER_GAIN: f32 = 0.42;

#[inline]
fn db_to_amp(db: f32) -> f32 {
    if db <= -140.0 {
        0.0
    } else {
        10.0_f32.powf(db * 0.05)
    }
}

#[derive(Clone, Copy)]
struct MidiChannel {
    program: u8,
    bank_msb: u8,
    bank_lsb: u8,
    volume: u8,
    pan: u8,
    expression: u8,
    modulation: u8,
    reverb_send: u8,
    sustain_pedal: bool,
    sostenuto_pedal: bool,
    soft_pedal: bool,
    pitch_bend: i16,          // -8192 .. +8191
    pitch_bend_range: f32,    // semitones (default 2.0)
    fine_tune_cents: f32,     // RPN 0,1 (-100 .. +100)
    coarse_tune_semis: f32,   // RPN 0,2 (-64 .. +63)
    channel_pressure: u8,
    rpn_msb: u8,
    rpn_lsb: u8,
}

impl MidiChannel {
    fn new(ch_idx: u8) -> Self {
        Self {
            program: 0,
            bank_msb: if ch_idx == 9 { 128 } else { 0 },
            bank_lsb: 0,
            volume: 100,
            pan: 64,
            expression: 127,
            modulation: 0,
            reverb_send: 40,
            sustain_pedal: false,
            sostenuto_pedal: false,
            soft_pedal: false,
            pitch_bend: 0,
            pitch_bend_range: 2.0,
            fine_tune_cents: 0.0,
            coarse_tune_semis: 0.0,
            channel_pressure: 0,
            rpn_msb: 0x7F,
            rpn_lsb: 0x7F,
        }
    }

    fn reset_controllers(&mut self) {
        self.expression = 127;
        self.modulation = 0;
        self.sustain_pedal = false;
        self.sostenuto_pedal = false;
        self.soft_pedal = false;
        self.pitch_bend = 0;
        self.channel_pressure = 0;
        self.rpn_msb = 0x7F;
        self.rpn_lsb = 0x7F;
    }

    #[inline]
    fn is_drum(&self, ch_idx: u8) -> bool {
        ch_idx == 9 || self.bank_msb == 128 || self.bank_msb == 127 || self.bank_msb == 120
    }

    #[inline]
    fn effective_bank(&self, ch_idx: u8) -> u16 {
        if self.is_drum(ch_idx) {
            128
        } else {
            self.bank_msb as u16
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum EnvStage {
    Delay,
    Attack,
    Hold,
    Decay,
    Sustain,
    Release,
    Off,
}

struct Voice {
    active: bool,
    ch: u8,
    key: u8,
    note_released: bool,
    sustained: bool,
    sostenuto_held: bool,
    exclusive_class: u16,
    age: u64,

    samples: Arc<[i16]>,
    sample_end: usize,
    loop_start: usize,
    loop_end: usize,
    loop_mode: LoopMode,
    pos_f64: f64,
    base_step_ratio: f64,

    // Static note parameters
    note_gain: f32,
    region_pan: f32,
    region_reverb: f32,
    base_pitch_cents: f32,

    // Volume envelope (Delay -> Attack linear -> Hold -> Decay dB -> Sustain -> Release dB)
    vol_stage: EnvStage,
    vol_stage_rem_sec: f32,
    vol_attack_inv_sec: f32,
    vol_hold_sec: f32,
    vol_decay_db_per_sec: f32,
    vol_sustain_db: f32,
    vol_release_db_per_sec: f32,
    vol_level_lin: f32,
    vol_atten_db: f32,

    // Mod envelope (0..1 linear/convex)
    mod_stage: EnvStage,
    mod_stage_rem_sec: f32,
    mod_attack_inv_sec: f32,
    mod_hold_sec: f32,
    mod_decay_per_sec: f32,
    mod_sustain: f32,
    mod_release_per_sec: f32,
    mod_level: f32,
    mod_to_pitch: f32,
    mod_to_filter: f32,

    // LFOs
    vib_delay_rem_sec: f32,
    vib_phase: f32,
    vib_freq_hz: f32,
    vib_to_pitch: f32,

    mod_lfo_delay_rem_sec: f32,
    mod_lfo_phase: f32,
    mod_lfo_freq_hz: f32,
    mod_lfo_to_pitch: f32,
    mod_lfo_to_filter: f32,
    mod_lfo_to_vol_db: f32,

    // Filter
    base_fc_cents: f32,
    svf_q_damp: f32,
    filter_active: bool,
    svf_f: f32,
    svf_low: f32,
    svf_band: f32,

    // Per-block smoothed synthesis targets
    cur_step: f64,
    gain_l: f32,
    gain_r: f32,
    step_gain_l: f32,
    step_gain_r: f32,
    reverb_amount: f32,
}

impl Voice {
    fn new_inactive(dummy_samples: Arc<[i16]>) -> Self {
        Self {
            active: false,
            ch: 0,
            key: 0,
            note_released: false,
            sustained: false,
            sostenuto_held: false,
            exclusive_class: 0,
            age: 0,
            samples: dummy_samples,
            sample_end: 0,
            loop_start: 0,
            loop_end: 0,
            loop_mode: LoopMode::NoLoop,
            pos_f64: 0.0,
            base_step_ratio: 1.0,
            note_gain: 0.0,
            region_pan: 0.0,
            region_reverb: 0.0,
            base_pitch_cents: 0.0,
            vol_stage: EnvStage::Off,
            vol_stage_rem_sec: 0.0,
            vol_attack_inv_sec: 0.0,
            vol_hold_sec: 0.0,
            vol_decay_db_per_sec: 0.0,
            vol_sustain_db: 0.0,
            vol_release_db_per_sec: 0.0,
            vol_level_lin: 0.0,
            vol_atten_db: 144.0,
            mod_stage: EnvStage::Off,
            mod_stage_rem_sec: 0.0,
            mod_attack_inv_sec: 0.0,
            mod_hold_sec: 0.0,
            mod_decay_per_sec: 0.0,
            mod_sustain: 0.0,
            mod_release_per_sec: 0.0,
            mod_level: 0.0,
            mod_to_pitch: 0.0,
            mod_to_filter: 0.0,
            vib_delay_rem_sec: 0.0,
            vib_phase: 0.0,
            vib_freq_hz: 5.0,
            vib_to_pitch: 0.0,
            mod_lfo_delay_rem_sec: 0.0,
            mod_lfo_phase: 0.0,
            mod_lfo_freq_hz: 5.0,
            mod_lfo_to_pitch: 0.0,
            mod_lfo_to_filter: 0.0,
            mod_lfo_to_vol_db: 0.0,
            base_fc_cents: 13500.0,
            svf_q_damp: 1.0,
            filter_active: false,
            svf_low: 0.0,
            svf_band: 0.0,
            svf_f: 1.0,
            cur_step: 1.0,
            gain_l: 0.0,
            gain_r: 0.0,
            step_gain_l: 0.0,
            step_gain_r: 0.0,
            reverb_amount: 0.0,
        }
    }

    fn start(
        &mut self,
        ch_idx: u8,
        midi_key: u8,
        midi_vel: u8,
        ch_state: &MidiChannel,
        reg: &ResolvedRegion,
        samples: Arc<[i16]>,
        age: u64,
    ) {
        let eff_key = if (0..=127).contains(&reg.fixed_key) {
            reg.fixed_key as i32
        } else {
            midi_key as i32
        };
        let eff_vel = if (1..=127).contains(&reg.fixed_vel) {
            reg.fixed_vel as u8
        } else {
            midi_vel.max(1)
        };

        self.active = true;
        self.ch = ch_idx;
        self.key = midi_key;
        self.note_released = false;
        self.sustained = false;
        self.sostenuto_held = ch_state.sostenuto_pedal;
        self.exclusive_class = reg.exclusive_class;
        self.age = age;

        self.samples = samples;
        self.pos_f64 = reg.sample_start as f64;
        self.sample_end = reg.sample_end as usize;
        self.loop_start = reg.loop_start as usize;
        self.loop_end = reg.loop_end as usize;
        self.loop_mode = reg.loop_mode;
        self.base_step_ratio = (reg.sample_rate as f64) / (MIDI_SAMPLE_RATE as f64);

        // Velocity concave curve + SF2 initial attenuation
        let vel_norm = (eff_vel as f32) / 127.0;
        let vel_db = 40.0 * vel_norm.max(0.004).log10(); // 0 dB at 127, ~-20 dB at 40
        self.note_gain = db_to_amp(vel_db - reg.attenuation_db) * (1.0 / 32768.0);
        self.region_pan = reg.pan;
        self.region_reverb = reg.reverb_send;

        self.base_pitch_cents =
            ((eff_key - reg.root_key) as f32) * reg.scale_tuning + reg.tune_cents;

        // Key-scaled hold & decay times (SF2 §8.1.2: key 60 is neutral)
        let key_delta = (60 - eff_key) as f32;
        let vol_hold_sec =
            timecents_to_seconds((reg.vol_hold_tc as f32) + key_delta * (reg.key_to_vol_hold as f32));
        let vol_decay_sec = timecents_to_seconds(
            (reg.vol_decay_tc as f32) + key_delta * (reg.key_to_vol_decay as f32),
        );

        self.vol_hold_sec = vol_hold_sec;
        self.vol_sustain_db = reg.vol_sustain_db;
        // SF2 spec: decay & release timecents specify time for full 100 dB (env) attenuation
        self.vol_decay_db_per_sec = if vol_decay_sec > 0.001 {
            100.0 / vol_decay_sec
        } else {
            100_000.0
        };
        self.vol_release_db_per_sec = 100.0 / reg.vol_release_sec.max(0.004);

        if reg.vol_delay_sec > 0.001 {
            self.vol_stage = EnvStage::Delay;
            self.vol_stage_rem_sec = reg.vol_delay_sec;
            self.vol_attack_inv_sec = if reg.vol_attack_sec > 0.001 {
                1.0 / reg.vol_attack_sec
            } else {
                0.0
            };
            self.vol_level_lin = 0.0;
            self.vol_atten_db = 144.0;
        } else if reg.vol_attack_sec > 0.001 {
            self.vol_stage = EnvStage::Attack;
            self.vol_stage_rem_sec = reg.vol_attack_sec;
            self.vol_attack_inv_sec = 1.0 / reg.vol_attack_sec;
            self.vol_level_lin = 0.0;
            self.vol_atten_db = 0.0;
        } else if vol_hold_sec > 0.001 {
            self.vol_stage = EnvStage::Hold;
            self.vol_stage_rem_sec = vol_hold_sec;
            self.vol_level_lin = 1.0;
            self.vol_atten_db = 0.0;
        } else {
            self.vol_stage = EnvStage::Decay;
            self.vol_level_lin = 1.0;
            self.vol_atten_db = 0.0;
        }

        // Mod envelope
        let mod_hold_sec =
            timecents_to_seconds((reg.mod_hold_tc as f32) + key_delta * (reg.key_to_mod_hold as f32));
        let mod_decay_sec = timecents_to_seconds(
            (reg.mod_decay_tc as f32) + key_delta * (reg.key_to_mod_decay as f32),
        );
        self.mod_hold_sec = mod_hold_sec;
        self.mod_sustain = reg.mod_sustain_level;
        self.mod_decay_per_sec = if mod_decay_sec > 0.001 {
            1.0 / mod_decay_sec
        } else {
            1000.0
        };
        self.mod_release_per_sec = 1.0 / reg.mod_release_sec.max(0.004);
        self.mod_to_pitch = reg.mod_env_to_pitch;
        self.mod_to_filter = reg.mod_env_to_filter_fc;

        if reg.mod_delay_sec > 0.001 {
            self.mod_stage = EnvStage::Delay;
            self.mod_stage_rem_sec = reg.mod_delay_sec;
            self.mod_attack_inv_sec = if reg.mod_attack_sec > 0.001 {
                1.0 / reg.mod_attack_sec
            } else {
                0.0
            };
            self.mod_level = 0.0;
        } else if reg.mod_attack_sec > 0.001 {
            self.mod_stage = EnvStage::Attack;
            self.mod_stage_rem_sec = reg.mod_attack_sec;
            self.mod_attack_inv_sec = 1.0 / reg.mod_attack_sec;
            self.mod_level = 0.0;
        } else {
            self.mod_stage = EnvStage::Hold;
            self.mod_stage_rem_sec = mod_hold_sec;
            self.mod_level = 1.0;
        }

        // LFOs
        self.vib_delay_rem_sec = reg.vib_lfo_delay_sec;
        self.vib_phase = 0.0;
        self.vib_freq_hz = reg.vib_lfo_freq_hz;
        self.vib_to_pitch = reg.vib_lfo_to_pitch;

        self.mod_lfo_delay_rem_sec = reg.mod_lfo_delay_sec;
        self.mod_lfo_phase = 0.0;
        self.mod_lfo_freq_hz = reg.mod_lfo_freq_hz;
        self.mod_lfo_to_pitch = reg.mod_lfo_to_pitch;
        self.mod_lfo_to_filter = reg.mod_lfo_to_filter_fc;
        self.mod_lfo_to_vol_db = reg.mod_lfo_to_vol_db;

        // Low-pass filter
        self.base_fc_cents = reg.filter_fc_cents;
        let q_lin = 10.0_f32.powf(reg.filter_q_db * 0.05).clamp(0.707, 16.0);
        self.svf_q_damp = (1.0 / q_lin).clamp(0.08, 1.414);
        self.svf_low = 0.0;
        self.svf_band = 0.0;
        self.filter_active = reg.filter_fc_cents < 13400.0
            || reg.filter_q_db > 0.5
            || reg.mod_env_to_filter_fc.abs() > 1.0
            || reg.mod_lfo_to_filter_fc.abs() > 1.0;

        self.gain_l = 0.0;
        self.gain_r = 0.0;
        self.step_gain_l = 0.0;
        self.step_gain_r = 0.0;
        self.update_control_block(ch_state, 0.0, 1);
        self.gain_l += self.step_gain_l;
        self.gain_r += self.step_gain_r;
        self.step_gain_l = 0.0;
        self.step_gain_r = 0.0;
    }

    fn trigger_release(&mut self) {
        if self.vol_stage != EnvStage::Release && self.vol_stage != EnvStage::Off {
            if self.vol_stage == EnvStage::Attack {
                self.vol_atten_db = if self.vol_level_lin > 0.0001 {
                    (-20.0 * self.vol_level_lin.log10()).clamp(0.0, 100.0)
                } else {
                    100.0
                };
            } else if self.vol_stage == EnvStage::Delay {
                self.active = false;
                self.vol_stage = EnvStage::Off;
                return;
            }
            self.vol_stage = EnvStage::Release;
        }
        if self.mod_stage != EnvStage::Release && self.mod_stage != EnvStage::Off {
            self.mod_stage = EnvStage::Release;
        }
    }

    fn fast_kill(&mut self) {
        self.trigger_release();
        self.vol_release_db_per_sec = self.vol_release_db_per_sec.max(4000.0);
    }

    #[inline]
    fn current_loudness(&self) -> f32 {
        self.gain_l + self.gain_r
    }

    #[inline]
    fn update_control_block(&mut self, ch: &MidiChannel, dt_sec: f32, frames_in_block: usize) {
        // Advance volume envelope
        if dt_sec > 0.0 {
            match self.vol_stage {
                EnvStage::Delay => {
                    self.vol_stage_rem_sec -= dt_sec;
                    if self.vol_stage_rem_sec <= 0.0 {
                        if self.vol_attack_inv_sec > 0.0 {
                            self.vol_stage = EnvStage::Attack;
                            self.vol_level_lin = 0.0;
                            self.vol_atten_db = 0.0;
                        } else if self.vol_hold_sec > 0.001 {
                            self.vol_stage = EnvStage::Hold;
                            self.vol_stage_rem_sec = self.vol_hold_sec;
                            self.vol_level_lin = 1.0;
                            self.vol_atten_db = 0.0;
                        } else {
                            self.vol_stage = EnvStage::Decay;
                            self.vol_level_lin = 1.0;
                            self.vol_atten_db = 0.0;
                        }
                    }
                }
                EnvStage::Attack => {
                    self.vol_level_lin += dt_sec * self.vol_attack_inv_sec;
                    if self.vol_level_lin >= 1.0 {
                        self.vol_level_lin = 1.0;
                        self.vol_atten_db = 0.0;
                        if self.vol_hold_sec > 0.001 {
                            self.vol_stage = EnvStage::Hold;
                            self.vol_stage_rem_sec = self.vol_hold_sec;
                        } else {
                            self.vol_stage = EnvStage::Decay;
                        }
                    }
                }
                EnvStage::Hold => {
                    self.vol_stage_rem_sec -= dt_sec;
                    if self.vol_stage_rem_sec <= 0.0 {
                        self.vol_stage = EnvStage::Decay;
                    }
                }
                EnvStage::Decay => {
                    self.vol_atten_db += dt_sec * self.vol_decay_db_per_sec;
                    if self.vol_atten_db >= self.vol_sustain_db {
                        self.vol_atten_db = self.vol_sustain_db;
                        if self.vol_atten_db >= 98.0 {
                            self.vol_stage = EnvStage::Off;
                            self.active = false;
                            return;
                        }
                        self.vol_stage = EnvStage::Sustain;
                    }
                }
                EnvStage::Sustain => {}
                EnvStage::Release => {
                    self.vol_atten_db += dt_sec * self.vol_release_db_per_sec;
                    if self.vol_atten_db >= 96.0 {
                        self.vol_stage = EnvStage::Off;
                        self.active = false;
                        return;
                    }
                }
                EnvStage::Off => {
                    self.active = false;
                    return;
                }
            }

            // Advance mod envelope
            match self.mod_stage {
                EnvStage::Delay => {
                    self.mod_stage_rem_sec -= dt_sec;
                    if self.mod_stage_rem_sec <= 0.0 {
                        if self.mod_attack_inv_sec > 0.0 {
                            self.mod_stage = EnvStage::Attack;
                            self.mod_level = 0.0;
                        } else {
                            self.mod_stage = EnvStage::Hold;
                            self.mod_stage_rem_sec = self.mod_hold_sec;
                            self.mod_level = 1.0;
                        }
                    }
                }
                EnvStage::Attack => {
                    self.mod_level += dt_sec * self.mod_attack_inv_sec;
                    if self.mod_level >= 1.0 {
                        self.mod_level = 1.0;
                        self.mod_stage = EnvStage::Hold;
                        self.mod_stage_rem_sec = self.mod_hold_sec;
                    }
                }
                EnvStage::Hold => {
                    self.mod_stage_rem_sec -= dt_sec;
                    if self.mod_stage_rem_sec <= 0.0 {
                        self.mod_stage = EnvStage::Decay;
                    }
                }
                EnvStage::Decay => {
                    self.mod_level -= dt_sec * self.mod_decay_per_sec;
                    if self.mod_level <= self.mod_sustain {
                        self.mod_level = self.mod_sustain;
                        self.mod_stage = EnvStage::Sustain;
                    }
                }
                EnvStage::Sustain => {}
                EnvStage::Release => {
                    self.mod_level = (self.mod_level - dt_sec * self.mod_release_per_sec).max(0.0);
                    if self.mod_level <= 0.0 {
                        self.mod_stage = EnvStage::Off;
                    }
                }
                EnvStage::Off => {}
            }
        }

        // LFOs (triangle wave -1..+1)
        let vib_val = if self.vib_delay_rem_sec > 0.0 {
            self.vib_delay_rem_sec = (self.vib_delay_rem_sec - dt_sec).max(0.0);
            0.0
        } else {
            self.vib_phase = (self.vib_phase + dt_sec * self.vib_freq_hz).fract();
            let p = self.vib_phase;
            if p < 0.25 {
                p * 4.0
            } else if p < 0.75 {
                2.0 - p * 4.0
            } else {
                p * 4.0 - 4.0
            }
        };

        let mod_lfo_val = if self.mod_lfo_delay_rem_sec > 0.0 {
            self.mod_lfo_delay_rem_sec = (self.mod_lfo_delay_rem_sec - dt_sec).max(0.0);
            0.0
        } else {
            self.mod_lfo_phase = (self.mod_lfo_phase + dt_sec * self.mod_lfo_freq_hz).fract();
            let p = self.mod_lfo_phase;
            if p < 0.25 {
                p * 4.0
            } else if p < 0.75 {
                2.0 - p * 4.0
            } else {
                p * 4.0 - 4.0
            }
        };

        // Pitch calculation (cents -> playback ratio)
        let bend_semis = ((ch.pitch_bend as f32) * (1.0 / 8192.0)) * ch.pitch_bend_range;
        let mod_wheel_cents =
            ((ch.modulation.max(ch.channel_pressure) as f32) * (1.0 / 127.0)) * 50.0;
        let total_cents = self.base_pitch_cents
            + (bend_semis + ch.coarse_tune_semis) * 100.0
            + ch.fine_tune_cents
            + self.mod_level * self.mod_to_pitch
            + vib_val * (self.vib_to_pitch + mod_wheel_cents)
            + mod_lfo_val * self.mod_lfo_to_pitch;

        self.cur_step = self.base_step_ratio * (2.0_f64.powf((total_cents as f64) * (1.0 / 1200.0)));

        // Envelope + channel volume/expression + pan
        let env_amp = match self.vol_stage {
            EnvStage::Delay | EnvStage::Off => 0.0,
            EnvStage::Attack => self.vol_level_lin,
            _ => db_to_amp(-self.vol_atten_db + mod_lfo_val * self.mod_lfo_to_vol_db),
        };

        let ch_vol = (ch.volume as f32) * (1.0 / 127.0);
        let ch_exp = (ch.expression as f32) * (1.0 / 127.0);
        let soft_scale = if ch.soft_pedal { 0.707 } else { 1.0 };
        let amp = self.note_gain * env_amp * (ch_vol * ch_vol) * (ch_exp * ch_exp) * soft_scale * MASTER_GAIN;

        // Combine region pan (-0.5..+0.5) and MIDI CC 10 pan (-0.5..+0.5)
        let ch_pan = ((ch.pan as f32) - 64.0) * (0.5 / 64.0);
        let pan_01 = (0.5 + self.region_pan + ch_pan).clamp(0.0, 1.0);
        let angle = pan_01 * ( core_pi_quarter() * 2.0 );
        let target_l = amp * angle.cos();
        let target_r = amp * angle.sin();

        let inv_n = 1.0 / (frames_in_block.max(1) as f32);
        self.step_gain_l = (target_l - self.gain_l) * inv_n;
        self.step_gain_r = (target_r - self.gain_r) * inv_n;

        let ch_rev = (ch.reverb_send as f32) * (1.0 / 127.0);
        self.reverb_amount = (self.region_reverb + ch_rev * 0.65).clamp(0.0, 1.0);

        // Update 2-pole SVF cutoff if active
        if self.filter_active {
            let soft_fc = if ch.soft_pedal { -300.0 } else { 0.0 };
            let fc_cents = (self.base_fc_cents
                + self.mod_level * self.mod_to_filter
                + mod_lfo_val * self.mod_lfo_to_filter
                + soft_fc)
                .clamp(1500.0, 13500.0);
            let fc_hz = (8.176_f32 * 2.0_f32.powf(fc_cents * (1.0 / 1200.0)))
                .clamp(20.0, (MIDI_SAMPLE_RATE as f32) * 0.42);
            self.svf_f = (2.0 * ((TAU * 0.5 * fc_hz) / (MIDI_SAMPLE_RATE as f32)).sin()).clamp(0.002, 1.85);
        }
    }

    fn render_frames(
        &mut self,
        mix_l: &mut [f32],
        mix_r: &mut [f32],
        rev_send: &mut [f32],
        n_frames: usize,
    ) {
        let pcm = &*self.samples;
        let end = self.sample_end.min(pcm.len());
        if end < 2 {
            self.active = false;
            return;
        }
        let looping = match self.loop_mode {
            LoopMode::Continuous => true,
            LoopMode::SustainLoop => !self.note_released,
            LoopMode::NoLoop => false,
        };
        let l_start = self.loop_start as f64;
        let l_end = self.loop_end as f64;
        let l_len = (l_end - l_start).max(1.0);
        let end_f = (end - 1) as f64;

        let mut pos = self.pos_f64;
        let step = self.cur_step;
        let mut gl = self.gain_l;
        let mut gr = self.gain_r;
        let dgl = self.step_gain_l;
        let dgr = self.step_gain_r;
        let rev = self.reverb_amount;

        let filt = self.filter_active;
        let f = self.svf_f;
        let q = self.svf_q_damp;
        let mut low = self.svf_low;
        let mut band = self.svf_band;

        for i in 0..n_frames {
            if looping {
                if pos >= l_end {
                    pos = l_start + (pos - l_start) % l_len;
                }
            } else if pos >= end_f {
                self.active = false;
                self.vol_stage = EnvStage::Off;
                break;
            }

            let idx = pos as usize;
            let frac = (pos - (idx as f64)) as f32;
            let s0 = pcm[idx] as f32;
            let next_idx = if looping && idx + 1 >= self.loop_end {
                self.loop_start
            } else {
                (idx + 1).min(end - 1)
            };
            let s1 = pcm[next_idx] as f32;
            let mut s = s0 + (s1 - s0) * frac;

            if filt {
                low += f * band;
                let high = s - low - q * band;
                band += f * high;
                // Guard against denormals/instability
                if !low.is_finite() {
                    low = 0.0;
                    band = 0.0;
                }
                s = low;
            }

            gl += dgl;
            gr += dgr;
            let out_l = s * gl;
            let out_r = s * gr;
            mix_l[i] += out_l;
            mix_r[i] += out_r;
            rev_send[i] += (out_l + out_r) * rev;

            pos += step;
        }

        self.pos_f64 = pos;
        self.gain_l = gl;
        self.gain_r = gr;
        self.svf_low = low;
        self.svf_band = band;
    }
}

#[inline]
const fn core_pi_quarter() -> f32 {
    std::f32::consts::FRAC_PI_4
}

/// Lightweight Freeverb-style stereo room reverb (heap-allocated delay lines).
struct Comb {
    buf: Vec<f32>,
    idx: usize,
    filt: f32,
}

impl Comb {
    fn new(len: usize) -> Self {
        Self {
            buf: vec![0.0; len.max(16)],
            idx: 0,
            filt: 0.0,
        }
    }

    fn clear(&mut self) {
        self.buf.fill(0.0);
        self.filt = 0.0;
        self.idx = 0;
    }

    #[inline]
    fn process(&mut self, input: f32, feedback: f32, damp: f32) -> f32 {
        let out = self.buf[self.idx];
        self.filt = out * (1.0 - damp) + self.filt * damp;
        self.buf[self.idx] = input + self.filt * feedback;
        self.idx += 1;
        if self.idx >= self.buf.len() {
            self.idx = 0;
        }
        out
    }
}

struct Allpass {
    buf: Vec<f32>,
    idx: usize,
}

impl Allpass {
    fn new(len: usize) -> Self {
        Self {
            buf: vec![0.0; len.max(8)],
            idx: 0,
        }
    }

    fn clear(&mut self) {
        self.buf.fill(0.0);
        self.idx = 0;
    }

    #[inline]
    fn process(&mut self, input: f32) -> f32 {
        let bufout = self.buf[self.idx];
        let out = -input + bufout;
        self.buf[self.idx] = input + bufout * 0.5;
        self.idx += 1;
        if self.idx >= self.buf.len() {
            self.idx = 0;
        }
        out
    }
}

struct StereoReverb {
    combs_l: [Comb; 4],
    combs_r: [Comb; 4],
    ap_l: [Allpass; 2],
    ap_r: [Allpass; 2],
}

impl StereoReverb {
    fn new() -> Self {
        const SPREAD: usize = 23;
        Self {
            combs_l: [
                Comb::new(1116),
                Comb::new(1188),
                Comb::new(1277),
                Comb::new(1356),
            ],
            combs_r: [
                Comb::new(1116 + SPREAD),
                Comb::new(1188 + SPREAD),
                Comb::new(1277 + SPREAD),
                Comb::new(1356 + SPREAD),
            ],
            ap_l: [Allpass::new(556), Allpass::new(441)],
            ap_r: [Allpass::new(556 + SPREAD), Allpass::new(441 + SPREAD)],
        }
    }

    fn clear(&mut self) {
        for c in &mut self.combs_l {
            c.clear();
        }
        for c in &mut self.combs_r {
            c.clear();
        }
        for a in &mut self.ap_l {
            a.clear();
        }
        for a in &mut self.ap_r {
            a.clear();
        }
    }

    fn process_add(&mut self, send: &[f32], mix_l: &mut [f32], mix_r: &mut [f32], n: usize) {
        const FEEDBACK: f32 = 0.76;
        const DAMP: f32 = 0.32;
        const WET: f32 = 0.14;
        for i in 0..n {
            let inp = send[i] * 0.035;
            let mut acc_l = 0.0_f32;
            let mut acc_r = 0.0_f32;
            for c in &mut self.combs_l {
                acc_l += c.process(inp, FEEDBACK, DAMP);
            }
            for c in &mut self.combs_r {
                acc_r += c.process(inp, FEEDBACK, DAMP);
            }
            for a in &mut self.ap_l {
                acc_l = a.process(acc_l);
            }
            for a in &mut self.ap_r {
                acc_r = a.process(acc_r);
            }
            mix_l[i] += acc_l * WET;
            mix_r[i] += acc_r * WET;
        }
    }
}

pub struct MidiDecoder {
    midi: ParsedMidi,
    sf2: Arc<SoundFont2>,
    sf2_gen: u32,
    channels: Box<[MidiChannel; 16]>,
    voices: Vec<Voice>,
    reverb: Box<StereoReverb>,
    mix_l: Vec<f32>,
    mix_r: Vec<f32>,
    rev_send: Vec<f32>,
    event_idx: usize,
    current_frame: u64,
    voice_age_counter: u64,
}

impl MidiDecoder {
    pub fn new(midi: ParsedMidi) -> Self {
        let sf2_gen = current_soundfont_generation();
        let sf2 = current_soundfont();
        Self::new_with_soundfont(midi, sf2, sf2_gen)
    }

    pub fn new_with_soundfont(midi: ParsedMidi, sf2: Arc<SoundFont2>, sf2_gen: u32) -> Self {
        let channels = Box::new(std::array::from_fn(|i| MidiChannel::new(i as u8)));
        let dummy = sf2.samples.clone();
        let mut voices = Vec::with_capacity(MAX_VOICES);
        for _ in 0..MAX_VOICES {
            voices.push(Voice::new_inactive(dummy.clone()));
        }
        Self {
            midi,
            sf2,
            sf2_gen,
            channels,
            voices,
            reverb: Box::new(StereoReverb::new()),
            mix_l: vec![0.0; CTRL_BLOCK],
            mix_r: vec![0.0; CTRL_BLOCK],
            rev_send: vec![0.0; CTRL_BLOCK],
            event_idx: 0,
            current_frame: 0,
            voice_age_counter: 0,
        }
    }

    pub fn duration_ms(&self) -> u32 {
        self.midi.meta.duration_ms
    }

    pub fn elapsed_ms(&self) -> u32 {
        ((self.current_frame * 1000) / (MIDI_SAMPLE_RATE as u64)) as u32
    }

    fn reset_synth_state(&mut self) {
        for (i, ch) in self.channels.iter_mut().enumerate() {
            *ch = MidiChannel::new(i as u8);
        }
        for v in &mut self.voices {
            v.active = false;
            v.vol_stage = EnvStage::Off;
        }
        self.reverb.clear();
        self.event_idx = 0;
        self.current_frame = 0;
        self.voice_age_counter = 0;
    }

    pub fn seek_ms(&mut self, target_ms: u32) {
        let target_frame =
            ((target_ms as u64) * (MIDI_SAMPLE_RATE as u64) / 1000).min(self.midi.total_frames);
        self.reset_synth_state();

        // Replay state-setting events (ProgramChange, ControlChange, PitchBend, SystemReset)
        // up to target_frame without spawning historical notes so seeking is instantaneous!
        while self.event_idx < self.midi.events.len() {
            let ev = self.midi.events[self.event_idx];
            if ev.frame > target_frame {
                break;
            }
            self.event_idx += 1;
            match ev.kind {
                MidiEventKind::NoteOn { .. } | MidiEventKind::NoteOff { .. } => {}
                other => self.dispatch_event(other),
            }
        }
        self.current_frame = target_frame;
    }

    fn alloc_voice_index(&self) -> usize {
        for (idx, v) in self.voices.iter().enumerate() {
            if !v.active {
                return idx;
            }
        }
        // Prefer stealing quietest releasing voice first, otherwise quietest/oldest active voice
        let mut best_idx = 0usize;
        let mut best_score = f32::MAX;
        for (idx, v) in self.voices.iter().enumerate() {
            let stage_bias = if v.vol_stage == EnvStage::Release {
                0.0
            } else {
                10.0
            };
            let score = stage_bias + v.current_loudness() * 100.0;
            if score < best_score
                || ((score - best_score).abs() < 1e-5 && v.age < self.voices[best_idx].age)
            {
                best_score = score;
                best_idx = idx;
            }
        }
        best_idx
    }

    fn handle_note_on(&mut self, ch_idx: u8, key: u8, vel: u8) {
        let ch = self.channels[(ch_idx & 0x0F) as usize];
        let is_drum = ch.is_drum(ch_idx);
        let bank = ch.effective_bank(ch_idx);
        let Some(preset) = self.sf2.find_preset(bank, ch.program, is_drum) else {
            return;
        };

        // Collect matching region indices (typically 1..2 for stereo L/R sample pairs)
        let mut matched: [usize; 8] = [0; 8];
        let mut n_matched = 0usize;
        for (r_idx, reg) in preset.regions.iter().enumerate() {
            if key >= reg.key_lo && key <= reg.key_hi && vel >= reg.vel_lo && vel <= reg.vel_hi {
                matched[n_matched] = r_idx;
                n_matched += 1;
                if n_matched == matched.len() {
                    break;
                }
            }
        }
        if n_matched == 0 {
            return;
        }

        // Choke any existing voices in the same non-zero exclusiveClass on this channel (e.g. hi-hats)
        for &r_idx in &matched[..n_matched] {
            let exc = preset.regions[r_idx].exclusive_class;
            if exc != 0 {
                for v in &mut self.voices {
                    if v.active && v.ch == ch_idx && v.exclusive_class == exc {
                        v.fast_kill();
                    }
                }
            }
        }

        // Release re-triggered identical note on non-drum channels to avoid unnatural buildup
        if !is_drum {
            for v in &mut self.voices {
                if v.active && v.ch == ch_idx && v.key == key && !v.note_released {
                    v.trigger_release();
                }
            }
        }

        let samples = self.sf2.samples.clone();
        // Clone matching regions to satisfy borrow checker cleanly before mutating self.voices
        let mut regs_buf: [Option<ResolvedRegion>; 8] = [None, None, None, None, None, None, None, None];
        for (slot, &r_idx) in matched[..n_matched].iter().enumerate() {
            regs_buf[slot] = Some(preset.regions[r_idx].clone());
        }

        for reg_opt in regs_buf.into_iter().take(n_matched) {
            if let Some(reg) = reg_opt {
                let v_idx = self.alloc_voice_index();
                self.voice_age_counter = self.voice_age_counter.wrapping_add(1);
                let age = self.voice_age_counter;
                self.voices[v_idx].start(ch_idx, key, vel, &ch, &reg, samples.clone(), age);
            }
        }
    }

    fn handle_note_off(&mut self, ch_idx: u8, key: u8) {
        let ch = &self.channels[(ch_idx & 0x0F) as usize];
        let sustain = ch.sustain_pedal;
        for v in &mut self.voices {
            if v.active && v.ch == ch_idx && v.key == key && !v.note_released {
                v.note_released = true;
                if sustain || v.sostenuto_held {
                    v.sustained = true;
                } else {
                    v.trigger_release();
                }
            }
        }
    }

    fn handle_cc(&mut self, ch_idx: u8, cc: u8, val: u8) {
        let idx = (ch_idx & 0x0F) as usize;
        match cc {
            0 => self.channels[idx].bank_msb = val,
            1 => self.channels[idx].modulation = val,
            6 => {
                // Data Entry MSB (RPN)
                let ch = &mut self.channels[idx];
                if ch.rpn_msb == 0 && ch.rpn_lsb == 0 {
                    let frac = ch.pitch_bend_range.fract();
                    ch.pitch_bend_range = ((val as f32) + frac).clamp(0.0, 48.0);
                } else if ch.rpn_msb == 0 && ch.rpn_lsb == 1 {
                    ch.fine_tune_cents = (((val as f32) - 64.0) * (100.0 / 64.0)).clamp(-100.0, 100.0);
                } else if ch.rpn_msb == 0 && ch.rpn_lsb == 2 {
                    ch.coarse_tune_semis = ((val as f32) - 64.0).clamp(-48.0, 48.0);
                }
            }
            38 => {
                // Data Entry LSB (RPN)
                let ch = &mut self.channels[idx];
                if ch.rpn_msb == 0 && ch.rpn_lsb == 0 {
                    let semis = ch.pitch_bend_range.floor();
                    ch.pitch_bend_range = (semis + (val as f32) * 0.01).clamp(0.0, 48.0);
                }
            }
            7 => self.channels[idx].volume = val,
            10 => self.channels[idx].pan = val,
            11 => self.channels[idx].expression = val,
            32 => self.channels[idx].bank_lsb = val,
            64 => {
                let down = val >= 64;
                self.channels[idx].sustain_pedal = down;
                if !down {
                    for v in &mut self.voices {
                        if v.active && v.ch == ch_idx && v.sustained && !v.sostenuto_held {
                            v.sustained = false;
                            v.trigger_release();
                        }
                    }
                }
            }
            66 => {
                let down = val >= 64;
                self.channels[idx].sostenuto_pedal = down;
                if down {
                    for v in &mut self.voices {
                        if v.active && v.ch == ch_idx && !v.note_released {
                            v.sostenuto_held = true;
                        }
                    }
                } else {
                    let sustain = self.channels[idx].sustain_pedal;
                    for v in &mut self.voices {
                        if v.active && v.ch == ch_idx && v.sostenuto_held {
                            v.sostenuto_held = false;
                            if v.note_released && !sustain {
                                v.sustained = false;
                                v.trigger_release();
                            }
                        }
                    }
                }
            }
            67 => self.channels[idx].soft_pedal = val >= 64,
            91 => self.channels[idx].reverb_send = val,
            98 | 99 => {
                // NRPN disarms RPN data entry
                self.channels[idx].rpn_msb = 0x7F;
                self.channels[idx].rpn_lsb = 0x7F;
            }
            100 => self.channels[idx].rpn_lsb = val,
            101 => self.channels[idx].rpn_msb = val,
            120 => {
                // All Sound Off
                for v in &mut self.voices {
                    if v.active && v.ch == ch_idx {
                        v.fast_kill();
                    }
                }
            }
            121 => {
                // Reset All Controllers
                self.channels[idx].reset_controllers();
                for v in &mut self.voices {
                    if v.active && v.ch == ch_idx && v.sustained {
                        v.sustained = false;
                        v.sostenuto_held = false;
                        v.trigger_release();
                    }
                }
            }
            123..=127 => {
                // All Notes Off
                let sustain = self.channels[idx].sustain_pedal;
                for v in &mut self.voices {
                    if v.active && v.ch == ch_idx && !v.note_released {
                        v.note_released = true;
                        if sustain || v.sostenuto_held {
                            v.sustained = true;
                        } else {
                            v.trigger_release();
                        }
                    }
                }
            }
            _ => {}
        }
    }

    fn dispatch_event(&mut self, kind: MidiEventKind) {
        match kind {
            MidiEventKind::NoteOn { ch, key, vel } => self.handle_note_on(ch & 0x0F, key, vel),
            MidiEventKind::NoteOff { ch, key } => self.handle_note_off(ch & 0x0F, key),
            MidiEventKind::ControlChange { ch, cc, val } => self.handle_cc(ch & 0x0F, cc, val),
            MidiEventKind::ProgramChange { ch, program } => {
                self.channels[(ch & 0x0F) as usize].program = program & 0x7F;
            }
            MidiEventKind::PitchBend { ch, value } => {
                self.channels[(ch & 0x0F) as usize].pitch_bend = value;
            }
            MidiEventKind::ChannelPressure { ch, pressure } => {
                self.channels[(ch & 0x0F) as usize].channel_pressure = pressure & 0x7F;
            }
            MidiEventKind::SystemReset => {
                for (i, ch) in self.channels.iter_mut().enumerate() {
                    *ch = MidiChannel::new(i as u8);
                }
                for v in &mut self.voices {
                    if v.active {
                        v.fast_kill();
                    }
                }
            }
        }
    }

    pub fn render_stereo_s16(&mut self, out_interleaved: &mut [i16], loop_forever: bool) -> usize {
        // Hot-swap SoundFont in real time if the user selected another .sf2 in the context menu!
        let latest_gen = current_soundfont_generation();
        if latest_gen != self.sf2_gen {
            self.sf2 = current_soundfont();
            self.sf2_gen = latest_gen;
        }

        let req_frames = out_interleaved.len() / 2;
        if req_frames == 0 {
            return 0;
        }

        let mut written = 0usize;
        while written < req_frames {
            if self.current_frame >= self.midi.total_frames {
                if loop_forever {
                    self.reset_synth_state();
                } else {
                    break;
                }
            }

            let rem_song = (self.midi.total_frames - self.current_frame) as usize;
            let mut step_frames = (req_frames - written).min(CTRL_BLOCK).min(rem_song);

            // Process all MIDI events scheduled at or before `current_frame`
            while self.event_idx < self.midi.events.len() {
                let ev = self.midi.events[self.event_idx];
                if ev.frame <= self.current_frame {
                    self.event_idx += 1;
                    self.dispatch_event(ev.kind);
                } else {
                    // Sub-block sample accuracy: split block right at the next event boundary!
                    let until_next = (ev.frame - self.current_frame) as usize;
                    if until_next < step_frames {
                        step_frames = until_next;
                    }
                    break;
                }
            }

            if step_frames == 0 {
                continue;
            }

            self.mix_l[..step_frames].fill(0.0);
            self.mix_r[..step_frames].fill(0.0);
            self.rev_send[..step_frames].fill(0.0);

            let dt_sec = (step_frames as f32) * (1.0 / (MIDI_SAMPLE_RATE as f32));
            for v in &mut self.voices {
                if !v.active {
                    continue;
                }
                let ch = &self.channels[(v.ch & 0x0F) as usize];
                v.update_control_block(ch, dt_sec, step_frames);
                if v.active {
                    v.render_frames(
                        &mut self.mix_l[..step_frames],
                        &mut self.mix_r[..step_frames],
                        &mut self.rev_send[..step_frames],
                        step_frames,
                    );
                }
            }

            self.reverb.process_add(
                &self.rev_send[..step_frames],
                &mut self.mix_l[..step_frames],
                &mut self.mix_r[..step_frames],
                step_frames,
            );

            // Soft knee limiter + convert to interleaved i16
            let dst = &mut out_interleaved[written * 2..(written + step_frames) * 2];
            for i in 0..step_frames {
                let l = soft_clip(self.mix_l[i]);
                let r = soft_clip(self.mix_r[i]);
                dst[i * 2] = (l * 32767.0) as i16;
                dst[i * 2 + 1] = (r * 32767.0) as i16;
            }

            self.current_frame += step_frames as u64;
            written += step_frames;
        }

        written
    }
}

#[inline]
fn soft_clip(x: f32) -> f32 {
    if x.abs() <= 0.85 {
        x
    } else {
        // Smooth Padé-like saturation above 0.85 so dense chords never hard-clip
        let sign = x.signum();
        let over = x.abs() - 0.85;
        sign * (0.85 + 0.15 * (over / (0.15 + over)))
    }
}
