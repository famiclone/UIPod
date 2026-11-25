//! `Beat Stack` (`Біт Стек`) — 4-Color Siemens Stack Attack Rhythm-Puzzle Arcade Plugin
//! for UIPod (`PluginKind::MenuDriven`).
//!
//! Overhead robotic cranes glide and drop 4 distinct industrial crate types in lockstep with the
//! live track's BPM / beat grid (monotonic `40–140 Hz` kick-band autocorrelation across `75–180 BPM`
//! plus `.nsf` hardware APU telemetry, with a `120 BPM` fallback clock when paused/silent).
//! Walk and push single crates with `Left`/`Right`, jump vertically with `B`, leap diagonally with
//! `B + Left/Right` or `L1`/`R1`, and form `2×2+` matching crate squares or full 12-column floor
//! rows that arm immediately and detonate right on the next **Strong Beat (`Beats 1 & 3`)**!

mod rhythm;

use std::path::Path;

use rhythm::{BeatClock, BeatFrameInput, BeatStep};
use uipod_plugin_sdk::{
    btn, uipod_declare_plugin, FontWeight, HostContext, MenuItem, MenuOutcome, PluginCategory,
    PluginKind, PluginTheme, TextAlign, CONTENT_TOP_Y,
};

const PLUGIN_ID: &str = "beat_stack";
const SAVE_FILE: &str = "beat_stack_save.bin";
const SAVE_MAGIC: &[u8; 4] = b"BST1";

pub const GRID_COLS: usize = 12;
pub const GRID_ROWS: usize = 7;
const MAX_CRANES: usize = 4;
const SYNC_OFFSETS_MS: [i32; 5] = [0, 25, 50, 100, 150];
const DEFAULT_PCM_SR: u32 = 44_100;
const MIN_ARM_FLASH_BEATS: f32 = 0.48;
const MIN_ARM_FLASH_SEC: f32 = 0.15;

// ============================================================================
// Declarative LayoutSpec (1:1 geometry & style tokens; zero magic numbers)
// ============================================================================

struct LayoutSpec {
    outer_pad_x: f32,
    outer_top_offset: f32,
    outer_bottom_pad: f32,
    hud_gap_y: f32,
    hud_bar_h: f32,
    hud_radius: f32,
    frame_radius: f32,
    wall_stroke: f32,
    thin_stroke: f32,
    hint_card_h: f32,
    hint_radius: f32,

    // Proportional ratios relative to computed square `tile` size
    crane_zone_ratio: f32,
    floor_plate_ratio: f32,
    rail_top_ratio: f32,
    rail_h_ratio: f32,
    crate_inset_ratio: f32,
    crate_radius_ratio: f32,

    min_tile: f32,
    max_tile: f32,

    // Typography tokens (`Geologica`)
    font_banner_lg: f32,
    font_banner_sm: f32,
    font_hud_val: f32,
    font_hud_lbl: f32,
    font_callout: f32,
    font_hint_main: f32,
    font_hint_sub: f32,
}

const SPEC: LayoutSpec = LayoutSpec {
    outer_pad_x: 24.0,
    outer_top_offset: 8.0,
    outer_bottom_pad: 12.0,
    hud_gap_y: 10.0,
    hud_bar_h: 56.0,
    hud_radius: 10.0,
    frame_radius: 8.0,
    wall_stroke: 3.5,
    thin_stroke: 2.0,
    hint_card_h: 48.0,
    hint_radius: 8.0,

    crane_zone_ratio: 1.50,
    floor_plate_ratio: 0.28,
    rail_top_ratio: 0.14,
    rail_h_ratio: 0.16,
    crate_inset_ratio: 0.04,
    crate_radius_ratio: 0.08,

    min_tile: 26.0,
    max_tile: 70.0,

    font_banner_lg: 32.0,
    font_banner_sm: 18.0,
    font_hud_val: 22.0,
    font_hud_lbl: 13.0,
    font_callout: 22.0,
    font_hint_main: 13.5,
    font_hint_sub: 12.5,
};

#[derive(Clone, Copy, Debug)]
pub struct ComputedLayout {
    pub win_w: f32,
    pub win_h: f32,
    pub tile: f32,
    pub shaft_x: f32,
    pub shaft_y: f32,
    pub shaft_w: f32,
    pub shaft_h: f32,
    pub crane_zone_h: f32,
    pub floor_plate_h: f32,
    pub grid_x: f32,
    pub grid_y: f32,
    pub grid_w: f32,
    pub grid_h: f32,
    pub hud_x: f32,
    pub hud_y: f32,
    pub hud_w: f32,
    pub hud_h: f32,
}

impl ComputedLayout {
    pub fn compute(win_w: f32, win_h: f32) -> Self {
        let top_y = CONTENT_TOP_Y + SPEC.outer_top_offset;
        let avail_w = (win_w - SPEC.outer_pad_x * 2.0).max(280.0);
        let avail_h =
            (win_h - top_y - SPEC.outer_bottom_pad - SPEC.hud_gap_y - SPEC.hud_bar_h).max(220.0);

        let total_w_tiles = (GRID_COLS as f32) + 0.25;
        let total_h_tiles =
            (GRID_ROWS as f32) + SPEC.crane_zone_ratio + SPEC.floor_plate_ratio;

        let tile = (avail_w / total_w_tiles)
            .min(avail_h / total_h_tiles)
            .floor()
            .clamp(SPEC.min_tile, SPEC.max_tile);

        let crane_zone_h = (tile * SPEC.crane_zone_ratio).round();
        let floor_plate_h = (tile * SPEC.floor_plate_ratio).round().max(10.0);
        let grid_w = tile * (GRID_COLS as f32);
        let grid_h = tile * (GRID_ROWS as f32);
        let wall_pad = SPEC.wall_stroke;

        let shaft_w = grid_w + wall_pad * 2.0;
        let shaft_h = crane_zone_h + grid_h + floor_plate_h;
        let total_block_h = shaft_h + SPEC.hud_gap_y + SPEC.hud_bar_h;

        let shaft_x = ((win_w - shaft_w) * 0.5).floor();
        let extra_y = ((win_h - top_y - SPEC.outer_bottom_pad - total_block_h) * 0.5).max(0.0);
        let shaft_y = (top_y + extra_y).floor();

        let grid_x = shaft_x + wall_pad;
        let grid_y = shaft_y + crane_zone_h;

        let hud_y = shaft_y + shaft_h + SPEC.hud_gap_y;

        Self {
            win_w,
            win_h,
            tile,
            shaft_x,
            shaft_y,
            shaft_w,
            shaft_h,
            crane_zone_h,
            floor_plate_h,
            grid_x,
            grid_y,
            grid_w,
            grid_h,
            hud_x: shaft_x,
            hud_y,
            hud_w: shaft_w,
            hud_h: SPEC.hud_bar_h,
        }
    }
}

// ============================================================================
// Persistent Preferences & Difficulty
// ============================================================================

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Difficulty {
    Shift = 0,   // Chill / Cadet
    Foreman = 1, // Standard
    Overtime = 2, // Fast / Half-beat drops at high waves
}

impl Difficulty {
    fn from_u8(v: u8) -> Self {
        match v {
            0 => Self::Shift,
            2 => Self::Overtime,
            _ => Self::Foreman,
        }
    }

    fn max_cranes_for_wave(self, wave: u32, drops_done: u32) -> usize {
        if drops_done < 2 && self != Self::Overtime {
            return 1;
        }
        let base = match wave {
            1..=2 => 2,
            3..=5 => 3,
            _ => 4,
        };
        match self {
            Self::Shift => base.min(3),
            Self::Foreman => base.min(MAX_CRANES),
            Self::Overtime => (base + 1).min(MAX_CRANES),
        }
    }

    fn uses_half_beat_fall(self, wave: u32) -> bool {
        match self {
            Self::Shift => false,
            Self::Foreman => wave >= 8,
            Self::Overtime => wave >= 4,
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct SavedPrefs {
    pub hi_score: u32,
    pub difficulty: Difficulty,
    pub latency_idx: usize,
}

impl Default for SavedPrefs {
    fn default() -> Self {
        Self {
            hi_score: 0,
            difficulty: Difficulty::Foreman,
            latency_idx: 1, // +25 ms default
        }
    }
}

impl SavedPrefs {
    fn load(ctx: &HostContext<'_>) -> Self {
        let Some(bytes) = ctx.load_plugin_data(PLUGIN_ID, SAVE_FILE) else {
            return Self::default();
        };
        if bytes.len() < 10 || &bytes[0..4] != SAVE_MAGIC {
            return Self::default();
        }
        let hi_score = u32::from_le_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]);
        let difficulty = Difficulty::from_u8(bytes[8]);
        let latency_idx = (bytes[9] as usize).min(SYNC_OFFSETS_MS.len() - 1);
        Self {
            hi_score,
            difficulty,
            latency_idx,
        }
    }

    fn save(&self, ctx: &HostContext<'_>) {
        let mut out = [0u8; 10];
        out[0..4].copy_from_slice(SAVE_MAGIC);
        out[4..8].copy_from_slice(&self.hi_score.to_le_bytes());
        out[8] = self.difficulty as u8;
        out[9] = self.latency_idx as u8;
        let _ = ctx.save_plugin_data(PLUGIN_ID, SAVE_FILE, &out);
    }

    fn latency_sec(&self) -> f64 {
        (SYNC_OFFSETS_MS[self.latency_idx] as f64) * 0.001
    }
}

// ============================================================================
// Core 4-Color Crate, Crane, Worker & Lumines Sweep Simulation
// ============================================================================

/// 4 distinct 4-shade-safe industrial crate types.
#[repr(u8)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CrateKind {
    /// Type 1 (Light): Shade 0 interior + bold Shade 2 frame + corner rivets.
    Light = 1,
    /// Type 2 (Checker/Cross): 50% Bayer dither (Shade 1 / Shade 2) + diagonal brace.
    Checker = 2,
    /// Type 3 (Medium): Solid Shade 2 fill + Shade 0 inner stencil stripes.
    Medium = 3,
    /// Type 4 (Heavy Dark): Solid Shade 3 fill + crisp Shade 0 X-cross insignia.
    Heavy = 4,
}

#[cfg(test)]
impl CrateKind {
    pub const ALL: [Self; 4] = [Self::Light, Self::Checker, Self::Medium, Self::Heavy];
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct GroundedCrate {
    pub kind: CrateKind,
    pub armed_square: bool,
    pub armed_row: bool,
    pub armed_sec: f32,
    pub armed_beats: f32,
    pub settle_px_frac: u8, // 0..=100 smooth drop interpolation percent
}

impl GroundedCrate {
    #[inline]
    pub const fn new(kind: CrateKind) -> Self {
        Self {
            kind,
            armed_square: false,
            armed_row: false,
            armed_sec: 0.0,
            armed_beats: 0.0,
            settle_px_frac: 0,
        }
    }

    #[inline]
    pub const fn is_armed(self) -> bool {
        self.armed_square || self.armed_row
    }

