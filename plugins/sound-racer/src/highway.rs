//! Shared pseudo-3D segmented road projection, spaced bass-driven elevation waves
//! (crests & dips with mandatory flat-road cooldowns), beat-synced *OutRun* roadside
//! props (utility poles & geometric retro billboards), and strict viewport clipping.
//!
//! Included 1:1 by both `plugins/sound-racer` (Firmware / Android) and `web/uipod-wasm`.

use std::f32::consts::PI;

use super::sprites::{
    car_draw_rect, CarSpriteCache, CAR_BASE_W, LINEAR_SPRITE_RADIUS,
};

/* ========================================================================== *
 * Declarative LayoutSpec & Pseudo-3D Road Geometry (`1024 x 768`)            *
 * ========================================================================== */

pub const VIEW_X: f32 = 32.0;
pub const VIEW_Y: f32 = 178.0; // `CONTENT_TOP_Y (136.0) + 42.0`
pub const VIEW_W: f32 = 700.0;
pub const VIEW_H: f32 = 562.0;
pub const CARD_RADIUS: f32 = 10.0;
pub const CARD_STROKE_W: f32 = 3.5;

/// Strict interior clip rectangle inside the `3.5 px` viewport card frame.
pub const CLIP_LEFT: f32 = VIEW_X + CARD_STROKE_W;
pub const CLIP_RIGHT: f32 = VIEW_X + VIEW_W - CARD_STROKE_W;
pub const CLIP_TOP: f32 = VIEW_Y + CARD_STROKE_W;
pub const CLIP_BOTTOM: f32 = VIEW_Y + VIEW_H - CARD_STROKE_W;

pub const SHIP_BASE_Y: f32 = VIEW_Y + VIEW_H - 58.0; // 682.0
pub const BASE_HORIZON_Y: f32 = VIEW_Y + 80.0; // 258.0
pub const HORIZON_MAX_SHIFT_PX: f32 = 14.0;

pub const PLAYER_TIRE_OFFSET_Y: f32 = 14.0;
pub const PLAYER_JUMP_HEIGHT_PX: f32 = 72.0;
pub const PLAYER_SHADOW_W: f32 = 96.0;
pub const PLAYER_SHADOW_H: f32 = 12.0;
pub const PLAYER_SHADOW_OFFSET_Y: f32 = 8.0;
pub const PLAYER_SHADOW_RADIUS: f32 = 5.0;
pub const OBSTACLE_MIN_W: f32 = 12.0;
pub const OBSTACLE_MIN_Z_VIS: f32 = -0.038;
pub const OBSTACLE_MAX_Z_VIS: f32 = 1.02;

/// Horizon-to-camera travel duration (`1.50s`), giving `>= 1.125s` unobstructed
/// reaction visibility even right after clearing the steepest uphill crest (`z_peak >= 0.750`).
pub const TRAVEL_TIME_MS: u32 = 1_500;
pub const TRAVEL_TIME_S: f32 = (TRAVEL_TIME_MS as f32) * 0.001;

/// Number of perspective `z`-slices (`28`, within `24..=32`) used for curved road rails & occlusion.
pub const ROAD_SLICES: usize = 28;
pub const GRID_LINE_COUNT: usize = 9;

const ROAD_CURVE_VP_DX: f32 = 105.0;
const ROAD_HALF_W_FAR: f32 = 24.0;
const ROAD_HALF_W_SPAN: f32 = 246.0;
const SCALE_FAR: f32 = 0.09;
const SCALE_SPAN: f32 = 0.91;

// Sky & Spectrum Bar LayoutSpec
const SUN_BASE_R: f32 = 16.0;
const SUN_KICK_R: f32 = 20.0;
const SUN_OFFSET_ABOVE_HORIZON_Y: f32 = 10.0;
const SUN_CORE_RATIO: f32 = 0.56;
const SKY_BAR_SPAN_W: f32 = 460.0;
const SKY_BAR_GAP_W: f32 = 6.0;
const SKY_BAR_MIN_H: f32 = 3.0;
const SKY_BAR_BASE_H: f32 = 4.0;
const SKY_BAR_GAIN_H: f32 = 50.0;
const SKY_TOP_HEADROOM_PAD: f32 = 12.0;

// Elevation Trigger & Wave Envelope Constants
pub const BASS_TRIGGER_MIN: f32 = 0.72;
pub const BASS_TRIGGER_REL_MULT: f32 = 1.35;
const BASS_WARMUP_S: f32 = 1.20;
const WAVE_MIN_DURATION_S: f32 = 3.80;
const WAVE_MAX_DURATION_S: f32 = 4.30;
pub const COOLDOWN_MIN_S: f32 = 8.50;
pub const COOLDOWN_MAX_S: f32 = 11.50;

const CREST_AMP_PX: f32 = -49.0;
const DIP_AMP_PX: f32 = 44.0;
const WAVE_HALF_WIDTH_Z: f32 = 0.36;
const HORIZON_WAVE_SHIFT_PX: f32 = 12.5;

// Roadside Props LayoutSpec (`<= 8` active props, `Shade 1` & `Shade 2` only)
pub const MAX_ROADSIDE_PROPS: usize = 8;
const POLE_LANE_OFFSET: f32 = 1.78;
const POLE_BASE_H: f32 = 112.0;
const POLE_ARM_DX: f32 = 25.0;
const POLE_ARM_DY: f32 = 5.0;

const BILLBOARD_LANE_OFFSET: f32 = 2.08;
const BILLBOARD_BASE_W: f32 = 114.0;
const BILLBOARD_BASE_H: f32 = 54.0;
const BILLBOARD_LEG_H: f32 = 22.0;

/* ========================================================================== *
 * Audio Low-Frequency (`30–140 Hz`) Telemetry Helper                         *
 * ========================================================================== */

/// Extracts normalized `[0.0..=1.0]` low-frequency bass energy (`30–140 Hz`) from
/// `TrackLiveFrame` / `PluginLiveTelemetry` (`spectrum_64[0..15]` covers `32..138 Hz`).
#[inline]
pub fn extract_bass_30_140(
    bands_5: &[f32; 5],
    spectrum_64: &[u8; 64],
    kick_impulse: f32,
) -> f32 {
    let mut spec_sum = 0u32;
    let mut spec_pk = 0u8;
    for &bin in &spectrum_64[0..15] {
        spec_sum += bin as u32;
        spec_pk = spec_pk.max(bin);
    }
    let spec_avg = (spec_sum as f32) / (15.0 * 255.0);
    let spec_max = (spec_pk as f32) / 255.0;
    let spec_low = spec_avg * 0.50 + spec_max * 0.50;
    let band_low = bands_5[0] * 0.62 + bands_5[1] * 0.38;
    (spec_low.max(band_low) * 0.82 + kick_impulse.clamp(0.0, 1.0) * 0.18).clamp(0.0, 1.0)
}

/* ========================================================================== *
 * Spaced Bass-Driven Elevation State Machine (Crests & Dips)                 *
 * ========================================================================== */

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ElevationKind {
    Crest,
    Dip,
}

impl ElevationKind {
    #[inline]
    pub const fn toggle(self) -> Self {
        match self {
            Self::Crest => Self::Dip,
            Self::Dip => Self::Crest,
        }
    }

