//! 4-shade iPod Mini `2048` puzzle plugin for UIPod (`PluginKind::MenuDriven`).
//!
//! Uses `B` for 1-step Undo and UIPod's J2ME-style native System Menu (`MENU` / `START`)
//! for `Continue`, `New Game`, `Save Game`, `Options`, and `Exit`.

use std::path::Path;

use uipod_plugin_sdk::{
    btn, uipod_declare_plugin, FontWeight, HostContext, MenuItem, MenuOutcome, PluginCategory,
    PluginKind, TextAlign, CONTENT_TOP_Y, SCREEN_WIDTH,
};

struct Game2048 {
    grid: [[u32; 4]; 4],
    prev_grid: Option<([[u32; 4]; 4], u32)>,
    score: u32,
    best: u32,
    allow_undo: bool,
    rng: u64,
    game_over: bool,
}

impl Game2048 {
    fn new(seed: u64, best: u32, allow_undo: bool) -> Self {
        let mut g = Self {
            grid: [[0; 4]; 4],
            prev_grid: None,
            score: 0,
            best,
            allow_undo,
            rng: seed | 1,
            game_over: false,
        };
        g.spawn_tile();
        g.spawn_tile();
        g
    }

    fn next_rand(&mut self) -> u32 {
        self.rng ^= self.rng << 13;
        self.rng ^= self.rng >> 7;
        self.rng ^= self.rng << 17;
        (self.rng >> 16) as u32
    }

    fn spawn_tile(&mut self) {
        let mut empty = [(0usize, 0usize); 16];
        let mut n = 0usize;
        for r in 0..4 {
            for c in 0..4 {
                if self.grid[r][c] == 0 {
                    empty[n] = (r, c);
                    n += 1;
                }
            }
        }
        if n == 0 {
            return;
        }
        let idx = (self.next_rand() as usize) % n;
        let val = if (self.next_rand() % 10) == 0 { 4 } else { 2 };
        let (r, c) = empty[idx];
        self.grid[r][c] = val;
    }

    fn slide_line(line: [u32; 4]) -> ([u32; 4], u32) {
        let mut compact = [0u32; 4];
        let mut idx = 0;
        for v in line {
            if v != 0 {
                compact[idx] = v;
                idx += 1;
            }
        }
        let mut out = [0u32; 4];
        let mut gained = 0u32;
        let mut i = 0;
        let mut o = 0;
        while i < 4 {
            if compact[i] == 0 {
                break;
            }
            if i + 1 < 4 && compact[i] == compact[i + 1] {
                let merged = compact[i] * 2;
                out[o] = merged;
                gained += merged;
                i += 2;
            } else {
                out[o] = compact[i];
                i += 1;
            }
            o += 1;
        }
        (out, gained)
    }

    fn apply_move(&mut self, dir: usize) -> bool {
        let before = self.grid;
        let before_score = self.score;
        let mut total_gain = 0u32;

        match dir {
            0 => {
                for r in 0..4 {
                    let (nl, g) = Self::slide_line(self.grid[r]);
                    self.grid[r] = nl;
                    total_gain += g;
                }
            }
            1 => {
                for r in 0..4 {
                    let mut rev = self.grid[r];
                    rev.reverse();
                    let (mut nl, g) = Self::slide_line(rev);
                    nl.reverse();
                    self.grid[r] = nl;
                    total_gain += g;
                }
            }
            2 => {
                for c in 0..4 {
                    let col = [
                        self.grid[0][c],
                        self.grid[1][c],
                        self.grid[2][c],
                        self.grid[3][c],
                    ];
                    let (nl, g) = Self::slide_line(col);
                    for r in 0..4 {
                        self.grid[r][c] = nl[r];
                    }
                    total_gain += g;
                }
            }
            _ => {
                for c in 0..4 {
                    let col = [
                        self.grid[3][c],
                        self.grid[2][c],
                        self.grid[1][c],
                        self.grid[0][c],
                    ];
                    let (nl, g) = Self::slide_line(col);
                    for r in 0..4 {
                        self.grid[3 - r][c] = nl[r];
                    }
                    total_gain += g;
                }
            }
        }

        if self.grid != before {
            self.prev_grid = Some((before, before_score));
            self.score += total_gain;
            if self.score > self.best {
                self.best = self.score;
            }
            self.spawn_tile();
            self.game_over = !self.can_move();
            true
        } else {
            false
        }
    }

