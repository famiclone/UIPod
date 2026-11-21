//! SoundFont 2 (`.sf2`, RIFF `sfbk`) parser and preset/instrument zone resolver.

use std::sync::Arc;

const NUM_GENERATORS: usize = 61;

// SF2 Generator IDs (SoundFont 2.01 specification §8.1.2)
const GEN_START_ADDRS_OFFSET: usize = 0;
const GEN_END_ADDRS_OFFSET: usize = 1;
const GEN_STARTLOOP_ADDRS_OFFSET: usize = 2;
const GEN_ENDLOOP_ADDRS_OFFSET: usize = 3;
const GEN_START_ADDRS_COARSE_OFFSET: usize = 4;
const GEN_MOD_LFO_TO_PITCH: usize = 5;
const GEN_VIB_LFO_TO_PITCH: usize = 6;
const GEN_MOD_ENV_TO_PITCH: usize = 7;
const GEN_INITIAL_FILTER_FC: usize = 8;
const GEN_INITIAL_FILTER_Q: usize = 9;
const GEN_MOD_LFO_TO_FILTER_FC: usize = 10;
const GEN_MOD_ENV_TO_FILTER_FC: usize = 11;
const GEN_END_ADDRS_COARSE_OFFSET: usize = 12;
const GEN_MOD_LFO_TO_VOLUME: usize = 13;
const GEN_CHORUS_EFFECTS_SEND: usize = 15;
const GEN_REVERB_EFFECTS_SEND: usize = 16;
const GEN_PAN: usize = 17;
const GEN_DELAY_MOD_LFO: usize = 21;
const GEN_FREQ_MOD_LFO: usize = 22;
const GEN_DELAY_VIB_LFO: usize = 23;
const GEN_FREQ_VIB_LFO: usize = 24;
const GEN_DELAY_MOD_ENV: usize = 25;
const GEN_ATTACK_MOD_ENV: usize = 26;
const GEN_HOLD_MOD_ENV: usize = 27;
const GEN_DECAY_MOD_ENV: usize = 28;
const GEN_SUSTAIN_MOD_ENV: usize = 29;
const GEN_RELEASE_MOD_ENV: usize = 30;
const GEN_KEYNUM_TO_MOD_ENV_HOLD: usize = 31;
const GEN_KEYNUM_TO_MOD_ENV_DECAY: usize = 32;
const GEN_DELAY_VOL_ENV: usize = 33;
const GEN_ATTACK_VOL_ENV: usize = 34;
const GEN_HOLD_VOL_ENV: usize = 35;
const GEN_DECAY_VOL_ENV: usize = 36;
const GEN_SUSTAIN_VOL_ENV: usize = 37;
const GEN_RELEASE_VOL_ENV: usize = 38;
const GEN_KEYNUM_TO_VOL_ENV_HOLD: usize = 39;
const GEN_KEYNUM_TO_VOL_ENV_DECAY: usize = 40;
const GEN_INSTRUMENT: usize = 41;
const GEN_KEY_RANGE: usize = 43;
const GEN_VEL_RANGE: usize = 44;
const GEN_STARTLOOP_ADDRS_COARSE_OFFSET: usize = 45;
const GEN_KEYNUM: usize = 46;
const GEN_VELOCITY: usize = 47;
const GEN_INITIAL_ATTENUATION: usize = 48;
const GEN_ENDLOOP_ADDRS_COARSE_OFFSET: usize = 50;
const GEN_COARSE_TUNE: usize = 51;
const GEN_FINE_TUNE: usize = 52;
const GEN_SAMPLE_ID: usize = 53;
const GEN_SAMPLE_MODES: usize = 54;
const GEN_SCALE_TUNING: usize = 56;
const GEN_EXCLUSIVE_CLASS: usize = 57;
const GEN_OVERRIDING_ROOT_KEY: usize = 58;

