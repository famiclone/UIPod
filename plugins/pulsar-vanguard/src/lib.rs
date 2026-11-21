//! `Pulsar Vanguard` — Cosmic rhythm-synchronized Galaga arcade plugin for UIPod (`PluginKind::MenuDriven`).
//!
//! All weapons fire automatically on the musical grid (`1/1` Wave Cannon, `1/4` Beat Pulse with
//! downbeat twin-lance accents, and `1/8` Focus Vulcan while holding `A`), driven by a monotonic
//! frequency-steering [`BarPhaseTracker`] that never rewinds phase or double-triggers.

mod rhythm;

use core::f32::consts::{PI, TAU};
use std::path::Path;

use rhythm::{
    AudioFrameInput, BarPhaseConfig, BarPhaseTracker, BarTick, LivePcmAnalyzer, SyncSource,
    WeaponGear,
};
use uipod_plugin_sdk::{
    btn, uipod_declare_plugin, FontWeight, HostContext, MenuItem, MenuOutcome, PluginCategory,
    PluginKind, PluginTheme, TextAlign, CONTENT_TOP_Y,
};

const PLUGIN_ID: &str = "pulsar_vanguard";
const SAVE_FILE: &str = "vanguard_save.bin";
const SAVE_MAGIC: &[u8; 4] = b"PVG1";

// ============================================================================
// Declarative LayoutSpec (1:1 geometry & typography tokens; no magic numbers)
// ============================================================================

struct LayoutSpec {
    outer_pad_x: f32,
    outer_top_offset: f32,
    outer_bottom_pad: f32,
    panel_gap: f32,
    sidebar_w: f32,
    frame_radius: f32,
    card_radius: f32,
    border_stroke: f32,
    inner_stroke: f32,

    // Swarm & ship proportions inside ArenaFrame
    swarm_cols: usize,
    swarm_rows: usize,
    swarm_top_offset: f32,
    swarm_col_pitch: f32,
    swarm_row_pitch: f32,
    swarm_pulse_amp: f32,
    ship_bottom_margin: f32,
    ship_half_w: f32,
    ship_half_h: f32,
    wingman_offset_x: f32,
    nose_ring_max_r: f32,

    // Sidebar HUD rhythm & telemetry widgets
    sidebar_pad: f32,
    section_gap: f32,
    pip_h: f32,
    pip_gap: f32,
    gauge_h: f32,

    // Typography sizes (`Geologica`)
    font_banner: f32,
    font_value_lg: f32,
    font_value_md: f32,
    font_label: f32,
    font_caption: f32,
}

#[allow(non_snake_case)]
const SPEC: LayoutSpec = LayoutSpec {
    outer_pad_x: 24.0,
    outer_top_offset: 12.0,
    outer_bottom_pad: 20.0,
    panel_gap: 20.0,
    sidebar_w: 256.0,
    frame_radius: 14.0,
    card_radius: 10.0,
    border_stroke: 3.0,
    inner_stroke: 2.0,

    swarm_cols: 8,
    swarm_rows: 4,
    swarm_top_offset: 74.0,
    swarm_col_pitch: 66.0,
    swarm_row_pitch: 50.0,
    swarm_pulse_amp: 14.0,
    ship_bottom_margin: 50.0,
    ship_half_w: 18.0,
    ship_half_h: 16.0,
    wingman_offset_x: 32.0,
    nose_ring_max_r: 22.0,

    sidebar_pad: 16.0,
    section_gap: 14.0,
    pip_h: 14.0,
    pip_gap: 8.0,
    gauge_h: 12.0,

    font_banner: 28.0,
    font_value_lg: 26.0,
    font_value_md: 20.0,
    font_label: 15.0,
    font_caption: 14.0,
};

#[derive(Clone, Copy)]
struct ComputedLayout {
    arena_x: f32,
    arena_y: f32,
    arena_w: f32,
    arena_h: f32,
    sidebar_x: f32,
    sidebar_y: f32,
    sidebar_w: f32,
    sidebar_h: f32,
    ship_y: f32,
}

impl ComputedLayout {
    fn compute(win_w: f32, win_h: f32) -> Self {
        let top_y = CONTENT_TOP_Y + SPEC.outer_top_offset;
        let usable_h = (win_h - top_y - SPEC.outer_bottom_pad).max(240.0);
        let sidebar_w = SPEC.sidebar_w.min(win_w * 0.28);
        let arena_x = SPEC.outer_pad_x;
        let arena_w =
            (win_w - SPEC.outer_pad_x * 2.0 - SPEC.panel_gap - sidebar_w).max(320.0);
        let sidebar_x = arena_x + arena_w + SPEC.panel_gap;

        Self {
            arena_x,
            arena_y: top_y,
            arena_w,
            arena_h: usable_h,
            sidebar_x,
            sidebar_y: top_y,
            sidebar_w,
            sidebar_h: usable_h,
            ship_y: top_y + usable_h - SPEC.ship_bottom_margin,
        }
    }
}

// ============================================================================
// Game Configuration & Persistent Save Data
// ============================================================================

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Difficulty {
    Cadet = 0,
    Veteran = 1,
    Ace = 2,
}

impl Difficulty {
    fn from_u8(v: u8) -> Self {
        match v {
            0 => Self::Cadet,
            2 => Self::Ace,
            _ => Self::Veteran,
        }
    }

    fn dive_interval_sec(self, stage: u32) -> f32 {
        let base = match self {
            Self::Cadet => 2.55,
            Self::Veteran => 1.85,
            Self::Ace => 1.30,
        };
        (base - (stage.saturating_sub(1) as f32) * 0.08).clamp(0.62, 3.0)
    }

    fn bullet_speed(self) -> f32 {
        match self {
            Self::Cadet => 235.0,
            Self::Veteran => 300.0,
            Self::Ace => 365.0,
        }
    }

    fn starting_lives(self) -> u8 {
        match self {
            Self::Cadet => 4,
            Self::Veteran => 3,
            Self::Ace => 3,
        }
    }
}

const SYNC_OFFSETS_MS: [i32; 5] = [0, 25, 50, 100, 150];

#[derive(Clone, Copy)]
struct SavedPrefs {
    hi_score: u32,
    primary_gear: WeaponGear,
    difficulty: Difficulty,
    latency_idx: usize,
}

impl Default for SavedPrefs {
    fn default() -> Self {
        Self {
            hi_score: 0,
            primary_gear: WeaponGear::BeatPulse,
            difficulty: Difficulty::Veteran,
            latency_idx: 1, // +25 ms default
        }
    }
}

impl SavedPrefs {
    fn load(ctx: &HostContext<'_>) -> Self {
        let Some(bytes) = ctx.load_plugin_data(PLUGIN_ID, SAVE_FILE) else {
            return Self::default();
        };
        if bytes.len() < 11 || &bytes[0..4] != SAVE_MAGIC {
            return Self::default();
        }
        let hi_score = u32::from_le_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]);
        let primary_gear = if bytes[8] == 1 {
            WeaponGear::BarCannon
        } else {
            WeaponGear::BeatPulse
        };
        let difficulty = Difficulty::from_u8(bytes[9]);
        let latency_idx = (bytes[10] as usize).min(SYNC_OFFSETS_MS.len() - 1);
        Self {
            hi_score,
            primary_gear,
            difficulty,
            latency_idx,
        }
    }

    fn save(&self, ctx: &HostContext<'_>) {
        let mut out = [0u8; 11];
        out[0..4].copy_from_slice(SAVE_MAGIC);
        out[4..8].copy_from_slice(&self.hi_score.to_le_bytes());
        out[8] = if self.primary_gear == WeaponGear::BarCannon {
            1
        } else {
            0
        };
        out[9] = self.difficulty as u8;
        out[10] = self.latency_idx as u8;
        let _ = ctx.save_plugin_data(PLUGIN_ID, SAVE_FILE, &out);
    }

    fn latency_sec(&self) -> f64 {
        (SYNC_OFFSETS_MS[self.latency_idx] as f64) * 0.001
    }
}

// ============================================================================
// Cosmic Entities: Starfield, Bullets, Wave Cannon, Enemies, Boss & Pickups
// ============================================================================

#[derive(Clone, Copy)]
struct Star {
    nx: f32, // Normalized 0..1 inside arena
    ny: f32,
    speed: f32,
    layer: u8,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ShotKind {
    PlasmaSingle,
    TwinLance,
    VulcanPellet,
}

#[derive(Clone, Copy)]
struct PlayerShot {
    x: f32,
    y: f32,
    vx: f32,
    vy: f32,
    damage: i32,
    pierce_left: u8,
    ttl_sec: f32,
    kind: ShotKind,
}

#[derive(Clone, Copy)]
struct WaveBeam {
    x: f32,
    width: f32,
    ttl_sec: f32,
    max_ttl_sec: f32,
}

#[derive(Clone, Copy)]
struct EnemyBullet {
    x: f32,
    y: f32,
    vx: f32,
    vy: f32,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum EnemyKind {
    Scout,     // 1 HP nimble diver
    Viper,     // 2 HP corkscrew striker
    Commander, // 3 HP tractor-beam flagship (drops Wingman Pod when destroyed mid-dive)
}

#[derive(Clone, Copy, PartialEq)]
enum EnemyState {
    Entering { progress: f32, delay: f32, from_left: bool },
    Docked,
    Diving {
        t: f32,
        start_x: f32,
        start_y: f32,
        curve_dir: f32,
        fired: bool,
        tractor_sec: f32,
    },
}

#[derive(Clone, Copy)]
struct Enemy {
    alive: bool,
    kind: EnemyKind,
    grid_row: usize,
    grid_col: usize,
    hp: i32,
    x: f32,
    y: f32,
    state: EnemyState,
    hit_flash_sec: f32,
}

#[derive(Clone, Copy)]
struct DreadnoughtBoss {
    active: bool,
    x: f32,
    y: f32,
    vx: f32,
    hp: i32,
    max_hp: i32,
    turret_cd_sec: f32,
    core_exposed: bool,
    hit_flash_sec: f32,
}

#[derive(Clone, Copy)]
struct WingmanPickup {
    x: f32,
    y: f32,
    vy: f32,
}

#[derive(Clone, Copy)]
struct Particle {
    x: f32,
    y: f32,
    vx: f32,
    vy: f32,
    life_sec: f32,
    max_life_sec: f32,
    size: f32,
    bright: bool,
}

#[derive(Clone, Copy)]
struct ShockRing {
    x: f32,
    y: f32,
    r: f32,
    max_r: f32,
}

// ============================================================================
// Deterministic PRNG (Zero external dependencies)
// ============================================================================

struct XorShift32 {
    state: u32,
}

impl XorShift32 {
    fn new(seed: u32) -> Self {
        Self {
            state: if seed == 0 { 0x9E37_79B9 } else { seed },
        }
    }

