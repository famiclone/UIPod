//! Classic iPod-inspired `Brick Breaker` game plugin for UIPod (`PluginKind::MenuDriven`).
//!
//! Uses UIPod's J2ME-style native System Menu (`MENU` / `START`) for `Continue`,
//! `New Game`, `Save Game`, `Options` (`Ball Speed`, `Paddle Size`, `Reset High Score`),
//! and `Exit`.

use std::path::Path;

use uipod_plugin_sdk::{
    btn, uipod_declare_plugin, FontWeight, HostContext, MenuItem, MenuOutcome, PluginCategory,
    PluginKind, TextAlign, CONTENT_TOP_Y, SCREEN_HEIGHT, SCREEN_WIDTH,
};

const COLS: usize = 10;
const ROWS: usize = 6;
const ARENA_X: f32 = 32.0;
const ARENA_Y: f32 = CONTENT_TOP_Y + 48.0;
const ARENA_W: f32 = (SCREEN_WIDTH as f32) - 64.0; // 960.0
const ARENA_H: f32 = (SCREEN_HEIGHT as f32) - ARENA_Y - 32.0;

#[derive(Clone, Copy)]
struct BrickOptions {
    fast_ball: bool,
    wide_paddle: bool,
}

impl Default for BrickOptions {
    fn default() -> Self {
        Self {
            fast_ball: false,
            wide_paddle: false,
        }
    }
}

impl BrickOptions {
    fn ball_mult(self) -> f32 {
        if self.fast_ball {
            1.22
        } else {
            1.0
        }
    }

    fn base_paddle_w(self, level: u32) -> f32 {
        let base = if self.wide_paddle { 204.0 } else { 164.0 };
        let min_w = if self.wide_paddle { 142.0 } else { 112.0 };
        (base - (level.saturating_sub(1) as f32) * 8.0).clamp(min_w, base)
    }
}

struct BrickGame {
    bricks: [[u8; COLS]; ROWS],
    paddle_x: f32,
    paddle_w: f32,
    ball_x: f32,
    ball_y: f32,
    ball_vx: f32,
    ball_vy: f32,
    ball_stuck: bool,
    lives: u8,
    level: u32,
    score: u32,
    high_score: u32,
    game_over: bool,
    opts: BrickOptions,
}

impl BrickGame {
    fn new(high_score: u32, opts: BrickOptions) -> Self {
        let pw = opts.base_paddle_w(1);
        let mut g = Self {
            bricks: [[0; COLS]; ROWS],
            paddle_x: ARENA_X + (ARENA_W - pw) * 0.5,
            paddle_w: pw,
            ball_x: 0.0,
            ball_y: 0.0,
            ball_vx: 5.5,
            ball_vy: -7.5,
            ball_stuck: true,
            lives: 3,
            level: 1,
            score: 0,
            high_score,
            game_over: false,
            opts,
        };
        g.populate_level();
        g.reset_ball();
        g
    }

    fn apply_options(&mut self, opts: BrickOptions) {
        self.opts = opts;
        self.paddle_w = opts.base_paddle_w(self.level);
        self.paddle_x = self
            .paddle_x
            .clamp(ARENA_X + 8.0, ARENA_X + ARENA_W - self.paddle_w - 8.0);
        if self.ball_stuck {
            self.reset_ball();
        }
    }

    fn populate_level(&mut self) {
        for r in 0..ROWS {
            for c in 0..COLS {
                let hp = match r {
                    0 => 3,
                    1 | 2 => 2,
                    _ => 1,
                };
                let skip = (self.level % 3 == 2 && (r + c) % 5 == 0)
                    || (self.level % 3 == 0 && c == 4 && r == 2);
                self.bricks[r][c] = if skip { 0 } else { hp };
            }
        }
        self.paddle_w = self.opts.base_paddle_w(self.level);
    }

    fn reset_ball(&mut self) {
        self.ball_stuck = true;
        self.ball_x = self.paddle_x + self.paddle_w * 0.5;
        self.ball_y = ARENA_Y + ARENA_H - 38.0;
        let speed = ((8.6 + (self.level.saturating_sub(1) as f32) * 0.65) * self.opts.ball_mult())
            .min(15.5);
        self.ball_vx = speed * 0.55;
        self.ball_vy = -speed * 0.84;
    }