fn default_generators() -> [i16; NUM_GENERATORS] {
    let mut g = [0i16; NUM_GENERATORS];
    g[GEN_INITIAL_FILTER_FC] = 13500;
    g[GEN_DELAY_MOD_LFO] = -12000;
    g[GEN_DELAY_VIB_LFO] = -12000;
    g[GEN_DELAY_MOD_ENV] = -12000;
    g[GEN_ATTACK_MOD_ENV] = -12000;
    g[GEN_HOLD_MOD_ENV] = -12000;
    g[GEN_DECAY_MOD_ENV] = -12000;
    g[GEN_RELEASE_MOD_ENV] = -12000;
    g[GEN_DELAY_VOL_ENV] = -12000;
    g[GEN_ATTACK_VOL_ENV] = -12000;
    g[GEN_HOLD_VOL_ENV] = -12000;
    g[GEN_DECAY_VOL_ENV] = -12000;
    g[GEN_RELEASE_VOL_ENV] = -12000;
    g[GEN_KEYNUM] = -1;
    g[GEN_VELOCITY] = -1;
    g[GEN_SCALE_TUNING] = 100;
    g[GEN_OVERRIDING_ROOT_KEY] = -1;
    g
}

fn is_additive_preset_gen(op: usize) -> bool {
    !matches!(
        op,
        GEN_START_ADDRS_OFFSET
            | GEN_END_ADDRS_OFFSET
            | GEN_STARTLOOP_ADDRS_OFFSET
            | GEN_ENDLOOP_ADDRS_OFFSET
            | GEN_START_ADDRS_COARSE_OFFSET
            | GEN_END_ADDRS_COARSE_OFFSET
            | GEN_STARTLOOP_ADDRS_COARSE_OFFSET
            | GEN_ENDLOOP_ADDRS_COARSE_OFFSET
            | GEN_INSTRUMENT
            | GEN_KEY_RANGE
            | GEN_VEL_RANGE
            | GEN_KEYNUM
            | GEN_VELOCITY
            | GEN_SAMPLE_ID
            | GEN_SAMPLE_MODES
            | GEN_EXCLUSIVE_CLASS
            | GEN_OVERRIDING_ROOT_KEY
    )
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LoopMode {
    NoLoop,
    Continuous,
    SustainLoop,
}

#[derive(Clone, Debug)]
pub struct ResolvedRegion {
    pub key_lo: u8,
    pub key_hi: u8,
    pub vel_lo: u8,
    pub vel_hi: u8,
    pub sample_start: u32,
    pub sample_end: u32,
    pub loop_start: u32,
    pub loop_end: u32,
    pub sample_rate: u32,
    pub root_key: i32,
    pub tune_cents: f32,
    pub scale_tuning: f32,
    pub loop_mode: LoopMode,
    pub attenuation_db: f32,
    pub pan: f32, // -0.5 .. +0.5
    pub reverb_send: f32,
    pub chorus_send: f32,
    pub exclusive_class: u16,
    pub fixed_key: i16,
    pub fixed_vel: i16,
    // Volume envelope (seconds & sustain attenuation dB)
    pub vol_delay_sec: f32,
    pub vol_attack_sec: f32,
    pub vol_hold_tc: i16,
    pub vol_decay_tc: i16,
    pub vol_sustain_db: f32,
    pub vol_release_sec: f32,
    pub key_to_vol_hold: i16,
    pub key_to_vol_decay: i16,
    // Mod envelope
    pub mod_delay_sec: f32,
    pub mod_attack_sec: f32,
    pub mod_hold_tc: i16,
    pub mod_decay_tc: i16,
    pub mod_sustain_level: f32,
    pub mod_release_sec: f32,
    pub key_to_mod_hold: i16,
    pub key_to_mod_decay: i16,
    pub mod_env_to_pitch: f32,
    pub mod_env_to_filter_fc: f32,
    // LFOs
    pub vib_lfo_delay_sec: f32,
    pub vib_lfo_freq_hz: f32,
    pub vib_lfo_to_pitch: f32,
    pub mod_lfo_delay_sec: f32,
    pub mod_lfo_freq_hz: f32,
    pub mod_lfo_to_pitch: f32,
    pub mod_lfo_to_filter_fc: f32,
    pub mod_lfo_to_vol_db: f32,
    // Resonant low-pass filter
    pub filter_fc_cents: f32,
    pub filter_q_db: f32,
}

#[derive(Clone, Debug)]
pub struct Sf2Preset {
    pub name: String,
    pub bank: u16,
    pub preset: u16,
    pub regions: Vec<ResolvedRegion>,
}

pub struct SoundFont2 {
    pub name: String,
    pub samples: Arc<[i16]>,
    pub presets: Vec<Sf2Preset>,
}

#[derive(Clone, Copy)]
struct Phdr {
    preset: u16,
    bank: u16,
    bag_ndx: u16,
    name_bytes: [u8; 20],
}

#[derive(Clone, Copy)]
struct InstHdr {
    bag_ndx: u16,
}

#[derive(Clone, Copy)]
struct Bag {
    gen_ndx: u16,
}

#[derive(Clone, Copy)]
struct GenRec {
    oper: u16,
    amount: i16,
}

#[derive(Clone, Copy)]
struct Shdr {
    start: u32,
    end: u32,
    start_loop: u32,
    end_loop: u32,
    sample_rate: u32,
    original_pitch: u8,
    pitch_correction: i8,
    sample_type: u16,
}

#[derive(Clone)]
struct ZoneGens {
    set_mask: u64,
    gens: [i16; NUM_GENERATORS],
    key_lo: u8,
    key_hi: u8,
    vel_lo: u8,
    vel_hi: u8,
    target_id: Option<u16>,
}

impl ZoneGens {
    fn new_empty() -> Self {
        Self {
            set_mask: 0,
            gens: [0i16; NUM_GENERATORS],
            key_lo: 0,
            key_hi: 127,
            vel_lo: 0,
            vel_hi: 127,
            target_id: None,
        }
    }

    fn apply_records(&mut self, recs: &[GenRec], terminal_oper: usize) {
        for r in recs {
            let op = r.oper as usize;
            if op >= NUM_GENERATORS {
                continue;
            }
            if op == GEN_KEY_RANGE {
                let raw = r.amount as u16;
                let lo = (raw & 0xFF) as u8;
                let hi = ((raw >> 8) & 0xFF) as u8;
                self.key_lo = lo.min(127);
                self.key_hi = hi.min(127);
            } else if op == GEN_VEL_RANGE {
                let raw = r.amount as u16;
                let lo = (raw & 0xFF) as u8;
                let hi = ((raw >> 8) & 0xFF) as u8;
                self.vel_lo = lo.min(127);
                self.vel_hi = hi.min(127);
            } else if op == terminal_oper {
                self.target_id = Some(r.amount as u16);
                break;
            } else {
                self.gens[op] = r.amount;
                self.set_mask |= 1u64 << op;
            }
        }
    }

    #[inline]
    fn has(&self, op: usize) -> bool {
        (self.set_mask & (1u64 << op)) != 0
    }
}

#[inline]
pub fn timecents_to_seconds(tc: f32) -> f32 {
    if tc <= -11950.0 {
        0.0
    } else {
        2.0_f32.powf(tc * (1.0 / 1200.0))
    }
}

#[inline]
pub fn cents_to_lfo_hz(cents: f32) -> f32 {
    8.176_f32 * 2.0_f32.powf(cents * (1.0 / 1200.0))
}

#[inline]
fn rd_u16_le(b: &[u8], off: usize) -> u16 {
    u16::from_le_bytes([b[off], b[off + 1]])
}

#[inline]
fn rd_i16_le(b: &[u8], off: usize) -> i16 {
    i16::from_le_bytes([b[off], b[off + 1]])
}

#[inline]
fn rd_u32_le(b: &[u8], off: usize) -> u32 {
    u32::from_le_bytes([b[off], b[off + 1], b[off + 2], b[off + 3]])
}

fn c_fixed_str(bytes: &[u8]) -> String {
    let end = bytes.iter().position(|&c| c == 0).unwrap_or(bytes.len());
    String::from_utf8_lossy(&bytes[..end]).trim().to_string()
}

impl SoundFont2 {
    /// Quick header check (`RIFF....sfbk`) without reading the entire file.
    pub fn has_sfbk_magic(header: &[u8]) -> bool {
        header.len() >= 12 && &header[0..4] == b"RIFF" && &header[8..12] == b"sfbk"
    }

    pub fn parse(data: &[u8]) -> Result<Self, &'static str> {
        if !Self::has_sfbk_magic(data) {
            return Err("Invalid SoundFont2 RIFF sfbk header");
        }
        let riff_len = (rd_u32_le(data, 4) as usize).saturating_add(8).min(data.len());
        let mut bank_name = String::from("SoundFont");
        let mut smpl_slice: Option<&[u8]> = None;

        let mut phdrs: Vec<Phdr> = Vec::new();
        let mut pbags: Vec<Bag> = Vec::new();
        let mut pgens: Vec<GenRec> = Vec::new();
        let mut insts: Vec<InstHdr> = Vec::new();
        let mut ibags: Vec<Bag> = Vec::new();
        let mut igens: Vec<GenRec> = Vec::new();
        let mut shdrs: Vec<Shdr> = Vec::new();

        let mut pos = 12usize;
        while pos + 8 <= riff_len {
            let cid = &data[pos..pos + 4];
            let clen = rd_u32_le(data, pos + 4) as usize;
            let body_start = pos + 8;
            let body_end = body_start.saturating_add(clen).min(riff_len);
            if cid == b"LIST" && body_start + 4 <= body_end {
                let list_type = &data[body_start..body_start + 4];
                let mut sub = body_start + 4;
                while sub + 8 <= body_end {
                    let sid = &data[sub..sub + 4];
                    let slen = rd_u32_le(data, sub + 4) as usize;
                    let sstart = sub + 8;
                    let send = sstart.saturating_add(slen).min(body_end);
                    let chunk = &data[sstart..send];

                    if list_type == b"INFO" {
                        if sid == b"INAM" {
                            let s = c_fixed_str(chunk);
                            if !s.is_empty() {
                                bank_name = s;
                            }
                        }
                    } else if list_type == b"sdta" {
                        if sid == b"smpl" {
                            smpl_slice = Some(chunk);
                        }
                    } else if list_type == b"pdta" {
                        match sid {
                            b"phdr" => {
                                let n = chunk.len() / 38;
                                phdrs.reserve(n);
                                for i in 0..n {
                                    let o = i * 38;
                                    let mut name_bytes = [0u8; 20];
                                    name_bytes.copy_from_slice(&chunk[o..o + 20]);
                                    phdrs.push(Phdr {
                                        preset: rd_u16_le(chunk, o + 20),
                                        bank: rd_u16_le(chunk, o + 22),
                                        bag_ndx: rd_u16_le(chunk, o + 24),
                                        name_bytes,
                                    });
                                }
                            }
                            b"pbag" => {
                                let n = chunk.len() / 4;
                                pbags.reserve(n);
                                for i in 0..n {
                                    pbags.push(Bag {
                                        gen_ndx: rd_u16_le(chunk, i * 4),
                                    });
                                }
                            }
                            b"pgen" => {
                                let n = chunk.len() / 4;
                                pgens.reserve(n);
                                for i in 0..n {
                                    let o = i * 4;
                                    pgens.push(GenRec {
                                        oper: rd_u16_le(chunk, o),
                                        amount: rd_i16_le(chunk, o + 2),
                                    });
                                }
                            }
                            b"inst" => {
                                let n = chunk.len() / 22;
                                insts.reserve(n);
                                for i in 0..n {
                                    insts.push(InstHdr {
                                        bag_ndx: rd_u16_le(chunk, i * 22 + 20),
                                    });
                                }
                            }
                            b"ibag" => {
                                let n = chunk.len() / 4;
                                ibags.reserve(n);
                                for i in 0..n {
                                    ibags.push(Bag {
                                        gen_ndx: rd_u16_le(chunk, i * 4),
                                    });
                                }
                            }
                            b"igen" => {
                                let n = chunk.len() / 4;
                                igens.reserve(n);
                                for i in 0..n {
                                    let o = i * 4;
                                    igens.push(GenRec {
                                        oper: rd_u16_le(chunk, o),
                                        amount: rd_i16_le(chunk, o + 2),
                                    });
                                }
                            }
                            b"shdr" => {
                                let n = chunk.len() / 46;
                                shdrs.reserve(n);
                                for i in 0..n {
                                    let o = i * 46;
                                    shdrs.push(Shdr {
                                        start: rd_u32_le(chunk, o + 20),
                                        end: rd_u32_le(chunk, o + 24),
                                        start_loop: rd_u32_le(chunk, o + 28),
                                        end_loop: rd_u32_le(chunk, o + 32),
                                        sample_rate: rd_u32_le(chunk, o + 36),
                                        original_pitch: chunk[o + 40],
                                        pitch_correction: chunk[o + 41] as i8,
                                        sample_type: rd_u16_le(chunk, o + 44),
                                    });
                                }
                            }
                            _ => {}
                        }
                    }

                    sub = send + (slen & 1);
                }
            }
            pos = body_end + (clen & 1);
        }

        let smpl_raw = smpl_slice.ok_or("SF2 missing smpl sample data")?;
        if phdrs.len() < 2 || insts.len() < 2 || shdrs.len() < 2 {
            return Err("SF2 missing preset/instrument/sample headers");
        }

        let sample_count = smpl_raw.len() / 2;
        let mut pcm = Vec::with_capacity(sample_count);
        for pair in smpl_raw.chunks_exact(2) {
            pcm.push(i16::from_le_bytes([pair[0], pair[1]]));
        }
        let samples: Arc<[i16]> = pcm.into();
        let total_samples = samples.len() as i64;

        // Pre-parse instrument zones once so presets sharing instruments build quickly.
        let inst_count = insts.len() - 1;
        let mut parsed_insts: Vec<(Option<ZoneGens>, Vec<ZoneGens>)> =
            Vec::with_capacity(inst_count);
        for idx in 0..inst_count {
            let b_start = insts[idx].bag_ndx as usize;
            let b_end = (insts[idx + 1].bag_ndx as usize).min(ibags.len().saturating_sub(1));
            let mut global_zone: Option<ZoneGens> = None;
            let mut local_zones: Vec<ZoneGens> = Vec::new();
            if b_start < b_end {
                for b in b_start..b_end {
                    let g_start = ibags[b].gen_ndx as usize;
                    let g_end =
                        (ibags[b + 1].gen_ndx as usize).min(igens.len().saturating_sub(1));
                    if g_start > g_end || g_end > igens.len() {
                        continue;
                    }
                    let mut z = ZoneGens::new_empty();
                    z.apply_records(&igens[g_start..g_end], GEN_SAMPLE_ID);
                    if z.target_id.is_none() && b == b_start {
                        global_zone = Some(z);
                    } else if z.target_id.is_some() {
                        local_zones.push(z);
                    }
                }
            }
            parsed_insts.push((global_zone, local_zones));
        }

        let preset_count = phdrs.len() - 1;
        let shdr_count = shdrs.len() - 1;
        let mut presets: Vec<Sf2Preset> = Vec::with_capacity(preset_count);

        for p_idx in 0..preset_count {
            let ph = &phdrs[p_idx];
            let b_start = ph.bag_ndx as usize;
            let b_end = (phdrs[p_idx + 1].bag_ndx as usize).min(pbags.len().saturating_sub(1));
            if b_start >= b_end {
                continue;
            }

            let mut p_global: Option<ZoneGens> = None;
            let mut regions: Vec<ResolvedRegion> = Vec::new();

            for b in b_start..b_end {
                let g_start = pbags[b].gen_ndx as usize;
                let g_end = (pbags[b + 1].gen_ndx as usize).min(pgens.len().saturating_sub(1));
                if g_start > g_end || g_end > pgens.len() {
                    continue;
                }
                let mut pz = ZoneGens::new_empty();
                pz.apply_records(&pgens[g_start..g_end], GEN_INSTRUMENT);
                if pz.target_id.is_none() {
                    if b == b_start {
                        p_global = Some(pz);
                    }
                    continue;
                }

                let inst_id = pz.target_id.unwrap() as usize;
                if inst_id >= parsed_insts.len() {
                    continue;
                }

                // Effective preset zone: preset global overridden by local preset zone
                let mut eff_p = ZoneGens::new_empty();
                if let Some(ref pg) = p_global {
                    eff_p = pg.clone();
                }
                for op in 0..NUM_GENERATORS {
                    if pz.has(op) {
                        eff_p.gens[op] = pz.gens[op];
                        eff_p.set_mask |= 1u64 << op;
                    }
                }
                let p_key_lo = if pz.key_lo != 0 || pz.key_hi != 127 {
                    pz.key_lo
                } else {
                    eff_p.key_lo
                };
                let p_key_hi = if pz.key_lo != 0 || pz.key_hi != 127 {
                    pz.key_hi
                } else {
                    eff_p.key_hi
                };
                let p_vel_lo = if pz.vel_lo != 0 || pz.vel_hi != 127 {
                    pz.vel_lo
                } else {
                    eff_p.vel_lo
                };
                let p_vel_hi = if pz.vel_lo != 0 || pz.vel_hi != 127 {
                    pz.vel_hi
                } else {
                    eff_p.vel_hi
                };

                let (ref i_global, ref i_zones) = parsed_insts[inst_id];

                for iz in i_zones {
                    let Some(sample_id) = iz.target_id else {
                        continue;
                    };
                    let s_idx = sample_id as usize;
                    if s_idx >= shdr_count {
                        continue;
                    }
                    let sh = &shdrs[s_idx];
                    // Ignore ROM sample headers (bit 15 set in sfSampleType)
                    if (sh.sample_type & 0x8000) != 0 {
                        continue;
                    }

                    // Intersect key & velocity ranges between preset zone and instrument zone
                    let i_klo = if iz.key_lo != 0 || iz.key_hi != 127 {
                        iz.key_lo
                    } else if let Some(ref ig) = i_global {
                        ig.key_lo
                    } else {
                        0
                    };
                    let i_khi = if iz.key_lo != 0 || iz.key_hi != 127 {
                        iz.key_hi
                    } else if let Some(ref ig) = i_global {
                        ig.key_hi
                    } else {
                        127
                    };
                    let i_vlo = if iz.vel_lo != 0 || iz.vel_hi != 127 {
                        iz.vel_lo
                    } else if let Some(ref ig) = i_global {
                        ig.vel_lo
                    } else {
                        0
                    };
                    let i_vhi = if iz.vel_lo != 0 || iz.vel_hi != 127 {
                        iz.vel_hi
                    } else if let Some(ref ig) = i_global {
                        ig.vel_hi
                    } else {
                        127
                    };

                    let key_lo = p_key_lo.max(i_klo);
                    let key_hi = p_key_hi.min(i_khi);
                    let vel_lo = p_vel_lo.max(i_vlo);
                    let vel_hi = p_vel_hi.min(i_vhi);
                    if key_lo > key_hi || vel_lo > vel_hi {
                        continue;
                    }

                    // Start from SF2 defaults, apply instrument global, override with instrument local,
                    // then add preset offsets for additive generators.
                    let mut g = default_generators();
                    if let Some(ref ig) = i_global {
                        for op in 0..NUM_GENERATORS {
                            if ig.has(op) {
                                g[op] = ig.gens[op];
                            }
                        }
                    }
                    for op in 0..NUM_GENERATORS {
                        if iz.has(op) {
                            g[op] = iz.gens[op];
                        }
                    }
                    for op in 0..NUM_GENERATORS {
                        if eff_p.has(op) && is_additive_preset_gen(op) {
                            g[op] = g[op].saturating_add(eff_p.gens[op]);
                        }
                    }

                    // Sample address computation
                    let start_off =
                        (g[GEN_START_ADDRS_OFFSET] as i64) + (g[GEN_START_ADDRS_COARSE_OFFSET] as i64) * 32768;
                    let end_off =
                        (g[GEN_END_ADDRS_OFFSET] as i64) + (g[GEN_END_ADDRS_COARSE_OFFSET] as i64) * 32768;
                    let lstart_off = (g[GEN_STARTLOOP_ADDRS_OFFSET] as i64)
                        + (g[GEN_STARTLOOP_ADDRS_COARSE_OFFSET] as i64) * 32768;
                    let lend_off = (g[GEN_ENDLOOP_ADDRS_OFFSET] as i64)
                        + (g[GEN_ENDLOOP_ADDRS_COARSE_OFFSET] as i64) * 32768;

                    let s_start = ((sh.start as i64) + start_off).clamp(0, total_samples);
                    let s_end = ((sh.end as i64) + end_off).clamp(s_start, total_samples);
                    if s_end - s_start < 2 {
                        continue;
                    }
                    let l_start = ((sh.start_loop as i64) + lstart_off).clamp(s_start, s_end);
                    let l_end = ((sh.end_loop as i64) + lend_off).clamp(l_start, s_end);

                    let raw_mode = (g[GEN_SAMPLE_MODES] as u16) & 3;
                    let loop_mode = if l_end - l_start >= 2 {
                        match raw_mode {
                            1 => LoopMode::Continuous,
                            3 => LoopMode::SustainLoop,
                            _ => LoopMode::NoLoop,
                        }
                    } else {
                        LoopMode::NoLoop
                    };

                    let root_key = if (0..=127).contains(&g[GEN_OVERRIDING_ROOT_KEY]) {
                        g[GEN_OVERRIDING_ROOT_KEY] as i32
                    } else if sh.original_pitch <= 127 {
                        sh.original_pitch as i32
                    } else {
                        60
                    };

                    let tune_cents = (g[GEN_COARSE_TUNE] as f32) * 100.0
                        + (g[GEN_FINE_TUNE] as f32)
                        + (sh.pitch_correction as f32);
                    let scale_tuning = (g[GEN_SCALE_TUNING] as f32).clamp(0.0, 1200.0);

                    // FluidSynth / EMU10K1 convention: initialAttenuation is 0.04 dB/unit
                    let attenuation_db =
                        ((g[GEN_INITIAL_ATTENUATION] as f32).max(0.0) * 0.04).min(144.0);
                    let pan = ((g[GEN_PAN] as f32) * 0.001).clamp(-0.5, 0.5);
                    let reverb_send = ((g[GEN_REVERB_EFFECTS_SEND] as f32) * 0.001).clamp(0.0, 1.0);
                    let chorus_send = ((g[GEN_CHORUS_EFFECTS_SEND] as f32) * 0.001).clamp(0.0, 1.0);

                    regions.push(ResolvedRegion {
                        key_lo,
                        key_hi,
                        vel_lo,
                        vel_hi,
                        sample_start: s_start as u32,
                        sample_end: s_end as u32,
                        loop_start: l_start as u32,
                        loop_end: l_end as u32,
                        sample_rate: sh.sample_rate.clamp(4000, 192_000),
                        root_key,
                        tune_cents,
                        scale_tuning,
                        loop_mode,
                        attenuation_db,
                        pan,
                        reverb_send,
                        chorus_send,
                        exclusive_class: g[GEN_EXCLUSIVE_CLASS].max(0) as u16,
                        fixed_key: g[GEN_KEYNUM],
                        fixed_vel: g[GEN_VELOCITY],
                        vol_delay_sec: timecents_to_seconds(g[GEN_DELAY_VOL_ENV] as f32),
                        vol_attack_sec: timecents_to_seconds(g[GEN_ATTACK_VOL_ENV] as f32),
                        vol_hold_tc: g[GEN_HOLD_VOL_ENV],
                        vol_decay_tc: g[GEN_DECAY_VOL_ENV],
                        vol_sustain_db: ((g[GEN_SUSTAIN_VOL_ENV] as f32) * 0.1).clamp(0.0, 144.0),
                        vol_release_sec: timecents_to_seconds(g[GEN_RELEASE_VOL_ENV] as f32)
                            .max(0.004),
                        key_to_vol_hold: g[GEN_KEYNUM_TO_VOL_ENV_HOLD],
                        key_to_vol_decay: g[GEN_KEYNUM_TO_VOL_ENV_DECAY],
                        mod_delay_sec: timecents_to_seconds(g[GEN_DELAY_MOD_ENV] as f32),
                        mod_attack_sec: timecents_to_seconds(g[GEN_ATTACK_MOD_ENV] as f32),
                        mod_hold_tc: g[GEN_HOLD_MOD_ENV],
                        mod_decay_tc: g[GEN_DECAY_MOD_ENV],
                        mod_sustain_level: (1.0 - (g[GEN_SUSTAIN_MOD_ENV] as f32) * 0.001)
                            .clamp(0.0, 1.0),
                        mod_release_sec: timecents_to_seconds(g[GEN_RELEASE_MOD_ENV] as f32)
                            .max(0.004),
                        key_to_mod_hold: g[GEN_KEYNUM_TO_MOD_ENV_HOLD],
                        key_to_mod_decay: g[GEN_KEYNUM_TO_MOD_ENV_DECAY],
                        mod_env_to_pitch: g[GEN_MOD_ENV_TO_PITCH] as f32,
                        mod_env_to_filter_fc: g[GEN_MOD_ENV_TO_FILTER_FC] as f32,
                        vib_lfo_delay_sec: timecents_to_seconds(g[GEN_DELAY_VIB_LFO] as f32),
                        vib_lfo_freq_hz: cents_to_lfo_hz(g[GEN_FREQ_VIB_LFO] as f32)
                            .clamp(0.05, 50.0),
                        vib_lfo_to_pitch: g[GEN_VIB_LFO_TO_PITCH] as f32,
                        mod_lfo_delay_sec: timecents_to_seconds(g[GEN_DELAY_MOD_LFO] as f32),
                        mod_lfo_freq_hz: cents_to_lfo_hz(g[GEN_FREQ_MOD_LFO] as f32)
                            .clamp(0.05, 50.0),
                        mod_lfo_to_pitch: g[GEN_MOD_LFO_TO_PITCH] as f32,
                        mod_lfo_to_filter_fc: g[GEN_MOD_LFO_TO_FILTER_FC] as f32,
                        mod_lfo_to_vol_db: (g[GEN_MOD_LFO_TO_VOLUME] as f32) * 0.1,
                        filter_fc_cents: (g[GEN_INITIAL_FILTER_FC] as f32).clamp(1500.0, 13500.0),
                        filter_q_db: ((g[GEN_INITIAL_FILTER_Q] as f32) * 0.1).clamp(0.0, 96.0),
                    });
                }
            }

            if !regions.is_empty() {
                presets.push(Sf2Preset {
                    name: c_fixed_str(&ph.name_bytes),
                    bank: ph.bank,
                    preset: ph.preset,
                    regions,
                });
            }
        }

        if presets.is_empty() {
            return Err("SF2 contains no playable presets");
        }

        presets.sort_by_key(|p| (p.bank, p.preset));

        Ok(Self {
            name: bank_name,
            samples,
            presets,
        })
    }

    /// Find best preset for `(bank, program)` with General MIDI fallback rules.
    pub fn find_preset(&self, bank: u16, program: u8, is_drum_channel: bool) -> Option<&Sf2Preset> {
        let prog = program as u16;
        let lookup = |b: u16, p: u16| -> Option<&Sf2Preset> {
            self.presets
                .binary_search_by_key(&(b, p), |pr| (pr.bank, pr.preset))
                .ok()
                .map(|idx| &self.presets[idx])
        };

        if is_drum_channel || bank == 128 {
            lookup(128, prog)
                .or_else(|| lookup(128, 0))
                .or_else(|| self.presets.iter().find(|p| p.bank == 128))
        } else {
            lookup(bank, prog)
                .or_else(|| lookup(0, prog))
                .or_else(|| lookup(0, 0))
                .or_else(|| self.presets.first())
        }
    }
}
