//! Native GPU-accelerated 4-shade `Snake` (`Змійка`) arcade game for UIPod (`PluginKind::MenuDriven`).
//!
//! Features a 2-step directional turn queue, full live-state save/resume,
//! and UIPod's J2ME-style native System Menu (`MENU` / `START`) for `Continue`,
//! `New Game`, `Save Game`, `Options` (`Arena Mode`, `Base Speed`, `Reset High Scores`),
//! and `Exit`.

use std::collections::VecDeque;
use std::path::Path;

use uipod_plugin_sdk::{
    btn, uipod_declare_plugin, FontWeight, HostContext, MenuItem, MenuOutcome, PluginCategory,
    PluginKind, TextAlign, CONTENT_TOP_Y, SCREEN_WIDTH,
};

const GRID_W: i32 = 20;
const GRID_H: i32 = 18;
const CELL_PX: f32 = 32.0;
const BOARD_PAD: f32 = 10.0;
const BOARD_X: f32 = 32.0;
const BOARD_Y: f32 = CONTENT_TOP_Y + 10.0;
const BOARD_OUTER_W: f32 = (GRID_W as f32) * CELL_PX + BOARD_PAD * 2.0; // 660
const BOARD_OUTER_H: f32 = (GRID_H as f32) * CELL_PX + BOARD_PAD * 2.0; // 596

const BONUS_DURATION_MS: u64 = 7_500;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Dir {
    Up,
    Down,
    Left,
    Right,
}

impl Dir {
    #[inline]
    const fn delta(self) -> (i32, i32) {
        match self {
            Self::Up => (0, -1),
            Self::Down => (0, 1),
            Self::Left => (-1, 0),
            Self::Right => (1, 0),
        }
    }

    #[inline]
    const fn is_opposite(self, other: Self) -> bool {
        matches!(
            (self, other),
            (Self::Up, Self::Down)
                | (Self::Down, Self::Up)
                | (Self::Left, Self::Right)
                | (Self::Right, Self::Left)
        )
    }

    const fn to_u8(self) -> u8 {
        match self {
            Self::Up => 0,
            Self::Down => 1,
            Self::Left => 2,
            Self::Right => 3,
        }
    }

    const fn from_u8(v: u8) -> Self {
        match v {
            0 => Self::Up,
            1 => Self::Down,
            2 => Self::Left,
            _ => Self::Right,
        }
    }
}

#[derive(Clone, Copy, Default)]
struct HighScores {
    walls: u32,
    wrap: u32,
}

impl HighScores {
    fn best_for(&self, wrap_mode: bool) -> u32 {
        if wrap_mode {
            self.wrap
        } else {
            self.walls
        }
    }

    fn update(&mut self, wrap_mode: bool, score: u32) -> bool {
        let slot = if wrap_mode {
            &mut self.wrap
        } else {
            &mut self.walls
        };
        if score > *slot {
            *slot = score;
            true
        } else {
            false
        }
    }
}

struct SnakeGame {
    body: VecDeque<(i32, i32)>,
    dir: Dir,
    turn_queue: [Option<Dir>; 2],
    food: (i32, i32),
    bonus: Option<((i32, i32), u64)>,
    apples_eaten: u32,
    score: u32,
    wrap_mode: bool,
    speed_preset: u8, // 0 = Normal, 1 = Fast, 2 = Turbo
    started: bool,
    game_over: bool,
    rng: u64,
}