    #[inline]
    fn next_u32(&mut self) -> u32 {
        let mut x = self.state;
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
        self.state = x;
        x
    }

    #[inline]
    fn next_f32(&mut self) -> f32 {
        ((self.next_u32() >> 8) as f32) * (1.0 / 16_777_216.0)
    }

    #[inline]
    fn range_f32(&mut self, lo: f32, hi: f32) -> f32 {
        lo + (hi - lo) * self.next_f32()
    }
}

// ============================================================================
// Core Pulsar Vanguard Simulation State
// ============================================================================

struct VanguardGame {
    prefs: SavedPrefs,
    rng: XorShift32,

    stage: u32,
    score: u32,
    chain: u32,
    streak_sync_actions: u32,
    lives: u8,
    dual_fighter: bool,
    game_over: bool,
    stage_banner_sec: f32,

    // Player starfighter kinematics & Phase Roll state
    ship_x: f32,
    dash_rem_sec: f32,
    dash_cd_sec: f32,
    dash_dir: f32,
    invuln_sec: f32,
    muzzle_flash_sec: f32,
    ghost_xs: [f32; 3],

    // Formation & entities
    formation_sway_phase: f32,
    dive_timer_sec: f32,
    enemies: Vec<Enemy>,
    boss: DreadnoughtBoss,
    player_shots: Vec<PlayerShot>,
    wave_beams: Vec<WaveBeam>,
    enemy_bullets: Vec<EnemyBullet>,
    pickups: Vec<WingmanPickup>,
    particles: Vec<Particle>,
    shock_rings: Vec<ShockRing>,
    stars: [Star; 44],

    // Live rhythm & HUD feedback
    last_tick: BarTick,
    focus_held: bool,
    bar_charge: f32,
    grid_expand_px: f32,
    onset_glow: f32,
    callout_text: String,
    callout_sec: f32,
}

#[allow(non_snake_case)]
impl VanguardGame {
    fn new(prefs: SavedPrefs, seed: u32, layout: ComputedLayout) -> Self {
        let mut rng = XorShift32::new(seed);
        let stars = core::array::from_fn(|i| Star {
            nx: rng.next_f32(),
            ny: rng.next_f32(),
            speed: match i % 3 {
                0 => 28.0,
                1 => 58.0,
                _ => 105.0,
            },
            layer: (i % 3) as u8,
        });

        let mut g = Self {
            prefs,
            rng,
            stage: 1,
            score: 0,
            chain: 1,
            streak_sync_actions: 0,
            lives: prefs.difficulty.starting_lives(),
            dual_fighter: false,
            game_over: false,
            stage_banner_sec: 1.6,

            ship_x: layout.arena_x + layout.arena_w * 0.5,
            dash_rem_sec: 0.0,
            dash_cd_sec: 0.0,
            dash_dir: 0.0,
            invuln_sec: 1.2,
            muzzle_flash_sec: 0.0,
            ghost_xs: [layout.arena_x + layout.arena_w * 0.5; 3],

            formation_sway_phase: 0.0,
            dive_timer_sec: 2.0,
            enemies: Vec::with_capacity(SPEC.swarm_cols * SPEC.swarm_rows),
            boss: DreadnoughtBoss {
                active: false,
                x: 0.0,
                y: 0.0,
                vx: 0.0,
                hp: 0,
                max_hp: 1,
                turret_cd_sec: 1.2,
                core_exposed: false,
                hit_flash_sec: 0.0,
            },
            player_shots: Vec::with_capacity(64),
            wave_beams: Vec::with_capacity(4),
            enemy_bullets: Vec::with_capacity(48),
            pickups: Vec::with_capacity(4),
            particles: Vec::with_capacity(96),
            shock_rings: Vec::with_capacity(4),
            stars,

            last_tick: BarTick::default(),
            focus_held: false,
            bar_charge: 0.0,
            grid_expand_px: 0.0,
            onset_glow: 0.0,
            callout_text: String::new(),
            callout_sec: 0.0,
        };
        g.spawn_stage(layout);
        g
    }

    fn restart(&mut self, layout: ComputedLayout) {
        self.stage = 1;
        self.score = 0;
        self.chain = 1;
        self.streak_sync_actions = 0;
        self.lives = self.prefs.difficulty.starting_lives();
        self.dual_fighter = false;
        self.game_over = false;
        self.ship_x = layout.arena_x + layout.arena_w * 0.5;
        self.ghost_xs = [self.ship_x; 3];
        self.dash_rem_sec = 0.0;
        self.dash_cd_sec = 0.0;
        self.invuln_sec = 1.5;
        self.player_shots.clear();
        self.wave_beams.clear();
        self.enemy_bullets.clear();
        self.pickups.clear();
        self.particles.clear();
        self.shock_rings.clear();
        self.callout_sec = 0.0;
        self.spawn_stage(layout);
    }

    fn spawn_stage(&mut self, layout: ComputedLayout) {
        self.enemies.clear();
        self.enemy_bullets.clear();
        self.stage_banner_sec = 1.65;
        self.dive_timer_sec = self.prefs.difficulty.dive_interval_sec(self.stage);

        // Every 4th stage spawns the Star-Core Leviathan Dreadnought + escort wing
        if self.stage % 4 == 0 {
            let hp = 42 + (self.stage as i32) * 8;
            self.boss = DreadnoughtBoss {
                active: true,
                x: layout.arena_x + layout.arena_w * 0.5,
                y: layout.arena_y + 102.0,
                vx: 95.0 + (self.stage as f32) * 6.0,
                hp,
                max_hp: hp,
                turret_cd_sec: 1.1,
                core_exposed: false,
                hit_flash_sec: 0.0,
            };
            // Escort row of Scouts below the Dreadnought
            for col in 0..SPEC.swarm_cols {
                self.enemies.push(Enemy {
                    alive: true,
                    kind: EnemyKind::Scout,
                    grid_row: 2,
                    grid_col: col,
                    hp: 1,
                    x: layout.arena_x + layout.arena_w * 0.5,
                    y: layout.arena_y - 30.0,
                    state: EnemyState::Entering {
                        progress: 0.0,
                        delay: (col as f32) * 0.09,
                        from_left: col < SPEC.swarm_cols / 2,
                    },
                    hit_flash_sec: 0.0,
                });
            }
            return;
        }

        self.boss.active = false;
        let hp_bonus = (self.stage.saturating_sub(1) / 3) as i32;
        for row in 0..SPEC.swarm_rows {
            for col in 0..SPEC.swarm_cols {
                // Classic Galaga silhouette: top row has 4 centered Commanders
                if row == 0 && (col < 2 || col >= SPEC.swarm_cols - 2) {
                    continue;
                }
                let kind = match row {
                    0 => EnemyKind::Commander,
                    1 => EnemyKind::Viper,
                    _ => EnemyKind::Scout,
                };
                let base_hp = match kind {
                    EnemyKind::Commander => 3 + hp_bonus,
                    EnemyKind::Viper => 2 + hp_bonus / 2,
                    EnemyKind::Scout => 1 + hp_bonus / 2,
                };
                let squad_order = row * SPEC.swarm_cols + col;
                self.enemies.push(Enemy {
                    alive: true,
                    kind,
                    grid_row: row,
                    grid_col: col,
                    hp: base_hp,
                    x: layout.arena_x + layout.arena_w * 0.5,
                    y: layout.arena_y - 36.0,
                    state: EnemyState::Entering {
                        progress: 0.0,
                        delay: (squad_order as f32) * 0.055,
                        from_left: (row + col) % 2 == 0,
                    },
                    hit_flash_sec: 0.0,
                });
            }
        }
    }

    fn slot_position(&self, row: usize, col: usize, layout: ComputedLayout) -> (f32, f32) {
        let cx = layout.arena_x + layout.arena_w * 0.5;
        let sway = self.formation_sway_phase.sin() * 28.0;
        let col_centered = (col as f32) - ((SPEC.swarm_cols - 1) as f32) * 0.5;
        let pulse_scale = 1.0 + (self.grid_expand_px / 220.0);
        let x = cx + sway + col_centered * SPEC.swarm_col_pitch * pulse_scale;
        let y = layout.arena_y
            + SPEC.swarm_top_offset
            + (row as f32) * SPEC.swarm_row_pitch
            + self.grid_expand_px * 0.25 * ((row as f32) - 1.5);
        (x, y)
    }

    fn add_score(&mut self, base_pts: u32) {
        let gained = base_pts.saturating_mul(self.chain.max(1));
        self.score = self.score.saturating_add(gained);
        if self.score > self.prefs.hi_score {
            self.prefs.hi_score = self.score;
        }
    }