    fn undo(&mut self) -> bool {
        if !self.allow_undo {
            return false;
        }
        if let Some((prev, prev_score)) = self.prev_grid.take() {
            self.grid = prev;
            self.score = prev_score;
            self.game_over = !self.can_move();
            true
        } else {
            false
        }
    }

    fn can_move(&self) -> bool {
        for r in 0..4 {
            for c in 0..4 {
                let v = self.grid[r][c];
                if v == 0 {
                    return true;
                }
                if c + 1 < 4 && self.grid[r][c + 1] == v {
                    return true;
                }
                if r + 1 < 4 && self.grid[r + 1][c] == v {
                    return true;
                }
            }
        }
        false
    }

    fn serialize(&self) -> String {
        let mut s = format!(
            "{} {} {}\n",
            self.score,
            self.best,
            u8::from(self.allow_undo)
        );
        for r in 0..4 {
            s.push_str(&format!(
                "{} {} {} {}\n",
                self.grid[r][0], self.grid[r][1], self.grid[r][2], self.grid[r][3]
            ));
        }
        s
    }

    fn deserialize(raw: &str, seed: u64) -> Option<Self> {
        let mut lines = raw.lines();
        let mut hdr = lines.next()?.split_whitespace();
        let score: u32 = hdr.next()?.parse().ok()?;
        let best: u32 = hdr.next()?.parse().ok()?;
        let allow_undo = hdr
            .next()
            .and_then(|v| v.parse::<u8>().ok())
            .map(|v| v != 0)
            .unwrap_or(true);
        let mut grid = [[0u32; 4]; 4];
        let mut non_zero = false;
        for r in 0..4 {
            let mut row_it = lines.next()?.split_whitespace();
            for c in 0..4 {
                grid[r][c] = row_it.next()?.parse().ok()?;
                if grid[r][c] > 0 {
                    non_zero = true;
                }
            }
        }
        if !non_zero {
            return None;
        }
        let mut g = Self {
            grid,
            prev_grid: None,
            score,
            best,
            allow_undo,
            rng: seed | 1,
            game_over: false,
        };
        g.game_over = !g.can_move();
        Some(g)
    }
}

fn save_board(ctx: &HostContext, game: &Game2048) {
    let _ = ctx.save_plugin_data("uipod_2048", "board.txt", game.serialize().as_bytes());
}

fn open_options_menu(ctx: &HostContext, game: &mut Game2048) -> bool {
    let mut sel = 0usize;
    loop {
        let undo_lbl = if game.allow_undo {
            ctx.tr("On", "Увімк")
        } else {
            ctx.tr("Off", "Вимк")
        };
        let best_str = game.best.to_string();
        let items = [
            MenuItem::option(ctx.tr("Undo Move (B)", "Скасування ходу (B)"), undo_lbl),
            MenuItem::option(ctx.tr("Reset Best Score", "Скинути рекорд"), &best_str),
        ];
        match ctx.show_menu(ctx.tr("Options", "Налаштування"), &items, sel) {
            MenuOutcome::Selected(0) => {
                sel = 0;
                game.allow_undo = !game.allow_undo;
                if !game.allow_undo {
                    game.prev_grid = None;
                }
                save_board(ctx, game);
            }
            MenuOutcome::Selected(1) => {
                sel = 1;
                game.best = game.score;
                save_board(ctx, game);
                ctx.show_splash(ctx.tr("Best score reset", "Рекорд скинуто"), 700);
            }
            MenuOutcome::Selected(_) | MenuOutcome::Cancelled => return false,
            MenuOutcome::ForceExit => return true,
        }
    }
}