impl SnakeGame {
    fn new(seed: u64, wrap_mode: bool, speed_preset: u8) -> Self {
        let mut body = VecDeque::with_capacity((GRID_W * GRID_H) as usize);
        let cx = GRID_W / 2;
        let cy = GRID_H / 2;
        body.push_back((cx, cy));
        body.push_back((cx - 1, cy));
        body.push_back((cx - 2, cy));
        body.push_back((cx - 3, cy));

        let mut g = Self {
            body,
            dir: Dir::Right,
            turn_queue: [None, None],
            food: (cx + 4, cy),
            bonus: None,
            apples_eaten: 0,
            score: 0,
            wrap_mode,
            speed_preset: speed_preset.min(2),
            started: false,
            game_over: false,
            rng: seed | 1,
        };
        g.food = g.spawn_free_cell(None).unwrap_or((cx + 4, cy));
        g
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

    fn queue_turn(&mut self, want: Dir) {
        let effective_last = self.turn_queue[1]
            .or(self.turn_queue[0])
            .unwrap_or(self.dir);
        if want == effective_last || want.is_opposite(effective_last) {
            return;
        }
        if self.turn_queue[0].is_none() {
            self.turn_queue[0] = Some(want);
        } else if self.turn_queue[1].is_none() {
            self.turn_queue[1] = Some(want);
        }
    }

    fn pop_turn(&mut self) -> Dir {
        if let Some(d) = self.turn_queue[0].take() {
            self.turn_queue[0] = self.turn_queue[1].take();
            d
        } else {
            self.dir
        }
    }

    fn speed_level(&self) -> u32 {
        (self.apples_eaten / 4) + 1 + (self.speed_preset as u32) * 2
    }

    fn step_interval_ms(&self, boosting: bool) -> u64 {
        let base_start = match self.speed_preset {
            0 => 132u64,
            1 => 104u64,
            _ => 80u64,
        };
        let ramp = ((self.apples_eaten / 4) as u64) * 7;
        let base = base_start.saturating_sub(ramp).max(56);
        if boosting {
            base.min(50)
        } else {
            base
        }
    }

    fn spawn_free_cell(&mut self, avoid: Option<(i32, i32)>) -> Option<(i32, i32)> {
        let mut free = Vec::with_capacity((GRID_W * GRID_H) as usize);
        for y in 0..GRID_H {
            for x in 0..GRID_W {
                let p = (x, y);
                if avoid == Some(p) || self.body.contains(&p) {
                    continue;
                }
                if let Some((bp, _)) = self.bonus {
                    if bp == p {
                        continue;
                    }
                }
                free.push(p);
            }
        }
        if free.is_empty() {
            None
        } else {
            let idx = (self.next_rand() as usize) % free.len();
            Some(free[idx])
        }
    }

    fn tick(&mut self, now_ms: u64, scores: &mut HighScores) -> bool {
        if !self.started || self.game_over {
            return false;
        }

        if let Some((_, exp)) = self.bonus {
            if now_ms >= exp {
                self.bonus = None;
            }
        }

        let next_dir = self.pop_turn();
        if !next_dir.is_opposite(self.dir) {
            self.dir = next_dir;
        }

        let (hx, hy) = *self.body.front().unwrap_or(&(0, 0));
        let (dx, dy) = self.dir.delta();
        let mut nx = hx + dx;
        let mut ny = hy + dy;

        if self.wrap_mode {
            nx = nx.rem_euclid(GRID_W);
            ny = ny.rem_euclid(GRID_H);
        } else if !(0..GRID_W).contains(&nx) || !(0..GRID_H).contains(&ny) {
            self.game_over = true;
            return false;
        }

        let next_head = (nx, ny);
        let eating_food = next_head == self.food;
        let eating_bonus = self.bonus.map(|(bp, _)| bp == next_head).unwrap_or(false);
        let grow = eating_food || eating_bonus;

        let hits_body = self
            .body
            .iter()
            .take(if grow {
                self.body.len()
            } else {
                self.body.len().saturating_sub(1)
            })
            .any(|&seg| seg == next_head);

        if hits_body {
            self.game_over = true;
            return false;
        }

        self.body.push_front(next_head);
        if !grow {
            self.body.pop_back();
        }

        let mut hs_changed = false;
        if eating_food {
            self.apples_eaten += 1;
            self.score += 10 + (self.speed_level() - 1) * 2;
            hs_changed |= scores.update(self.wrap_mode, self.score);

            match self.spawn_free_cell(None) {
                Some(cell) => self.food = cell,
                None => {
                    self.game_over = true;
                    return hs_changed;
                }
            }

            if self.apples_eaten % 5 == 0 && self.bonus.is_none() {
                if let Some(bcell) = self.spawn_free_cell(Some(self.food)) {
                    self.bonus = Some((bcell, now_ms + BONUS_DURATION_MS));
                }
            }
        } else if eating_bonus {
            self.bonus = None;
            self.score += 50;
            hs_changed |= scores.update(self.wrap_mode, self.score);
        }

        hs_changed
    }

    fn serialize(&self, scores: HighScores) -> String {
        let mut s = format!(
            "V2 {} {} {} {} {} {} {} {} {} {}\n",
            scores.walls,
            scores.wrap,
            u8::from(self.wrap_mode),
            self.speed_preset,
            self.score,
            self.apples_eaten,
            self.dir.to_u8(),
            self.food.0,
            self.food.1,
            u8::from(self.game_over || !self.started)
        );
        if self.started && !self.game_over {
            for &(bx, by) in &self.body {
                s.push_str(&format!("{bx},{by} "));
            }
            s.push('\n');
        }
        s
    }

    fn deserialize(raw: &str, seed: u64) -> Option<(Self, HighScores)> {
        let mut lines = raw.lines();
        let hdr: Vec<&str> = lines.next()?.split_whitespace().collect();
        if hdr.first().copied() != Some("V2") || hdr.len() < 11 {
            return None;
        }
        let hs = HighScores {
            walls: hdr[1].parse().unwrap_or(0),
            wrap: hdr[2].parse().unwrap_or(0),
        };
        let wrap_mode = hdr[3].parse::<u8>().unwrap_or(0) != 0;
        let speed_preset = hdr[4].parse::<u8>().unwrap_or(0).min(2);
        let score = hdr[5].parse::<u32>().unwrap_or(0);
        let apples = hdr[6].parse::<u32>().unwrap_or(0);
        let dir = Dir::from_u8(hdr[7].parse::<u8>().unwrap_or(3));
        let fx = hdr[8].parse::<i32>().unwrap_or(10).clamp(0, GRID_W - 1);
        let fy = hdr[9].parse::<i32>().unwrap_or(9).clamp(0, GRID_H - 1);
        let inactive = hdr[10].parse::<u8>().unwrap_or(1) != 0;
        if inactive {
            return Some((Self::new(seed, wrap_mode, speed_preset), hs));
        }

        let mut body = VecDeque::new();
        for tok in lines.next()?.split_whitespace() {
            let (xs, ys) = tok.split_once(',')?;
            let bx = xs.parse::<i32>().ok()?;
            let by = ys.parse::<i32>().ok()?;
            if (0..GRID_W).contains(&bx) && (0..GRID_H).contains(&by) {
                body.push_back((bx, by));
            }
        }
        if body.len() < 2 {
            return Some((Self::new(seed, wrap_mode, speed_preset), hs));
        }
        Some((
            Self {
                body,
                dir,
                turn_queue: [None, None],
                food: (fx, fy),
                bonus: None,
                apples_eaten: apples,
                score,
                wrap_mode,
                speed_preset,
                started: false, // Wait on ready banner after resume so player isn't ambushed!
                game_over: false,
                rng: seed | 1,
            },
            hs,
        ))
    }
}

fn load_state(ctx: &HostContext, seed: u64) -> (SnakeGame, HighScores) {
    if let Some(raw) = ctx
        .load_plugin_data("snake", "state.txt")
        .and_then(|b| String::from_utf8(b).ok())
    {
        if let Some(pair) = SnakeGame::deserialize(&raw, seed) {
            return pair;
        }
    }
    (SnakeGame::new(seed, false, 0), HighScores::default())
}

fn save_state(ctx: &HostContext, game: &SnakeGame, scores: HighScores) {
    let data = game.serialize(scores);
    let _ = ctx.save_plugin_data("snake", "state.txt", data.as_bytes());
}

fn open_options_menu(ctx: &HostContext, game: &mut SnakeGame, scores: &mut HighScores) -> bool {
    let mut sel = 0usize;
    loop {
        let mode_lbl = if game.wrap_mode {
            ctx.tr("Wrap", "Без стін")
        } else {
            ctx.tr("Walls", "Стіни")
        };
        let spd_lbl = match game.speed_preset {
            0 => ctx.tr("Normal", "Звичайна"),
            1 => ctx.tr("Fast", "Швидка"),
            _ => ctx.tr("Turbo", "Турбо"),
        };
        let best_str = scores.best_for(game.wrap_mode).to_string();
        let items = [
            MenuItem::option(ctx.tr("Arena Mode", "Режим арени"), mode_lbl),
            MenuItem::option(ctx.tr("Start Speed", "Початкова швидкість"), spd_lbl),
            MenuItem::option(ctx.tr("Reset Best Score", "Скинути рекорд"), &best_str),
        ];
        match ctx.show_menu(ctx.tr("Options", "Налаштування"), &items, sel) {
            MenuOutcome::Selected(0) => {
                sel = 0;
                let next_wrap = !game.wrap_mode;
                *game = SnakeGame::new(ctx.ticks_ms() ^ game.rng, next_wrap, game.speed_preset);
                save_state(ctx, game, *scores);
            }
            MenuOutcome::Selected(1) => {
                sel = 1;
                game.speed_preset = (game.speed_preset + 1) % 3;
                save_state(ctx, game, *scores);
            }
            MenuOutcome::Selected(2) => {
                sel = 2;
                *scores = HighScores::default();
                scores.update(game.wrap_mode, game.score);
                save_state(ctx, game, *scores);
                ctx.show_splash(ctx.tr("Best scores reset", "Рекорди скинуто"), 700);
            }
            MenuOutcome::Selected(_) | MenuOutcome::Cancelled => return false,
            MenuOutcome::ForceExit => return true,
        }
    }
}

fn open_system_menu(ctx: &HostContext, game: &mut SnakeGame, scores: &mut HighScores) -> bool {
    let mut sel = 0usize;
    loop {
        let title = ctx.tr("Snake", "Змійка");
        let items = [
            MenuItem::action(ctx.tr("Continue", "Продовжити")),
            MenuItem::action(ctx.tr("New Game", "Нова гра")),
            MenuItem::action(ctx.tr("Save Game", "Зберегти гру")),
            MenuItem::submenu(ctx.tr("Options", "Налаштування")),
            MenuItem::action(ctx.tr("Exit", "Вихід")),
        ];
        match ctx.show_menu(title, &items, sel) {
            MenuOutcome::Selected(0) | MenuOutcome::Cancelled => return false,
            MenuOutcome::Selected(1) => {
                *game = SnakeGame::new(
                    ctx.ticks_ms() ^ game.rng,
                    game.wrap_mode,
                    game.speed_preset,
                );
                save_state(ctx, game, *scores);
                return false;
            }
            MenuOutcome::Selected(2) => {
                sel = 2;
                save_state(ctx, game, *scores);
                ctx.show_splash(ctx.tr("Game saved", "Гру збережено"), 700);
            }
            MenuOutcome::Selected(3) => {
                sel = 3;
                if open_options_menu(ctx, game, scores) {
                    save_state(ctx, game, *scores);
                    return true;
                }
            }
            MenuOutcome::Selected(_) | MenuOutcome::ForceExit => {
                save_state(ctx, game, *scores);
                return true;
            }
        }
    }
}

fn draw_snake(ctx: &HostContext, game: &SnakeGame, scores: HighScores, now_ms: u64) {
    let t = ctx.theme();
    let title = ctx.tr("Snake", "Змійка");
    ctx.begin_frame(t.bg, true, title);

    ctx.stroke_rect(
        BOARD_X,
        BOARD_Y,
        BOARD_OUTER_W,
        BOARD_OUTER_H,
        10.0,
        if game.wrap_mode { 2.5 } else { 5.0 },
        if game.wrap_mode { t.fg1 } else { t.fg },
    );
    if !game.wrap_mode {
        ctx.stroke_rect(
            BOARD_X + 6.0,
            BOARD_Y + 6.0,
            BOARD_OUTER_W - 12.0,
            BOARD_OUTER_H - 12.0,
            6.0,
            2.0,
            t.fg1,
        );
    }

    let gx0 = BOARD_X + BOARD_PAD;
    let gy0 = BOARD_Y + BOARD_PAD;

    for gy in (1..GRID_H).step_by(2) {
        for gx in (1..GRID_W).step_by(2) {
            let px = gx0 + (gx as f32) * CELL_PX;
            let py = gy0 + (gy as f32) * CELL_PX;
            ctx.fill_rect(px - 1.5, py - 1.5, 3.0, 3.0, 0.0, t.fg1);
        }
    }

    {
        let fx = gx0 + (game.food.0 as f32 + 0.5) * CELL_PX;
        let fy = gy0 + (game.food.1 as f32 + 0.5) * CELL_PX;
        ctx.fill_circle(fx, fy + 1.0, 11.0, t.fg);
        ctx.fill_circle(fx - 3.5, fy - 2.5, 3.0, t.bg);
        ctx.fill_rect(fx - 1.5, fy - 14.0, 4.0, 5.0, 1.5, t.fg2);
    }

    if let Some(((bx, by), exp)) = game.bonus {
        let cx = gx0 + (bx as f32 + 0.5) * CELL_PX;
        let cy = gy0 + (by as f32 + 0.5) * CELL_PX;
        let blink_on = exp.saturating_sub(now_ms) > 1_800 || ((now_ms / 120) % 2 == 0);
        if blink_on {
            ctx.stroke_rect(cx - 12.0, cy - 12.0, 24.0, 24.0, 7.0, 3.5, t.fg);
            ctx.fill_circle(cx, cy, 6.5, t.fg2);
        }
    }

    let len = game.body.len();
    for (idx, &(sx, sy)) in game.body.iter().enumerate().rev() {
        let x = gx0 + (sx as f32) * CELL_PX;
        let y = gy0 + (sy as f32) * CELL_PX;
        if idx == 0 {
            ctx.fill_rect(x + 2.0, y + 2.0, CELL_PX - 4.0, CELL_PX - 4.0, 8.0, t.fg);
            let (e1, e2) = match game.dir {
                Dir::Right => ((x + 21.0, y + 10.0), (x + 21.0, y + 22.0)),
                Dir::Left => ((x + 11.0, y + 10.0), (x + 11.0, y + 22.0)),
                Dir::Up => ((x + 10.0, y + 11.0), (x + 22.0, y + 11.0)),
                Dir::Down => ((x + 10.0, y + 21.0), (x + 22.0, y + 21.0)),
            };
            ctx.fill_circle(e1.0, e1.1, 2.8, t.bg);
            ctx.fill_circle(e2.0, e2.1, 2.8, t.bg);
        } else {
            let pad = if idx + 1 == len { 5.5 } else { 3.5 };
            let shade = if idx % 2 == 0 { t.fg } else { t.fg2 };
            ctx.fill_rect(
                x + pad,
                y + pad,
                CELL_PX - pad * 2.0,
                CELL_PX - pad * 2.0,
                6.0,
                shade,
            );
        }
    }

    let side_x = BOARD_X + BOARD_OUTER_W + 24.0;
    let side_w = (SCREEN_WIDTH as f32) - side_x - 32.0;

    ctx.stroke_rect(side_x, BOARD_Y, side_w, 112.0, 10.0, 3.5, t.fg1);
    ctx.draw_text(
        side_x + side_w * 0.5,
        BOARD_Y + 30.0,
        ctx.tr("SCORE", "РАХУНОК"),
        24.0,
        FontWeight::SemiBold,
        TextAlign::Center,
        t.fg2,
    );
    ctx.draw_text(
        side_x + side_w * 0.5,
        BOARD_Y + 76.0,
        &game.score.to_string(),
        44.0,
        FontWeight::Bold,
        TextAlign::Center,
        t.fg,
    );

    let best_y = BOARD_Y + 128.0;
    ctx.stroke_rect(side_x, best_y, side_w, 112.0, 10.0, 3.5, t.fg1);
    ctx.draw_text(
        side_x + side_w * 0.5,
        best_y + 30.0,
        ctx.tr("BEST", "РЕКОРД"),
        24.0,
        FontWeight::SemiBold,
        TextAlign::Center,
        t.fg2,
    );
    ctx.draw_text(
        side_x + side_w * 0.5,
        best_y + 76.0,
        &scores.best_for(game.wrap_mode).to_string(),
        44.0,
        FontWeight::Bold,
        TextAlign::Center,
        t.fg,
    );

    let info_y = best_y + 128.0;
    ctx.stroke_rect(side_x, info_y, side_w, 116.0, 10.0, 3.5, t.fg1);
    let mode_lbl = if game.wrap_mode {
        ctx.tr("Mode: Wrap", "Режим: Без стін")
    } else {
        ctx.tr("Mode: Walls", "Режим: Стіни")
    };
    let spd_lbl = format!(
        "{}: {}   •   {}: {}",
        ctx.tr("Lv", "Рів"),
        game.speed_level(),
        ctx.tr("Len", "Довж"),
        game.body.len()
    );
    ctx.draw_text(
        side_x + side_w * 0.5,
        info_y + 36.0,
        mode_lbl,
        25.0,
        FontWeight::SemiBold,
        TextAlign::Center,
        t.fg,
    );
    ctx.draw_text(
        side_x + side_w * 0.5,
        info_y + 80.0,
        &spd_lbl,
        24.0,
        FontWeight::Medium,
        TextAlign::Center,
        t.fg2,
    );

    let ctrl_y = info_y + 136.0;
    if let Some((_, exp)) = game.bonus {
        let rem = exp.saturating_sub(now_ms) as f32 / (BONUS_DURATION_MS as f32);
        ctx.draw_text(
            side_x + side_w * 0.5,
            ctrl_y + 12.0,
            ctx.tr("BONUS +50", "БОНУС +50"),
            24.0,
            FontWeight::Bold,
            TextAlign::Center,
            t.fg,
        );
        ctx.draw_progress_bar(side_x, ctrl_y + 32.0, side_w, 20.0, rem.clamp(0.0, 1.0));
    }

    let hint_base = BOARD_Y + BOARD_OUTER_H - 94.0;
    let hints = [
        ctx.tr("D-Pad: Steer", "Хрестовина: Рух"),
        ctx.tr("A (hold): Boost", "A (утр.): Прискор."),
        ctx.tr("Menu / Start: Menu", "Menu / Start: Меню"),
    ];
    for (i, h) in hints.iter().enumerate() {
        ctx.draw_text(
            side_x + 4.0,
            hint_base + (i as f32) * 34.0,
            h,
            23.0,
            FontWeight::Medium,
            TextAlign::Left,
            t.fg2,
        );
    }

    if !game.started || game.game_over {
        let msg = if game.game_over {
            ctx.tr("GAME OVER  —  A: Retry", "ГРУ ЗАКІНЧЕНО  —  A: Ще раз")
        } else {
            ctx.tr("Press D-Pad or A to Start", "Хрестовина або A — Старт")
        };
        let bw =
            (ctx.measure_text(msg, 30.0, FontWeight::SemiBold) + 52.0).min(BOARD_OUTER_W - 40.0);
        let bx = BOARD_X + (BOARD_OUTER_W - bw) * 0.5;
        let by = BOARD_Y + BOARD_OUTER_H * 0.5 - 32.0;
        ctx.fill_rect(bx, by, bw, 64.0, 8.0, t.bg);
        ctx.stroke_rect(bx, by, bw, 64.0, 8.0, 4.0, t.fg);
        ctx.draw_text(
            BOARD_X + BOARD_OUTER_W * 0.5,
            by + 32.0,
            msg,
            30.0,
            FontWeight::SemiBold,
            TextAlign::Center,
            t.fg,
        );
    }

    ctx.present();
}

fn run_snake(ctx: &mut HostContext, _file: Option<&Path>) -> i32 {
    let seed = ctx.ticks_ms().wrapping_mul(0x9E37_79B9_7F4A_7C15);
    let (mut game, mut scores) = load_state(ctx, seed);
    let mut last_step_ms = ctx.ticks_ms();

    loop {
        let frame_start = ctx.ticks_ms();
        let inp = ctx.poll_input(0);

        if inp.should_exit {
            save_state(ctx, &game, scores);
            return 0;
        }
        if inp.menu_requested {
            if open_system_menu(ctx, &mut game, &mut scores) {
                return 0;
            }
            last_step_ms = ctx.ticks_ms();
            continue;
        }

        if inp.pressed(btn::A) {
            if game.game_over {
                game = SnakeGame::new(
                    frame_start ^ game.rng,
                    game.wrap_mode,
                    game.speed_preset,
                );
                game.started = true;
                last_step_ms = frame_start;
            } else if !game.started {
                game.started = true;
                last_step_ms = frame_start;
            }
        }

        let dpad_dir = if inp.pressed(btn::UP) {
            Some(Dir::Up)
        } else if inp.pressed(btn::DOWN) {
            Some(Dir::Down)
        } else if inp.pressed(btn::LEFT) {
            Some(Dir::Left)
        } else if inp.pressed(btn::RIGHT) {
            Some(Dir::Right)
        } else {
            None
        };

        if let Some(d) = dpad_dir {
            if !game.game_over {
                if !game.started {
                    game.started = true;
                    last_step_ms = frame_start;
                }
                game.queue_turn(d);
            }
        }

        let boosting = inp.held(btn::A) && game.started && !game.game_over;
        let step_ms = game.step_interval_ms(boosting);
        let now = ctx.ticks_ms();
        if game.started && !game.game_over && now.saturating_sub(last_step_ms) >= step_ms {
            last_step_ms = now;
            let _ = game.tick(now, &mut scores);
        }

        draw_snake(ctx, &game, scores, now);

        let elapsed = ctx.ticks_ms().saturating_sub(frame_start);
        if elapsed < 16 {
            ctx.sleep_ms((16 - elapsed) as u32);
        } else {
            ctx.sleep_ms(0);
        }
    }
}

uipod_declare_plugin! {
    id: "snake",
    name: "Snake",
    name_uk: "Змійка",
    author: "UIPod",
    version: "0.1.0",
    category: PluginCategory::Game,
    kind: PluginKind::MenuDriven,
    extensions: "",
    entry: run_snake,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn turn_queue_prevents_self_reversal_on_rapid_uturn() {
        let mut g = SnakeGame::new(42, false, 0);
        g.started = true;
        g.queue_turn(Dir::Up);
        g.queue_turn(Dir::Left);
        let mut hs = HighScores::default();
        g.tick(100, &mut hs);
        assert!(!g.game_over);
        assert_eq!(g.dir, Dir::Up);
        g.tick(200, &mut hs);
        assert!(!g.game_over);
        assert_eq!(g.dir, Dir::Left);
    }

    #[test]
    fn wrap_mode_wraps_around_board_edges() {
        let mut g = SnakeGame::new(7, true, 0);
        g.started = true;
        g.body.clear();
        g.body.push_back((GRID_W - 1, 5));
        g.body.push_back((GRID_W - 2, 5));
        g.dir = Dir::Right;
        g.food = (3, 3);
        let mut hs = HighScores::default();
        g.tick(100, &mut hs);
        assert!(!g.game_over);
        assert_eq!(g.body.front().copied(), Some((0, 5)));
    }
}