    fn set_callout(&mut self, text: String, dur_sec: f32) {
        self.callout_text = text;
        self.callout_sec = dur_sec;
    }

    fn emit_explosion(&mut self, x: f32, y: f32, count: usize, bright: bool) {
        for _ in 0..count {
            if self.particles.len() >= 120 {
                break;
            }
            let ang = self.rng.range_f32(0.0, TAU);
            let spd = self.rng.range_f32(45.0, 220.0);
            let life = self.rng.range_f32(0.22, 0.52);
            self.particles.push(Particle {
                x,
                y,
                vx: ang.cos() * spd,
                vy: ang.sin() * spd,
                life_sec: life,
                max_life_sec: life,
                size: self.rng.range_f32(2.5, 5.5),
                bright,
            });
        }
    }

    // --- Auto-Weapon Spawners Driven Strictly by BarPhaseTracker ---

    fn gun_mounts(&self) -> ([f32; 2], usize) {
        if self.dual_fighter {
            (
                [
                    self.ship_x - SPEC.wingman_offset_x * 0.5,
                    self.ship_x + SPEC.wingman_offset_x * 0.5,
                ],
                2,
            )
        } else {
            ([self.ship_x, 0.0], 1)
        }
    }

    fn spawn_wave_cannon(&mut self, beam_width: f32, ship_y: f32) {
        self.muzzle_flash_sec = 0.18;
        let width = if self.dual_fighter {
            beam_width * 1.25
        } else {
            beam_width
        };
        self.wave_beams.push(WaveBeam {
            x: self.ship_x,
            width,
            ttl_sec: 0.28,
            max_ttl_sec: 0.28,
        });

        // Apply instant piercing bar-cannon damage along the vertical beam corridor
        let half = width * 0.5 + 12.0;
        let dmg = if beam_width >= 44.0 { 5 } else { 3 };
        let mut destroyed_positions: [(f32, f32, EnemyKind, bool); 16] =
            [(0.0, 0.0, EnemyKind::Scout, false); 16];
        let mut n_dead = 0usize;

        for e in &mut self.enemies {
            if !e.alive || e.y > ship_y {
                continue;
            }
            if (e.x - self.ship_x).abs() <= half {
                e.hp -= dmg;
                e.hit_flash_sec = 0.14;
                if e.hp <= 0 {
                    e.alive = false;
                    if n_dead < destroyed_positions.len() {
                        let diving = matches!(e.state, EnemyState::Diving { .. });
                        destroyed_positions[n_dead] = (e.x, e.y, e.kind, diving);
                        n_dead += 1;
                    }
                }
            }
        }

        for &(ex, ey, kind, was_diving) in &destroyed_positions[..n_dead] {
            self.on_enemy_destroyed(ex, ey, kind, was_diving);
        }

        if self.boss.active && (self.boss.x - self.ship_x).abs() <= half + 46.0 {
            let boss_dmg = if self.boss.core_exposed { dmg * 2 } else { dmg };
            self.boss.hp -= boss_dmg;
            self.boss.hit_flash_sec = 0.16;
            self.emit_explosion(self.ship_x, self.boss.y + 18.0, 10, true);
            if self.boss.hp <= 0 {
                self.boss.active = false;
                self.add_score(1500);
                self.emit_explosion(self.boss.x, self.boss.y, 32, true);
            }
        }

        // Vaporize enemy bullets caught inside the Wave Cannon beam!
        self.enemy_bullets
            .retain(|b| (b.x - self.ship_x).abs() > half);
    }

    fn spawn_piercing_twin_shot(&mut self, ship_y: f32) {
        self.muzzle_flash_sec = 0.12;
        let (mounts, count) = self.gun_mounts();
        for &mx in &mounts[..count] {
            for side in [-7.0_f32, 7.0_f32] {
                self.player_shots.push(PlayerShot {
                    x: mx + side,
                    y: ship_y - SPEC.ship_half_h,
                    vx: 0.0,
                    vy: -680.0,
                    damage: 2,
                    pierce_left: 1,
                    ttl_sec: 1.4,
                    kind: ShotKind::TwinLance,
                });
            }
        }
    }

    fn spawn_single_plasma(&mut self, ship_y: f32) {
        self.muzzle_flash_sec = 0.08;
        let (mounts, count) = self.gun_mounts();
        for &mx in &mounts[..count] {
            self.player_shots.push(PlayerShot {
                x: mx,
                y: ship_y - SPEC.ship_half_h,
                vx: 0.0,
                vy: -610.0,
                damage: 1,
                pierce_left: 0,
                ttl_sec: 1.4,
                kind: ShotKind::PlasmaSingle,
            });
        }
    }

    fn spawn_short_vulcan_spread(&mut self, ship_y: f32) {
        self.muzzle_flash_sec = 0.06;
        let (mounts, count) = self.gun_mounts();
        for &mx in &mounts[..count] {
            for &(vx, vy) in &[(-135.0_f32, -620.0_f32), (0.0, -650.0), (135.0, -620.0)] {
                self.player_shots.push(PlayerShot {
                    x: mx,
                    y: ship_y - SPEC.ship_half_h,
                    vx,
                    vy,
                    damage: 1,
                    pierce_left: 0,
                    ttl_sec: 0.56, // Short-range concentrated spread
                    kind: ShotKind::VulcanPellet,
                });
            }
        }
    }

    fn trigger_phase_roll(&mut self, move_dir: f32, perfect_sync: bool, ship_y: f32, uk: bool) {
        if self.dash_cd_sec > 0.0 {
            return;
        }
        let dir = if move_dir.abs() > 0.1 {
            move_dir.signum()
        } else if self.rng.next_u32() & 1 == 0 {
            1.0
        } else {
            -1.0
        };
        self.dash_dir = dir;
        self.dash_rem_sec = 0.19;
        self.invuln_sec = self.invuln_sec.max(0.28);
        self.ghost_xs = [self.ship_x; 3];

        if perfect_sync {
            self.dash_cd_sec = 0.36;
            self.streak_sync_actions += 1;
            self.chain = (self.chain + 1).min(8);
            self.add_score(75);
            self.shock_rings.push(ShockRing {
                x: self.ship_x,
                y: ship_y,
                r: 12.0,
                max_r: 92.0,
            });
            // EMP pulse clears enemy bullets immediately adjacent to the fighter
            let sx = self.ship_x;
            self.enemy_bullets
                .retain(|b| (b.x - sx).hypot(b.y - ship_y) > 88.0);
            let msg = if uk {
                format!("ІДЕАЛЬНА ФАЗА! x{}", self.chain)
            } else {
                format!("PERFECT PHASE! x{}", self.chain)
            };
            self.set_callout(msg, 0.85);
        } else {
            self.dash_cd_sec = 0.68;
        }
    }

    fn on_enemy_destroyed(&mut self, x: f32, y: f32, kind: EnemyKind, was_diving: bool) {
        let base = match (kind, was_diving) {
            (EnemyKind::Scout, false) => 50,
            (EnemyKind::Scout, true) => 100,
            (EnemyKind::Viper, false) => 80,
            (EnemyKind::Viper, true) => 160,
            (EnemyKind::Commander, false) => 150,
            (EnemyKind::Commander, true) => 400,
        };
        self.add_score(base);
        self.emit_explosion(
            x,
            y,
            if kind == EnemyKind::Commander { 18 } else { 10 },
            kind == EnemyKind::Commander,
        );

        // Destroying a diving Commander when not yet Dual-Fighter drops a Wingman Pod!
        if kind == EnemyKind::Commander && was_diving && !self.dual_fighter && self.pickups.is_empty()
        {
            self.pickups.push(WingmanPickup { x, y, vy: 125.0 });
        }
    }

    fn damage_player(&mut self, layout: ComputedLayout, uk: bool) {
        if self.invuln_sec > 0.0 || self.game_over {
            return;
        }
        self.emit_explosion(self.ship_x, layout.ship_y, 24, true);
        self.chain = 1;
        self.streak_sync_actions = 0;

        // If Dual Fighter is active, the wingman absorbs the hit first (classic Galaga mechanic!)
        if self.dual_fighter {
            self.dual_fighter = false;
            self.invuln_sec = 1.6;
            self.set_callout(
                if uk {
                    "ВЕДЕНИЙ ВТРАЧЕНИЙ!".to_string()
                } else {
                    "WINGMAN LOST!".to_string()
                },
                1.1,
            );
            return;
        }

        if self.lives > 1 {
            self.lives -= 1;
            self.invuln_sec = 2.0;
            self.ship_x = layout.arena_x + layout.arena_w * 0.5;
            self.set_callout(
                if uk {
                    "КОРПУС ПОШКОДЖЕНО!".to_string()
                } else {
                    "HULL BREACH!".to_string()
                },
                1.1,
            );
        } else {
            self.lives = 0;
            self.game_over = true;
            self.set_callout(
                if uk {
                    "МІСІЮ ЗАВЕРШЕНО — НАТИСНИ A".to_string()
                } else {
                    "GAME OVER — PRESS A".to_string()
                },
                99.0,
            );
        }
    }