    fn remaining_bricks(&self) -> usize {
        self.bricks
            .iter()
            .flat_map(|r| r.iter())
            .filter(|&&hp| hp > 0)
            .count()
    }

    fn step(&mut self, held: u32) -> bool {
        if self.game_over {
            return false;
        }
        let mut hs_changed = false;
        let boost = if (held & (btn::L | btn::R)) != 0 {
            15.5
        } else {
            10.5
        };
        if (held & btn::LEFT) != 0 {
            self.paddle_x = (self.paddle_x - boost).max(ARENA_X + 8.0);
        }
        if (held & btn::RIGHT) != 0 {
            self.paddle_x = (self.paddle_x + boost).min(ARENA_X + ARENA_W - self.paddle_w - 8.0);
        }

        let paddle_y = ARENA_Y + ARENA_H - 26.0;
        let ball_r = 9.0_f32;

        if self.ball_stuck {
            self.ball_x = self.paddle_x + self.paddle_w * 0.5;
            self.ball_y = paddle_y - ball_r - 2.0;
            return false;
        }

        self.ball_x += self.ball_vx;
        self.ball_y += self.ball_vy;

        if self.ball_x - ball_r <= ARENA_X + 4.0 {
            self.ball_x = ARENA_X + 4.0 + ball_r;
            self.ball_vx = self.ball_vx.abs();
        } else if self.ball_x + ball_r >= ARENA_X + ARENA_W - 4.0 {
            self.ball_x = ARENA_X + ARENA_W - 4.0 - ball_r;
            self.ball_vx = -self.ball_vx.abs();
        }
        if self.ball_y - ball_r <= ARENA_Y + 4.0 {
            self.ball_y = ARENA_Y + 4.0 + ball_r;
            self.ball_vy = self.ball_vy.abs();
        }

        // Paddle collision
        if self.ball_vy > 0.0
            && self.ball_y + ball_r >= paddle_y
            && self.ball_y - ball_r <= paddle_y + 16.0
            && self.ball_x >= self.paddle_x - ball_r * 0.6
            && self.ball_x <= self.paddle_x + self.paddle_w + ball_r * 0.6
        {
            let rel = ((self.ball_x - (self.paddle_x + self.paddle_w * 0.5))
                / (self.paddle_w * 0.5))
                .clamp(-1.0, 1.0);
            let min_spd = 8.6 * self.opts.ball_mult();
            let speed = (self.ball_vx * self.ball_vx + self.ball_vy * self.ball_vy)
                .sqrt()
                .clamp(min_spd, 15.8);
            self.ball_vx = rel * speed * 0.78;
            let vy_sq = (speed * speed - self.ball_vx * self.ball_vx).max(16.0);
            self.ball_vy = -vy_sq.sqrt();
            self.ball_y = paddle_y - ball_r - 1.0;
        }

        // Brick collisions
        let pad_x = 18.0_f32;
        let pad_top = 22.0_f32;
        let gap = 8.0_f32;
        let bw = (ARENA_W - pad_x * 2.0 - gap * ((COLS - 1) as f32)) / (COLS as f32);
        let bh = 28.0_f32;

        'outer: for r in 0..ROWS {
            for c in 0..COLS {
                let hp = self.bricks[r][c];
                if hp == 0 {
                    continue;
                }
                let bx = ARENA_X + pad_x + (c as f32) * (bw + gap);
                let by = ARENA_Y + pad_top + (r as f32) * (bh + gap);
                if self.ball_x + ball_r >= bx
                    && self.ball_x - ball_r <= bx + bw
                    && self.ball_y + ball_r >= by
                    && self.ball_y - ball_r <= by + bh
                {
                    self.bricks[r][c] = hp - 1;
                    self.score += if self.bricks[r][c] == 0 { 25 } else { 10 };
                    if self.score > self.high_score {
                        self.high_score = self.score;
                        hs_changed = true;
                    }

                    let overlap_left = (self.ball_x + ball_r) - bx;
                    let overlap_right = (bx + bw) - (self.ball_x - ball_r);
                    let overlap_top = (self.ball_y + ball_r) - by;
                    let overlap_bottom = (by + bh) - (self.ball_y - ball_r);
                    let min_x = overlap_left.min(overlap_right);
                    let min_y = overlap_top.min(overlap_bottom);
                    if min_x < min_y {
                        self.ball_vx = -self.ball_vx;
                    } else {
                        self.ball_vy = -self.ball_vy;
                    }
                    break 'outer;
                }
            }
        }