    #[inline]
    pub const fn sign(self) -> f32 {
        match self {
            Self::Crest => 1.0,
            Self::Dip => -1.0,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum ElevationPhase {
    Idle,
    Active {
        kind: ElevationKind,
        elapsed_s: f32,
        duration_s: f32,
        strength: f32,
    },
    Cooldown {
        rem_s: f32,
    },
}

#[derive(Clone, Copy, Debug)]
pub struct ElevationController {
    phase: ElevationPhase,
    next_kind: ElevationKind,
    warmup_s: f32,
    bass_fast: f32,
    bass_mean: f32,
    wave_counter: u32,
}

impl Default for ElevationController {
    fn default() -> Self {
        Self::new()
    }
}

impl ElevationController {
    pub const fn new() -> Self {
        Self {
            phase: ElevationPhase::Idle,
            next_kind: ElevationKind::Crest,
            warmup_s: 0.0,
            bass_fast: 0.25,
            bass_mean: 0.32,
            wave_counter: 0,
        }
    }

    pub fn reset(&mut self) {
        *self = Self::new();
    }

    #[inline]
    pub const fn active_kind(&self) -> Option<ElevationKind> {
        match self.phase {
            ElevationPhase::Active { kind, .. } => Some(kind),
            ElevationPhase::Idle | ElevationPhase::Cooldown { .. } => None,
        }
    }

    #[cfg(test)]
    #[inline]
    pub const fn is_in_cooldown(&self) -> bool {
        matches!(self.phase, ElevationPhase::Cooldown { .. })
    }

    #[cfg(test)]
    #[inline]
    pub const fn wave_count(&self) -> u32 {
        self.wave_counter
    }

    /// Advances bass smoothing, active hill/dip wave traversal (`3.8–4.3s`), and mandatory
    /// flat-road cooldown (`8.5–11.5s`). Call only while audio is actively playing.
    pub fn update(&mut self, dt_s: f32, raw_bass: f32, seed_u32: u32) {
        let dt = dt_s.clamp(0.0, 0.10);
        if dt <= 0.0 {
            return;
        }
        let bass = raw_bass.clamp(0.0, 1.0);
        self.warmup_s = (self.warmup_s + dt).min(10.0);

        let fast_alpha = (dt * 9.5).min(1.0);
        let mean_alpha = (dt * 0.32).min(1.0);
        self.bass_fast += (bass - self.bass_fast) * fast_alpha;

        // Update rolling track bass baseline slightly slower during an extreme drop peak
        // so sustained 0.8+ bass surges cleanly cross `1.35 * bass_mean`.
        let baseline_rate = if bass > self.bass_mean * 1.25 {
            mean_alpha * 0.35
        } else {
            mean_alpha
        };
        self.bass_mean = (self.bass_mean + (bass - self.bass_mean) * baseline_rate).clamp(0.12, 0.85);

        let jitter = ((seed_u32.wrapping_mul(0x9E37_79B9) ^ self.wave_counter.wrapping_mul(0x85EB_CA6B))
            >> 16) as f32
            * (1.0 / 65535.0);

        match self.phase {
            ElevationPhase::Active {
                kind,
                mut elapsed_s,
                duration_s,
                strength,
            } => {
                elapsed_s += dt;
                if elapsed_s >= duration_s {
                    let cd = COOLDOWN_MIN_S + (COOLDOWN_MAX_S - COOLDOWN_MIN_S) * jitter;
                    self.phase = ElevationPhase::Cooldown { rem_s: cd };
                } else {
                    self.phase = ElevationPhase::Active {
                        kind,
                        elapsed_s,
                        duration_s,
                        strength,
                    };
                }
            }
            ElevationPhase::Cooldown { mut rem_s } => {
                rem_s -= dt;
                if rem_s <= 0.0 {
                    self.phase = ElevationPhase::Idle;
                } else {
                    self.phase = ElevationPhase::Cooldown { rem_s };
                }
            }
            ElevationPhase::Idle => {
                if self.warmup_s >= BASS_WARMUP_S
                    && self.bass_fast > BASS_TRIGGER_MIN
                    && self.bass_fast > self.bass_mean * BASS_TRIGGER_REL_MULT
                {
                    let kind = self.next_kind;
                    self.next_kind = self.next_kind.toggle();
                    self.wave_counter = self.wave_counter.wrapping_add(1);
                    let dur = WAVE_MIN_DURATION_S + (WAVE_MAX_DURATION_S - WAVE_MIN_DURATION_S) * jitter;
                    let strength = ((self.bass_fast - 0.55) / 0.35).clamp(0.86, 1.0);
                    self.phase = ElevationPhase::Active {
                        kind,
                        elapsed_s: 0.0,
                        duration_s: dur,
                        strength,
                    };
                }
            }
        }
    }

    /// Evaluates the current frame's immutable elevation parameters for `z`-projection.
    pub fn snapshot(&self) -> ElevationSnapshot {
        match self.phase {
            ElevationPhase::Active {
                kind,
                elapsed_s,
                duration_s,
                strength,
            } => {
                let u = (elapsed_s / duration_s.max(0.1)).clamp(0.0, 1.0);
                let shaped_u = u.powf(0.80);
                let env = 0.5 * (1.0 - (2.0 * PI * shaped_u).cos()) * strength;
                let sign = kind.sign();
                let z_center = 0.96 - 0.96 * u;
                let horizon_shift = (sign * HORIZON_WAVE_SHIFT_PX * env)
                    .clamp(-HORIZON_MAX_SHIFT_PX, HORIZON_MAX_SHIFT_PX);
                ElevationSnapshot {
                    sign,
                    env,
                    z_center,
                    horizon_shift,
                }
            }
            ElevationPhase::Idle | ElevationPhase::Cooldown { .. } => ElevationSnapshot::flat(),
        }
    }

    /// Synchronized `Slope:` indicator text preserving `RUSH!` during intense flat sections
    /// and reflecting active `Crest` (`▲ Climb` / `▲ Підйом`) and `Dip` (`▼ Dip` / `▼ Спад`).
    pub fn slope_label(&self, intensity: f32, uk: bool) -> &'static str {
        match self.active_kind() {
            Some(ElevationKind::Crest) => {
                if uk {
                    "Схил: ▲ Підйом"
                } else {
                    "Slope: ▲ Climb"
                }
            }
            Some(ElevationKind::Dip) => {
                if intensity > 0.68 {
                    if uk {
                        "Схил: ▼ ДРАЙВ!"
                    } else {
                        "Slope: ▼ RUSH!"
                    }
                } else if uk {
                    "Схил: ▼ Спад"
                } else {
                    "Slope: ▼ Dip"
                }
            }
            None => {
                if intensity > 0.68 {
                    if uk {
                        "Схил: ▼ ДРАЙВ!"
                    } else {
                        "Slope: ▼ RUSH!"
                    }
                } else if uk {
                    "Схил: ► Ритм"
                } else {
                    "Slope: ► Groove"
                }
            }
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct RoadPoint {
    pub x: f32,
    pub y: f32,
    pub scale: f32,
}

#[derive(Clone, Copy, Debug)]
pub struct ElevationSnapshot {
    pub sign: f32,
    pub env: f32,
    pub z_center: f32,
    pub horizon_shift: f32,
}

impl ElevationSnapshot {
    #[inline]
    pub const fn flat() -> Self {
        Self {
            sign: 0.0,
            env: 0.0,
            z_center: 0.5,
            horizon_shift: 0.0,
        }
    }

    #[inline]
    pub fn vp_y(&self) -> f32 {
        BASE_HORIZON_Y + self.horizon_shift.clamp(-HORIZON_MAX_SHIFT_PX, HORIZON_MAX_SHIFT_PX)
    }

    /// Vertical road elevation displacement `dy(z)` (`0.0` when flat; `< 0` on uphill crest; `> 0` on dip).
    #[inline]
    pub fn dy(&self, z: f32) -> f32 {
        if self.env <= 1e-4 || z <= 0.0 {
            return 0.0;
        }
        let q = ((z - self.z_center) / WAVE_HALF_WIDTH_Z).clamp(-1.0, 1.0);
        let bell = 0.5 * (1.0 + (PI * q).cos());
        let t_cam = (z / 0.14).clamp(0.0, 1.0);
        let cam_taper = t_cam * t_cam * (3.0 - 2.0 * t_cam);
        let amp = if self.sign >= 0.0 {
            CREST_AMP_PX
        } else {
            DIP_AMP_PX
        };
        amp * self.env * bell * cam_taper
    }

    /// Projects `(z, lane_offset)` into screen space `(x, y_ground(z), scale)`.
    /// Convention: `z = 0.0` at camera/player (`SHIP_BASE_Y`), `z = 1.0` at horizon (`vp_y`).
    #[inline]
    pub fn project(&self, z: f32, lane_offset: f32, road_curve: f32) -> RoadPoint {
        let zc = z.clamp(OBSTACLE_MIN_Z_VIS, 1.0);
        let curve_z = zc.max(0.0);
        let inv = 1.0 - zc;
        let p = inv * inv;
        let scale = SCALE_FAR + SCALE_SPAN * p;

        let base_cx = VIEW_X + VIEW_W * 0.5;
        let vp_x = base_cx + road_curve * ROAD_CURVE_VP_DX;
        let center_x = base_cx + (vp_x - base_cx) * (curve_z * curve_z);

        let vp_y = self.vp_y();
        let base_y = vp_y + (SHIP_BASE_Y - vp_y) * p;
        let y = base_y + self.dy(curve_z);

        let road_half_w = ROAD_HALF_W_FAR + ROAD_HALF_W_SPAN * p;
        let lane_w = (road_half_w * 2.0) / 3.0;
        let x = center_x + lane_offset * lane_w;

        RoadPoint { x, y, scale }
    }

    /// Precomputes the `28`-slice front-to-back horizon occlusion profile (`min_visible_y`).
    pub fn compute_profile(&self, road_curve: f32) -> RoadProfile {
        let mut slice_z = [0.0f32; ROAD_SLICES + 1];
        let mut min_vis_y = [CLIP_BOTTOM; ROAD_SLICES + 1];
        let mut slice_visible = [true; ROAD_SLICES + 1];

        let mut running_min_y = CLIP_BOTTOM;
        let mut crest_idx = ROAD_SLICES;
        let mut max_occluded_drop = 0.0f32;

        for i in 0..=ROAD_SLICES {
            let z = (i as f32) / (ROAD_SLICES as f32);
            let pt = self.project(z, 0.0, road_curve);
            slice_z[i] = z;
            if i == 0 {
                slice_visible[i] = pt.y <= CLIP_BOTTOM;
                running_min_y = running_min_y.min(pt.y);
                min_vis_y[i] = running_min_y;
            } else if pt.y < running_min_y - 0.05 {
                slice_visible[i] = true;
                running_min_y = pt.y;
                min_vis_y[i] = running_min_y;
                crest_idx = i;
            } else {
                slice_visible[i] = false;
                min_vis_y[i] = running_min_y;
                max_occluded_drop = max_occluded_drop.max(pt.y - running_min_y);
            }
        }

        // Refine exact sub-slice crest peak when road folds over
        let crest_z = if max_occluded_drop > 0.25 {
            slice_z[crest_idx]
        } else {
            1.0
        };
        let crest_y = min_vis_y[ROAD_SLICES].clamp(CLIP_TOP, CLIP_BOTTOM);

        RoadProfile {
            slice_z,
            min_vis_y,
            slice_visible,
            crest_idx,
            crest_z,
            crest_y,
            max_occluded_drop,
        }
    }
}

pub struct RoadProfile {
    pub slice_z: [f32; ROAD_SLICES + 1],
    pub min_vis_y: [f32; ROAD_SLICES + 1],
    pub slice_visible: [bool; ROAD_SLICES + 1],
    pub crest_idx: usize,
    pub crest_z: f32,
    pub crest_y: f32,
    pub max_occluded_drop: f32,
}

impl RoadProfile {
    /// Returns the foreground horizon occlusion ceiling `min_visible_y` in front of depth `z`.
    #[inline]
    pub fn occluding_y_before(&self, z: f32) -> f32 {
        if z <= 0.0 {
            return CLIP_BOTTOM;
        }
        let f = (z.clamp(0.0, 1.0) * (ROAD_SLICES as f32)) - 0.5;
        if f <= 0.0 {
            return CLIP_BOTTOM;
        }
        let i0 = (f.floor() as usize).min(ROAD_SLICES);
        self.min_vis_y[i0].min(CLIP_BOTTOM)
    }
}

/* ========================================================================== *
 * Beat-Synced OutRun Roadside Scenery (Poles & Geometric Billboards)         *
 * ========================================================================== */

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PropKind {
    Pole,
    Billboard { motif: u8 },
}

#[derive(Clone, Copy, Debug)]
pub struct RoadsideProp {
    pub z: f32,
    pub side: f32, // `-1.0` = left shoulder, `+1.0` = right shoulder
    pub kind: PropKind,
}

pub struct RoadsideController {
    props: [Option<RoadsideProp>; MAX_ROADSIDE_PROPS],
    beat_count: u64,
    next_billboard_beat: u64,
    next_pole_side: f32,
    next_billboard_side: f32,
    last_ext_beat_idx: u64,
    fallback_beat_cd_s: f32,
    prev_kick: f32,
}

impl Default for RoadsideController {
    fn default() -> Self {
        Self::new()
    }
}

impl RoadsideController {
    pub const fn new() -> Self {
        Self {
            props: [None; MAX_ROADSIDE_PROPS],
            beat_count: 0,
            next_billboard_beat: 8,
            next_pole_side: -1.0,
            next_billboard_side: 1.0,
            last_ext_beat_idx: u64::MAX,
            fallback_beat_cd_s: 0.0,
            prev_kick: 0.0,
        }
    }

    pub fn reset(&mut self) {
        *self = Self::new();
    }

    pub fn active_count(&self) -> usize {
        self.props.iter().filter(|s| s.is_some()).count()
    }

    pub fn props_slice(&self) -> &[Option<RoadsideProp>; MAX_ROADSIDE_PROPS] {
        &self.props
    }

    /// Detects a clean beat/bar edge from either `PluginLiveTelemetry::beat_index` (when available)
    /// or low-end kick onset fallback, then scrolls and spawns roadside props.
    #[allow(clippy::too_many_arguments)]
    pub fn update_with_telemetry(
        &mut self,
        dt_s: f32,
        z_speed: f32,
        has_live_beat: bool,
        live_beat_index: u64,
        bar_beat: u8,
        kick_level: f32,
        seed_u32: u32,
    ) {
        let mut beat_fired = false;
        let mut bar_downbeat = false;

        if has_live_beat {
            if self.last_ext_beat_idx == u64::MAX {
                self.last_ext_beat_idx = live_beat_index;
            } else if live_beat_index != self.last_ext_beat_idx {
                self.last_ext_beat_idx = live_beat_index;
                beat_fired = true;
                bar_downbeat = bar_beat == 0;
            }
        } else {
            self.fallback_beat_cd_s = (self.fallback_beat_cd_s - dt_s.max(0.0)).max(0.0);
            let rising = kick_level > 0.56 && self.prev_kick <= 0.50;
            if rising && self.fallback_beat_cd_s <= 0.0 {
                self.fallback_beat_cd_s = 0.40;
                beat_fired = true;
                bar_downbeat = (self.beat_count + 1) % 4 == 0;
            }
        }
        self.prev_kick = kick_level;

        self.step(dt_s, z_speed, beat_fired, bar_downbeat, seed_u32);
    }

    /// Scrolls active roadside props by `dz = -dt_s * z_speed` and spawns poles (`2`/`4` beats)
    /// & retro geometric billboards (`8–16` beats on bar downbeats).
    pub fn step(
        &mut self,
        dt_s: f32,
        z_speed: f32,
        beat_fired: bool,
        bar_downbeat: bool,
        seed_u32: u32,
    ) {
        let dt = dt_s.clamp(0.0, 0.10);
        if dt > 0.0 && z_speed > 0.0 {
            let dz = dt * z_speed;
            for slot in &mut self.props {
                if let Some(mut p) = *slot {
                    p.z -= dz;
                    if p.z < OBSTACLE_MIN_Z_VIS {
                        *slot = None;
                    } else {
                        *slot = Some(p);
                    }
                }
            }
        }

        if !beat_fired {
            return;
        }

        self.beat_count = self.beat_count.wrapping_add(1);
        let h = seed_u32
            .wrapping_mul(0x9E37_79B9)
            .wrapping_add((self.beat_count as u32).wrapping_mul(0x85EB_CA6B));

        let mut billboard_side_spawned: Option<f32> = None;

        // 1. Retro Billboard every `8–16` beats aligned to bar downbeats
        if self.beat_count >= self.next_billboard_beat
            && (bar_downbeat || self.beat_count >= self.next_billboard_beat + 3)
        {
            let side = self.next_billboard_side;
            self.next_billboard_side = if (h & 4) != 0 { side } else { -side };
            let motif = ((h >> 8) & 3) as u8;
            if self.try_spawn(RoadsideProp {
                z: 1.0,
                side,
                kind: PropKind::Billboard { motif },
            }) {
                billboard_side_spawned = Some(side);
            }
            let gap_beats = match (h >> 12) % 3 {
                0 => 8,
                1 => 12,
                _ => 16,
            };
            self.next_billboard_beat = self.beat_count + gap_beats;
        }

        // 2. Highway Utility Pole every `2` beats (or `4` beats when busy)
        let pole_interval = if self.active_count() >= 5 { 4 } else { 2 };
        if self.beat_count % pole_interval == 0 {
            let mut side = self.next_pole_side;
            self.next_pole_side = -self.next_pole_side;
            if billboard_side_spawned == Some(side) {
                side = -side;
            }
            let _ = self.try_spawn(RoadsideProp {
                z: 1.0,
                side,
                kind: PropKind::Pole,
            });
        }
    }

    fn try_spawn(&mut self, prop: RoadsideProp) -> bool {
        // Avoid stacking two props on the exact same shoulder right at the horizon
        for existing in self.props.iter().flatten() {
            if (existing.side - prop.side).abs() < 0.1 && existing.z > 0.88 {
                return false;
            }
        }
        for slot in &mut self.props {
            if slot.is_none() {
                *slot = Some(prop);
                return true;
            }
        }
        false
    }
}

/* ========================================================================== *
 * Strict Viewport Clipping & Shared 1:1 Highway Renderer                     *
 * ========================================================================== */

#[derive(Clone, Copy)]
pub struct ClipBox {
    pub left: f32,
    pub top: f32,
    pub right: f32,
    pub bottom: f32,
}

impl ClipBox {
    pub const VIEWPORT: Self = Self {
        left: CLIP_LEFT,
        top: CLIP_TOP,
        right: CLIP_RIGHT,
        bottom: CLIP_BOTTOM,
    };

    #[inline]
    pub fn with_bottom(self, max_y: f32) -> Self {
        Self {
            bottom: self.bottom.min(max_y),
            ..self
        }
    }
}

/// Liang-Barsky parametric segment clipper against `clip`.
pub fn clip_line_segment(
    x0: f32,
    y0: f32,
    x1: f32,
    y1: f32,
    clip: ClipBox,
) -> Option<(f32, f32, f32, f32)> {
    if clip.right <= clip.left || clip.bottom <= clip.top {
        return None;
    }
    let dx = x1 - x0;
    let dy = y1 - y0;
    let mut t0 = 0.0f32;
    let mut t1 = 1.0f32;

    let checks = [
        (-dx, x0 - clip.left),
        (dx, clip.right - x0),
        (-dy, y0 - clip.top),
        (dy, clip.bottom - y0),
    ];
    for (p, q) in checks {
        if p.abs() < 1e-6 {
            if q < 0.0 {
                return None;
            }
        } else {
            let r = q / p;
            if p < 0.0 {
                if r > t1 {
                    return None;
                }
                if r > t0 {
                    t0 = r;
                }
            } else {
                if r < t0 {
                    return None;
                }
                if r < t1 {
                    t1 = r;
                }
            }
        }
    }
    if t0 > t1 {
        return None;
    }
    Some((x0 + t0 * dx, y0 + t0 * dy, x0 + t1 * dx, y0 + t1 * dy))
}

#[inline]
pub fn clip_rect_bounds(x: f32, y: f32, w: f32, h: f32, clip: ClipBox) -> Option<(f32, f32, f32, f32)> {
    let x0 = x.max(clip.left);
    let y0 = y.max(clip.top);
    let x1 = (x + w).min(clip.right);
    let y1 = (y + h).min(clip.bottom);
    if x1 > x0 + 0.25 && y1 > y0 + 0.25 {
        Some((x0, y0, x1 - x0, y1 - y0))
    } else {
        None
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum RenderBlockKind {
    Heavy,
    Medium,
    Light,
    Spike,
}

#[derive(Clone, Copy)]
pub struct VisibleBlock {
    pub z: f32,
    pub lane: usize,
    pub kind: RenderBlockKind,
}

#[derive(Clone, Copy)]
pub struct Palette4 {
    pub bg: u32,
    pub fg: u32,
    pub fg1: u32,
    pub fg2: u32,
}

pub trait RoadCanvas {
    fn fill_rect(&mut self, x: f32, y: f32, w: f32, h: f32, radius: f32, rgb: u32);
    #[allow(clippy::too_many_arguments)]
    fn stroke_rect(
        &mut self,
        x: f32,
        y: f32,
        w: f32,
        h: f32,
        radius: f32,
        stroke_w: f32,
        rgb: u32,
    );
    fn fill_circle(&mut self, cx: f32, cy: f32, r: f32, rgb: u32);
    fn draw_line(&mut self, x0: f32, y0: f32, x1: f32, y1: f32, stroke_w: f32, rgb: u32);
    #[allow(clippy::too_many_arguments)]
    fn draw_rgba(
        &mut self,
        tex_key: u64,
        src_w: u32,
        src_h: u32,
        rgba: &[u8],
        dst_x: f32,
        dst_y: f32,
        dst_w: f32,
        dst_h: f32,
        radius: f32,
    );
}

#[inline]
fn draw_clipped_line<C: RoadCanvas>(
    c: &mut C,
    x0: f32,
    y0: f32,
    x1: f32,
    y1: f32,
    stroke_w: f32,
    rgb: u32,
    clip: ClipBox,
) {
    if let Some((cx0, cy0, cx1, cy1)) = clip_line_segment(x0, y0, x1, y1, clip) {
        c.draw_line(cx0, cy0, cx1, cy1, stroke_w, rgb);
    }
}

#[inline]
fn draw_clipped_fill_rect<C: RoadCanvas>(
    c: &mut C,
    x: f32,
    y: f32,
    w: f32,
    h: f32,
    radius: f32,
    rgb: u32,
    clip: ClipBox,
) {
    if let Some((cx, cy, cw, ch)) = clip_rect_bounds(x, y, w, h, clip) {
        let r = if (cw - w).abs() < 0.5 && (ch - h).abs() < 0.5 {
            radius
        } else {
            0.0
        };
        c.fill_rect(cx, cy, cw, ch, r, rgb);
    }
}

#[inline]
#[allow(clippy::too_many_arguments)]
fn draw_clipped_box_outline<C: RoadCanvas>(
    c: &mut C,
    x: f32,
    y: f32,
    w: f32,
    h: f32,
    stroke_w: f32,
    rgb: u32,
    clip: ClipBox,
) {
    let x1 = x + w;
    let y1 = y + h;
    draw_clipped_line(c, x, y, x1, y, stroke_w, rgb, clip);
    draw_clipped_line(c, x, y1, x1, y1, stroke_w, rgb, clip);
    draw_clipped_line(c, x, y, x, y1, stroke_w, rgb, clip);
    draw_clipped_line(c, x1, y, x1, y1, stroke_w, rgb, clip);
}

fn draw_roadside_prop<C: RoadCanvas>(
    c: &mut C,
    pal: Palette4,
    snap: &ElevationSnapshot,
    road_curve: f32,
    prop: RoadsideProp,
    clip: ClipBox,
) {
    match prop.kind {
        PropKind::Pole => {
            let pt = snap.project(prop.z, prop.side * POLE_LANE_OFFSET, road_curve);
            let sc = pt.scale;
            if sc < 0.08 {
                return;
            }
            let pole_h = POLE_BASE_H * sc;
            let top_y = pt.y - pole_h;
            if top_y >= clip.bottom || pt.y <= clip.top {
                return;
            }

            let post_w = (3.8 * sc).max(1.2);
            // Main post (`Shade 2`)
            draw_clipped_line(c, pt.x, top_y, pt.x, pt.y, post_w, pal.fg2, clip);
            // High-visibility reflector band (`Shade 1`)
            draw_clipped_line(
                c,
                pt.x,
                pt.y - pole_h * 0.36,
                pt.x,
                pt.y - pole_h * 0.20,
                (post_w + 1.2 * sc).max(1.6),
                pal.fg1,
                clip,
            );
            // Base plinth (`Shade 1`)
            let base_w = (8.0 * sc).max(2.5);
            let base_h = (3.0 * sc).max(1.2);
            draw_clipped_fill_rect(
                c,
                pt.x - base_w * 0.5,
                pt.y - base_h,
                base_w,
                base_h,
                0.0,
                pal.fg1,
                clip,
            );

            // Inward highway lamp overhang (`Shade 2` arm + `Shade 1` luminaire & brace)
            let inward = -prop.side;
            let arm_x = pt.x + inward * POLE_ARM_DX * sc;
            let arm_y = top_y - POLE_ARM_DY * sc;
            draw_clipped_line(
                c,
                pt.x,
                top_y + 3.5 * sc,
                arm_x,
                arm_y,
                (2.6 * sc).max(1.1),
                pal.fg2,
                clip,
            );
            draw_clipped_line(
                c,
                pt.x,
                top_y + 15.0 * sc,
                pt.x + inward * (POLE_ARM_DX * 0.52 * sc),
                top_y - 1.0 * sc,
                (1.6 * sc).max(1.0),
                pal.fg1,
                clip,
            );
            let lamp_w = (12.0 * sc).max(3.0);
            let lamp_h = (4.2 * sc).max(1.5);
            let lamp_x = if inward > 0.0 {
                arm_x - lamp_w * 0.35
            } else {
                arm_x - lamp_w * 0.65
            };
            draw_clipped_fill_rect(c, lamp_x, arm_y, lamp_w, lamp_h, 0.0, pal.fg1, clip);
        }
        PropKind::Billboard { motif } => {
            let pt = snap.project(prop.z, prop.side * BILLBOARD_LANE_OFFSET, road_curve);
            let sc = pt.scale;
            if sc < 0.08 {
                return;
            }
            let sign_w = BILLBOARD_BASE_W * sc;
            let sign_h = BILLBOARD_BASE_H * sc;
            let leg_h = BILLBOARD_LEG_H * sc;
            let x0 = pt.x - sign_w * 0.5;
            let sign_bot = pt.y - leg_h;
            let sign_top = sign_bot - sign_h;

            if sign_top >= clip.bottom || x0 >= clip.right || x0 + sign_w <= clip.left {
                return;
            }

            // Two short support legs + cross-brace (`Shade 2` & `Shade 1`)
            let leg_w = (4.0 * sc).max(1.3);
            let lx0 = x0 + sign_w * 0.22;
            let lx1 = x0 + sign_w * 0.78;
            draw_clipped_line(c, lx0, sign_bot, lx0, pt.y, leg_w, pal.fg2, clip);
            draw_clipped_line(c, lx1, sign_bot, lx1, pt.y, leg_w, pal.fg2, clip);
            let tie_y = sign_bot + leg_h * 0.45;
            draw_clipped_line(c, lx0, tie_y, lx1, tie_y, (1.8 * sc).max(1.0), pal.fg1, clip);

            // Sign board opaque interior (`bg`) + crisp double `1px` frame (`Shade 2` outer, `Shade 1` inner)
            draw_clipped_fill_rect(c, x0, sign_top, sign_w, sign_h, 0.0, pal.bg, clip);
            draw_clipped_box_outline(
                c,
                x0,
                sign_top,
                sign_w,
                sign_h,
                (1.8 * sc).max(1.0),
                pal.fg2,
                clip,
            );

            let inset = (4.2 * sc).max(2.0);
            let ix = x0 + inset;
            let iy = sign_top + inset;
            let iw = sign_w - inset * 2.0;
            let ih = sign_h - inset * 2.0;
            if iw <= 4.0 || ih <= 3.0 {
                return;
            }
            draw_clipped_box_outline(c, ix, iy, iw, ih, 1.0, pal.fg1, clip);

            if iw < 14.0 || ih < 8.0 {
                return;
            }

            let pad_x = iw * 0.12;
            let pad_y = ih * 0.18;
            let gx = ix + pad_x;
            let gy = iy + pad_y;
            let gw = iw - pad_x * 2.0;
            let gh = ih - pad_y * 2.0;
            let sw = (2.4 * sc).max(1.1);

            match motif & 3 {
                0 => {
                    // Geometric Motif 0: Triple roadward chevrons (`>>>` / `<<<`)
                    let dir = -prop.side;
                    let mid_y = gy + gh * 0.5;
                    let chev_dx = gw * 0.16 * dir;
                    for k in 0..3 {
                        let base_x = if dir > 0.0 {
                            gx + gw * (0.14 + (k as f32) * 0.28)
                        } else {
                            gx + gw * (0.86 - (k as f32) * 0.28)
                        };
                        let tip_x = base_x + chev_dx;
                        let col = if k == 1 { pal.fg1 } else { pal.fg2 };
                        draw_clipped_line(c, base_x, gy, tip_x, mid_y, sw, col, clip);
                        draw_clipped_line(c, tip_x, mid_y, base_x, gy + gh, sw, col, clip);
                    }
                }
                1 => {
                    // Geometric Motif 1: Diagonal OutRun racing stripes + baseline rule
                    let slant = gw * 0.20;
                    for k in 0..3 {
                        let sx = gx + gw * (0.12 + (k as f32) * 0.24);
                        let col = if k == 1 { pal.fg1 } else { pal.fg2 };
                        draw_clipped_line(
                            c,
                            sx,
                            gy + gh * 0.80,
                            sx + slant,
                            gy,
                            (3.2 * sc).max(1.2),
                            col,
                            clip,
                        );
                    }
                    draw_clipped_line(
                        c,
                        gx,
                        gy + gh * 0.92,
                        gx + gw,
                        gy + gh * 0.92,
                        (1.6 * sc).max(1.0),
                        pal.fg1,
                        clip,
                    );
                }
                2 => {
                    // Geometric Motif 2: 5-bar retro graphic equalizer
                    const BAR_RATIOS: [f32; 5] = [0.45, 0.85, 0.62, 0.95, 0.52];
                    let step = gw / 5.0;
                    let bw = (step * 0.66).max(1.4);
                    for (k, &ratio) in BAR_RATIOS.iter().enumerate() {
                        let bx = gx + (k as f32) * step + (step - bw) * 0.5;
                        let bh = (gh * ratio).max(1.5);
                        let by = gy + gh - bh;
                        let col = if k % 2 == 0 { pal.fg2 } else { pal.fg1 };
                        draw_clipped_fill_rect(c, bx, by, bw, bh, 0.0, col, clip);
                    }
                }
                _ => {
                    // Geometric Motif 3: Twin-reel cassette graphic
                    let r = (gh * 0.36).max(1.8);
                    let cy = gy + gh * 0.52;
                    let cx0 = gx + gw * 0.28;
                    let cx1 = gx + gw * 0.72;
                    if cy - r >= clip.top && cy + r <= clip.bottom {
                        for &rcx in &[cx0, cx1] {
                            if rcx - r >= clip.left && rcx + r <= clip.right {
                                c.fill_circle(rcx, cy, r, pal.fg2);
                                if r > 3.2 {
                                    c.fill_circle(rcx, cy, r * 0.52, pal.bg);
                                    c.fill_circle(rcx, cy, r * 0.22, pal.fg1);
                                }
                            }
                        }
                    }
                    draw_clipped_line(c, cx0, cy - r, cx1, cy - r, (1.5 * sc).max(1.0), pal.fg1, clip);
                    draw_clipped_line(c, cx0, cy + r, cx1, cy + r, (1.5 * sc).max(1.0), pal.fg1, clip);
                }
            }
        }
    }
}

fn draw_highway_block<C: RoadCanvas>(
    c: &mut C,
    sprites: &CarSpriteCache,
    pal: Palette4,
    lane: usize,
    cx: f32,
    cy: f32,
    scale: f32,
    kind: RenderBlockKind,
    clip: ClipBox,
) {
    if kind == RenderBlockKind::Spike {
        let (dst_x, dst_y, dst_w, dst_h) = car_draw_rect(
            sprites,
            lane,
            cx,
            cy,
            scale,
            PLAYER_TIRE_OFFSET_Y,
            0.0,
            OBSTACLE_MIN_W,
        );
        if dst_y >= clip.bottom || dst_y + dst_h <= clip.top {
            return;
        }
        if dst_x + dst_w <= clip.left || dst_x >= clip.right {
            return;
        }
        let spr = sprites.obstacle_sprite(lane, dst_w);
        c.draw_rgba(
            spr.tex_key,
            spr.width,
            spr.height,
            &spr.rgba,
            dst_x,
            dst_y,
            dst_w,
            dst_h,
            LINEAR_SPRITE_RADIUS,
        );
        return;
    }

    let w = (98.0 * scale).max(7.0);
    let h = (38.0 * scale).max(4.5);
    let lift = (18.0 * scale).max(2.5);
    let r = (5.0 * scale).max(0.0);
    let x = cx - w * 0.5;
    let y_front = cy - h * 0.5;
    let y_top = y_front - lift;
    let tot_h = h + lift;

    if y_top >= clip.bottom || y_top + tot_h <= clip.top {
        return;
    }

    match kind {
        RenderBlockKind::Spike => {}
        RenderBlockKind::Heavy => {
            draw_clipped_fill_rect(c, x, y_top, w, tot_h, r, pal.fg, clip);
            if w > 18.0 {
                let inset = (4.5 * scale).max(1.5);
                draw_clipped_fill_rect(
                    c,
                    x + inset,
                    y_top + inset,
                    w - inset * 2.0,
                    (4.0 * scale).max(1.5),
                    0.0,
                    pal.bg,
                    clip,
                );
            }
        }
        RenderBlockKind::Medium => {
            draw_clipped_fill_rect(c, x, y_top, w, tot_h, r, pal.fg2, clip);
            if let Some((cxr, cyr, cwr, chr)) = clip_rect_bounds(x, y_top, w, tot_h, clip) {
                let cr = if (chr - tot_h).abs() < 0.5 { r } else { 0.0 };
                c.stroke_rect(cxr, cyr, cwr, chr, cr, (2.4 * scale).max(1.2), pal.fg);
            }
            if w > 18.0 {
                draw_clipped_fill_rect(
                    c,
                    x + w * 0.22,
                    y_top + h * 0.45,
                    w * 0.56,
                    (4.0 * scale).max(1.5),
                    0.0,
                    pal.bg,
                    clip,
                );
            }
        }
        RenderBlockKind::Light => {
            draw_clipped_fill_rect(c, x, y_top, w, tot_h, r, pal.fg1, clip);
            if let Some((cxr, cyr, cwr, chr)) = clip_rect_bounds(x, y_top, w, tot_h, clip) {
                let cr = if (chr - tot_h).abs() < 0.5 { r } else { 0.0 };
                c.stroke_rect(cxr, cyr, cwr, chr, cr, (2.4 * scale).max(1.2), pal.fg);
            }
            if w > 18.0 {
                let ps = (9.0 * scale).max(3.0);
                draw_clipped_fill_rect(
                    c,
                    cx - ps * 0.5,
                    y_top + (h - ps) * 0.5,
                    ps,
                    ps,
                    2.0,
                    pal.fg,
                    clip,
                );
            }
        }
    }
}

fn draw_player_car<C: RoadCanvas>(
    c: &mut C,
    sprites: &CarSpriteCache,
    lane: usize,
    ship_x: f32,
    ship_ground_y: f32,
    jump_px: f32,
) {
    let (dst_x, dst_y, dst_w, dst_h) = car_draw_rect(
        sprites,
        lane,
        ship_x,
        ship_ground_y,
        1.0,
        PLAYER_TIRE_OFFSET_Y,
        jump_px,
        CAR_BASE_W,
    );
    let spr = sprites.player_sprite(lane);
    c.draw_rgba(
        spr.tex_key,
        spr.width,
        spr.height,
        &spr.rgba,
        dst_x,
        dst_y,
        dst_w,
        dst_h,
        LINEAR_SPRITE_RADIUS,
    );
}

pub struct HighwayRenderInput<'a> {
    pub pal: Palette4,
    pub road_curve: f32,
    pub scroll_phase: f32,
    pub live_bars: &'a [f32; 16],
    pub live_kick: f32,
    pub ship_pos: f32,
    pub player_lane: usize,
    pub jump_frac: f32,
    pub hit_blink_hidden: bool,
    pub elevation: ElevationSnapshot,
    pub roadside: &'a RoadsideController,
    pub sorted_blocks_far_to_near: &'a [VisibleBlock],
}

/// Unified 5-layer pseudo-3D highway renderer with *OutRun* crest occlusion,
/// beat-synced roadside props, and strict viewport clipping (`700 x 562`).
pub fn render_highway_viewport<C: RoadCanvas>(
    c: &mut C,
    sprites: &CarSpriteCache,
    inp: &HighwayRenderInput<'_>,
) {
    let pal = inp.pal;
    let snap = &inp.elevation;
    let profile = snap.compute_profile(inp.road_curve);
    let vp = snap.project(1.0, 0.0, inp.road_curve);

    let sky_floor_y = vp.y.min(profile.crest_y).clamp(CLIP_TOP + 24.0, CLIP_BOTTOM);
    let sky_clip = ClipBox::VIEWPORT.with_bottom(sky_floor_y);

    // ---- Layer 1: Sky, Horizon-Clipped Synthwave Sun & Top 16 FFT Spectrum Bars ----
    let sun_r = SUN_BASE_R + inp.live_kick.clamp(0.0, 1.0) * SUN_KICK_R;
    let sun_cy = sky_floor_y - SUN_OFFSET_ABOVE_HORIZON_Y;
    if vp.x - sun_r >= CLIP_LEFT && vp.x + sun_r <= CLIP_RIGHT && sun_cy - sun_r >= CLIP_TOP {
        c.fill_circle(vp.x, sun_cy, sun_r, pal.fg1);
        c.fill_circle(vp.x, sun_cy, sun_r * SUN_CORE_RATIO, pal.bg);
        // Cleanly slice lower arc of sun at the horizon ridge
        draw_clipped_fill_rect(
            c,
            CLIP_LEFT,
            sky_floor_y,
            CLIP_RIGHT - CLIP_LEFT,
            sun_r + 4.0,
            0.0,
            pal.bg,
            ClipBox::VIEWPORT,
        );
    }

    let bar_step = SKY_BAR_SPAN_W / 16.0;
    let bar_w = bar_step - SKY_BAR_GAP_W;
    let max_bar_h = (sky_floor_y - (VIEW_Y + SKY_TOP_HEADROOM_PAD)).max(SKY_BAR_MIN_H);
    for (i, &lvl) in inp.live_bars.iter().enumerate() {
        let bx = (vp.x - SKY_BAR_SPAN_W * 0.5 + (i as f32) * bar_step)
            .clamp(CLIP_LEFT + 6.0, CLIP_RIGHT - bar_w - 6.0);
        let bh = (SKY_BAR_BASE_H + lvl.clamp(0.0, 1.0) * SKY_BAR_GAIN_H)
            .min(max_bar_h)
            .max(SKY_BAR_MIN_H);
        let shade = if lvl > 0.62 {
            pal.fg
        } else if lvl > 0.30 {
            pal.fg2
        } else {
            pal.fg1
        };
        draw_clipped_fill_rect(c, bx, sky_floor_y - bh, bar_w, bh, 0.0, shade, sky_clip);
    }

    // Sort active roadside props far -> near (`z` descending) on stack (zero alloc)
    let mut sorted_props = [RoadsideProp {
        z: 0.0,
        side: -1.0,
        kind: PropKind::Pole,
    }; MAX_ROADSIDE_PROPS];
    let mut prop_n = 0usize;
    for slot in inp.roadside.props_slice().iter().flatten() {
        if prop_n < MAX_ROADSIDE_PROPS {
            sorted_props[prop_n] = *slot;
            prop_n += 1;
        }
    }
    sorted_props[..prop_n]
        .sort_unstable_by(|a, b| b.z.partial_cmp(&a.z).unwrap_or(std::cmp::Ordering::Equal));

    // ---- Layer 2: Distant Entities Behind Uphill Crest (`z > crest_z`) ----
    let has_crest_occlusion = profile.crest_z < 0.999 && profile.max_occluded_drop > 0.25;
    if has_crest_occlusion {
        let behind_clip = ClipBox::VIEWPORT.with_bottom(profile.crest_y);
        for &p in &sorted_props[..prop_n] {
            if p.z > profile.crest_z {
                draw_roadside_prop(c, pal, snap, inp.road_curve, p, behind_clip);
            }
        }
        for b in inp.sorted_blocks_far_to_near {
            if b.z > profile.crest_z {
                let pt = snap.project(b.z, (b.lane as f32) - 1.0, inp.road_curve);
                draw_highway_block(c, sprites, pal, b.lane, pt.x, pt.y, pt.scale, b.kind, behind_clip);
            }
        }
        // Crisp foreground hill-crest mask occluding lower halves/tires below `crest_y`
        draw_clipped_fill_rect(
            c,
            CLIP_LEFT,
            profile.crest_y,
            CLIP_RIGHT - CLIP_LEFT,
            CLIP_BOTTOM - profile.crest_y,
            0.0,
            pal.bg,
            ClipBox::VIEWPORT,
        );
    }

    // ---- Layer 3: Horizon / Crest Line, Perspective Grid & 28-Slice Segmented Curved Road ----
    draw_clipped_line(
        c,
        CLIP_LEFT + 4.0,
        sky_floor_y,
        CLIP_RIGHT - 4.0,
        sky_floor_y,
        2.0,
        pal.fg1,
        ClipBox::VIEWPORT,
    );

    for i in 0..GRID_LINE_COUNT {
        let z = ((i as f32 + inp.scroll_phase) / (GRID_LINE_COUNT as f32)).rem_euclid(1.0);
        if z > profile.crest_z {
            continue;
        }
        let occ_y = profile.occluding_y_before(z);
        let lpt = snap.project(z, -1.5, inp.road_curve);
        if lpt.y > occ_y + 0.2 {
            continue;
        }
        let rpt = snap.project(z, 1.5, inp.road_curve);
        draw_clipped_line(
            c,
            lpt.x,
            lpt.y,
            rpt.x,
            rpt.y,
            (2.0 * lpt.scale).max(1.0),
            pal.fg1,
            ClipBox::VIEWPORT,
        );
    }

    let max_vis_slice = if has_crest_occlusion {
        profile.crest_idx
    } else {
        ROAD_SLICES
    };
    for &lane_edge in &[-1.5_f32, -0.5, 0.5, 1.5] {
        let is_outer = lane_edge.abs() > 1.0;
        let color = if is_outer { pal.fg } else { pal.fg2 };
        let base_w = if is_outer { 4.8 } else { 2.2 };
        let mut prev = snap.project(0.0, lane_edge, inp.road_curve);
        for s in 1..=max_vis_slice {
            let z1 = profile.slice_z[s];
            let next = snap.project(z1, lane_edge, inp.road_curve);
            if profile.slice_visible[s] {
                draw_clipped_line(
                    c,
                    prev.x,
                    prev.y,
                    next.x,
                    next.y,
                    (base_w * prev.scale).max(1.3),
                    color,
                    ClipBox::VIEWPORT,
                );
            }
            prev = next;
        }
    }

    // ---- Layer 4: Foreground Roadside Props, Highway Blocks & Player Car (`z <= crest_z`) ----
    for &p in &sorted_props[..prop_n] {
        if !has_crest_occlusion || p.z <= profile.crest_z {
            let prop_clip = ClipBox::VIEWPORT.with_bottom(profile.occluding_y_before(p.z));
            draw_roadside_prop(c, pal, snap, inp.road_curve, p, prop_clip);
        }
    }

    let ship_pt = snap.project(0.0, inp.ship_pos - 1.0, inp.road_curve);
    let jump_px = inp.jump_frac.clamp(0.0, 1.0) * PLAYER_JUMP_HEIGHT_PX;
    let airborne = jump_px > 2.0;
    let vis_player_lane = (inp.ship_pos.round() as i32).clamp(0, 2) as usize;
    let _ = inp.player_lane;

    if airborne {
        for b in inp.sorted_blocks_far_to_near {
            if has_crest_occlusion && b.z > profile.crest_z {
                continue;
            }
            let pt = snap.project(b.z, (b.lane as f32) - 1.0, inp.road_curve);
            draw_highway_block(
                c,
                sprites,
                pal,
                b.lane,
                pt.x,
                pt.y,
                pt.scale,
                b.kind,
                ClipBox::VIEWPORT,
            );
        }
        let sh_w = PLAYER_SHADOW_W * (1.0 - inp.jump_frac * 0.35);
        draw_clipped_fill_rect(
            c,
            ship_pt.x - sh_w * 0.5,
            ship_pt.y + PLAYER_SHADOW_OFFSET_Y,
            sh_w,
            PLAYER_SHADOW_H,
            PLAYER_SHADOW_RADIUS,
            pal.fg1,
            ClipBox::VIEWPORT,
        );
        if !inp.hit_blink_hidden {
            draw_player_car(c, sprites, vis_player_lane, ship_pt.x, ship_pt.y, jump_px);
        }
    } else {
        let mut player_drawn = false;
        for b in inp.sorted_blocks_far_to_near {
            if has_crest_occlusion && b.z > profile.crest_z {
                continue;
            }
            if !player_drawn && b.z < 0.0 {
                player_drawn = true;
                if !inp.hit_blink_hidden {
                    draw_player_car(c, sprites, vis_player_lane, ship_pt.x, ship_pt.y, 0.0);
                }
            }
            let pt = snap.project(b.z, (b.lane as f32) - 1.0, inp.road_curve);
            draw_highway_block(
                c,
                sprites,
                pal,
                b.lane,
                pt.x,
                pt.y,
                pt.scale,
                b.kind,
                ClipBox::VIEWPORT,
            );
        }
        if !player_drawn && !inp.hit_blink_hidden {
            draw_player_car(c, sprites, vis_player_lane, ship_pt.x, ship_pt.y, 0.0);
        }
    }

    // ---- Layer 5: Crisp Viewport Frame (`700 x 562`) ----
    c.stroke_rect(VIEW_X, VIEW_Y, VIEW_W, VIEW_H, CARD_RADIUS, CARD_STROKE_W, pal.fg1);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bass_surges_enforce_mandatory_cooldown_and_alternate_crest_and_dip() {
        let mut ctrl = ElevationController::new();

        // Warm up with moderate groove bass (`0.34`) for `1.5s` -> remains flat
        for _ in 0..90 {
            ctrl.update(1.0 / 60.0, 0.34, 0x1234_5678);
        }
        assert_eq!(ctrl.active_kind(), None);
        assert_eq!(ctrl.snapshot().dy(0.5), 0.0);

        // Heavy bass surge (`0.92`) triggers Wave #1 (`Crest`)
        for _ in 0..20 {
            ctrl.update(1.0 / 60.0, 0.92, 0x1234_5678);
        }
        assert_eq!(ctrl.active_kind(), Some(ElevationKind::Crest));
        assert_eq!(ctrl.wave_count(), 1);

        // Step until wave finishes (`~4.0s`), then verify mandatory `8.5..11.5s` flat cooldown
        // rejects repeated heavy bass drops (`0.95`) on every beat/bar!
        let mut entered_cooldown = false;
        for _ in 0..(5 * 60) {
            ctrl.update(1.0 / 60.0, 0.35, 0x1234_5678);
            if ctrl.is_in_cooldown() {
                entered_cooldown = true;
                break;
            }
        }
        assert!(entered_cooldown, "Wave must transition into flat-road cooldown");

        // Hammer with continuous max bass (`0.96`) for `8.0` full seconds -> MUST stay flat!
        for step in 0..(8 * 60) {
            let b = if step % 15 < 8 { 0.96 } else { 0.25 };
            ctrl.update(1.0 / 60.0, b, 0x1234_5678);
            assert_eq!(
                ctrl.active_kind(),
                None,
                "Must never bounce during mandatory >= 8.5s flat-road pause (step {step})"
            );
            assert_eq!(ctrl.snapshot().dy(0.6), 0.0);
        }

        // Wait out remaining cooldown (`4.0s`) at moderate bass, then trigger Wave #2 (`Dip`)
        for _ in 0..(4 * 60) {
            ctrl.update(1.0 / 60.0, 0.32, 0x1234_5678);
        }
        assert!(!ctrl.is_in_cooldown());
        for _ in 0..20 {
            ctrl.update(1.0 / 60.0, 0.92, 0x9ABC_DEF0);
        }
        assert_eq!(ctrl.active_kind(), Some(ElevationKind::Dip));
        assert_eq!(ctrl.wave_count(), 2);
    }

    #[test]
    fn crest_and_dip_preserve_headroom_outrun_occlusion_and_fair_reaction_time() {
        let mut max_occlusion = 0.0f32;
        let mut min_crest_z = 1.0f32;

        for kind in [ElevationKind::Crest, ElevationKind::Dip] {
            for step in 0..=100 {
                let u = (step as f32) / 100.0;
                let shaped_u = u.powf(0.80);
                let env = 0.5 * (1.0 - (2.0 * PI * shaped_u).cos());
                let sign = kind.sign();
                let snap = ElevationSnapshot {
                    sign,
                    env,
                    z_center: 0.96 - 0.96 * u,
                    horizon_shift: (sign * HORIZON_WAVE_SHIFT_PX * env)
                        .clamp(-HORIZON_MAX_SHIFT_PX, HORIZON_MAX_SHIFT_PX),
                };

                assert!(snap.horizon_shift.abs() <= HORIZON_MAX_SHIFT_PX + 1e-5);
                let profile = snap.compute_profile(0.0);

                // Top FFT spectrum bars always retain >= 52 px sky headroom above `crest_y` / `vp_y`
                let sky_floor = snap.vp_y().min(profile.crest_y);
                assert!(
                    sky_floor - VIEW_Y >= 64.0,
                    "Insufficient sky headroom: {}",
                    sky_floor - VIEW_Y
                );

                if kind == ElevationKind::Crest {
                    max_occlusion = max_occlusion.max(profile.max_occluded_drop);
                    min_crest_z = min_crest_z.min(profile.crest_z);
                }
            }
        }

        // Genuine OutRun crest horizon foldover (> 12 px)
        assert!(
            max_occlusion >= 12.0,
            "Expected crisp OutRun crest occlusion, got {max_occlusion}"
        );
        // Fair reaction visibility >= 1.10s after clearing any uphill crest
        let min_reaction_s = min_crest_z * TRAVEL_TIME_S;
        assert!(
            min_reaction_s >= 1.10,
            "Obstacle reaction time after crest too short: {min_reaction_s:.3}s (crest_z={min_crest_z:.3})"
        );
    }

    #[test]
    fn roadside_props_spawn_on_beats_cap_at_eight_freeze_on_pause_and_clip_strictly() {
        let mut rs = RoadsideController::new();
        for beat in 1..=32 {
            rs.step(0.25, 0.68, true, beat % 4 == 0, 0xCAFE_BABE);
            assert!(rs.active_count() <= MAX_ROADSIDE_PROPS);
        }
        assert!(rs.active_count() >= 2);
        let has_billboard = rs
            .props_slice()
            .iter()
            .flatten()
            .any(|p| matches!(p.kind, PropKind::Billboard { .. }));
        assert!(has_billboard, "Should spawn retro billboard within 16..32 beats");

        // Frozen on pause (`dt = 0` / no `step` when paused): `z` positions remain unchanged
        let before: Vec<f32> = rs.props_slice().iter().flatten().map(|p| p.z).collect();
        rs.step(0.0, 0.0, false, false, 0);
        let after: Vec<f32> = rs.props_slice().iter().flatten().map(|p| p.z).collect();
        assert_eq!(before, after);

        // Verify Liang-Barsky & rect clippers never emit coordinates outside `ClipBox::VIEWPORT`
        let seg = clip_line_segment(-50.0, 120.0, 900.0, 800.0, ClipBox::VIEWPORT).unwrap();
        assert!(seg.0 >= CLIP_LEFT - 1e-4 && seg.2 <= CLIP_RIGHT + 1e-4);
        assert!(seg.1 >= CLIP_TOP - 1e-4 && seg.3 <= CLIP_BOTTOM + 1e-4);
    }
}