    /// Core 60 FPS step integrating `BarPhaseTracker`, auto-weapons, Phase Roll, and Galaga AI.
    #[allow(clippy::too_many_arguments)]
    fn step_frame(
        &mut self,
        tracker: &mut BarPhaseTracker,
        source: SyncSource<'_>,
        audio: AudioFrameInput,
        move_axis: f32,
        focus_held: bool,
        dodge_pressed: bool,
        layout: ComputedLayout,
        uk: bool,
    ) {
        let dt = audio.dt_sec.clamp(0.0, 0.05);
        let tick = tracker.update(source, audio);
        self.last_tick = tick;
        self.focus_held = focus_held;

        if tick.raw_onset {
            self.onset_glow = 1.0;
        } else {
            self.onset_glow = (self.onset_glow - dt * 5.0).max(0.0);
        }

        // Scroll parallax starfield (surges faster on musical transients and Wave Cannon blasts)
        let star_boost = 1.0 + self.onset_glow * 0.9 + (self.wave_beams.len() as f32) * 1.4;
        for st in &mut self.stars {
            st.ny += (st.speed * star_boost * dt) / layout.arena_h.max(1.0);
            if st.ny >= 1.0 {
                st.ny -= 1.0;
                st.nx = self.rng.next_f32();
            }
        }

        if self.game_over {
            self.update_particles_and_rings(dt);
            return;
        }

        // Effective weapon gear: holding A shifts into 1/8 Focus Vulcan overdrive
        let active_gear = if focus_held {
            WeaponGear::FocusEight
        } else {
            self.prefs.primary_gear
        };

        // 1. Auto-fire strictly locked to the bar/beat grid
        match active_gear {
            WeaponGear::BarCannon if tick.fire_bar => {
                let beam_width = 16.0 + tick.completed_bar_energy * 48.0;
                self.spawn_wave_cannon(beam_width, layout.ship_y);
            }
            WeaponGear::BeatPulse if tick.fire_beat => {
                if tick.beat_index == 0 {
                    self.spawn_piercing_twin_shot(layout.ship_y);
                } else {
                    self.spawn_single_plasma(layout.ship_y);
                }
            }
            WeaponGear::FocusEight if tick.fire_eighth => {
                self.spawn_short_vulcan_spread(layout.ship_y);
            }
            _ => {}
        }

        // 2. Phase Roll dodge on B/X with ±65 ms beat-window bonus
        if dodge_pressed {
            let perfect_sync = tracker.is_in_beat_window(65.0);
            self.trigger_phase_roll(move_axis, perfect_sync, layout.ship_y, uk);
        }

        // 3. Swarm breathing choreography & Wave Cannon nose charge ring
        self.grid_expand_px = tracker.swarm_pulse() * SPEC.swarm_pulse_amp;
        self.bar_charge = tracker.bar_charge_ratio();
        self.boss.core_exposed = tick.beat_index == 0;

        // 4. Advance timers & starfighter movement
        self.stage_banner_sec = (self.stage_banner_sec - dt).max(0.0);
        self.dash_cd_sec = (self.dash_cd_sec - dt).max(0.0);
        self.invuln_sec = (self.invuln_sec - dt).max(0.0);
        self.muzzle_flash_sec = (self.muzzle_flash_sec - dt).max(0.0);
        if self.callout_sec > 0.0 {
            self.callout_sec = (self.callout_sec - dt).max(0.0);
        }

        let ship_margin = if self.dual_fighter {
            SPEC.ship_half_w + SPEC.wingman_offset_x * 0.5 + 8.0
        } else {
            SPEC.ship_half_w + 8.0
        };
        let min_x = layout.arena_x + ship_margin;
        let max_x = layout.arena_x + layout.arena_w - ship_margin;

        if self.dash_rem_sec > 0.0 {
            self.ghost_xs[2] = self.ghost_xs[1];
            self.ghost_xs[1] = self.ghost_xs[0];
            self.ghost_xs[0] = self.ship_x;
            self.dash_rem_sec = (self.dash_rem_sec - dt).max(0.0);
            self.ship_x = (self.ship_x + self.dash_dir * 740.0 * dt).clamp(min_x, max_x);
        } else {
            let base_spd = if focus_held { 255.0 } else { 420.0 };
            self.ship_x = (self.ship_x + move_axis * base_spd * dt).clamp(min_x, max_x);
        }

        self.formation_sway_phase =
            (self.formation_sway_phase + dt * (tick.effective_bpm / 120.0) * 1.45) % TAU;

        // 5. Update Wave Cannon beams, player shots, enemies, boss, and collisions
        for wb in &mut self.wave_beams {
            wb.ttl_sec -= dt;
        }
        self.wave_beams.retain(|wb| wb.ttl_sec > 0.0);

        self.update_enemies_and_boss(dt, layout, uk);
        self.update_projectiles_and_pickups(dt, layout, uk);
        self.update_particles_and_rings(dt);

        // Advance to next stage when all enemies and boss are cleared
        let any_enemy = self.enemies.iter().any(|e| e.alive) || self.boss.active;
        if !any_enemy {
            self.stage += 1;
            self.spawn_stage(layout);
        }
    }

    fn update_enemies_and_boss(&mut self, dt: f32, layout: ComputedLayout, uk: bool) {
        // Trigger periodic Galaga dive runs from docked ships
        self.dive_timer_sec -= dt;
        if self.dive_timer_sec <= 0.0 {
            self.dive_timer_sec = self.prefs.difficulty.dive_interval_sec(self.stage);
            let mut docked_indices: [usize; 32] = [0; 32];
            let mut n_docked = 0usize;
            for (idx, e) in self.enemies.iter().enumerate() {
                if e.alive && matches!(e.state, EnemyState::Docked) && n_docked < docked_indices.len()
                {
                    docked_indices[n_docked] = idx;
                    n_docked += 1;
                }
            }
            if n_docked > 0 {
                let pick = docked_indices[(self.rng.next_u32() as usize) % n_docked];
                let e = &mut self.enemies[pick];
                let dir = if e.x < self.ship_x { 1.0 } else { -1.0 };
                e.state = EnemyState::Diving {
                    t: 0.0,
                    start_x: e.x,
                    start_y: e.y,
                    curve_dir: dir,
                    fired: false,
                    tractor_sec: 0.0,
                };
            }
        }

        let bspd = self.prefs.difficulty.bullet_speed();
        let arena_bottom = layout.arena_y + layout.arena_h;
        let mut player_hit = false;

        for idx in 0..self.enemies.len() {
            if !self.enemies[idx].alive {
                continue;
            }
            let (slot_x, slot_y) = self.slot_position(
                self.enemies[idx].grid_row,
                self.enemies[idx].grid_col,
                layout,
            );
            let e = &mut self.enemies[idx];
            e.hit_flash_sec = (e.hit_flash_sec - dt).max(0.0);

            match &mut e.state {
                EnemyState::Entering {
                    progress,
                    delay,
                    from_left,
                } => {
                    if *delay > 0.0 {
                        *delay -= dt;
                    } else {
                        *progress += dt * 1.15;
                        let p = (*progress).clamp(0.0, 1.0);
                        let entry_x = if *from_left {
                            layout.arena_x - 20.0
                        } else {
                            layout.arena_x + layout.arena_w + 20.0
                        };
                        let entry_y = layout.arena_y + 30.0;
                        let arc = (p * PI).sin() * (if *from_left { 110.0 } else { -110.0 });
                        e.x = entry_x + (slot_x - entry_x) * p + arc * (1.0 - p);
                        e.y = entry_y + (slot_y - entry_y) * p + (p * TAU).sin() * 26.0 * (1.0 - p);
                        if p >= 1.0 {
                            e.x = slot_x;
                            e.y = slot_y;
                            e.state = EnemyState::Docked;
                        }
                    }
                }
                EnemyState::Docked => {
                    e.x = slot_x;
                    e.y = slot_y;
                }
                EnemyState::Diving {
                    t,
                    start_x,
                    start_y,
                    curve_dir,
                    fired,
                    tractor_sec,
                } => {
                    // Commander pauses mid-arena to project its Galaga Tractor Cone!
                    if e.kind == EnemyKind::Commander
                        && *t >= 0.48
                        && *tractor_sec < 1.35
                        && !*fired
                    {
                        *tractor_sec += dt;
                        // Tractor cone pull / hazard zone directly beneath Commander
                        let beam_top = e.y + 18.0;
                        if layout.ship_y > beam_top
                            && (self.ship_x - e.x).abs() < 34.0
                            && *tractor_sec > 0.45
                        {
                            player_hit = true;
                        }
                        if *tractor_sec >= 1.35 {
                            *fired = true;
                        }
                    } else {
                        *t += dt * 0.62;
                        let u = *t;
                        let weave = match e.kind {
                            EnemyKind::Scout => (u * 7.5).sin() * 68.0 * *curve_dir,
                            EnemyKind::Viper => (u * 10.5).sin() * 92.0 * *curve_dir,
                            EnemyKind::Commander => (u * 5.0).sin() * 52.0 * *curve_dir,
                        };
                        e.x = (*start_x + (self.ship_x - *start_x) * (u * 0.65).min(1.0) + weave)
                            .clamp(layout.arena_x + 22.0, layout.arena_x + layout.arena_w - 22.0);
                        e.y = *start_y + u * (layout.arena_h * 0.92);

                        if !*fired && u >= 0.36 && self.enemy_bullets.len() < 44 {
                            *fired = true;
                            let dx = self.ship_x - e.x;
                            let dy = (layout.ship_y - e.y).max(60.0);
                            let len = dx.hypot(dy).max(1.0);
                            let vx = (dx / len) * bspd;
                            let vy = (dy / len) * bspd;
                            self.enemy_bullets.push(EnemyBullet { x: e.x, y: e.y + 12.0, vx, vy });
                            if e.kind == EnemyKind::Viper {
                                self.enemy_bullets.push(EnemyBullet {
                                    x: e.x,
                                    y: e.y + 12.0,
                                    vx: vx + 55.0 * *curve_dir,
                                    vy,
                                });
                            }
                        }

                        // Collide diving ship body against player starfighter
                        if (e.x - self.ship_x).abs() < 24.0 && (e.y - layout.ship_y).abs() < 22.0 {
                            player_hit = true;
                        }

                        // Wrap back into top of arena and re-dock cleanly
                        if e.y > arena_bottom + 24.0 {
                            e.x = slot_x;
                            e.y = slot_y;
                            e.state = EnemyState::Docked;
                        }
                    }
                }
            }
        }

        // Update Boss Dreadnought if active
        if self.boss.active {
            self.boss.hit_flash_sec = (self.boss.hit_flash_sec - dt).max(0.0);
            self.boss.x += self.boss.vx * dt;
            let min_bx = layout.arena_x + 86.0;
            let max_bx = layout.arena_x + layout.arena_w - 86.0;
            if self.boss.x <= min_bx {
                self.boss.x = min_bx;
                self.boss.vx = self.boss.vx.abs();
            } else if self.boss.x >= max_bx {
                self.boss.x = max_bx;
                self.boss.vx = -self.boss.vx.abs();
            }

            self.boss.turret_cd_sec -= dt;
            if self.boss.turret_cd_sec <= 0.0 && self.enemy_bullets.len() < 42 {
                self.boss.turret_cd_sec = match self.prefs.difficulty {
                    Difficulty::Cadet => 1.25,
                    Difficulty::Veteran => 0.90,
                    Difficulty::Ace => 0.65,
                };
                for turret_dx in [-48.0_f32, 48.0_f32] {
                    let tx = self.boss.x + turret_dx;
                    let ty = self.boss.y + 24.0;
                    let dx = self.ship_x - tx;
                    let dy = (layout.ship_y - ty).max(80.0);
                    let len = dx.hypot(dy).max(1.0);
                    self.enemy_bullets.push(EnemyBullet {
                        x: tx,
                        y: ty,
                        vx: (dx / len) * bspd,
                        vy: (dy / len) * bspd,
                    });
                }
            }
        }

        if player_hit {
            self.damage_player(layout, uk);
        }
    }