        if self.remaining_bricks() == 0 {
            self.level += 1;
            self.populate_level();
            self.reset_ball();
        } else if self.ball_y - ball_r > ARENA_Y + ARENA_H {
            if self.lives > 1 {
                self.lives -= 1;
                self.reset_ball();
            } else {
                self.lives = 0;
                self.game_over = true;
            }
        }

        hs_changed
    }

    fn serialize(&self) -> String {
        let mut s = format!(
            "V2 {} {} {} {} {} {} {}\n",
            self.high_score,
            u8::from(self.opts.fast_ball),
            u8::from(self.opts.wide_paddle),
            self.score,
            self.level,
            self.lives,
            u8::from(self.game_over)
        );
        if !self.game_over {
            s.push_str(&format!(
                "{:.2} {:.2} {:.2} {:.2} {:.2} {}\n",
                self.paddle_x,
                self.ball_x,
                self.ball_y,
                self.ball_vx,
                self.ball_vy,
                u8::from(self.ball_stuck)
            ));
            for r in 0..ROWS {
                for c in 0..COLS {
                    s.push(char::from(b'0' + self.bricks[r][c]));
                }
                s.push('\n');
            }
        }
        s
    }

    fn deserialize(raw: &str) -> Option<Self> {
        let mut lines = raw.lines();
        let hdr: Vec<&str> = lines.next()?.split_whitespace().collect();
        if hdr.first().copied() != Some("V2") || hdr.len() < 8 {
            let hs = raw.trim().parse::<u32>().ok()?;
            return Some(Self::new(hs, BrickOptions::default()));
        }
        let hs = hdr[1].parse::<u32>().unwrap_or(0);
        let opts = BrickOptions {
            fast_ball: hdr[2].parse::<u8>().unwrap_or(0) != 0,
            wide_paddle: hdr[3].parse::<u8>().unwrap_or(0) != 0,
        };
        let score = hdr[4].parse::<u32>().unwrap_or(0);
        let level = hdr[5].parse::<u32>().unwrap_or(1).max(1);
        let lives = hdr[6].parse::<u8>().unwrap_or(3).clamp(1, 5);
        let over = hdr[7].parse::<u8>().unwrap_or(0) != 0;
        if over {
            return Some(Self::new(hs, opts));
        }

        let phys: Vec<&str> = lines.next()?.split_whitespace().collect();
        if phys.len() < 6 {
            return Some(Self::new(hs, opts));
        }
        let paddle_w = opts.base_paddle_w(level);
        let paddle_x = phys[0]
            .parse::<f32>()
            .unwrap_or(ARENA_X + (ARENA_W - paddle_w) * 0.5)
            .clamp(ARENA_X + 8.0, ARENA_X + ARENA_W - paddle_w - 8.0);
        let ball_x = phys[1].parse::<f32>().unwrap_or(paddle_x + paddle_w * 0.5);
        let ball_y = phys[2].parse::<f32>().unwrap_or(ARENA_Y + ARENA_H - 38.0);
        let ball_vx = phys[3].parse::<f32>().unwrap_or(5.5);
        let ball_vy = phys[4].parse::<f32>().unwrap_or(-7.5);
        let ball_stuck = phys[5].parse::<u8>().unwrap_or(1) != 0;

        let mut bricks = [[0u8; COLS]; ROWS];
        let mut any = false;
        for row in &mut bricks {
            let rline = lines.next()?;
            for (c, ch) in rline.bytes().take(COLS).enumerate() {
                if (b'1'..=b'3').contains(&ch) {
                    row[c] = ch - b'0';
                    any = true;
                }
            }
        }
        if !any {
            return Some(Self::new(hs, opts));
        }
        Some(Self {
            bricks,
            paddle_x,
            paddle_w,
            ball_x,
            ball_y,
            ball_vx,
            ball_vy,
            ball_stuck,
            lives,
            level,
            score,
            high_score: hs.max(score),
            game_over: false,
            opts,
        })
    }
}