    #[inline]
    pub fn ready_to_detonate(self) -> bool {
        self.is_armed()
            && self.armed_beats >= MIN_ARM_FLASH_BEATS
            && self.armed_sec >= MIN_ARM_FLASH_SEC
    }
}

/// Crate actively falling from an overhead crane on quantized beat steps.
#[derive(Clone, Copy, Debug)]
pub struct FallingCrate {
    pub col: usize,
    pub row: usize,
    pub kind: CrateKind,
    /// Visual offset in `[-1.0..0.0]` tiles smoothly easing to `0.0` after each beat snap.
    pub snap_anim: f32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CraneMode {
    Carrying,
    Telegraph,
    EmptyReturn,
}

#[derive(Clone, Copy, Debug)]
pub struct OverheadCrane {
    pub active: bool,
    pub col: i32,
    pub vis_col: f32,
    pub dir: i32,
    pub target_col: usize,
    pub cargo: Option<CrateKind>,
    pub mode: CraneMode,
    pub telegraph_beats: f32,
    pub rail_steps_loaded: u8,
    pub open_flash: f32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LoseReason {
    Crushed,
    ShaftJammed,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum WorkerAction {
    Idle,
    Walking {
        from_col: i32,
        progress: f32,
    },
    Pushing {
        dir: i32,
        progress: f32,
    },
    Jumping {
        from_col: f32,
        from_row: f32,
        to_col: i32,
        to_row: i32,
        progress: f32,
        blocked_wall: bool,
        steered_apex: bool,
    },
    Falling {
        vis_row: f32,
    },
}

#[derive(Clone, Copy, Debug)]
pub struct Worker {
    pub col: i32,
    pub row: i32,
    pub facing: i32, // -1 left, +1 right
    pub action: WorkerAction,
    pub walk_phase: u8,
    pub danger_above: bool,
}

#[derive(Clone, Copy, Debug)]
struct BurstParticle {
    x: f32, // in grid tile units
    y: f32,
    vx: f32,
    vy: f32,
    life: f32,
    shade: u8,
}

#[derive(Clone, Debug)]
struct FloatingCallout {
    text: String,
    col_center: f32,
    row_y: f32,
    timer: f32,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct FrameInput {
    pub move_dir: i32, // -1 left, 0 none, +1 right (held)
    pub press_left: bool,
    pub press_right: bool,
    pub jump_b: bool,
    pub held_b: bool,
    pub diag_left: bool,
    pub diag_right: bool,
    pub toggle_hint: bool,
}

pub struct BeatStackGame {
    pub prefs: SavedPrefs,
    pub grid: [[Option<GroundedCrate>; GRID_COLS]; GRID_ROWS],
    pub falling: Vec<FallingCrate>,
    pub cranes: [OverheadCrane; MAX_CRANES],
    pub worker: Worker,

    pub score: u32,
    pub chain: u32,
    pub max_chain: u32,
    pub chain_active: bool,
    pub cleared_crates: u32,
    pub wave: u32,
    pub drops_done: u32,

    pub last_beat: BeatStep,
    pub bpm_pulse: f32,
    pub strong_pulse: f32,
    pub show_hint: bool,
    pub hint_pinned: bool,
    pub lose_reason: Option<LoseReason>,

    bag: [CrateKind; 8],
    bag_pos: usize,
    rng: u64,
    gravity_accum: f32,
    move_cooldown: f32,
    particles: Vec<BurstParticle>,
    callout: Option<FloatingCallout>,
}

impl BeatStackGame {
    pub fn new(prefs: SavedPrefs, seed: u64) -> Self {
        let mut g = Self {
            prefs,
            grid: [[None; GRID_COLS]; GRID_ROWS],
            falling: Vec::with_capacity(8),
            cranes: [OverheadCrane {
                active: false,
                col: 0,
                vis_col: 0.0,
                dir: 1,
                target_col: 4,
                cargo: None,
                mode: CraneMode::EmptyReturn,
                telegraph_beats: 0.0,
                rail_steps_loaded: 0,
                open_flash: 0.0,
            }; MAX_CRANES],
            worker: Worker {
                col: 5,
                row: (GRID_ROWS - 1) as i32,
                facing: 1,
                action: WorkerAction::Idle,
                walk_phase: 0,
                danger_above: false,
            },
            score: 0,
            chain: 1,
            max_chain: 1,
            chain_active: false,
            cleared_crates: 0,
            wave: 1,
            drops_done: 0,
            last_beat: BeatStep::default(),
            bpm_pulse: 0.0,
            strong_pulse: 0.0,
            show_hint: true,
            hint_pinned: false,
            lose_reason: None,
            bag: [
                CrateKind::Light,
                CrateKind::Light,
                CrateKind::Checker,
                CrateKind::Checker,
                CrateKind::Medium,
                CrateKind::Medium,
                CrateKind::Heavy,
                CrateKind::Heavy,
            ],
            bag_pos: 8,
            rng: seed | 1,
            gravity_accum: 0.0,
            move_cooldown: 0.0,
            particles: Vec::with_capacity(64),
            callout: None,
        };
        g.seed_starter_floor();
        g.ensure_cranes_spawned();
        g
    }

    pub fn restart(&mut self, seed: u64) {
        let prefs = self.prefs;
        *self = Self::new(prefs, seed);
    }

    #[inline]
    fn next_rand(&mut self) -> u32 {
        let mut x = self.rng;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.rng = x;
        (x >> 16) as u32
    }

    fn draw_crate_kind(&mut self) -> CrateKind {
        if self.bag_pos >= self.bag.len() {
            self.bag = [
                CrateKind::Light,
                CrateKind::Light,
                CrateKind::Checker,
                CrateKind::Checker,
                CrateKind::Medium,
                CrateKind::Medium,
                CrateKind::Heavy,
                CrateKind::Heavy,
            ];
            for i in (1..self.bag.len()).rev() {
                let j = (self.next_rand() as usize) % (i + 1);
                self.bag.swap(i, j);
            }
            self.bag_pos = 0;
        }
        let k = self.bag[self.bag_pos];
        self.bag_pos += 1;
        k
    }

    /// Seeds a low starter warehouse floor (leaving center spawn clear) so the player can
    /// immediately push crates and build clusters on Beat 1.
    fn seed_starter_floor(&mut self) {
        let bottom = GRID_ROWS - 1;
        for col in [0usize, 1, 3, 8, 10, 11] {
            let k = self.draw_crate_kind();
            self.grid[bottom][col] = Some(GroundedCrate::new(k));
        }
        self.recompute_armed();
    }

    fn ensure_cranes_spawned(&mut self) {
        let wanted = self
            .prefs
            .difficulty
            .max_cranes_for_wave(self.wave, self.drops_done);
        let active_cnt = self.cranes.iter().filter(|c| c.active).count();
        if active_cnt >= wanted {
            return;
        }
        for idx in 0..wanted {
            if !self.cranes[idx].active {
                let from_left = idx % 2 == 0;
                let start_col = if from_left { 0 } else { (GRID_COLS - 1) as i32 };
                let cargo = self.draw_crate_kind();
                let target = self.pick_drop_column(start_col, Some(idx));
                let dir = if (target as i32) >= start_col { 1 } else { -1 };
                self.cranes[idx] = OverheadCrane {
                    active: true,
                    col: start_col,
                    vis_col: start_col as f32,
                    dir,
                    target_col: target,
                    cargo: Some(cargo),
                    mode: CraneMode::Carrying,
                    telegraph_beats: 0.0,
                    rail_steps_loaded: 0,
                    open_flash: 0.0,
                };
                break;
            }
        }
    }

    fn column_stack_height(&self, col: usize) -> usize {
        let mut h = 0usize;
        for r in (0..GRID_ROWS).rev() {
            if self.grid[r][col].is_some() {
                h = GRID_ROWS - r;
            }
        }
        h
    }

    fn column_has_telegraph_other(&self, col: usize, self_idx: Option<usize>) -> bool {
        self.cranes.iter().enumerate().any(|(j, c)| {
            Some(j) != self_idx
                && c.active
                && c.mode == CraneMode::Telegraph
                && c.col.clamp(0, (GRID_COLS - 1) as i32) as usize == col
        })
    }

    fn column_targeted_by_other(&self, col: usize, self_idx: Option<usize>) -> bool {
        self.cranes.iter().enumerate().any(|(j, c)| {
            Some(j) != self_idx
                && c.active
                && c.mode != CraneMode::EmptyReturn
                && c.target_col == col
        })
    }

    /// Picks a valid non-full column (`0..11`, `grid[0][col].is_none()`) for a loaded crane,
    /// falling back to any column only if the entire ceiling row is jammed.
    fn pick_drop_column(&mut self, crane_col: i32, self_idx: Option<usize>) -> usize {
        let seed_off = self.next_rand() as usize;
        let mut best_col = None;
        let mut best_score = i32::MIN;

        for step in 0..GRID_COLS {
            let cand = (seed_off + step * 5) % GRID_COLS;
            if self.grid[0][cand].is_some() {
                continue;
            }
            let h = self.column_stack_height(cand) as i32;
            let dist = ((cand as i32) - crane_col).abs();
            let high_drop = self.falling.iter().any(|f| f.col == cand && f.row <= 2);
            let tele_busy = self.column_has_telegraph_other(cand, self_idx);
            let tgt_busy = self.column_targeted_by_other(cand, self_idx);
            let busy_pen = (if tele_busy { 50 } else { 0 })
                + (if high_drop { 28 } else { 0 })
                + (if tgt_busy { 22 } else { 0 });
            let jitter = ((self.next_rand() % 7) as i32) - 3;
            let score = (dist.min(6) * 3) - (h * 6) - busy_pen + jitter;
            if score > best_score {
                best_score = score;
                best_col = Some(cand);
            }
        }

        best_col.unwrap_or_else(|| crane_col.clamp(0, (GRID_COLS - 1) as i32) as usize)
    }

    /// Finds the nearest valid non-full column without another crane actively telegraphing in it.
    fn resolve_free_drop_col(&self, preferred: usize, self_idx: usize) -> usize {
        let pref = preferred.min(GRID_COLS - 1);
        if self.grid[0][pref].is_none() && !self.column_has_telegraph_other(pref, Some(self_idx)) {
            return pref;
        }
        for delta in 1..GRID_COLS {
            for sign in [-1i32, 1i32] {
                let c = (pref as i32) + sign * (delta as i32);
                if (0..GRID_COLS as i32).contains(&c) {
                    let cu = c as usize;
                    if self.grid[0][cu].is_none()
                        && !self.column_has_telegraph_other(cu, Some(self_idx))
                    {
                        return cu;
                    }
                }
            }
        }
        for cu in 0..GRID_COLS {
            if self.grid[0][cu].is_none() {
                return cu;
            }
        }
        pref
    }

    pub fn step(
        &mut self,
        clock: &mut BeatClock,
        beat_in: BeatFrameInput<'_>,
        inp: FrameInput,
        uk: bool,
    ) {
        let dt = beat_in.dt_sec.clamp(1.0 / 240.0, 0.05);
        let step = clock.update(beat_in);
        self.last_beat = step;

        if inp.toggle_hint {
            self.show_hint = !self.show_hint;
            self.hint_pinned = self.show_hint;
        }

        if step.fire_strong_beat {
            self.strong_pulse = 1.0;
            self.bpm_pulse = 1.0;
        } else if step.fire_beat || step.raw_onset {
            self.bpm_pulse = self.bpm_pulse.max(0.75);
            self.strong_pulse = (self.strong_pulse - dt * 3.4).max(0.0);
        } else {
            self.bpm_pulse = (self.bpm_pulse - dt * 4.5).max(0.0);
            self.strong_pulse = (self.strong_pulse - dt * 3.4).max(0.0);
        }

        // Decay visual particles & callouts even on Game Over
        self.update_fx(dt);

        if self.lose_reason.is_some() {
            return;
        }

        // Advance perceptual flash age on all currently armed crates
        for r in 0..GRID_ROWS {
            for c in 0..GRID_COLS {
                if let Some(ref mut cell) = self.grid[r][c] {
                    if cell.is_armed() {
                        cell.armed_sec += dt;
                        cell.armed_beats += step.step_beats;
                    }
                }
            }
        }

        // 1. Settle any unsupported grounded crates onto the beat / gravity cascade
        if step.fire_half_beat && self.has_unsupported_grounded_crates() {
            let _ = self.drop_unsupported_crates_one_row();
            self.gravity_accum = 0.0;
        } else {
            self.step_settled_gravity(dt);
        }

        // 2. Step worker controls, walking, single-crate pushing, and `B` / `L1` / `R1` jumps
        self.step_worker(dt, inp);
        if self.lose_reason.is_some() {
            return;
        }

        // 3. Strong-Beat Quantized Detonation (`Beats 1 & 3`, `beat_index % 2 == 0`)
        if step.fire_strong_beat {
            self.step_strong_beat_detonation(uk);
        }

        // 4. Step beat-quantized falling crates
        let half_beat_fall = self.prefs.difficulty.uses_half_beat_fall(self.wave);
        let fall_trigger = if half_beat_fall {
            step.fire_half_beat
        } else {
            step.fire_beat
        };
        if fall_trigger {
            self.step_falling_crates_on_beat();
            if self.lose_reason.is_some() {
                return;
            }
        }

        // Ease visual snap on falling crates & crane rail glide
        for fc in &mut self.falling {
            fc.snap_anim = (fc.snap_anim + dt * 13.0).min(0.0);
        }
        for cr in &mut self.cranes {
            if cr.active {
                let target_x = cr.col as f32;
                cr.vis_col += (target_x - cr.vis_col) * (dt * 18.0).min(1.0);
                cr.open_flash = (cr.open_flash - dt * 3.2).max(0.0);
            }
        }

        // 5. Step unbreakable overhead robotic crane state machine
        self.step_cranes(step.step_beats, step.fire_beat, step.fire_half_beat);
        if self.lose_reason.is_some() {
            return;
        }

        // 6. Update worker hard-hat danger indicator (falling crate or telegraph directly overhead)
        self.update_danger_indicator();
    }

    fn update_fx(&mut self, dt: f32) {
        for r in 0..GRID_ROWS {
            for c in 0..GRID_COLS {
                if let Some(ref mut cell) = self.grid[r][c] {
                    if cell.settle_px_frac > 0 {
                        let dec = ((dt * 1200.0) as u8).max(1);
                        cell.settle_px_frac = cell.settle_px_frac.saturating_sub(dec);
                    }
                }
            }
        }
        self.particles.retain_mut(|p| {
            p.x += p.vx * dt;
            p.y += p.vy * dt;
            p.vy += 18.0 * dt;
            p.life -= dt * 2.4;
            p.life > 0.0
        });
        if let Some(ref mut co) = self.callout {
            co.row_y -= dt * 0.9;
            co.timer -= dt;
            if co.timer <= 0.0 {
                self.callout = None;
            }
        }
    }

    fn update_danger_indicator(&mut self) {
        let wc = self.worker.col as usize;
        let wr = self.worker.row as usize;
        let falling_above = self.falling.iter().any(|f| f.col == wc && f.row <= wr);
        let telegraph_above = self
            .cranes
            .iter()
            .any(|c| c.active && c.mode == CraneMode::Telegraph && c.col as usize == wc);
        self.worker.danger_above = falling_above || telegraph_above;
    }

    fn step_settled_gravity(&mut self, dt: f32) {
        if !self.has_unsupported_grounded_crates() {
            self.gravity_accum = 0.0;
            return;
        }
        self.gravity_accum += dt;
        while self.gravity_accum >= 0.085 {
            self.gravity_accum -= 0.085;
            if !self.drop_unsupported_crates_one_row() {
                self.gravity_accum = 0.0;
                break;
            }
        }
    }

    fn has_unsupported_grounded_crates(&self) -> bool {
        for c in 0..GRID_COLS {
            let mut seen_empty = false;
            for r in (0..GRID_ROWS).rev() {
                if self.grid[r][c].is_none() {
                    seen_empty = true;
                } else if seen_empty {
                    return true;
                }
            }
        }
        false
    }

    /// Drops any grounded crate sitting above an empty cell by 1 row and re-arms clusters.
    pub fn drop_unsupported_crates_one_row(&mut self) -> bool {
        let mut moved = false;
        for c in 0..GRID_COLS {
            for r in (0..GRID_ROWS - 1).rev() {
                if self.grid[r][c].is_some() && self.grid[r + 1][c].is_none() {
                    // Check if worker is occupying (c, r + 1) — pushed crates never land on worker,
                    // and cascade crates above worker can't exist because worker stands on top of stacks.
                    let mut crate_cell = self.grid[r][c].take().unwrap();
                    crate_cell.settle_px_frac = 100;
                    self.grid[r + 1][c] = Some(crate_cell);
                    moved = true;
                }
            }
        }
        if moved {
            self.recompute_armed();
        }
        moved
    }

    /// Immediately settles all unsupported grounded crates to rest (used in deterministic tests
    /// and when finalizing cascade checks).
    pub fn settle_all_grounded_now(&mut self) {
        while self.drop_unsupported_crates_one_row() {}
    }

    fn is_cell_solid(&self, col: i32, row: i32) -> bool {
        if col < 0 || col >= (GRID_COLS as i32) || row >= (GRID_ROWS as i32) {
            return true;
        }
        if row < 0 {
            return false;
        }
        self.grid[row as usize][col as usize].is_some()
    }

    fn has_falling_crate_at(&self, col: i32, row: i32) -> bool {
        if col < 0 || col >= (GRID_COLS as i32) || row < 0 || row >= (GRID_ROWS as i32) {
            return false;
        }
        self.falling
            .iter()
            .any(|f| f.col == col as usize && f.row == row as usize)
    }

    fn check_worker_falling_collision(&mut self) -> bool {
        if self.has_falling_crate_at(self.worker.col, self.worker.row) {
            self.trigger_lose(LoseReason::Crushed);
            return true;
        }
        false
    }

    fn trigger_lose(&mut self, reason: LoseReason) {
        self.lose_reason = Some(reason);
        if self.score > self.prefs.hi_score {
            self.prefs.hi_score = self.score;
        }
        let cx = self.worker.col as f32 + 0.5;
        let cy = self.worker.row as f32 + 0.5;
        self.spawn_burst(cx, cy, 14);
    }

    fn step_worker(&mut self, dt: f32, inp: FrameInput) {
        if self.move_cooldown > 0.0 {
            self.move_cooldown = (self.move_cooldown - dt).max(0.0);
        }

        // Check if support vanished beneath a grounded worker (e.g. Sweep cleared floor row)
        if matches!(self.worker.action, WorkerAction::Idle)
            && self.worker.row < (GRID_ROWS as i32 - 1)
            && !self.is_cell_solid(self.worker.col, self.worker.row + 1)
        {
            self.worker.action = WorkerAction::Falling {
                vis_row: self.worker.row as f32,
            };
        }

        match self.worker.action {
            WorkerAction::Walking {
                from_col,
                mut progress,
            } => {
                progress += dt / 0.090;
                if progress >= 1.0 {
                    self.worker.walk_phase = self.worker.walk_phase.wrapping_add(1);
                    if self.worker.row < (GRID_ROWS as i32 - 1)
                        && !self.is_cell_solid(self.worker.col, self.worker.row + 1)
                    {
                        self.worker.action = WorkerAction::Falling {
                            vis_row: self.worker.row as f32,
                        };
                    } else {
                        self.worker.action = WorkerAction::Idle;
                    }
                } else {
                    self.worker.action = WorkerAction::Walking { from_col, progress };
                }
            }
            WorkerAction::Pushing { dir, mut progress } => {
                progress += dt / 0.115;
                if progress >= 1.0 {
                    self.worker.walk_phase = self.worker.walk_phase.wrapping_add(1);
                    if self.worker.row < (GRID_ROWS as i32 - 1)
                        && !self.is_cell_solid(self.worker.col, self.worker.row + 1)
                    {
                        self.worker.action = WorkerAction::Falling {
                            vis_row: self.worker.row as f32,
                        };
                    } else {
                        self.worker.action = WorkerAction::Idle;
                    }
                } else {
                    self.worker.action = WorkerAction::Pushing { dir, progress };
                }
            }
            WorkerAction::Jumping {
                from_col,
                from_row,
                mut to_col,
                to_row,
                mut progress,
                blocked_wall,
                mut steered_apex,
            } => {
                // Allow steering left/right mid-vertical-jump near apex onto an adjacent 1-high step!
                if !steered_apex
                    && !blocked_wall
                    && (to_col as f32 - from_col).abs() < 0.01
                    && (0.20..=0.78).contains(&progress)
                    && inp.move_dir != 0
                {
                    let cand_col = self.worker.col + inp.move_dir;
                    if (0..GRID_COLS as i32).contains(&cand_col)
                        && !self.is_cell_solid(cand_col, to_row)
                        && !self.has_falling_crate_at(cand_col, to_row)
                    {
                        self.worker.facing = inp.move_dir;
                        to_col = cand_col;
                        steered_apex = true;
                    }
                }

                progress += dt / 0.195;
                if progress >= 1.0 {
                    if !blocked_wall {
                        self.worker.col = to_col;
                        self.worker.row = to_row;
                    }
                    if self.check_worker_falling_collision() {
                        return;
                    }
                    if self.worker.row < (GRID_ROWS as i32 - 1)
                        && !self.is_cell_solid(self.worker.col, self.worker.row + 1)
                    {
                        self.worker.action = WorkerAction::Falling {
                            vis_row: self.worker.row as f32,
                        };
                    } else {
                        self.worker.action = WorkerAction::Idle;
                    }
                } else {
                    self.worker.action = WorkerAction::Jumping {
                        from_col,
                        from_row,
                        to_col,
                        to_row,
                        progress,
                        blocked_wall,
                        steered_apex,
                    };
                }
            }
            WorkerAction::Falling { mut vis_row } => {
                vis_row += dt * 10.5;
                let next_row = vis_row.floor() as i32;
                while self.worker.row < next_row && self.worker.row < (GRID_ROWS as i32 - 1) {
                    if self.is_cell_solid(self.worker.col, self.worker.row + 1) {
                        self.worker.action = WorkerAction::Idle;
                        let _ = self.check_worker_falling_collision();
                        return;
                    }
                    self.worker.row += 1;
                    if self.check_worker_falling_collision() {
                        return;
                    }
                }
                if self.worker.row >= (GRID_ROWS as i32 - 1)
                    || self.is_cell_solid(self.worker.col, self.worker.row + 1)
                {
                    self.worker.row = self.worker.row.min(GRID_ROWS as i32 - 1);
                    self.worker.action = WorkerAction::Idle;
                } else {
                    self.worker.action = WorkerAction::Falling { vis_row };
                }
            }
            WorkerAction::Idle => {
                self.handle_grounded_worker_input(inp);
            }
        }
    }

    fn handle_grounded_worker_input(&mut self, inp: FrameInput) {
        // 1. Diagonal Jump (`L1`/`R1` or `B + D-Pad Left/Right` chord; ZERO `X`/`Y` jump bindings!)
        let diag_dir = if inp.diag_left
            || (inp.jump_b && inp.move_dir < 0)
            || (inp.held_b && inp.press_left)
        {
            -1
        } else if inp.diag_right
            || (inp.jump_b && inp.move_dir > 0)
            || (inp.held_b && inp.press_right)
        {
            1
        } else {
            0
        };

        if diag_dir != 0 {
            self.try_diagonal_jump(diag_dir);
            return;
        }

        // 2. Vertical Jump (`B` only)
        if inp.jump_b {
            self.try_vertical_jump();
            return;
        }

        // 3. Horizontal Walk / Single-Crate Push (`D-Pad Left / Right`)
        let dir = if inp.press_left {
            -1
        } else if inp.press_right {
            1
        } else if self.move_cooldown <= 0.0 {
            inp.move_dir
        } else {
            0
        };

        if dir != 0 {
            self.try_walk_or_push(dir);
        }
    }

    pub fn try_vertical_jump(&mut self) -> bool {
        let r = self.worker.row;
        let c = self.worker.col;
        if r <= 0 || self.is_cell_solid(c, r - 1) {
            return false;
        }
        if self.has_falling_crate_at(c, r - 1) {
            self.worker.row = r - 1;
            self.trigger_lose(LoseReason::Crushed);
            return true;
        }
        self.worker.action = WorkerAction::Jumping {
            from_col: c as f32,
            from_row: r as f32,
            to_col: c,
            to_row: r - 1,
            progress: 0.0,
            blocked_wall: false,
            steered_apex: false,
        };
        true
    }

    pub fn try_diagonal_jump(&mut self, dir: i32) -> bool {
        self.worker.facing = dir;
        let c = self.worker.col;
        let r = self.worker.row;
        if r <= 0 || self.is_cell_solid(c, r - 1) {
            return false;
        }
        let nx = c + dir;
        if !(0..GRID_COLS as i32).contains(&nx) {
            return self.try_vertical_jump();
        }
        if self.has_falling_crate_at(c, r - 1) {
            self.worker.row = r - 1;
            self.trigger_lose(LoseReason::Crushed);
            return true;
        }

        // If target diagonal cell `(nx, r - 1)` is blocked by a 2-high crate stack,
        // perform Siemens-style blocked bounce back into `(c, r)`.
        let blocked = self.is_cell_solid(nx, r - 1);
        if !blocked && self.has_falling_crate_at(nx, r - 1) {
            self.worker.col = nx;
            self.worker.row = r - 1;
            self.trigger_lose(LoseReason::Crushed);
            return true;
        }

        self.worker.action = WorkerAction::Jumping {
            from_col: c as f32,
            from_row: r as f32,
            to_col: if blocked { c } else { nx },
            to_row: if blocked { r } else { r - 1 },
            progress: 0.0,
            blocked_wall: blocked,
            steered_apex: true,
        };
        true
    }

    /// Walks 1 tile in `dir` or pushes a single grounded crate 1 tile horizontally.
    pub fn try_walk_or_push(&mut self, dir: i32) -> bool {
        self.worker.facing = dir;
        let c = self.worker.col;
        let r = self.worker.row;
        let nx = c + dir;
        if !(0..GRID_COLS as i32).contains(&nx) {
            return false;
        }

        // Empty cell ahead: walk straight into `(nx, r)`
        if !self.is_cell_solid(nx, r) {
            if self.has_falling_crate_at(nx, r) {
                self.worker.col = nx;
                self.trigger_lose(LoseReason::Crushed);
                return true;
            }
            let prev_c = self.worker.col;
            self.worker.col = nx;
            self.worker.action = WorkerAction::Walking {
                from_col: prev_c,
                progress: 0.0,
            };
            self.move_cooldown = 0.11;
            return true;
        }

        // Grounded crate ahead at `(nx, r)`: check Stack Attack single-crate push rules!
        // 1) Space behind crate `(nnx, r)` must be inside shaft and completely empty.
        let nnx = nx + dir;
        if !(0..GRID_COLS as i32).contains(&nnx) {
            return false;
        }
        if self.is_cell_solid(nnx, r) || self.has_falling_crate_at(nnx, r) {
            return false;
        }
        // 2) Cannot push a stack of 2+ crates: cell directly above `(nx, r - 1)` must be empty!
        if r > 0 && (self.is_cell_solid(nx, r - 1) || self.has_falling_crate_at(nx, r - 1)) {
            return false;
        }

        let mut moved_crate = self.grid[r as usize][nx as usize].take().unwrap();
        moved_crate.armed_sec = 0.0;
        moved_crate.armed_beats = 0.0;
        self.grid[r as usize][nnx as usize] = Some(moved_crate);
        self.worker.col = nx;
        self.worker.action = WorkerAction::Pushing { dir, progress: 0.0 };
        self.move_cooldown = 0.135;

        // If pushed over an empty ledge, start gravity drop immediately; otherwise re-arm!
        if (r as usize) + 1 < GRID_ROWS && self.grid[(r as usize) + 1][nnx as usize].is_none() {
            let _ = self.drop_unsupported_crates_one_row();
        } else {
            self.recompute_armed();
        }
        true
    }

    fn step_falling_crates_on_beat(&mut self) {
        // Sort bottom-up so stacked drops never collide with each other out of order
        self.falling.sort_by(|a, b| b.row.cmp(&a.row));

        let mut idx = 0;
        while idx < self.falling.len() {
            let col = self.falling[idx].col;
            let row = self.falling[idx].row;
            let kind = self.falling[idx].kind;

            if self.worker.col == col as i32 && self.worker.row == row as i32 {
                self.trigger_lose(LoseReason::Crushed);
                return;
            }

            // Land if on floor or grounded crate directly underneath
            if row + 1 >= GRID_ROWS || self.grid[row + 1][col].is_some() {
                self.falling.remove(idx);
                self.grid[row][col] = Some(GroundedCrate::new(kind));
                self.recompute_armed();
                continue;
            }

            // Step 1 row downward on the beat
            let next_row = row + 1;
            if self.worker.col == col as i32 && self.worker.row == next_row as i32 {
                self.falling[idx].row = next_row;
                self.trigger_lose(LoseReason::Crushed);
                return;
            }

            if next_row + 1 >= GRID_ROWS || self.grid[next_row + 1][col].is_some() {
                self.falling.remove(idx);
                let mut landed = GroundedCrate::new(kind);
                landed.settle_px_frac = 65;
                self.grid[next_row][col] = Some(landed);
                self.recompute_armed();
            } else {
                self.falling[idx].row = next_row;
                self.falling[idx].snap_anim = -1.0;
                idx += 1;
            }
        }
    }

    /// Unbreakable crane state machine (`>=` / `<=` progress thresholds + rail watchdog).
    fn step_cranes(&mut self, step_beats: f32, fire_beat: bool, fire_half_beat: bool) {
        self.ensure_cranes_spawned();

        for i in 0..MAX_CRANES {
            if !self.cranes[i].active {
                continue;
            }
            if self.cranes[i].mode != CraneMode::EmptyReturn && self.cranes[i].cargo.is_none() {
                self.cranes[i].mode = CraneMode::EmptyReturn;
            }

            match self.cranes[i].mode {
                CraneMode::Telegraph => {
                    self.cranes[i].telegraph_beats += step_beats;
                    if (fire_beat && self.cranes[i].telegraph_beats >= 0.72)
                        || self.cranes[i].telegraph_beats >= 1.25
                    {
                        let drop_col =
                            (self.cranes[i].col.clamp(0, (GRID_COLS - 1) as i32)) as usize;
                        if let Some(kind) = self.cranes[i].cargo.take() {
                            if self.grid[0][drop_col].is_some() {
                                self.trigger_lose(LoseReason::ShaftJammed);
                                return;
                            }
                            if self.worker.col == drop_col as i32 && self.worker.row == 0 {
                                self.trigger_lose(LoseReason::Crushed);
                                return;
                            }
                            self.falling.push(FallingCrate {
                                col: drop_col,
                                row: 0,
                                kind,
                                snap_anim: -0.65,
                            });
                            self.drops_done += 1;
                        }
                        self.cranes[i].mode = CraneMode::EmptyReturn;
                        self.cranes[i].telegraph_beats = 0.0;
                        self.cranes[i].rail_steps_loaded = 0;
                        self.cranes[i].open_flash = 1.0;
                        self.cranes[i].dir = if drop_col < GRID_COLS / 2 { -1 } else { 1 };
                    }
                }
                CraneMode::Carrying => {
                    let tgt = self.cranes[i].target_col as i32;
                    let reached = if self.cranes[i].dir >= 0 {
                        self.cranes[i].col >= tgt
                    } else {
                        self.cranes[i].col <= tgt
                    };
                    let watchdog = self.cranes[i].rail_steps_loaded >= (GRID_COLS as u8 + 2);

                    if (reached && (fire_beat || fire_half_beat)) || watchdog {
                        let valid_col = self.resolve_free_drop_col(self.cranes[i].target_col, i);
                        if valid_col == self.cranes[i].target_col
                            || watchdog
                            || self.cranes[i].col == valid_col as i32
                        {
                            self.cranes[i].col = valid_col as i32;
                            self.cranes[i].target_col = valid_col;
                            self.cranes[i].mode = CraneMode::Telegraph;
                            self.cranes[i].telegraph_beats = 0.0;
                            self.cranes[i].rail_steps_loaded = 0;
                        } else {
                            self.cranes[i].target_col = valid_col;
                            self.cranes[i].dir =
                                if (valid_col as i32) >= self.cranes[i].col { 1 } else { -1 };
                        }
                    } else if fire_half_beat {
                        let dir = if tgt >= self.cranes[i].col { 1 } else { -1 };
                        self.cranes[i].dir = dir;
                        self.cranes[i].col =
                            (self.cranes[i].col + dir).clamp(0, (GRID_COLS - 1) as i32);
                        self.cranes[i].rail_steps_loaded =
                            self.cranes[i].rail_steps_loaded.saturating_add(1);
                    }
                }
                CraneMode::EmptyReturn => {
                    if fire_half_beat {
                        let dir = if self.cranes[i].dir >= 0 { 1 } else { -1 };
                        let next_c =
                            (self.cranes[i].col + dir).clamp(0, (GRID_COLS - 1) as i32);
                        if next_c <= 0 || next_c >= (GRID_COLS as i32 - 1) {
                            let bay_col = if next_c <= 0 {
                                0
                            } else {
                                (GRID_COLS - 1) as i32
                            };
                            let new_cargo = self.draw_crate_kind();
                            let tgt = self.pick_drop_column(bay_col, Some(i));
                            let new_dir = if (tgt as i32) >= bay_col { 1 } else { -1 };
                            self.cranes[i].col = bay_col;
                            self.cranes[i].cargo = Some(new_cargo);
                            self.cranes[i].target_col = tgt;
                            self.cranes[i].dir = new_dir;
                            self.cranes[i].telegraph_beats = 0.0;
                            self.cranes[i].rail_steps_loaded = 0;
                            self.cranes[i].mode = CraneMode::Carrying;
                        } else {
                            self.cranes[i].col = next_c;
                        }
                    }
                }
            }
        }

        self.ensure_cranes_spawned();
    }

    // ========================================================================
    // Dual Arming (`2×2+` Color Square + `12/12` Floor Row) & Strong-Beat Detonation
    // ========================================================================

    pub fn recompute_armed(&mut self) {
        let mut sq_mask = [[false; GRID_COLS]; GRID_ROWS];
        for r in 0..(GRID_ROWS - 1) {
            for c in 0..(GRID_COLS - 1) {
                if let (Some(a), Some(b), Some(d), Some(e)) = (
                    self.grid[r][c],
                    self.grid[r][c + 1],
                    self.grid[r + 1][c],
                    self.grid[r + 1][c + 1],
                ) {
                    if a.kind == b.kind && a.kind == d.kind && a.kind == e.kind {
                        sq_mask[r][c] = true;
                        sq_mask[r][c + 1] = true;
                        sq_mask[r + 1][c] = true;
                        sq_mask[r + 1][c + 1] = true;
                    }
                }
            }
        }

        let floor_r = GRID_ROWS - 1;
        let full_floor = (0..GRID_COLS).all(|c| self.grid[floor_r][c].is_some());

        for r in 0..GRID_ROWS {
            for c in 0..GRID_COLS {
                if let Some(ref mut cell) = self.grid[r][c] {
                    let was_armed = cell.is_armed();
                    cell.armed_square = sq_mask[r][c];
                    cell.armed_row = r == floor_r && full_floor;
                    if !cell.is_armed() || !was_armed {
                        cell.armed_sec = 0.0;
                        cell.armed_beats = 0.0;
                    }
                }
            }
        }
    }

    fn any_crate_armed(&self) -> bool {
        for r in 0..GRID_ROWS {
            for c in 0..GRID_COLS {
                if let Some(cell) = self.grid[r][c] {
                    if cell.is_armed() {
                        return true;
                    }
                }
            }
        }
        false
    }

    /// Detonates all armed `2×2+` same-color clusters and `12/12` floor rows that have
    /// completed at least `1/2 beat` (`>= 150 ms`) perceptual flash on Strong Beats (`Beats 1 & 3`).
    pub fn step_strong_beat_detonation(&mut self, uk: bool) -> u32 {
        let mut visited = [[false; GRID_COLS]; GRID_ROWS];
        let mut clear_mask = [[false; GRID_COLS]; GRID_ROWS];
        let mut groups_detonated = 0u32;
        let mut mono_row_bonus = false;
        let mut last_group_center = (6.0_f32, 4.0_f32);

        // 1. Connected `2×2+` same-color square clusters
        for r in 0..GRID_ROWS {
            for c in 0..GRID_COLS {
                if visited[r][c] {
                    continue;
                }
                let Some(seed) = self.grid[r][c] else {
                    continue;
                };
                if !seed.armed_square {
                    continue;
                }
                let mut stack = vec![(c, r)];
                let mut members = Vec::with_capacity(8);
                let mut min_c = c;
                let mut max_c = c;
                let mut group_ready = false;
                visited[r][c] = true;

                while let Some((cx, cy)) = stack.pop() {
                    members.push((cx, cy));
                    min_c = min_c.min(cx);
                    max_c = max_c.max(cx);
                    if let Some(cell) = self.grid[cy][cx] {
                        if cell.ready_to_detonate() {
                            group_ready = true;
                        }
                    }
                    for (dx, dy) in [(-1i32, 0i32), (1, 0), (0, -1), (0, 1)] {
                        let nx = cx as i32 + dx;
                        let ny = cy as i32 + dy;
                        if (0..GRID_COLS as i32).contains(&nx)
                            && (0..GRID_ROWS as i32).contains(&ny)
                        {
                            let ux = nx as usize;
                            let uy = ny as usize;
                            if !visited[uy][ux] {
                                if let Some(nb) = self.grid[uy][ux] {
                                    if nb.armed_square && nb.kind == seed.kind {
                                        visited[uy][ux] = true;
                                        stack.push((ux, uy));
                                    }
                                }
                            }
                        }
                    }
                }

                if group_ready {
                    for &(mx, my) in &members {
                        clear_mask[my][mx] = true;
                    }
                    groups_detonated += 1;
                    last_group_center = (
                        (min_c + max_c) as f32 * 0.5 + 0.5,
                        members[0].1 as f32,
                    );
                }
            }
        }

        // 2. Classic `12/12` Full Floor Row (`MONO ROW ×3` if all 12 crates match)
        let floor_r = GRID_ROWS - 1;
        let floor_armed = (0..GRID_COLS).all(|c| {
            self.grid[floor_r][c]
                .map(|cell| cell.armed_row)
                .unwrap_or(false)
        });
        if floor_armed {
            let row_ready = (0..GRID_COLS).any(|c| {
                self.grid[floor_r][c]
                    .map(|cell| cell.ready_to_detonate())
                    .unwrap_or(false)
            });
            if row_ready {
                let first_kind = self.grid[floor_r][0].map(|c| c.kind);
                let is_mono = first_kind.is_some()
                    && (0..GRID_COLS).all(|c| self.grid[floor_r][c].map(|x| x.kind) == first_kind);
                for c in 0..GRID_COLS {
                    clear_mask[floor_r][c] = true;
                }
                groups_detonated += 1;
                mono_row_bonus = is_mono;
                last_group_center = (6.0, floor_r as f32);
            }
        }

        // 3. Apply simultaneous strong-beat detonation, burst particles, chain & gravity cascade
        if groups_detonated > 0 {
            let mut crates_detonated = 0u32;
            for r in 0..GRID_ROWS {
                for c in 0..GRID_COLS {
                    if clear_mask[r][c] && self.grid[r][c].take().is_some() {
                        crates_detonated += 1;
                        self.spawn_burst(c as f32 + 0.5, r as f32 + 0.5, 6);
                    }
                }
            }

            if crates_detonated > 0 {
                if self.chain_active {
                    self.chain = (self.chain + 1).min(16);
                } else {
                    self.chain = 1;
                    self.chain_active = true;
                }
                self.max_chain = self.max_chain.max(self.chain);

                let base_pts = crates_detonated * 50 + groups_detonated.saturating_sub(1) * 100;
                let mono_mult = if mono_row_bonus { 3 } else { 1 };
                let gained = base_pts * mono_mult * self.chain;
                self.score = self.score.saturating_add(gained);
                if self.score > self.prefs.hi_score {
                    self.prefs.hi_score = self.score;
                }

                self.cleared_crates += crates_detonated;
                self.wave = 1 + (self.cleared_crates / 12);
                if !self.hint_pinned {
                    self.show_hint = false;
                }

                let label = if mono_row_bonus && self.chain > 1 {
                    format!(
                        "+{}  MONO ×3 · {} ×{}",
                        gained,
                        if uk { "ЛАНЦЮГ" } else { "CHAIN" },
                        self.chain
                    )
                } else if mono_row_bonus {
                    format!(
                        "+{}  {}",
                        gained,
                        if uk { "МОНО РЯД ×3" } else { "MONO ROW ×3" }
                    )
                } else if self.chain > 1 {
                    format!(
                        "+{}  {} ×{}",
                        gained,
                        if uk { "ЛАНЦЮГ" } else { "CHAIN" },
                        self.chain
                    )
                } else {
                    format!("+{}", gained)
                };
                self.callout = Some(FloatingCallout {
                    text: label,
                    col_center: last_group_center.0.clamp(2.4, 9.6),
                    row_y: last_group_center.1.clamp(1.2, 5.8),
                    timer: 1.10,
                });

                // Drop unsupported upper crates onto the beat so cascade matches arm for next strong beat!
                let _ = self.drop_unsupported_crates_one_row();
                self.recompute_armed();
                return crates_detonated;
            }
        }

        // Preserve chain if crates are still settling or flashing for the next strong beat;
        // otherwise reset chain cleanly to `×1`.
        if !self.any_crate_armed() && !self.has_unsupported_grounded_crates() {
            self.chain_active = false;
            self.chain = 1;
        }
        0
    }

    fn spawn_burst(&mut self, cx: f32, cy: f32, count: usize) {
        for i in 0..count {
            if self.particles.len() >= 64 {
                break;
            }
            let ang = (i as f32 / count.max(1) as f32) * core::f32::consts::TAU
                + ((self.next_rand() % 100) as f32) * 0.02;
            let spd = 2.6 + ((self.next_rand() % 100) as f32) * 0.04;
            self.particles.push(BurstParticle {
                x: cx,
                y: cy,
                vx: ang.cos() * spd,
                vy: ang.sin() * spd - 2.5,
                life: 1.0,
                shade: ((i % 2) + 2) as u8,
            });
        }
    }

    pub fn worker_visual_pos(&self) -> (f32, f32, bool, i32) {
        match self.worker.action {
            WorkerAction::Idle => (self.worker.col as f32, self.worker.row as f32, false, 0),
            WorkerAction::Walking { from_col, progress } => {
                let x = from_col as f32 + (self.worker.col as f32 - from_col as f32) * progress;
                let hop = (progress * core::f32::consts::PI).sin() * 0.08;
                (x, self.worker.row as f32 - hop, false, 0)
            }
            WorkerAction::Pushing { dir, progress } => {
                let from_c = (self.worker.col - dir) as f32;
                let x = from_c + (dir as f32) * progress;
                (x, self.worker.row as f32, false, dir)
            }
            WorkerAction::Jumping {
                from_col,
                from_row,
                to_col,
                to_row,
                progress,
                blocked_wall,
                ..
            } => {
                let x = if blocked_wall {
                    let bump = (progress * core::f32::consts::PI).sin()
                        * (self.worker.facing as f32)
                        * 0.28;
                    from_col + bump
                } else {
                    from_col + (to_col as f32 - from_col) * progress
                };
                let lin_y = from_row + (to_row as f32 - from_row) * progress;
                let arc = 4.0 * progress * (1.0 - progress) * 0.85;
                (x, lin_y - arc, true, 0)
            }
            WorkerAction::Falling { vis_row } => (self.worker.col as f32, vis_row, true, 0),
        }
    }
}

// ============================================================================
// 4-Shade Industrial Vector Renderer (Siemens Stack Attack — Strong-Beat Edition)
// ============================================================================

fn draw_crate(
    ctx: &HostContext<'_>,
    t: PluginTheme,
    x: f32,
    y: f32,
    tile: f32,
    kind: CrateKind,
    armed: bool,
    telegraph_flash: bool,
    beat_frac: f32,
) {
    let inset = (tile * SPEC.crate_inset_ratio).round().max(1.5);
    let bx = x + inset;
    let by = y + inset;
    let bw = (tile - inset * 2.0).max(8.0);
    let bh = bw;
    let rad = (tile * SPEC.crate_radius_ratio).round().max(2.0);
    let border = (tile * 0.065).clamp(2.0, 4.0);

    match kind {
        CrateKind::Light => {
            ctx.fill_rect(bx, by, bw, bh, rad, t.bg);
            ctx.stroke_rect(bx, by, bw, bh, rad, border, t.fg);
            let in_pad = border + 2.0;
            ctx.stroke_rect(
                bx + in_pad,
                by + in_pad,
                bw - in_pad * 2.0,
                bh - in_pad * 2.0,
                1.5,
                border,
                t.fg2,
            );
            let rv = (tile * 0.11).clamp(3.0, 6.5);
            let roff = in_pad + border + 2.0;
            for (rx, ry) in [
                (bx + roff, by + roff),
                (bx + bw - roff - rv, by + roff),
                (bx + roff, by + bh - roff - rv),
                (bx + bw - roff - rv, by + bh - roff - rv),
            ] {
                ctx.fill_rect(rx, ry, rv, rv, 1.0, t.fg2);
            }
        }
        CrateKind::Checker => {
            ctx.fill_rect(bx, by, bw, bh, rad, t.fg1);
            let inner = border;
            let iw = (bw - inner * 2.0).max(4.0);
            let cell = iw / 4.0;
            for sr in 0..4 {
                for sc in 0..4 {
                    if (sr + sc) % 2 == 1 {
                        ctx.fill_rect(
                            bx + inner + (sc as f32) * cell,
                            by + inner + (sr as f32) * cell,
                            cell.ceil(),
                            cell.ceil(),
                            0.0,
                            t.fg2,
                        );
                    }
                }
            }
            ctx.draw_line(
                bx + inner + 2.0,
                by + bh - inner - 2.0,
                bx + bw - inner - 2.0,
                by + inner + 2.0,
                border * 1.15,
                t.fg,
            );
            ctx.stroke_rect(bx, by, bw, bh, rad, border, t.fg);
        }
        CrateKind::Medium => {
            ctx.fill_rect(bx, by, bw, bh, rad, t.fg2);
            let stripe_h = (bh * 0.13).clamp(3.0, 7.0);
            let pad_x = (bw * 0.18).round();
            let sw = (bw - pad_x * 2.0).max(4.0);
            ctx.fill_rect(bx + pad_x, by + bh * 0.28, sw, stripe_h, 1.0, t.bg);
            ctx.fill_rect(
                bx + pad_x,
                by + bh - bh * 0.28 - stripe_h,
                sw,
                stripe_h,
                1.0,
                t.bg,
            );
            ctx.stroke_rect(bx, by, bw, bh, rad, border, t.fg);
        }
        CrateKind::Heavy => {
            ctx.fill_rect(bx, by, bw, bh, rad, t.fg);
            let pad = (bw * 0.22).round();
            ctx.draw_line(
                bx + pad,
                by + pad,
                bx + bw - pad,
                by + bh - pad,
                border * 1.1,
                t.bg,
            );
            ctx.draw_line(
                bx + bw - pad,
                by + pad,
                bx + pad,
                by + bh - pad,
                border * 1.1,
                t.bg,
            );
            ctx.stroke_rect(
                bx + border,
                by + border,
                bw - border * 2.0,
                bh - border * 2.0,
                1.5,
                1.5,
                t.fg2,
            );
        }
    }

    if telegraph_flash {
        ctx.stroke_rect(bx - 1.0, by - 1.0, bw + 2.0, bh + 2.0, rad, border + 1.5, t.fg);
        ctx.stroke_rect(
            bx + border,
            by + border,
            bw - border * 2.0,
            bh - border * 2.0,
            2.0,
            border,
            t.bg,
        );
    } else if armed {
        let pulse_on = beat_frac < 0.52;
        let inner_col = if pulse_on { t.bg } else { t.fg1 };
        ctx.stroke_rect(
            bx + border * 0.8,
            by + border * 0.8,
            bw - border * 1.6,
            bh - border * 1.6,
            2.0,
            border,
            inner_col,
        );
        let core = if pulse_on {
            (bw * 0.26).max(6.0)
        } else {
            (bw * 0.18).max(4.5)
        };
        ctx.fill_rect(
            bx + (bw - core) * 0.5,
            by + (bh - core) * 0.5,
            core,
            core,
            2.0,
            t.bg,
        );
        ctx.stroke_rect(
            bx + (bw - core) * 0.5,
            by + (bh - core) * 0.5,
            core,
            core,
            2.0,
            2.0,
            t.fg,
        );
    }
}

/// Draws a bright pulsing outer highlight perimeter around every armed `2×2+` cluster
/// and around an armed `12/12` floor row so matches are unmistakable before strong-beat detonation.
fn draw_armed_group_highlights(
    ctx: &HostContext<'_>,
    t: PluginTheme,
    lay: &ComputedLayout,
    game: &BeatStackGame,
    beat_frac: f32,
) {
    let tile = lay.tile;
    let pulse_bright = beat_frac < 0.52;
    let outer_col = if pulse_bright { t.fg } else { t.fg2 };
    let inner_col = if pulse_bright { t.bg } else { t.fg };
    let w_outer = 5.0;
    let w_inner = 2.2;

    // 1. Perimeter edges of connected same-color `armed_square` clusters
    for r in 0..GRID_ROWS {
        for c in 0..GRID_COLS {
            let Some(cell) = game.grid[r][c] else {
                continue;
            };
            if !cell.armed_square {
                continue;
            }
            let x0 = lay.grid_x + (c as f32) * tile;
            let y0 = lay.grid_y + (r as f32) * tile;
            let x1 = x0 + tile;
            let y1 = y0 + tile;

            let same_at = |nc: i32, nr: i32| -> bool {
                if !(0..GRID_COLS as i32).contains(&nc) || !(0..GRID_ROWS as i32).contains(&nr) {
                    return false;
                }
                game.grid[nr as usize][nc as usize]
                    .map(|nb| nb.armed_square && nb.kind == cell.kind)
                    .unwrap_or(false)
            };

            if !same_at(c as i32, r as i32 - 1) {
                ctx.draw_line(x0, y0, x1, y0, w_outer, outer_col);
                ctx.draw_line(x0, y0, x1, y0, w_inner, inner_col);
            }
            if !same_at(c as i32, r as i32 + 1) {
                ctx.draw_line(x0, y1, x1, y1, w_outer, outer_col);
                ctx.draw_line(x0, y1, x1, y1, w_inner, inner_col);
            }
            if !same_at(c as i32 - 1, r as i32) {
                ctx.draw_line(x0, y0, x0, y1, w_outer, outer_col);
                ctx.draw_line(x0, y0, x0, y1, w_inner, inner_col);
            }
            if !same_at(c as i32 + 1, r as i32) {
                ctx.draw_line(x1, y0, x1, y1, w_outer, outer_col);
                ctx.draw_line(x1, y0, x1, y1, w_inner, inner_col);
            }
        }
    }

    // 2. Full `12/12` floor row outer highlight frame
    let floor_r = GRID_ROWS - 1;
    let floor_armed = (0..GRID_COLS).all(|c| {
        game.grid[floor_r][c]
            .map(|cell| cell.armed_row)
            .unwrap_or(false)
    });
    if floor_armed {
        let rx = lay.grid_x + 1.0;
        let ry = lay.grid_y + (floor_r as f32) * tile + 1.0;
        let rw = lay.grid_w - 2.0;
        let rh = tile - 2.0;
        ctx.stroke_rect(rx, ry, rw, rh, 4.0, w_outer, outer_col);
        ctx.stroke_rect(rx + 2.0, ry + 2.0, rw - 4.0, rh - 4.0, 3.0, w_inner, inner_col);
    }
}

fn draw_crane(
    ctx: &HostContext<'_>,
    t: PluginTheme,
    lay: &ComputedLayout,
    crane: &OverheadCrane,
    beat_frac: f32,
) {
    let tile = lay.tile;
    let rail_y = lay.shaft_y + tile * SPEC.rail_top_ratio;
    let cx = lay.grid_x + (crane.vis_col + 0.5) * tile;

    // Trolley carriage block with dual Siemens-style sensor eyes
    let head_w = tile * 0.58;
    let head_h = tile * 0.34;
    let head_x = cx - head_w * 0.5;
    let head_y = rail_y - 2.0;
    let flash_telegraph = crane.mode == CraneMode::Telegraph && beat_frac < 0.5;

    ctx.fill_rect(
        head_x,
        head_y,
        head_w,
        head_h,
        3.0,
        if flash_telegraph { t.bg } else { t.fg },
    );
    ctx.stroke_rect(head_x, head_y, head_w, head_h, 3.0, 2.5, t.fg);
    let eye_col = if flash_telegraph { t.fg } else { t.bg };
    let eye_s = (tile * 0.08).max(3.0);
    ctx.fill_rect(cx - head_w * 0.24, head_y + head_h * 0.34, eye_s, eye_s, 1.0, eye_col);
    ctx.fill_rect(
        cx + head_w * 0.24 - eye_s,
        head_y + head_h * 0.34,
        eye_s,
        eye_s,
        1.0,
        eye_col,
    );

    // Piston stem & mechanical two-pronged pincers
    let stem_top = head_y + head_h;
    let stem_bot = stem_top + tile * 0.16;
    ctx.draw_line(cx, stem_top, cx, stem_bot, 3.5, t.fg);

    let spread = if crane.cargo.is_some() {
        if crane.mode == CraneMode::Telegraph && flash_telegraph {
            tile * 0.54
        } else {
            tile * 0.48
        }
    } else {
        tile * (0.56 + crane.open_flash * 0.12)
    };
    let jaw_bot = stem_bot + tile * 0.44;
    ctx.draw_line(cx - spread, stem_bot, cx + spread, stem_bot, 3.0, t.fg);
    ctx.draw_line(cx - spread, stem_bot, cx - spread, jaw_bot, 3.0, t.fg);
    ctx.draw_line(cx + spread, stem_bot, cx + spread, jaw_bot, 3.0, t.fg);
    ctx.draw_line(
        cx - spread,
        jaw_bot,
        cx - spread + tile * 0.10,
        jaw_bot + tile * 0.08,
        3.0,
        t.fg,
    );
    ctx.draw_line(
        cx + spread,
        jaw_bot,
        cx + spread - tile * 0.10,
        jaw_bot + tile * 0.08,
        3.0,
        t.fg,
    );

    if let Some(kind) = crane.cargo {
        let crate_y = stem_bot + tile * 0.04;
        draw_crate(
            ctx,
            t,
            cx - tile * 0.5,
            crate_y,
            tile,
            kind,
            false,
            flash_telegraph,
            beat_frac,
        );
        if crane.mode == CraneMode::Telegraph {
            let ay = crate_y + tile + 3.0;
            let aw = tile * 0.22;
            let col = if flash_telegraph { t.fg } else { t.fg2 };
            ctx.draw_line(cx - aw, ay, cx, ay + aw * 0.65, 3.0, col);
            ctx.draw_line(cx + aw, ay, cx, ay + aw * 0.65, 3.0, col);
            let guide_x = lay.grid_x + (crane.col as f32 + 0.5) * tile;
            ctx.draw_line(
                guide_x,
                lay.grid_y + 2.0,
                guide_x,
                lay.grid_y + tile * 0.35,
                2.0,
                t.fg1,
            );
        }
    }
}

fn draw_worker(
    ctx: &HostContext<'_>,
    t: PluginTheme,
    lay: &ComputedLayout,
    game: &BeatStackGame,
    beat_frac: f32,
) {
    let tile = lay.tile;
    let (vx, vy, airborne, push_dir) = game.worker_visual_pos();
    let dir = game.worker.facing as f32;
    let x = lay.grid_x + vx * tile;
    let y = lay.grid_y + vy * tile;
    let cx = x + tile * 0.5 + (push_dir as f32) * tile * 0.08;

    if !airborne {
        ctx.fill_rect(
            cx - tile * 0.28,
            y + tile - 4.0,
            tile * 0.56,
            4.0,
            2.0,
            t.fg2,
        );
    }

    let leg_y = y + tile * 0.72;
    let leg_h = if airborne { tile * 0.18 } else { tile * 0.25 };
    let leg_w = (tile * 0.15).max(4.0);
    let stride = if matches!(game.worker.action, WorkerAction::Walking { .. })
        || game.worker.walk_phase % 2 == 1
    {
        tile * 0.06
    } else {
        0.0
    };
    ctx.fill_rect(cx - tile * 0.20 - stride, leg_y, leg_w, leg_h, 2.0, t.fg);
    ctx.fill_rect(cx + tile * 0.06 + stride, leg_y, leg_w, leg_h, 2.0, t.fg);

    let body_w = tile * 0.44;
    let body_h = tile * 0.36;
    let body_x = cx - body_w * 0.5;
    let body_y = y + tile * 0.40;
    ctx.fill_rect(body_x, body_y, body_w, body_h, 3.0, t.fg);
    let strap_w = (tile * 0.08).max(2.5);
    ctx.fill_rect(
        body_x + tile * 0.07,
        body_y + 3.0,
        strap_w,
        body_h * 0.52,
        1.0,
        t.bg,
    );
    ctx.fill_rect(
        body_x + body_w - tile * 0.07 - strap_w,
        body_y + 3.0,
        strap_w,
        body_h * 0.52,
        1.0,
        t.bg,
    );

    if push_dir != 0 {
        let arm_x = if push_dir > 0 {
            body_x + body_w - 2.0
        } else {
            body_x - tile * 0.20
        };
        ctx.fill_rect(
            arm_x,
            body_y + tile * 0.05,
            tile * 0.22,
            (tile * 0.11).max(3.5),
            2.0,
            t.fg,
        );
    }

    let head_w = tile * 0.36;
    let head_h = tile * 0.24;
    let head_x = cx - head_w * 0.5;
    let head_y = y + tile * 0.18;
    ctx.fill_rect(head_x, head_y, head_w, head_h, 2.0, t.bg);
    ctx.stroke_rect(head_x, head_y, head_w, head_h, 2.0, 2.2, t.fg);
    let eye_x = cx + dir * tile * 0.06;
    ctx.fill_rect(eye_x - 1.5, head_y + head_h * 0.32, 3.5, 3.5, 1.0, t.fg);
    let nose_x = if dir > 0.0 {
        head_x + head_w - 1.0
    } else {
        head_x - tile * 0.09
    };
    ctx.fill_rect(
        nose_x,
        head_y + head_h * 0.38,
        tile * 0.10,
        (tile * 0.08).max(2.5),
        1.0,
        t.fg,
    );

    let hat_w = tile * 0.44;
    let hat_h = tile * 0.16;
    let hat_x = cx - hat_w * 0.5;
    let hat_y = y + tile * 0.05;
    ctx.fill_rect(hat_x, hat_y, hat_w, hat_h, 3.0, t.fg2);
    ctx.stroke_rect(hat_x, hat_y, hat_w, hat_h, 3.0, 2.2, t.fg);
    let brim_x = if dir > 0.0 {
        hat_x
    } else {
        hat_x - tile * 0.10
    };
    ctx.fill_rect(
        brim_x,
        hat_y + hat_h - 3.0,
        hat_w + tile * 0.10,
        3.5,
        1.0,
        t.fg,
    );

    if game.worker.danger_above && beat_frac < 0.72 {
        let sy = hat_y - tile * 0.16;
        ctx.draw_line(cx, sy - 6.0, cx, sy + 1.0, 3.0, t.fg);
        ctx.fill_circle(cx, sy + 5.0, 2.0, t.fg);
        ctx.draw_line(
            cx - tile * 0.22,
            hat_y + 2.0,
            cx - tile * 0.30,
            hat_y - 4.0,
            2.2,
            t.fg2,
        );
        ctx.draw_line(
            cx + tile * 0.22,
            hat_y + 2.0,
            cx + tile * 0.30,
            hat_y - 4.0,
            2.2,
            t.fg2,
        );
    }
}

fn draw_game_frame(
    ctx: &HostContext<'_>,
    game: &BeatStackGame,
    track_badge: &str,
    music_playing: bool,
) {
    let t = ctx.theme();
    let title = ctx.tr("Beat Stack", "Біт Стек");
    let (win_w, win_h) = ctx.begin_responsive_frame(t.bg, true, title);
    let lay = ComputedLayout::compute(win_w, win_h);
    let tile = lay.tile;
    let beat_frac = game.last_beat.beat_frac;

    // 1. Clean Symmetric Outer Warehouse Shaft Frame (12 × 7 crate room + crane rail + floor)
    ctx.stroke_rect(
        lay.shaft_x,
        lay.shaft_y,
        lay.shaft_w,
        lay.shaft_h,
        SPEC.frame_radius,
        SPEC.wall_stroke,
        t.fg,
    );

    // 2. Overhead Gantry Rail + Strong-Beat Rhythm Tick Markers
    let rail_y = lay.shaft_y + tile * SPEC.rail_top_ratio;
    let rail_h = tile * SPEC.rail_h_ratio;
    ctx.fill_rect(lay.grid_x, rail_y, lay.grid_w, rail_h, 0.0, t.fg2);
    ctx.draw_line(
        lay.grid_x,
        rail_y,
        lay.grid_x + lay.grid_w,
        rail_y,
        2.5,
        t.fg,
    );
    ctx.draw_line(
        lay.grid_x,
        rail_y + rail_h,
        lay.grid_x + lay.grid_w,
        rail_y + rail_h,
        2.5,
        t.fg,
    );

    // Subtle rail tick markers pulsing on Strong Beats (`Beats 1 & 3`)
    let strong_hi = game.strong_pulse > 0.35;
    for c in 0..GRID_COLS {
        let tx = lay.grid_x + (c as f32 + 0.5) * tile;
        let tick_pad = if strong_hi { 1.0 } else { rail_h * 0.24 };
        ctx.draw_line(
            tx,
            rail_y + tick_pad,
            tx,
            rail_y + rail_h - tick_pad,
            if strong_hi { 2.5 } else { 1.5 },
            if strong_hi { t.bg } else { t.fg1 },
        );
    }

    // Subtle column guides & ceiling danger line (row 0 top)
    ctx.draw_line(
        lay.grid_x + 2.0,
        lay.grid_y,
        lay.grid_x + lay.grid_w - 2.0,
        lay.grid_y,
        if strong_hi { 2.2 } else { 1.5 },
        if strong_hi { t.fg2 } else { t.fg1 },
    );
    for c in 1..GRID_COLS {
        let gx = lay.grid_x + (c as f32) * tile;
        ctx.draw_line(
            gx,
            lay.grid_y + 4.0,
            gx,
            lay.grid_y + lay.grid_h - 4.0,
            1.0,
            t.fg1,
        );
    }

    // 3. Industrial Conveyor / Roller Floor Plate below Row 6
    let floor_y = lay.grid_y + lay.grid_h;
    ctx.fill_rect(
        lay.grid_x,
        floor_y,
        lay.grid_w,
        lay.floor_plate_h,
        0.0,
        t.fg2,
    );
    ctx.draw_line(
        lay.grid_x,
        floor_y,
        lay.grid_x + lay.grid_w,
        floor_y,
        SPEC.wall_stroke,
        t.fg,
    );
    let roller_r = (lay.floor_plate_h * 0.28).clamp(2.5, 5.5);
    for c in 0..GRID_COLS {
        let rcx = lay.grid_x + (c as f32 + 0.5) * tile;
        ctx.fill_circle(rcx, floor_y + lay.floor_plate_h * 0.54, roller_r, t.bg);
    }

    // 4. Grounded Crates (`12 × 7` active grid) + Pulsing Armed Group Frames
    for r in 0..GRID_ROWS {
        for c in 0..GRID_COLS {
            if let Some(cell) = game.grid[r][c] {
                let x = lay.grid_x + (c as f32) * tile;
                let settle_off = (cell.settle_px_frac as f32 * 0.01) * tile * 0.65;
                let y = lay.grid_y + (r as f32) * tile - settle_off;
                draw_crate(
                    ctx,
                    t,
                    x,
                    y,
                    tile,
                    cell.kind,
                    cell.is_armed(),
                    false,
                    beat_frac,
                );
            }
        }
    }
    draw_armed_group_highlights(ctx, t, &lay, game, beat_frac);

    // 5. Beat-Quantized Falling Crates
    for fc in &game.falling {
        let x = lay.grid_x + (fc.col as f32) * tile;
        let y = lay.grid_y + ((fc.row as f32) + fc.snap_anim * 0.45) * tile;
        draw_crate(ctx, t, x, y, tile, fc.kind, false, false, beat_frac);
    }

    // 6. Overhead Robotic Cranes
    for crane in &game.cranes {
        if crane.active {
            draw_crane(ctx, t, &lay, crane, beat_frac);
        }
    }

    // 7. Warehouse Worker
    draw_worker(ctx, t, &lay, game, beat_frac);

    // 8. Detonation Shatter Burst Particles & Floating Combo Callout
    for p in &game.particles {
        let px = lay.grid_x + p.x * tile;
        let py = lay.grid_y + p.y * tile;
        let sz = (p.life * tile * 0.17).max(2.5);
        let col = if p.shade >= 3 { t.fg } else { t.fg2 };
        ctx.fill_rect(px - sz * 0.5, py - sz * 0.5, sz, sz, 1.5, col);
    }
    if let Some(ref co) = game.callout {
        let cx = lay.grid_x + co.col_center * tile;
        let cy = lay.grid_y + co.row_y * tile;
        let tw = ctx.measure_text(&co.text, SPEC.font_callout, FontWeight::Bold) + 20.0;
        ctx.fill_rect(cx - tw * 0.5, cy - 16.0, tw, 30.0, 7.0, t.fg);
        ctx.draw_text(
            cx,
            cy - 1.0,
            &co.text,
            SPEC.font_callout,
            FontWeight::Bold,
            TextAlign::Center,
            t.bg,
        );
    }

    // 9. Concise 2-Line Onboarding Rules Hint Banner (fades after first clear or toggle via X / Pause)
    if game.show_hint && game.lose_reason.is_none() {
        let hw = (lay.grid_w - 20.0).max(260.0);
        let hx = lay.grid_x + (lay.grid_w - hw) * 0.5;
        let hy = lay.grid_y + 6.0;
        ctx.fill_rect(hx, hy, hw, SPEC.hint_card_h, SPEC.hint_radius, t.bg);
        ctx.stroke_rect(
            hx,
            hy,
            hw,
            SPEC.hint_card_h,
            SPEC.hint_radius,
            SPEC.thin_stroke,
            t.fg2,
        );
        ctx.draw_text(
            hx + hw * 0.5,
            hy + 16.0,
            ctx.tr(
                "Push crates into 2×2 matching squares or a full floor row — matches explode on the strong beat!",
                "Складай ящики у квадрати 2×2 або повний ряд підлоги — вибух на сильну долю!",
            ),
            SPEC.font_hint_main,
            FontWeight::SemiBold,
            TextAlign::Center,
            t.fg,
        );
        ctx.draw_text(
            hx + hw * 0.5,
            hy + 35.0,
            ctx.tr(
                "B: Jump · L1/R1: Diag Jump · START: Pause",
                "B: Стрибок · L1/R1: Діаг. стрибок · START: Пауза",
            ),
            SPEC.font_hint_sub,
            FontWeight::Medium,
            TextAlign::Center,
            t.fg2,
        );
    }

    // 10. Compact Floor HUD Strip (`SCORE`, `HIGH`, `CHAIN ×N`, `BPM` Strong-Beat Pulse)
    ctx.fill_rect(
        lay.hud_x,
        lay.hud_y,
        lay.hud_w,
        lay.hud_h,
        SPEC.hud_radius,
        t.bg,
    );
    ctx.stroke_rect(
        lay.hud_x,
        lay.hud_y,
        lay.hud_w,
        lay.hud_h,
        SPEC.hud_radius,
        if strong_hi { SPEC.thin_stroke + 0.8 } else { SPEC.thin_stroke },
        if strong_hi { t.fg } else { t.fg2 },
    );

    let col_w = lay.hud_w / 4.0;
    let lbl_y = lay.hud_y + 16.0;
    let val_y = lay.hud_y + 39.0;

    // Slot 1: SCORE
    let c0 = lay.hud_x + col_w * 0.5;
    ctx.draw_text(
        c0,
        lbl_y,
        ctx.tr("SCORE", "РАХУНОК"),
        SPEC.font_hud_lbl,
        FontWeight::SemiBold,
        TextAlign::Center,
        t.fg2,
    );
    ctx.draw_text(
        c0,
        val_y,
        &game.score.to_string(),
        SPEC.font_hud_val,
        FontWeight::Bold,
        TextAlign::Center,
        t.fg,
    );

    // Slot 2: HIGH
    let c1 = lay.hud_x + col_w * 1.5;
    ctx.draw_text(
        c1,
        lbl_y,
        ctx.tr("HIGH", "РЕКОРД"),
        SPEC.font_hud_lbl,
        FontWeight::SemiBold,
        TextAlign::Center,
        t.fg2,
    );
    ctx.draw_text(
        c1,
        val_y,
        &game.prefs.hi_score.max(game.score).to_string(),
        SPEC.font_hud_val,
        FontWeight::Bold,
        TextAlign::Center,
        t.fg,
    );

    // Slot 3: CHAIN ×N & WAVE
    let c2 = lay.hud_x + col_w * 2.5;
    let wave_lbl = format!("{} {} · CHAIN", ctx.tr("W", "ХВ"), game.wave);
    ctx.draw_text(
        c2,
        lbl_y,
        &wave_lbl,
        SPEC.font_hud_lbl,
        FontWeight::SemiBold,
        TextAlign::Center,
        t.fg2,
    );
    let chain_str = format!("×{}", game.chain);
    ctx.draw_text(
        c2,
        val_y,
        &chain_str,
        SPEC.font_hud_val,
        FontWeight::Bold,
        TextAlign::Center,
        t.fg,
    );

    // Slot 4: Live BPM + Strong-Beat Pulse Pip + Track / Clock Status
    let c3 = lay.hud_x + col_w * 3.5;
    let src_lbl = if music_playing && !track_badge.is_empty() {
        track_badge
    } else if music_playing {
        ctx.tr("LIVE AUDIO", "ЖИВИЙ ЗВУК")
    } else {
        ctx.tr("INTERNAL CLOCK", "ВНУТР. РИТМ")
    };
    ctx.draw_text(
        c3,
        lbl_y,
        src_lbl,
        SPEC.font_hud_lbl,
        FontWeight::SemiBold,
        TextAlign::Center,
        t.fg2,
    );
    let bpm_str = format!("{:.0} BPM", game.last_beat.effective_bpm.round());
    let bpm_w = ctx.measure_text(&bpm_str, SPEC.font_hud_val, FontWeight::Bold);
    let pip_r = 4.5 + game.bpm_pulse * 2.0 + game.strong_pulse * 2.8;
    ctx.fill_circle(
        c3 - bpm_w * 0.5 - 12.0,
        val_y,
        pip_r,
        if game.strong_pulse > 0.35 || game.bpm_pulse > 0.55 {
            t.fg
        } else {
            t.fg2
        },
    );
    ctx.draw_text(
        c3 + 4.0,
        val_y,
        &bpm_str,
        SPEC.font_hud_val + game.strong_pulse * 1.5,
        FontWeight::Bold,
        TextAlign::Center,
        t.fg,
    );

    // Dividers between HUD slots
    for div in 1..4 {
        let dx = lay.hud_x + col_w * (div as f32);
        ctx.draw_line(dx, lay.hud_y + 10.0, dx, lay.hud_y + lay.hud_h - 10.0, 1.5, t.fg1);
    }

    // 11. Game Over Banner Overlay
    if let Some(reason) = game.lose_reason {
        let card_w = (lay.grid_w * 0.76).clamp(320.0, 520.0);
        let card_h = 168.0;
        let card_x = lay.grid_x + (lay.grid_w - card_w) * 0.5;
        let card_y = lay.grid_y + (lay.grid_h - card_h) * 0.5;

        ctx.fill_rect(card_x, card_y, card_w, card_h, 14.0, t.bg);
        ctx.stroke_rect(card_x, card_y, card_w, card_h, 14.0, 4.0, t.fg);

        let headline = match reason {
            LoseReason::Crushed => ctx.tr("CRUSHED!", "ПРИДАВИЛО!"),
            LoseReason::ShaftJammed => ctx.tr("SHAFT JAMMED!", "ШАХТУ ЗАБЛОКОВАНО!"),
        };
        ctx.draw_text(
            card_x + card_w * 0.5,
            card_y + 44.0,
            headline,
            SPEC.font_banner_lg,
            FontWeight::Bold,
            TextAlign::Center,
            t.fg,
        );

        let stats = format!(
            "{}: {}   ·   {}: ×{}",
            ctx.tr("Score", "Рахунок"),
            game.score,
            ctx.tr("Max Chain", "Макс. ланцюг"),
            game.max_chain
        );
        ctx.draw_text(
            card_x + card_w * 0.5,
            card_y + 92.0,
            &stats,
            SPEC.font_banner_sm,
            FontWeight::SemiBold,
            TextAlign::Center,
            t.fg2,
        );

        ctx.draw_text(
            card_x + card_w * 0.5,
            card_y + 134.0,
            ctx.tr(
                "A: Play Again  ·  START: Pause / Quit",
                "A: Ще раз  ·  START: Пауза / Вихід",
            ),
            15.0,
            FontWeight::Medium,
            TextAlign::Center,
            t.fg,
        );
    }

    ctx.present();
}

fn truncate_badge(ctx: &HostContext<'_>, raw: &str, max_px: f32) -> String {
    if ctx.measure_text(raw, SPEC.font_hud_lbl, FontWeight::SemiBold) <= max_px {
        return raw.to_string();
    }
    let mut out = String::new();
    for ch in raw.chars() {
        let mut probe = out.clone();
        probe.push(ch);
        probe.push('…');
        if ctx.measure_text(&probe, SPEC.font_hud_lbl, FontWeight::SemiBold) > max_px {
            break;
        }
        out.push(ch);
    }
    out.push('…');
    out
}

// ============================================================================
// Standard In-Game Pause Menu (`START` / `MENU`)
// ============================================================================

fn run_pause_menu(
    ctx: &HostContext<'_>,
    game: &mut BeatStackGame,
    clock: &mut BeatClock,
) -> bool {
    let mut sel = 0usize;
    loop {
        let music_playing = ctx.audio_status() == 1;
        let music_lbl = if music_playing {
            ctx.tr("Pause Music", "Пауза музики")
        } else {
            ctx.tr("Play Music", "Увімкнути музику")
        };
        let diff_lbl = match game.prefs.difficulty {
            Difficulty::Shift => ctx.tr("Chill", "Легка"),
            Difficulty::Foreman => ctx.tr("Normal", "Стандарт"),
            Difficulty::Overtime => ctx.tr("Overtime", "Хардкор"),
        };
        let sync_lbl = format!("+{} ms", SYNC_OFFSETS_MS[game.prefs.latency_idx]);
        let hint_lbl = if game.show_hint {
            ctx.tr("On", "Увімк.")
        } else {
            ctx.tr("Off", "Вимк.")
        };

        let items = [
            MenuItem::action(ctx.tr("Resume", "Продовжити")),
            MenuItem::action(ctx.tr("Restart", "Почати знову")),
            MenuItem::action(music_lbl),
            MenuItem::option(ctx.tr("Difficulty", "Складність"), diff_lbl),
            MenuItem::option(ctx.tr("Audio Sync", "Синхронізація"), &sync_lbl),
            MenuItem::option(ctx.tr("Rules Hint", "Підказка"), hint_lbl),
            MenuItem::action(ctx.tr("Quit", "Вихід")),
        ];

        let menu_title = ctx.tr("Beat Stack", "Біт Стек");
        match ctx.show_menu(menu_title, &items, sel) {
            MenuOutcome::Cancelled => return false,
            MenuOutcome::ForceExit => return true,
            MenuOutcome::Selected(idx) => {
                sel = idx;
                match idx {
                    0 => return false,
                    1 => {
                        let seed = ctx.ticks_ms() ^ 0xB3A7_57AC;
                        game.restart(seed);
                        clock.reset_full();
                        return false;
                    }
                    2 => {
                        if ctx.audio_status() == 0 {
                            let cnt = ctx.library_track_count();
                            if cnt > 0 {
                                let pick = (ctx.ticks_ms() as usize) % cnt;
                                if let Some(lt) = ctx.library_track_at(pick) {
                                    let _ = ctx.audio_play_track(&lt.path, 0);
                                }
                            }
                        } else {
                            ctx.audio_toggle_play_pause();
                        }
                    }
                    3 => {
                        game.prefs.difficulty = match game.prefs.difficulty {
                            Difficulty::Shift => Difficulty::Foreman,
                            Difficulty::Foreman => Difficulty::Overtime,
                            Difficulty::Overtime => Difficulty::Shift,
                        };
                        game.prefs.save(ctx);
                    }
                    4 => {
                        game.prefs.latency_idx =
                            (game.prefs.latency_idx + 1) % SYNC_OFFSETS_MS.len();
                        clock.set_audio_latency_sec(game.prefs.latency_sec());
                        game.prefs.save(ctx);
                    }
                    5 => {
                        game.show_hint = !game.show_hint;
                        game.hint_pinned = game.show_hint;
                    }
                    _ => return true,
                }
            }
        }
    }
}

// ============================================================================
// Plugin Entry Point
// ============================================================================

fn run_beat_stack(ctx: &mut HostContext<'_>, _file_path: Option<&Path>) -> i32 {
    let prefs = SavedPrefs::load(ctx);
    let mut clock = BeatClock::new(prefs.latency_sec());
    let mut pcm_window = [0i16; 1024 * 2];

    let mut game = BeatStackGame::new(prefs, ctx.ticks_ms() ^ 0x57AC_CA7E);

    let mut last_frame_ms = ctx.ticks_ms();
    let mut last_track_poll_ms = 0u64;
    let mut current_track_path = String::new();
    let mut track_badge = String::new();
    let mut prev_saved_hi = game.prefs.hi_score;

    loop {
        let now_ms = ctx.ticks_ms();
        let raw_dt = ((now_ms.saturating_sub(last_frame_ms)) as f32) * 0.001;
        let dt_sec = raw_dt.clamp(1.0 / 240.0, 0.05);
        last_frame_ms = now_ms;

        let inp = ctx.poll_input(0);
        if inp.should_exit {
            game.prefs.save(ctx);
            return 0;
        }

        // `START` / `MENU` opens Pause Menu (`B` & `Hold B` NEVER exit gameplay!)
        if inp.menu_requested {
            if run_pause_menu(ctx, &mut game, &mut clock) {
                game.prefs.save(ctx);
                return 0;
            }
            last_frame_ms = ctx.ticks_ms();
            continue;
        }

        // L2 / R2: Skip to Prev / Next track without leaving the game + show toast
        if inp.pressed(btn::L2) {
            ctx.audio_prev();
            last_track_poll_ms = 0;
        } else if inp.pressed(btn::R2) {
            ctx.audio_next();
            last_track_poll_ms = 0;
        }

        // Quick restart with A on Game Over
        if game.lose_reason.is_some() && inp.pressed(btn::A) {
            game.restart(now_ms ^ 0x9E37_79B9);
            clock.reset_full();
        }

        // Poll active background track metadata ~4x per second
        let astat = ctx.audio_status();
        let music_playing = astat == 1;
        let (win_w, win_h) = ctx.responsive_size();
        let lay = ComputedLayout::compute(win_w, win_h);

        if now_ms.saturating_sub(last_track_poll_ms) >= 250 {
            let had_prev_track = !current_track_path.is_empty();
            last_track_poll_ms = now_ms;
            if let Some(info) = ctx.audio_track_info() {
                if info.path != current_track_path {
                    current_track_path = info.path.clone();
                    clock.on_track_change();
                    let raw_title = if !info.title.is_empty() {
                        info.title
                    } else {
                        info.path
                            .rsplit('/')
                            .next()
                            .unwrap_or("Track")
                            .to_string()
                    };
                    track_badge = truncate_badge(ctx, &raw_title, (lay.hud_w * 0.25) - 16.0);
                    if had_prev_track {
                        let toast = format!("♪ {}", raw_title);
                        ctx.show_splash(&toast, 1400);
                    }
                }
            } else {
                current_track_path.clear();
                track_badge.clear();
            }
        }

        let (n_frames, sample_rate, apu_pulse, intel_sync) = if music_playing {
            let live_opt = ctx.audio_live_telemetry();
            let rec_opt = ctx.audio_track_analysis();
            if let (Some(live), Some(rec)) = (live_opt, rec_opt) {
                if live.status == 1 && live.has_analysis != 0 && live.track_key_hash == rec.key_hash
                {
                    let apu_hit = if live.apu.is_available() {
                        live.apu.percussion_trigger().max(live.apu.bass_trigger() * 0.85)
                    } else {
                        0.0
                    };
                    (
                        0usize,
                        live.sample_rate.max(DEFAULT_PCM_SR),
                        apu_hit,
                        Some((
                            rec.bpm,
                            rec.first_beat_offset_ms,
                            live.playhead_ms,
                            live.kick_impulse.max(apu_hit),
                        )),
                    )
                } else {
                    let nf = ctx.audio_read_pcm(0, &mut pcm_window);
                    let apu = ctx.audio_apu_telemetry();
                    let apu_hit = if apu.is_available() {
                        apu.percussion_trigger().max(apu.bass_trigger() * 0.85)
                    } else {
                        0.0
                    };
                    (nf, DEFAULT_PCM_SR, apu_hit, None)
                }
            } else {
                let nf = ctx.audio_read_pcm(0, &mut pcm_window);
                let apu = ctx.audio_apu_telemetry();
                let apu_hit = if apu.is_available() {
                    apu.percussion_trigger().max(apu.bass_trigger() * 0.85)
                } else {
                    0.0
                };
                (nf, DEFAULT_PCM_SR, apu_hit, None)
            }
        } else {
            (0usize, DEFAULT_PCM_SR, 0.0, None)
        };

        let mut move_dir = 0;
        if inp.held(btn::LEFT) {
            move_dir -= 1;
        }
        if inp.held(btn::RIGHT) {
            move_dir += 1;
        }

        let frame_inp = FrameInput {
            move_dir,
            press_left: inp.pressed(btn::LEFT),
            press_right: inp.pressed(btn::RIGHT),
            jump_b: inp.pressed(btn::B),
            held_b: inp.held(btn::B),
            diag_left: inp.pressed(btn::L),
            diag_right: inp.pressed(btn::R),
            toggle_hint: inp.pressed(btn::X),
        };

        let pcm_slice = &pcm_window[..n_frames.min(1024) * 2];
        game.step(
            &mut clock,
            BeatFrameInput {
                dt_sec,
                music_playing,
                pcm_stereo: pcm_slice,
                sample_rate,
                apu_pulse,
                intel_sync,
            },
            frame_inp,
            ctx.is_ukrainian(),
        );

        if game.prefs.hi_score > prev_saved_hi {
            prev_saved_hi = game.prefs.hi_score;
            game.prefs.save(ctx);
        }

        draw_game_frame(ctx, &game, &track_badge, music_playing);

        let spent_ms = ctx.ticks_ms().saturating_sub(now_ms);
        if spent_ms < 16 {
            ctx.sleep_ms((16 - spent_ms) as u32);
        } else {
            ctx.sleep_ms(0);
        }
    }
}

uipod_declare_plugin! {
    id: "beat_stack",
    name: "Beat Stack",
    name_uk: "Біт Стек",
    author: "UIPod",
    version: "0.2.0",
    category: PluginCategory::Game,
    kind: PluginKind::MenuDriven,
    extensions: "",
    entry: run_beat_stack,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn empty_board_game() -> BeatStackGame {
        let mut g = BeatStackGame::new(SavedPrefs::default(), 42);
        g.grid = [[None; GRID_COLS]; GRID_ROWS];
        g.falling.clear();
        for cr in &mut g.cranes {
            cr.active = false;
        }
        g.worker.col = 4;
        g.worker.row = (GRID_ROWS - 1) as i32;
        g.worker.action = WorkerAction::Idle;
        g
    }

    fn silent_beat_in(dt_sec: f32) -> BeatFrameInput<'static> {
        BeatFrameInput {
            dt_sec,
            music_playing: false,
            pcm_stereo: &[],
            sample_rate: 44_100,
            apu_pulse: 0.0,
            intel_sync: None,
        }
    }

    #[test]
    fn responsive_layout_is_symmetric_without_left_girder_protrusion() {
        for (w, h) in [
            (1024.0, 768.0), // 4:3 TrimUI Brick
            (920.0, 920.0),  // 1:1 Square handheld
            (1152.0, 768.0), // 3:2 GBA/Handheld
            (1365.0, 768.0), // 16:9 Android handheld
        ] {
            let lay = ComputedLayout::compute(w, h);
            assert!(lay.tile >= SPEC.min_tile && lay.tile <= SPEC.max_tile);
            assert!(lay.shaft_x >= SPEC.outer_pad_x - 1.0);
            assert!(lay.shaft_x + lay.shaft_w <= w - SPEC.outer_pad_x + 2.0);
            assert!(lay.hud_y + lay.hud_h <= h);
            let left_margin = lay.grid_x - lay.shaft_x;
            let right_margin = (lay.shaft_x + lay.shaft_w) - (lay.grid_x + lay.grid_w);
            assert!((left_margin - right_margin).abs() < 0.01);
            assert_eq!((lay.grid_w / lay.tile).round() as usize, GRID_COLS);
            assert_eq!((lay.grid_h / lay.tile).round() as usize, GRID_ROWS);
        }
    }

    #[test]
    fn worker_pushes_single_crate_blocks_two_high_stack_and_drops_crate_off_ledge() {
        let mut g = empty_board_game();
        let floor = GRID_ROWS - 1;

        g.grid[floor][5] = Some(GroundedCrate::new(CrateKind::Heavy));
        assert!(g.try_walk_or_push(1));
        assert_eq!(g.worker.col, 5);
        assert!(g.grid[floor][5].is_none());
        assert_eq!(g.grid[floor][6].map(|c| c.kind), Some(CrateKind::Heavy));

        // Cannot push a 2-high stack
        g.worker.action = WorkerAction::Idle;
        g.grid[floor - 1][6] = Some(GroundedCrate::new(CrateKind::Light));
        assert!(!g.try_walk_or_push(1));
        assert_eq!(g.worker.col, 5);

        // Push top crate off ledge so it falls to the floor
        g.grid[floor][5] = Some(GroundedCrate::new(CrateKind::Medium));
        g.worker.col = 5;
        g.worker.row = (floor - 1) as i32;
        g.worker.action = WorkerAction::Idle;
        assert!(g.try_walk_or_push(1));
        g.settle_all_grounded_now();
        assert!(g.grid[floor - 1][7].is_none());
        assert_eq!(g.grid[floor][7].map(|c| c.kind), Some(CrateKind::Light));
    }

    #[test]
    fn b_jumps_vertically_b_plus_dir_and_l1_r1_jump_diagonally_and_x_toggles_hint_only() {
        let mut g = empty_board_game();
        let floor = GRID_ROWS - 1;
        let mut clock = BeatClock::new(0.0);

        // X only toggles hint banner — NEVER triggers a jump!
        assert!(g.show_hint);
        g.step(
            &mut clock,
            silent_beat_in(1.0 / 60.0),
            FrameInput {
                toggle_hint: true,
                ..Default::default()
            },
            false,
        );
        assert!(!g.show_hint);
        assert_eq!(g.worker.action, WorkerAction::Idle);

        // B alone jumps vertically
        g.step(
            &mut clock,
            silent_beat_in(1.0 / 60.0),
            FrameInput {
                jump_b: true,
                held_b: true,
                ..Default::default()
            },
            false,
        );
        assert!(matches!(
            g.worker.action,
            WorkerAction::Jumping { to_col: 4, .. }
        ));
        for _ in 0..30 {
            g.step(&mut clock, silent_beat_in(1.0 / 60.0), FrameInput::default(), false);
        }
        assert_eq!(g.worker.row, floor as i32);
        assert_eq!(g.worker.action, WorkerAction::Idle);

        // B + Right climbs onto a 1-high crate at col 5
        g.grid[floor][5] = Some(GroundedCrate::new(CrateKind::Checker));
        g.step(
            &mut clock,
            silent_beat_in(1.0 / 60.0),
            FrameInput {
                move_dir: 1,
                jump_b: true,
                held_b: true,
                ..Default::default()
            },
            false,
        );
        for _ in 0..16 {
            g.step(&mut clock, silent_beat_in(1.0 / 60.0), FrameInput::default(), false);
        }
        assert_eq!(g.worker.col, 5);
        assert_eq!(g.worker.row, (floor - 1) as i32);

        // R1 against a 2-high sheer wall at col 6 bounces back cleanly
        g.grid[floor][6] = Some(GroundedCrate::new(CrateKind::Heavy));
        g.grid[floor - 1][6] = Some(GroundedCrate::new(CrateKind::Heavy));
        g.grid[floor - 2][6] = Some(GroundedCrate::new(CrateKind::Heavy));
        g.step(
            &mut clock,
            silent_beat_in(1.0 / 60.0),
            FrameInput {
                diag_right: true,
                ..Default::default()
            },
            false,
        );
        for _ in 0..16 {
            g.step(&mut clock, silent_beat_in(1.0 / 60.0), FrameInput::default(), false);
        }
        assert_eq!(g.worker.col, 5);
        assert_eq!(g.worker.row, (floor - 1) as i32);
    }

    #[test]
    fn strong_beat_detonates_2x2_and_mono_row_with_flash_guard_and_cascade_chain() {
        let mut g = empty_board_game();
        let floor = GRID_ROWS - 1;
        let mut clock = BeatClock::new(0.0);

        // Set up a 2x2 Light cluster at cols 1..=2, AND 2 Heavy crates on top of it + 2 Heavy
        // crates next to it so when Light explodes on Strong Beat #1, Heavy drops into a new 2x2
        // that explodes on Strong Beat #2 with `CHAIN ×2`!
        g.grid[floor][1] = Some(GroundedCrate::new(CrateKind::Light));
        g.grid[floor][2] = Some(GroundedCrate::new(CrateKind::Light));
        g.grid[floor - 1][1] = Some(GroundedCrate::new(CrateKind::Light));
        g.grid[floor - 1][2] = Some(GroundedCrate::new(CrateKind::Light));

        // Upper Heavy crates at cols 1..=2 that will drop 2 rows onto floor..floor-1?
        // Put 2 Heavy crates at `floor` + `floor - 1` on col 3, and 2 Heavy crates above Light at col 2!
        g.grid[floor][3] = Some(GroundedCrate::new(CrateKind::Heavy));
        g.grid[floor - 1][3] = Some(GroundedCrate::new(CrateKind::Heavy));
        g.grid[floor - 2][2] = Some(GroundedCrate::new(CrateKind::Heavy));
        g.grid[floor - 3][2] = Some(GroundedCrate::new(CrateKind::Heavy));
        g.worker.col = 9;
        g.recompute_armed();

        // Immediate strong-beat call BEFORE 0.5-beat flash guard must NOT detonate prematurely!
        assert_eq!(g.step_strong_beat_detonation(false), 0);

        // Advance simulation across 2 full strong beats (~2.2 seconds @ 120 BPM) without crane interference
        for cr in &mut g.cranes {
            cr.active = false;
        }
        for _ in 0..140 {
            g.cranes = [OverheadCrane {
                active: false,
                col: 0,
                vis_col: 0.0,
                dir: 1,
                target_col: 0,
                cargo: None,
                mode: CraneMode::EmptyReturn,
                telegraph_beats: 0.0,
                rail_steps_loaded: 0,
                open_flash: 0.0,
            }; MAX_CRANES];
            let dt = 1.0 / 60.0;
            let step = clock.update(silent_beat_in(dt));
            for r in 0..GRID_ROWS {
                for c in 0..GRID_COLS {
                    if let Some(ref mut cell) = g.grid[r][c] {
                        if cell.is_armed() {
                            cell.armed_sec += dt;
                            cell.armed_beats += step.step_beats;
                        }
                    }
                }
            }
            g.step_settled_gravity(dt);
            if step.fire_strong_beat {
                g.step_strong_beat_detonation(false);
            }
        }

        // Both the Light 2x2 AND the cascaded Heavy 2x2 must have detonated (`8` crates, `CHAIN ×2`!)
        assert_eq!(g.cleared_crates, 8);
        assert_eq!(g.max_chain, 2);
        assert!(!g.show_hint);

        // Now test 12/12 MONO ROW ×3 bonus!
        let prev_score = g.score;
        for c in 0..GRID_COLS {
            let mut cr = GroundedCrate::new(CrateKind::Medium);
            cr.armed_row = true;
            cr.armed_sec = 0.5;
            cr.armed_beats = 1.0;
            g.grid[floor][c] = Some(cr);
        }
        g.chain_active = false;
        g.chain = 1;
        let cleared = g.step_strong_beat_detonation(false);
        assert_eq!(cleared, 12);
        // 12 * 50 * 3 (MONO ROW ×3) = 1800 points!
        assert_eq!(g.score - prev_score, 1800);
    }

    #[test]
    fn cranes_continuously_drop_past_two_drops_without_ever_freezing() {
        let mut g = BeatStackGame::new(SavedPrefs::default(), 777);
        let mut clock = BeatClock::new(0.0);

        // Simulate 45 seconds (~90 beats @ 120 BPM) keeping worker safe and clearing rows so shaft never jams
        for _ in 0..(45 * 60) {
            // Keep bottom rows clear so we isolate pure multi-crane rail/drop/reload cycling
            for r in 2..GRID_ROWS {
                for c in 0..GRID_COLS {
                    g.grid[r][c] = None;
                }
            }
            g.falling.retain(|f| f.row < 2);
            g.worker.col = 0;
            g.worker.row = (GRID_ROWS - 1) as i32;
            g.step(&mut clock, silent_beat_in(1.0 / 60.0), FrameInput::default(), false);
            assert!(g.lose_reason.is_none());
        }

        assert!(
            g.drops_done >= 20,
            "expected continuous multi-crane drops >> 2, got drops_done={}",
            g.drops_done
        );
    }

    #[test]
    fn falling_crate_crushes_worker_and_ceiling_stack_jams_shaft() {
        let mut g = empty_board_game();
        let floor = GRID_ROWS - 1;
        g.worker.col = 3;
        g.worker.row = floor as i32;
        g.falling.push(FallingCrate {
            col: 3,
            row: floor - 1,
            kind: CrateKind::Heavy,
            snap_anim: 0.0,
        });
        g.step_falling_crates_on_beat();
        assert_eq!(g.lose_reason, Some(LoseReason::Crushed));

        // Shaft Jammed when crane releases onto a full column (`row == 0` occupied)
        let mut g2 = empty_board_game();
        for r in 0..GRID_ROWS {
            g2.grid[r][6] = Some(GroundedCrate::new(CrateKind::ALL[r % 4]));
        }
        g2.cranes[0] = OverheadCrane {
            active: true,
            col: 6,
            vis_col: 6.0,
            dir: 1,
            target_col: 6,
            cargo: Some(CrateKind::Light),
            mode: CraneMode::Telegraph,
            telegraph_beats: 1.0,
            rail_steps_loaded: 0,
            open_flash: 0.0,
        };
        g2.step_cranes(0.2, true, true);
        assert_eq!(g2.lose_reason, Some(LoseReason::ShaftJammed));
    }
}