fn open_system_menu(ctx: &HostContext, game: &mut Game2048) -> bool {
    let mut sel = 0usize;
    loop {
        let items = [
            MenuItem::action(ctx.tr("Continue", "Продовжити")),
            MenuItem::action(ctx.tr("New Game", "Нова гра")),
            MenuItem::action(ctx.tr("Save Game", "Зберегти гру")),
            MenuItem::submenu(ctx.tr("Options", "Налаштування")),
            MenuItem::action(ctx.tr("Exit", "Вихід")),
        ];
        match ctx.show_menu("2048", &items, sel) {
            MenuOutcome::Selected(0) | MenuOutcome::Cancelled => return false,
            MenuOutcome::Selected(1) => {
                *game = Game2048::new(ctx.ticks_ms() ^ game.rng, game.best, game.allow_undo);
                save_board(ctx, game);
                return false;
            }
            MenuOutcome::Selected(2) => {
                sel = 2;
                save_board(ctx, game);
                ctx.show_splash(ctx.tr("Game saved", "Гру збережено"), 700);
            }
            MenuOutcome::Selected(3) => {
                sel = 3;
                if open_options_menu(ctx, game) {
                    save_board(ctx, game);
                    return true;
                }
            }
            MenuOutcome::Selected(_) | MenuOutcome::ForceExit => {
                save_board(ctx, game);
                return true;
            }
        }
    }
}

fn draw_2048(ctx: &HostContext, game: &Game2048) {
    let t = ctx.theme();
    ctx.begin_frame(t.bg, true, "2048");

    let board_size = 556.0_f32;
    let board_x = 40.0_f32;
    let board_y = CONTENT_TOP_Y + 20.0;

    ctx.stroke_rect(board_x, board_y, board_size, board_size, 12.0, 4.0, t.fg1);

    let gap = 14.0_f32;
    let cell = (board_size - gap * 5.0) / 4.0;

    for r in 0..4 {
        for c in 0..4 {
            let x = board_x + gap + (c as f32) * (cell + gap);
            let y = board_y + gap + (r as f32) * (cell + gap);
            let val = game.grid[r][c];

            if val == 0 {
                ctx.stroke_rect(x, y, cell, cell, 8.0, 3.0, t.fg1);
            } else {
                let (fill_c, txt_c) = match val {
                    2 | 4 => (t.fg1, t.fg),
                    8 | 16 | 32 => (t.fg2, t.bg),
                    _ => (t.fg, t.bg),
                };
                ctx.fill_rect(x, y, cell, cell, 8.0, fill_c);
                if val >= 512 {
                    ctx.stroke_rect(x + 6.0, y + 6.0, cell - 12.0, cell - 12.0, 5.0, 3.0, t.fg1);
                }
                let s = val.to_string();
                let fsz = if val >= 1024 {
                    34.0
                } else if val >= 128 {
                    40.0
                } else {
                    46.0
                };
                ctx.draw_text(
                    x + cell * 0.5,
                    y + cell * 0.5,
                    &s,
                    fsz,
                    FontWeight::Bold,
                    TextAlign::Center,
                    txt_c,
                );
            }
        }
    }

    let side_x = board_x + board_size + 32.0;
    let side_w = (SCREEN_WIDTH as f32) - side_x - 32.0;

    ctx.stroke_rect(side_x, board_y, side_w, 124.0, 10.0, 4.0, t.fg1);
    ctx.draw_text(
        side_x + side_w * 0.5,
        board_y + 34.0,
        ctx.tr("SCORE", "РАХУНОК"),
        26.0,
        FontWeight::SemiBold,
        TextAlign::Center,
        t.fg2,
    );
    ctx.draw_text(
        side_x + side_w * 0.5,
        board_y + 84.0,
        &game.score.to_string(),
        46.0,
        FontWeight::Bold,
        TextAlign::Center,
        t.fg,
    );

    let best_y = board_y + 144.0;
    ctx.stroke_rect(side_x, best_y, side_w, 124.0, 10.0, 4.0, t.fg1);
    ctx.draw_text(
        side_x + side_w * 0.5,
        best_y + 34.0,
        ctx.tr("BEST", "РЕКОРД"),
        26.0,
        FontWeight::SemiBold,
        TextAlign::Center,
        t.fg2,
    );
    ctx.draw_text(
        side_x + side_w * 0.5,
        best_y + 84.0,
        &game.best.to_string(),
        46.0,
        FontWeight::Bold,
        TextAlign::Center,
        t.fg,
    );

    let hint_y = best_y + 156.0;
    ctx.draw_text(
        side_x + 8.0,
        hint_y,
        ctx.tr("D-Pad: Slide tiles", "Хрестовина: Рух"),
        25.0,
        FontWeight::Medium,
        TextAlign::Left,
        t.fg2,
    );
    if game.allow_undo {
        ctx.draw_text(
            side_x + 8.0,
            hint_y + 42.0,
            ctx.tr("B: Undo last move", "B: Скасувати хід"),
            25.0,
            FontWeight::Medium,
            TextAlign::Left,
            t.fg2,
        );
    }
    ctx.draw_text(
        side_x + 8.0,
        hint_y + if game.allow_undo { 84.0 } else { 42.0 },
        ctx.tr("Menu / Start: Menu", "Menu / Start: Меню"),
        25.0,
        FontWeight::Medium,
        TextAlign::Left,
        t.fg2,
    );

    if game.game_over {
        let banner_y = hint_y + 138.0;
        ctx.fill_rect(side_x, banner_y, side_w, 72.0, 8.0, t.fg);
        ctx.draw_text(
            side_x + side_w * 0.5,
            banner_y + 36.0,
            ctx.tr("No moves! Press Menu", "Кінець! Натисни Menu"),
            26.0,
            FontWeight::Bold,
            TextAlign::Center,
            t.bg,
        );
    }

    ctx.present();
}