fn load_game(ctx: &HostContext) -> BrickGame {
    if let Some(raw) = ctx
        .load_plugin_data("brick_breaker", "state.txt")
        .and_then(|b| String::from_utf8(b).ok())
    {
        if let Some(g) = BrickGame::deserialize(&raw) {
            return g;
        }
    }
    let legacy_hs = ctx
        .load_plugin_data("brick_breaker", "highscore.txt")
        .and_then(|b| String::from_utf8(b).ok())
        .and_then(|s| s.trim().parse::<u32>().ok())
        .unwrap_or(0);
    BrickGame::new(legacy_hs, BrickOptions::default())
}

fn save_game(ctx: &HostContext, game: &BrickGame) {
    let _ = ctx.save_plugin_data("brick_breaker", "state.txt", game.serialize().as_bytes());
}

/// Returns `true` if the plugin should exit immediately.
fn open_options_menu(ctx: &HostContext, game: &mut BrickGame) -> bool {
    let mut sel = 0usize;
    loop {
        let spd_val = if game.opts.fast_ball {
            ctx.tr("Fast", "Швидка")
        } else {
            ctx.tr("Normal", "Звичайна")
        };
        let pad_val = if game.opts.wide_paddle {
            ctx.tr("Wide", "Широка")
        } else {
            ctx.tr("Normal", "Звичайна")
        };
        let best_str = game.high_score.to_string();
        let items = [
            MenuItem::option(ctx.tr("Ball Speed", "Швидкість м'яча"), spd_val),
            MenuItem::option(ctx.tr("Paddle Size", "Розмір платформи"), pad_val),
            MenuItem::option(ctx.tr("Reset Best Score", "Скинути рекорд"), &best_str),
        ];
        match ctx.show_menu(ctx.tr("Options", "Налаштування"), &items, sel) {
            MenuOutcome::Selected(0) => {
                sel = 0;
                let mut o = game.opts;
                o.fast_ball = !o.fast_ball;
                game.apply_options(o);
                save_game(ctx, game);
            }
            MenuOutcome::Selected(1) => {
                sel = 1;
                let mut o = game.opts;
                o.wide_paddle = !o.wide_paddle;
                game.apply_options(o);
                save_game(ctx, game);
            }
            MenuOutcome::Selected(2) => {
                sel = 2;
                game.high_score = game.score;
                save_game(ctx, game);
                ctx.show_splash(ctx.tr("Best score reset", "Рекорд скинуто"), 700);
            }
            MenuOutcome::Selected(_) | MenuOutcome::Cancelled => return false,
            MenuOutcome::ForceExit => return true,
        }
    }
}

/// Returns `true` if the user chose `Exit` (or triggered `ForceExit`).
fn open_system_menu(ctx: &HostContext, game: &mut BrickGame) -> bool {
    let mut sel = 0usize;
    loop {
        let title = ctx.tr("Brick Breaker", "Цеглинки");
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
                *game = BrickGame::new(game.high_score, game.opts);
                save_game(ctx, game);
                return false;
            }
            MenuOutcome::Selected(2) => {
                sel = 2;
                save_game(ctx, game);
                ctx.show_splash(ctx.tr("Game saved", "Гру збережено"), 700);
            }
            MenuOutcome::Selected(3) => {
                sel = 3;
                if open_options_menu(ctx, game) {
                    save_game(ctx, game);
                    return true;
                }
            }
            MenuOutcome::Selected(_) | MenuOutcome::ForceExit => {
                save_game(ctx, game);
                return true;
            }
        }
    }
}