    fn update_projectiles_and_pickups(&mut self, dt: f32, layout: ComputedLayout, uk: bool) {
        let arena_top = layout.arena_y - 16.0;
        let arena_bottom = layout.arena_y + layout.arena_h + 16.0;
        let arena_left = layout.arena_x - 16.0;
        let arena_right = layout.arena_x + layout.arena_w + 16.0;

        // Advance player shots & resolve enemy/boss hits
        let mut dead_events: Vec<(f32, f32, EnemyKind, bool)> = Vec::new();

        for s in &mut self.player_shots {
            s.x += s.vx * dt;
            s.y += s.vy * dt;
            s.ttl_sec -= dt;
            if s.ttl_sec <= 0.0 || s.y < arena_top || s.x < arena_left || s.x > arena_right {
                s.ttl_sec = 0.0;
                continue;
            }

            // Check enemies
            for e in &mut self.enemies {
                if !e.alive {
                    continue;
                }
                let hit_r = if e.kind == EnemyKind::Commander { 20.0 } else { 16.0 };
                if (s.x - e.x).abs() <= hit_r && (s.y - e.y).abs() <= hit_r {
                    e.hp -= s.damage;
                    e.hit_flash_sec = 0.10;
                    if e.hp <= 0 {
                        e.alive = false;
                        let was_diving = matches!(e.state, EnemyState::Diving { .. });
                        dead_events.push((e.x, e.y, e.kind, was_diving));
                    }
                    if s.pierce_left > 0 {
                        s.pierce_left -= 1;
                    } else {
                        s.ttl_sec = 0.0;
                        break;
                    }
                }
            }

            // Check Dreadnought Boss
            if s.ttl_sec > 0.0
                && self.boss.active
                && (s.x - self.boss.x).abs() <= 68.0
                && (s.y - self.boss.y).abs() <= 28.0
            {
                let core_hit = self.boss.core_exposed && (s.x - self.boss.x).abs() <= 22.0;
                let dmg = if core_hit { s.damage * 2 } else { s.damage };
                self.boss.hp -= dmg;
                self.boss.hit_flash_sec = 0.09;
                s.ttl_sec = 0.0;
            }
        }
        self.player_shots.retain(|s| s.ttl_sec > 0.0);

        for (ex, ey, kind, diving) in dead_events {
            self.on_enemy_destroyed(ex, ey, kind, diving);
        }

        if self.boss.active && self.boss.hp <= 0 {
            self.boss.active = false;
            self.add_score(1500);
            self.emit_explosion(self.boss.x, self.boss.y, 34, true);
        }

        // Advance enemy bullets & check player hitbox
        let mut bullet_hit_player = false;
        let hit_half_w = if self.dual_fighter { 28.0 } else { 13.0 };
        for b in &mut self.enemy_bullets {
            b.x += b.vx * dt;
            b.y += b.vy * dt;
            if (b.x - self.ship_x).abs() <= hit_half_w && (b.y - layout.ship_y).abs() <= 13.0 {
                bullet_hit_player = true;
                b.y = arena_bottom + 100.0;
            }
        }
        self.enemy_bullets.retain(|b| {
            b.y <= arena_bottom && b.y >= arena_top && b.x >= arena_left && b.x <= arena_right
        });

        if bullet_hit_player {
            self.damage_player(layout, uk);
        }

        // Advance Wingman Pod pickups
        let mut caught_pod = false;
        for p in &mut self.pickups {
            p.y += p.vy * dt;
            if (p.x - self.ship_x).abs() <= 28.0 && (p.y - layout.ship_y).abs() <= 24.0 {
                caught_pod = true;
                p.y = arena_bottom + 100.0;
            }
        }
        self.pickups.retain(|p| p.y <= arena_bottom);

        if caught_pod {
            self.dual_fighter = true;
            self.add_score(500);
            self.set_callout(
                if uk {
                    "ПОДВІЙНИЙ ВИНИЩУВАЧ!".to_string()
                } else {
                    "DUAL FIGHTER DOCKED!".to_string()
                },
                1.35,
            );
        }
    }