fn run_2048(ctx: &mut HostContext, _file: Option<&Path>) -> i32 {
    let seed = ctx.ticks_ms().wrapping_mul(0x9E37_79B9_7F4A_7C15);
    let mut game = ctx
        .load_plugin_data("uipod_2048", "board.txt")
        .and_then(|b| String::from_utf8(b).ok())
        .and_then(|s| Game2048::deserialize(&s, seed))
        .unwrap_or_else(|| Game2048::new(seed, 0, true));

    draw_2048(ctx, &game);

    loop {
        let inp = ctx.poll_input(250);
        if inp.should_exit {
            save_board(ctx, &game);
            return 0;
        }
        if inp.menu_requested {
            if open_system_menu(ctx, &mut game) {
                return 0;
            }
            draw_2048(ctx, &game);
            continue;
        }

        let mut changed = false;
        if inp.pressed(btn::B) {
            changed = game.undo();
        } else if game.game_over && inp.pressed(btn::A) {
            game = Game2048::new(ctx.ticks_ms() ^ game.rng, game.best, game.allow_undo);
            changed = true;
        } else if inp.pressed(btn::LEFT) {
            changed = game.apply_move(0);
        } else if inp.pressed(btn::RIGHT) {
            changed = game.apply_move(1);
        } else if inp.pressed(btn::UP) {
            changed = game.apply_move(2);
        } else if inp.pressed(btn::DOWN) {
            changed = game.apply_move(3);
        }

        if changed {
            save_board(ctx, &game);
        }
        draw_2048(ctx, &game);
    }
}

uipod_declare_plugin! {
    id: "uipod_2048",
    name: "2048",
    name_uk: "2048",
    author: "UIPod",
    version: "0.1.0",
    category: PluginCategory::Game,
    kind: PluginKind::MenuDriven,
    extensions: "",
    entry: run_2048,
}