fn run_brick_breaker(ctx: &mut HostContext, _file: Option<&Path>) -> i32 {
    let mut game = load_game(ctx);
    let mut dirty = false;

    loop {
        let frame_start = ctx.ticks_ms();
        let inp = ctx.poll_input(0);
        if inp.should_exit {
            save_game(ctx, &game);
            return 0;
        }
        if inp.menu_requested {
            if open_system_menu(ctx, &mut game) {
                return 0;
            }
            dirty = false;
            continue;
        }

        if inp.pressed(btn::A) {
            if game.game_over {
                game = BrickGame::new(game.high_score, game.opts);
                dirty = true;
            } else if game.ball_stuck {
                game.ball_stuck = false;
            }
        }

        if game.step(inp.held) {
            dirty = true;
        }

        let t = ctx.theme();
        let title = ctx.tr("Brick Breaker", "Цеглинки");
        ctx.begin_frame(t.bg, true, title);

        let hud_y = CONTENT_TOP_Y + 16.0;
        let score_lbl = format!(
            "{}: {}   {}: {}   {}: {}",
            ctx.tr("Score", "Рахунок"),
            game.score,
            ctx.tr("Best", "Рекорд"),
            game.high_score,
            ctx.tr("Lv", "Рів"),
            game.level
        );
        ctx.draw_text(
            ARENA_X,
            hud_y,
            &score_lbl,
            28.0,
            FontWeight::SemiBold,
            TextAlign::Left,
            t.fg,
        );

        // Lives dots on right of HUD
        for i in 0..3u8 {
            let cx = ARENA_X + ARENA_W - 12.0 - (i as f32) * 28.0;
            if i < game.lives {
                ctx.fill_circle(cx, hud_y, 9.0, t.fg);
            } else {
                ctx.stroke_rect(cx - 9.0, hud_y - 9.0, 18.0, 18.0, 9.0, 3.0, t.fg1);
            }
        }

        ctx.stroke_rect(ARENA_X, ARENA_Y, ARENA_W, ARENA_H, 8.0, 4.0, t.fg1);

        let pad_x = 18.0_f32;
        let pad_top = 22.0_f32;
        let gap = 8.0_f32;
        let bw = (ARENA_W - pad_x * 2.0 - gap * ((COLS - 1) as f32)) / (COLS as f32);
        let bh = 28.0_f32;
        for r in 0..ROWS {
            for c in 0..COLS {
                let hp = game.bricks[r][c];
                if hp == 0 {
                    continue;
                }
                let bx = ARENA_X + pad_x + (c as f32) * (bw + gap);
                let by = ARENA_Y + pad_top + (r as f32) * (bh + gap);
                let color = match hp {
                    3 => t.fg,
                    2 => t.fg2,
                    _ => t.fg1,
                };
                ctx.fill_rect(bx, by, bw, bh, 5.0, color);
            }
        }

        let paddle_y = ARENA_Y + ARENA_H - 26.0;
        ctx.fill_rect(game.paddle_x, paddle_y, game.paddle_w, 16.0, 8.0, t.fg);
        ctx.fill_circle(game.ball_x, game.ball_y, 9.0, t.fg2);

        if game.game_over || game.ball_stuck {
            let msg = if game.game_over {
                ctx.tr(
                    "GAME OVER  —  A: Retry  •  Menu: Options",
                    "ГРУ ЗАКІНЧЕНО  —  A: Ще раз  •  Menu: Меню",
                )
            } else {
                ctx.tr(
                    "A: Launch Ball  •  Menu: Game Menu",
                    "A: Запуск м'яча  •  Menu: Меню гри",
                )
            };
            let box_w = (ctx.measure_text(msg, 30.0, FontWeight::SemiBold) + 56.0).min(900.0);
            let bx = ARENA_X + (ARENA_W - box_w) * 0.5;
            let by = ARENA_Y + ARENA_H * 0.52;
            ctx.fill_rect(bx, by, box_w, 64.0, 8.0, t.bg);
            ctx.stroke_rect(bx, by, box_w, 64.0, 8.0, 4.0, t.fg1);
            ctx.draw_text(
                ARENA_X + ARENA_W * 0.5,
                by + 32.0,
                msg,
                30.0,
                FontWeight::SemiBold,
                TextAlign::Center,
                t.fg,
            );
        }

        ctx.present();

        if dirty && game.ball_stuck {
            save_game(ctx, &game);
            dirty = false;
        }

        let elapsed = ctx.ticks_ms().saturating_sub(frame_start);
        if elapsed < 16 {
            ctx.sleep_ms((16 - elapsed) as u32);
        } else {
            ctx.sleep_ms(0);
        }
    }
}

uipod_declare_plugin! {
    id: "brick_breaker",
    name: "Brick Breaker",
    name_uk: "Цеглинки",
    author: "UIPod",
    version: "0.1.0",
    category: PluginCategory::Game,
    kind: PluginKind::MenuDriven,
    extensions: "",
    entry: run_brick_breaker,
}