    fn update_particles_and_rings(&mut self, dt: f32) {
        for p in &mut self.particles {
            p.x += p.vx * dt;
            p.y += p.vy * dt;
            p.vx *= (1.0 - dt * 2.4).max(0.0);
            p.vy *= (1.0 - dt * 2.4).max(0.0);
            p.life_sec -= dt;
        }
        self.particles.retain(|p| p.life_sec > 0.0);

        for r in &mut self.shock_rings {
            r.r += 360.0 * dt;
        }
        self.shock_rings.retain(|r| r.r < r.max_r);
    }
}

// ============================================================================
// Pure 4-Shade Vector Renderer (Uses ComputedLayout + SPEC exclusively)
// ============================================================================

fn draw_starfighter(
    ctx: &HostContext<'_>,
    cx: f32,
    cy: f32,
    scale: f32,
    hull_col: u32,
    accent_col: u32,
    core_col: u32,
) {
    let hw = SPEC.ship_half_w * scale;
    let hh = SPEC.ship_half_h * scale;
    // Delta wing sweeps
    ctx.draw_line(cx, cy - hh, cx - hw, cy + hh, 3.0 * scale, hull_col);
    ctx.draw_line(cx, cy - hh, cx + hw, cy + hh, 3.0 * scale, hull_col);
    ctx.draw_line(cx - hw, cy + hh, cx - hw * 0.35, cy + hh * 0.55, 2.5 * scale, accent_col);
    ctx.draw_line(cx + hw, cy + hh, cx + hw * 0.35, cy + hh * 0.55, 2.5 * scale, accent_col);
    // Twin wingtip laser pods
    ctx.fill_rect(cx - hw - 1.5, cy + 2.0 * scale, 3.5 * scale, hh * 0.85, 1.0, hull_col);
    ctx.fill_rect(cx + hw - 2.0, cy + 2.0 * scale, 3.5 * scale, hh * 0.85, 1.0, hull_col);
    // Central fuselage & cockpit
    ctx.fill_rect(cx - 4.0 * scale, cy - hh * 0.65, 8.0 * scale, hh * 1.45, 2.5, hull_col);
    ctx.fill_circle(cx, cy - 1.5 * scale, 3.2 * scale, core_col);
}

fn draw_alien_sprite(
    ctx: &HostContext<'_>,
    e: &Enemy,
    wing_open: bool,
    t: &PluginTheme,
) {
    let body_col = if e.hit_flash_sec > 0.0 {
        t.bg
    } else {
        match e.kind {
            EnemyKind::Commander => t.fg,
            EnemyKind::Viper => t.fg,
            EnemyKind::Scout => t.fg2,
        }
    };
    let wing_col = if e.hit_flash_sec > 0.0 { t.fg } else { t.fg2 };
    let span = if wing_open { 16.0 } else { 11.0 };

    match e.kind {
        EnemyKind::Scout => {
            ctx.fill_rect(e.x - span, e.y - 5.0, span * 2.0, 6.0, 2.0, wing_col);
            ctx.fill_rect(e.x - 7.0, e.y - 9.0, 14.0, 16.0, 4.0, body_col);
            ctx.fill_circle(e.x - 3.5, e.y - 2.0, 2.0, t.bg);
            ctx.fill_circle(e.x + 3.5, e.y - 2.0, 2.0, t.bg);
        }
        EnemyKind::Viper => {
            ctx.draw_line(e.x - span - 2.0, e.y - 8.0, e.x - 6.0, e.y + 4.0, 3.0, wing_col);
            ctx.draw_line(e.x + span + 2.0, e.y - 8.0, e.x + 6.0, e.y + 4.0, 3.0, wing_col);
            ctx.fill_rect(e.x - 8.0, e.y - 10.0, 16.0, 18.0, 4.0, body_col);
            ctx.draw_line(e.x - 5.0, e.y + 8.0, e.x, e.y + 13.0, 2.5, body_col);
            ctx.draw_line(e.x + 5.0, e.y + 8.0, e.x, e.y + 13.0, 2.5, body_col);
            ctx.fill_rect(e.x - 4.0, e.y - 4.0, 8.0, 4.0, 1.0, t.bg);
        }
        EnemyKind::Commander => {
            // Tractor beam cone if projecting mid-dive
            if let EnemyState::Diving { tractor_sec, .. } = e.state {
                if tractor_sec > 0.0 && tractor_sec < 1.35 {
                    for ring in 0..5 {
                        let ry = e.y + 24.0 + (ring as f32) * 38.0;
                        let rw = 20.0 + (ring as f32) * 14.0;
                        let col = if ring % 2 == 0 { t.fg1 } else { t.fg2 };
                        ctx.stroke_rect(e.x - rw * 0.5, ry, rw, 14.0, 6.0, 2.0, col);
                    }
                }
            }
            // Crown horns + armored carapace
            ctx.draw_line(e.x - 8.0, e.y - 11.0, e.x - 13.0, e.y - 17.0, 3.0, body_col);
            ctx.draw_line(e.x + 8.0, e.y - 11.0, e.x + 13.0, e.y - 17.0, 3.0, body_col);
            ctx.fill_rect(e.x - span - 4.0, e.y - 4.0, (span + 4.0) * 2.0, 10.0, 3.0, wing_col);
            ctx.fill_rect(e.x - 11.0, e.y - 11.0, 22.0, 22.0, 5.0, body_col);
            ctx.fill_circle(e.x, e.y - 1.0, 4.2, if e.hp > 1 { t.bg } else { t.fg1 });
        }
    }
}

fn draw_game_frame(
    ctx: &HostContext<'_>,
    game: &VanguardGame,
    track_label: &str,
    music_playing: bool,
) {
    let t = ctx.theme();
    let title = ctx.tr("Pulsar Vanguard", "Pulsar Vanguard");
    let (win_w, win_h) = ctx.begin_responsive_frame(t.bg, true, title);
    let lay = ComputedLayout::compute(win_w, win_h);

    // ---- 1. Cosmic Arena Frame ----
    ctx.fill_rect(
        lay.arena_x,
        lay.arena_y,
        lay.arena_w,
        lay.arena_h,
        SPEC.frame_radius,
        t.bg,
    );
    let arena_border_col = if game.onset_glow > 0.45 { t.fg } else { t.fg2 };
    ctx.stroke_rect(
        lay.arena_x,
        lay.arena_y,
        lay.arena_w,
        lay.arena_h,
        SPEC.frame_radius,
        SPEC.border_stroke,
        arena_border_col,
    );

    // Parallax 3-layer starfield
    for st in &game.stars {
        let sx = lay.arena_x + 10.0 + st.nx * (lay.arena_w - 20.0);
        let sy = lay.arena_y + 10.0 + st.ny * (lay.arena_h - 20.0);
        let (sz, col) = match st.layer {
            0 => (2.0, t.fg1),
            1 => (3.0, t.fg2),
            _ => (4.0, t.fg),
        };
        ctx.fill_rect(sx, sy, sz, sz, 0.5, col);
    }

    // Active Wave Cannon beams (1/1 bar pillar)
    for wb in &game.wave_beams {
        let ratio = (wb.ttl_sec / wb.max_ttl_sec).clamp(0.0, 1.0);
        let cur_w = wb.width * (0.45 + 0.55 * ratio);
        let beam_top = lay.arena_y + 6.0;
        let beam_h = (lay.ship_y - SPEC.ship_half_h - beam_top).max(10.0);
        ctx.fill_rect(wb.x - cur_w * 0.5, beam_top, cur_w, beam_h, 4.0, t.fg1);
        ctx.fill_rect(wb.x - cur_w * 0.26, beam_top, cur_w * 0.52, beam_h, 2.0, t.fg);
    }

    // Shock rings from Perfect Phase Rolls
    for sr in &game.shock_rings {
        ctx.stroke_rect(
            sr.x - sr.r,
            sr.y - sr.r * 0.55,
            sr.r * 2.0,
            sr.r * 1.1,
            sr.r * 0.5,
            2.5,
            t.fg2,
        );
    }

    // Alien Swarm
    let wing_open = game.last_tick.beat_frac < 0.45;
    for e in &game.enemies {
        if e.alive && e.y >= lay.arena_y + 8.0 {
            draw_alien_sprite(ctx, e, wing_open, &t);
        }
    }

    // Star-Core Leviathan Dreadnought Boss
    if game.boss.active {
        let bx = game.boss.x;
        let by = game.boss.y;
        let hull = if game.boss.hit_flash_sec > 0.0 { t.fg1 } else { t.fg };
        ctx.fill_rect(bx - 72.0, by - 20.0, 144.0, 38.0, 9.0, hull);
        ctx.fill_rect(bx - 56.0, by + 14.0, 20.0, 14.0, 3.0, t.fg2);
        ctx.fill_rect(bx + 36.0, by + 14.0, 20.0, 14.0, 3.0, t.fg2);
        // Shielded vs Exposed Beat-1 Pulsar Core
        if game.boss.core_exposed {
            ctx.fill_circle(bx, by, 14.0, t.bg);
            ctx.fill_circle(bx, by, 8.0, t.fg2);
        } else {
            ctx.stroke_rect(bx - 14.0, by - 10.0, 28.0, 20.0, 5.0, 2.5, t.bg);
        }
        // Boss HP bar
        let hp_ratio = (game.boss.hp.max(0) as f32) / (game.boss.max_hp.max(1) as f32);
        ctx.fill_rect(bx - 64.0, by - 32.0, 128.0, 6.0, 3.0, t.fg1);
        ctx.fill_rect(bx - 64.0, by - 32.0, 128.0 * hp_ratio, 6.0, 3.0, t.fg);
    }

    // Wingman Pod Pickups
    for p in &game.pickups {
        ctx.stroke_rect(p.x - 13.0, p.y - 13.0, 26.0, 26.0, 8.0, 2.5, t.fg);
        ctx.draw_text(
            p.x,
            p.y + 5.0,
            "2X",
            SPEC.font_caption,
            FontWeight::Bold,
            TextAlign::Center,
            t.fg,
        );
    }

    // Player Shots
    for s in &game.player_shots {
        match s.kind {
            ShotKind::TwinLance => {
                ctx.fill_rect(s.x - 2.5, s.y - 12.0, 5.0, 22.0, 2.0, t.fg);
            }
            ShotKind::PlasmaSingle => {
                ctx.fill_rect(s.x - 2.0, s.y - 8.0, 4.0, 15.0, 2.0, t.fg);
            }
            ShotKind::VulcanPellet => {
                ctx.fill_circle(s.x, s.y, 3.2, t.fg2);
            }
        }
    }

    // Enemy Bullets
    for b in &game.enemy_bullets {
        ctx.fill_circle(b.x, b.y, 4.5, t.fg);
        ctx.fill_circle(b.x, b.y, 2.0, t.bg);
    }

    // Explosion Particles
    for p in &game.particles {
        let col = if p.bright { t.fg } else { t.fg2 };
        let sz = p.size * (p.life_sec / p.max_life_sec).clamp(0.2, 1.0);
        ctx.fill_rect(p.x - sz * 0.5, p.y - sz * 0.5, sz, sz, 1.0, col);
    }

    // Player Starfighter (+ Phase Roll Ghost Afterimages + Wave Cannon Nose Charge Ring)
    if !game.game_over {
        if game.dash_rem_sec > 0.0 {
            for &gx in &game.ghost_xs {
                draw_starfighter(ctx, gx, lay.ship_y, 0.9, t.fg1, t.fg1, t.bg);
            }
        }

        let blink_skip =
            game.invuln_sec > 0.0 && ((game.invuln_sec * 18.0) as i32 % 2 == 1);
        if !blink_skip {
            let (mounts, count) = game.gun_mounts();
            for &mx in &mounts[..count] {
                draw_starfighter(ctx, mx, lay.ship_y, 1.0, t.fg, t.fg2, t.bg);
            }
        }

        // Nose charge telegraph ring when Bar Cannon (1/1) is armed
        if !game.focus_held && game.prefs.primary_gear == WeaponGear::BarCannon {
            let r = 4.0 + game.bar_charge * SPEC.nose_ring_max_r;
            let nose_y = lay.ship_y - SPEC.ship_half_h - 6.0;
            let ring_col = if game.bar_charge > 0.80 { t.fg } else { t.fg2 };
            ctx.stroke_rect(
                game.ship_x - r,
                nose_y - r * 0.5,
                r * 2.0,
                r,
                r * 0.5,
                2.0,
                ring_col,
            );
        } else if game.muzzle_flash_sec > 0.0 {
            ctx.fill_circle(game.ship_x, lay.ship_y - SPEC.ship_half_h - 4.0, 5.0, t.fg1);
        }
    }

    // Stage Banner / Callout Overlay inside Arena
    let arena_cx = lay.arena_x + lay.arena_w * 0.5;
    if game.game_over {
        let box_w = 360.0;
        let box_h = 104.0;
        let bx = arena_cx - box_w * 0.5;
        let by = lay.arena_y + lay.arena_h * 0.42;
        ctx.fill_rect(bx, by, box_w, box_h, SPEC.card_radius, t.bg);
        ctx.stroke_rect(bx, by, box_w, box_h, SPEC.card_radius, SPEC.border_stroke, t.fg);
        ctx.draw_text(
            arena_cx,
            by + 42.0,
            ctx.tr("GAME OVER", "ГРУ ЗАВЕРШЕНО"),
            SPEC.font_banner,
            FontWeight::Bold,
            TextAlign::Center,
            t.fg,
        );
        ctx.draw_text(
            arena_cx,
            by + 78.0,
            ctx.tr("Press A to Launch Again", "Натисни A для нового вильоту"),
            SPEC.font_label,
            FontWeight::SemiBold,
            TextAlign::Center,
            t.fg2,
        );
    } else if game.stage_banner_sec > 0.0 {
        let banner = if game.stage % 4 == 0 {
            format!("{} {}", ctx.tr("DREADNOUGHT SECTOR", "СЕКТОР ДРЕДНОУТА"), game.stage)
        } else {
            format!("{} {}", ctx.tr("SECTOR", "СЕКТОР"), game.stage)
        };
        ctx.draw_text(
            arena_cx,
            lay.arena_y + lay.arena_h * 0.48,
            &banner,
            SPEC.font_banner,
            FontWeight::Bold,
            TextAlign::Center,
            t.fg,
        );
    } else if game.callout_sec > 0.0 && !game.callout_text.is_empty() {
        ctx.draw_text(
            arena_cx,
            lay.ship_y - 46.0,
            &game.callout_text,
            SPEC.font_value_md,
            FontWeight::Bold,
            TextAlign::Center,
            t.fg,
        );
    }

    // ---- 2. Right Tactical Telemetry Sidebar ----
    ctx.fill_rect(
        lay.sidebar_x,
        lay.sidebar_y,
        lay.sidebar_w,
        lay.sidebar_h,
        SPEC.frame_radius,
        t.bg,
    );
    ctx.stroke_rect(
        lay.sidebar_x,
        lay.sidebar_y,
        lay.sidebar_w,
        lay.sidebar_h,
        SPEC.frame_radius,
        SPEC.border_stroke,
        t.fg2,
    );

    let sx = lay.sidebar_x + SPEC.sidebar_pad;
    let sw = lay.sidebar_w - SPEC.sidebar_pad * 2.0;
    let right_x = sx + sw;
    let mut cy = lay.sidebar_y + SPEC.sidebar_pad + 18.0;

    // Score & Hi-Score
    ctx.draw_text(
        sx,
        cy,
        ctx.tr("SCORE", "РАХУНОК"),
        SPEC.font_caption,
        FontWeight::SemiBold,
        TextAlign::Left,
        t.fg2,
    );
    let chain_str = format!("x{}", game.chain);
    ctx.draw_text(
        right_x,
        cy,
        &chain_str,
        SPEC.font_label,
        FontWeight::Bold,
        TextAlign::Right,
        t.fg,
    );
    cy += 30.0;
    let score_str = format!("{:07}", game.score);
    ctx.draw_text(
        sx,
        cy,
        &score_str,
        SPEC.font_value_lg,
        FontWeight::Bold,
        TextAlign::Left,
        t.fg,
    );

    cy += 26.0;
    let hi_str = format!("{}: {:07}", ctx.tr("HI", "РЕК"), game.prefs.hi_score);
    ctx.draw_text(
        sx,
        cy,
        &hi_str,
        SPEC.font_caption,
        FontWeight::Medium,
        TextAlign::Left,
        t.fg2,
    );

    // Divider
    cy += SPEC.section_gap;
    ctx.draw_line(sx, cy, right_x, cy, SPEC.inner_stroke, t.fg1);

    // 4-Beat Musical Bar Metronome Pips (`1 · 2 · 3 · 4`)
    cy += SPEC.section_gap + 14.0;
    ctx.draw_text(
        sx,
        cy,
        ctx.tr("BAR PHASE", "ФАЗА ТАКТУ"),
        SPEC.font_caption,
        FontWeight::SemiBold,
        TextAlign::Left,
        t.fg2,
    );
    let bpm_str = format!("{:.0} BPM", game.last_tick.effective_bpm);
    ctx.draw_text(
        right_x,
        cy,
        &bpm_str,
        SPEC.font_caption,
        FontWeight::Bold,
        TextAlign::Right,
        t.fg,
    );

    cy += 10.0;
    let pip_w = (sw - SPEC.pip_gap * 3.0) * 0.25;
    for b in 0..4u8 {
        let px = sx + (b as f32) * (pip_w + SPEC.pip_gap);
        let is_cur = game.last_tick.beat_index == b;
        let fill_col = if is_cur {
            t.fg
        } else if b < game.last_tick.beat_index {
            t.fg2
        } else {
            t.bg
        };
        ctx.fill_rect(px, cy, pip_w, SPEC.pip_h, 4.0, fill_col);
        ctx.stroke_rect(px, cy, pip_w, SPEC.pip_h, 4.0, SPEC.inner_stroke, t.fg);
    }

    // Bar Charge Gauge (driven directly by BarTick::bar_phase [0.0, 4.0))
    cy += SPEC.pip_h + 10.0;
    ctx.fill_rect(sx, cy, sw, SPEC.gauge_h, 5.0, t.bg);
    ctx.stroke_rect(sx, cy, sw, SPEC.gauge_h, 5.0, SPEC.inner_stroke, t.fg2);
    let phase_ratio = (game.last_tick.bar_phase * 0.25).max(game.bar_charge).clamp(0.0, 1.0);
    let fill_w = (sw - 4.0) * phase_ratio;
    if fill_w > 1.0 {
        ctx.fill_rect(sx + 2.0, cy + 2.0, fill_w, SPEC.gauge_h - 4.0, 3.0, t.fg);
    }

    // Active Auto-Weapon Gear Card
    cy += SPEC.gauge_h + SPEC.section_gap + 14.0;
    ctx.draw_text(
        sx,
        cy,
        ctx.tr("AUTO-WEAPON (Y / HOLD A)", "АВТО-ЗБРОЯ (Y / A)"),
        SPEC.font_caption,
        FontWeight::SemiBold,
        TextAlign::Left,
        t.fg2,
    );
    cy += 8.0;
    let gear_label = if game.focus_held {
        ctx.tr("1/8 FOCUS VULCAN", "1/8 ВУЛКАН-ФОКУС")
    } else {
        match game.prefs.primary_gear {
            WeaponGear::BeatPulse => ctx.tr("1/4 BEAT PULSE", "1/4 ІМПУЛЬС ДОЛІ"),
            WeaponGear::BarCannon => ctx.tr("1/1 WAVE CANNON", "1/1 ХВИЛЬОВА ГАРМАТА"),
            WeaponGear::FocusEight => ctx.tr("1/8 FOCUS VULCAN", "1/8 ВУЛКАН-ФОКУС"),
        }
    };
    ctx.fill_rect(sx, cy, sw, 36.0, SPEC.card_radius, if game.focus_held { t.fg } else { t.bg });
    ctx.stroke_rect(sx, cy, sw, 36.0, SPEC.card_radius, SPEC.inner_stroke, t.fg);
    ctx.draw_text(
        sx + sw * 0.5,
        cy + 24.0,
        gear_label,
        SPEC.font_label,
        FontWeight::Bold,
        TextAlign::Center,
        if game.focus_held { t.bg } else { t.fg },
    );

    // Sector & Remaining Ships / Lives
    cy += 36.0 + SPEC.section_gap + 16.0;
    let sec_lbl = format!("{}: {}", ctx.tr("SECTOR", "СЕКТОР"), game.stage);
    ctx.draw_text(
        sx,
        cy,
        &sec_lbl,
        SPEC.font_label,
        FontWeight::Bold,
        TextAlign::Left,
        t.fg,
    );

    cy += 24.0;
    for i in 0..game.lives {
        let lx = sx + 12.0 + (i as f32) * 30.0;
        draw_starfighter(ctx, lx, cy, 0.58, t.fg, t.fg2, t.bg);
    }

    // Controls hint + Live Track Footer at bottom of Sidebar
    let footer_y = lay.sidebar_y + lay.sidebar_h - SPEC.sidebar_pad;
    let track_status = if music_playing {
        track_label
    } else {
        ctx.tr("120 BPM Internal Clock", "120 BPM Внутр. генератор")
    };
    ctx.draw_text(
        sx,
        footer_y - 22.0,
        ctx.tr("B/X: Phase Roll · L2/R2: Track", "B/X: Ривок · L2/R2: Трек"),
        SPEC.font_caption,
        FontWeight::Regular,
        TextAlign::Left,
        t.fg2,
    );
    ctx.draw_text(
        sx,
        footer_y,
        track_status,
        SPEC.font_caption,
        FontWeight::SemiBold,
        TextAlign::Left,
        t.fg,
    );

    ctx.present();
}

fn truncate_track_badge(ctx: &HostContext<'_>, raw: &str, max_px: f32) -> String {
    if ctx.measure_text(raw, SPEC.font_caption, FontWeight::SemiBold) <= max_px {
        return raw.to_string();
    }
    let mut out = String::new();
    for ch in raw.chars() {
        let mut probe = out.clone();
        probe.push(ch);
        probe.push('…');
        if ctx.measure_text(&probe, SPEC.font_caption, FontWeight::SemiBold) > max_px {
            break;
        }
        out.push(ch);
    }
    out.push('…');
    out
}

// ============================================================================
// Native J2ME-Style System Menu (`START` / `MENU`)
// ============================================================================

fn run_system_menu(
    ctx: &HostContext<'_>,
    game: &mut VanguardGame,
    tracker: &mut BarPhaseTracker,
    layout: ComputedLayout,
) -> bool {
    let mut sel = 0usize;
    loop {
        let music_playing = ctx.audio_status() == 1;
        let music_lbl = if music_playing {
            ctx.tr("Pause Music", "Пауза музики")
        } else {
            ctx.tr("Play Music", "Увімкнути музику")
        };
        let gear_val = match game.prefs.primary_gear {
            WeaponGear::BeatPulse => ctx.tr("1/4 Beat Pulse", "1/4 Імпульс"),
            WeaponGear::BarCannon => ctx.tr("1/1 Wave Cannon", "1/1 Гармата"),
            WeaponGear::FocusEight => ctx.tr("1/8 Focus", "1/8 Фокус"),
        };
        let diff_val = match game.prefs.difficulty {
            Difficulty::Cadet => ctx.tr("Cadet", "Кадет"),
            Difficulty::Veteran => ctx.tr("Veteran", "Ветеран"),
            Difficulty::Ace => ctx.tr("Ace", "Ас"),
        };
        let sync_str = format!("+{} ms", SYNC_OFFSETS_MS[game.prefs.latency_idx]);

        let items = [
            MenuItem::action(ctx.tr("Continue", "Продовжити")),
            MenuItem::action(music_lbl),
            MenuItem::action(ctx.tr("New Mission", "Новий виліт")),
            MenuItem::option(ctx.tr("Auto-Weapon", "Авто-зброя"), gear_val),
            MenuItem::option(ctx.tr("Difficulty", "Складність"), diff_val),
            MenuItem::option(ctx.tr("Audio Sync", "Синхронізація"), &sync_str),
            MenuItem::action(ctx.tr("Exit", "Вихід")),
        ];

        match ctx.show_menu("Pulsar Vanguard", &items, sel) {
            MenuOutcome::Cancelled => return false,
            MenuOutcome::ForceExit => return true,
            MenuOutcome::Selected(idx) => {
                sel = idx;
                match idx {
                    0 => return false,
                    1 => {
                        if ctx.audio_status() == 0 {
                            // Start first available library track if nothing was loaded
                            if ctx.library_track_count() > 0 {
                                let pick = (ctx.ticks_ms() as usize) % ctx.library_track_count();
                                if let Some(lt) = ctx.library_track_at(pick) {
                                    let _ = ctx.audio_play_track(&lt.path, 0);
                                }
                            }
                        } else {
                            ctx.audio_toggle_play_pause();
                        }
                    }
                    2 => {
                        game.restart(layout);
                        return false;
                    }
                    3 => {
                        game.prefs.primary_gear = match game.prefs.primary_gear {
                            WeaponGear::BeatPulse => WeaponGear::BarCannon,
                            _ => WeaponGear::BeatPulse,
                        };
                        game.prefs.save(ctx);
                    }
                    4 => {
                        game.prefs.difficulty = match game.prefs.difficulty {
                            Difficulty::Cadet => Difficulty::Veteran,
                            Difficulty::Veteran => Difficulty::Ace,
                            Difficulty::Ace => Difficulty::Cadet,
                        };
                        game.prefs.save(ctx);
                    }
                    5 => {
                        game.prefs.latency_idx =
                            (game.prefs.latency_idx + 1) % SYNC_OFFSETS_MS.len();
                        tracker.set_audio_latency(game.prefs.latency_sec());
                        game.prefs.save(ctx);
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

fn run_pulsar_vanguard(ctx: &mut HostContext<'_>, _file_path: Option<&Path>) -> i32 {
    let prefs = SavedPrefs::load(ctx);
    let (init_w, init_h) = ctx.responsive_size();
    let init_layout = ComputedLayout::compute(init_w, init_h);

    let mut cfg = BarPhaseConfig::default();
    cfg.audio_latency_sec = prefs.latency_sec();
    let mut tracker = BarPhaseTracker::new(cfg);
    let mut pcm_analyzer = LivePcmAnalyzer::new();
    let mut pcm_window = [0i16; 1024 * 2];

    let mut game = VanguardGame::new(prefs, (ctx.ticks_ms() as u32) ^ 0xA5C3_19E7, init_layout);

    let mut last_frame_ms = ctx.ticks_ms();
    let mut last_track_poll_ms = 0u64;
    let mut current_track_path = String::new();
    let mut track_badge = String::new();

    loop {
        let now_ms = ctx.ticks_ms();
        let raw_dt = ((now_ms.saturating_sub(last_frame_ms)) as f32) * 0.001;
        let dt_sec = raw_dt.clamp(1.0 / 240.0, 0.05);
        last_frame_ms = now_ms;

        let (win_w, win_h) = ctx.responsive_size();
        let layout = ComputedLayout::compute(win_w, win_h);

        let inp = ctx.poll_input(0);
        if inp.should_exit {
            game.prefs.save(ctx);
            return 0;
        }
        if inp.menu_requested {
            if run_system_menu(ctx, &mut game, &mut tracker, layout) {
                game.prefs.save(ctx);
                return 0;
            }
            last_frame_ms = ctx.ticks_ms();
            continue;
        }

        // Quick track skip on L2 / R2
        if inp.pressed(btn::L2) {
            ctx.audio_prev();
        } else if inp.pressed(btn::R2) {
            ctx.audio_next();
        }

        // Toggle primary gear (1/4 Beat Pulse <-> 1/1 Wave Cannon) with Y
        if inp.pressed(btn::Y) {
            game.prefs.primary_gear = match game.prefs.primary_gear {
                WeaponGear::BeatPulse => WeaponGear::BarCannon,
                _ => WeaponGear::BeatPulse,
            };
            game.prefs.save(ctx);
        }

        // Restart immediately with A on Game Over
        if game.game_over && inp.pressed(btn::A) {
            game.restart(layout);
        }

        // Poll current background track metadata ~4x per second
        let astat = ctx.audio_status();
        let music_playing = astat == 1;
        if now_ms.saturating_sub(last_track_poll_ms) >= 250 {
            last_track_poll_ms = now_ms;
            if let Some(info) = ctx.audio_track_info() {
                if info.path != current_track_path {
                    current_track_path = info.path.clone();
                    tracker.reset(120.0);
                    let raw_title = if !info.title.is_empty() {
                        info.title
                    } else {
                        info.path
                            .rsplit('/')
                            .next()
                            .unwrap_or("Track")
                            .to_string()
                    };
                    track_badge =
                        truncate_track_badge(ctx, &raw_title, layout.sidebar_w - SPEC.sidebar_pad * 2.0);
                }
            } else {
                current_track_path.clear();
                track_badge.clear();
            }
        }

        // Read live pre-volume PCM from audio sink and extract 40..120 Hz bass + flux + VU
        let (low_energy, flux_hint, vu_level) = if music_playing {
            let n_frames = ctx.audio_read_pcm(0, &mut pcm_window);
            if n_frames > 0 {
                pcm_analyzer.analyze_window(&pcm_window, n_frames, dt_sec)
            } else {
                (0.0, 0.0, 0.35)
            }
        } else {
            // Internal 120 BPM silent pulsar clock so the game remains 100% playable when music is paused/stopped
            (0.0, 0.0, 0.45)
        };

        let mut move_axis = 0.0_f32;
        if inp.held(btn::LEFT | btn::L) {
            move_axis -= 1.0;
        }
        if inp.held(btn::RIGHT | btn::R) {
            move_axis += 1.0;
        }

        let focus_held = !game.game_over && inp.held(btn::A);
        let dodge_pressed = inp.pressed(btn::B | btn::X);

        game.step_frame(
            &mut tracker,
            SyncSource::LivePll,
            AudioFrameInput {
                dt_sec,
                playback_pos_sec: None,
                low_energy,
                flux_hint,
                vu_level,
            },
            move_axis,
            focus_held,
            dodge_pressed,
            layout,
            ctx.is_ukrainian(),
        );

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
    id: "pulsar_vanguard",
    name: "Pulsar Vanguard",
    name_uk: "Pulsar Vanguard",
    author: "UIPod",
    version: "0.1.0",
    category: PluginCategory::Game,
    kind: PluginKind::MenuDriven,
    extensions: "",
    entry: run_pulsar_vanguard,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn auto_weapons_fire_on_bar_beat_and_focus_eighth_triggers() {
        let lay = ComputedLayout::compute(1024.0, 768.0);
        let mut prefs = SavedPrefs::default();
        prefs.primary_gear = WeaponGear::BeatPulse;

        let mut game = VanguardGame::new(prefs, 42, lay);
        let mut tracker = BarPhaseTracker::new(BarPhaseConfig::default());

        // Step 60 frames (~1 second at 120 BPM = 2 quarter-note beats)
        let mut total_spawned = 0usize;
        for _ in 0..65 {
            let prev_n = game.player_shots.len();
            game.step_frame(
                &mut tracker,
                SyncSource::LivePll,
                AudioFrameInput {
                    dt_sec: 1.0 / 60.0,
                    playback_pos_sec: None,
                    low_energy: 0.3,
                    flux_hint: 0.2,
                    vu_level: 0.6,
                },
                0.0,
                false,
                false,
                lay,
                false,
            );
            if game.player_shots.len() > prev_n {
                total_spawned += game.player_shots.len() - prev_n;
            }
        }
        assert!(total_spawned >= 2, "Expected auto-fired BeatPulse shots, got {total_spawned}");

        // Switch to BarCannon (1/1) and step across a full 4-beat bar (~2.1s)
        game.prefs.primary_gear = WeaponGear::BarCannon;
        let mut fired_beam = false;
        for _ in 0..135 {
            game.step_frame(
                &mut tracker,
                SyncSource::LivePll,
                AudioFrameInput {
                    dt_sec: 1.0 / 60.0,
                    playback_pos_sec: None,
                    low_energy: 0.4,
                    flux_hint: 0.3,
                    vu_level: 0.85,
                },
                0.0,
                false,
                false,
                lay,
                false,
            );
            if !game.wave_beams.is_empty() {
                fired_beam = true;
            }
        }
        assert!(fired_beam, "Expected 1/1 Wave Cannon beam to fire on bar boundary");
    }
}
