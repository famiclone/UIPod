//! Native GPU-accelerated 4-shade Guideline `Tetris` (`Тетріс`) for UIPod (`PluginKind::MenuDriven`).
//!
//! Includes 7-bag randomizer, SRS wall kicks, authentic `A` = CW / `B` = CCW rotation,
//! Ghost piece projection, Hold slot, 3-piece Next queue, 60 FPS DAS/ARR auto-shift,
//! and UIPod's J2ME-style native System Menu (`MENU` / `START`) for `Continue`,
//! `New Game`, `Save Game`, `Options` (`Ghost Piece`, `Start Level`, `Reset Best Score`),
//! and `Exit`.

use std::path::Path;

use uipod_plugin_sdk::{
    btn, uipod_declare_plugin, FontWeight, HostContext, MenuItem, MenuOutcome, PluginCategory,
    PluginKind, PluginTheme, TextAlign, CONTENT_TOP_Y,
};

const COLS: usize = 10;
const VIS_ROWS: usize = 20;
const HIDDEN_ROWS: usize = 4;
const TOTAL_ROWS: usize = VIS_ROWS + HIDDEN_ROWS;

const CELL: f32 = 29.0;
const WELL_PAD: f32 = 6.0;
const WELL_W: f32 = (COLS as f32) * CELL + WELL_PAD * 2.0; // 302
const WELL_H: f32 = (VIS_ROWS as f32) * CELL + WELL_PAD * 2.0; // 592
const WELL_X: f32 = (1024.0 - WELL_W) * 0.5; // 361
const WELL_Y: f32 = CONTENT_TOP_Y + 8.0; // 144

const DAS_DELAY_MS: u64 = 145;
const ARR_STEP_MS: u64 = 36;
const SOFT_DROP_MS: u64 = 42;
const LOCK_DELAY_MS: u64 = 480;
const MAX_LOCK_RESETS: u8 = 12;
const CLEAR_FLASH_MS: u64 = 150;

const PIECE_SHAPES: [[[(i32, i32); 4]; 4]; 7] = [
    [
        [(0, 1), (1, 1), (2, 1), (3, 1)],
        [(2, 0), (2, 1), (2, 2), (2, 3)],
        [(0, 2), (1, 2), (2, 2), (3, 2)],
        [(1, 0), (1, 1), (1, 2), (1, 3)],
    ],
    [
        [(1, 0), (2, 0), (1, 1), (2, 1)],
        [(1, 0), (2, 0), (1, 1), (2, 1)],
        [(1, 0), (2, 0), (1, 1), (2, 1)],
        [(1, 0), (2, 0), (1, 1), (2, 1)],
    ],
    [
        [(1, 0), (0, 1), (1, 1), (2, 1)],
        [(1, 0), (1, 1), (2, 1), (1, 2)],
        [(0, 1), (1, 1), (2, 1), (1, 2)],
        [(1, 0), (0, 1), (1, 1), (1, 2)],
    ],
    [
        [(1, 0), (2, 0), (0, 1), (1, 1)],
        [(1, 0), (1, 1), (2, 1), (2, 2)],
        [(1, 1), (2, 1), (0, 2), (1, 2)],
        [(0, 0), (0, 1), (1, 1), (1, 2)],
    ],
    [
        [(0, 0), (1, 0), (1, 1), (2, 1)],
        [(2, 0), (1, 1), (2, 1), (1, 2)],
        [(0, 1), (1, 1), (1, 2), (2, 2)],
        [(1, 0), (0, 1), (1, 1), (0, 2)],
    ],
    [
        [(0, 0), (0, 1), (1, 1), (2, 1)],
        [(1, 0), (2, 0), (1, 1), (1, 2)],
        [(0, 1), (1, 1), (2, 1), (2, 2)],
        [(1, 0), (1, 1), (0, 2), (1, 2)],
    ],
    [
        [(2, 0), (0, 1), (1, 1), (2, 1)],
        [(1, 0), (1, 1), (1, 2), (2, 2)],
        [(0, 1), (1, 1), (2, 1), (0, 2)],
        [(0, 0), (1, 0), (1, 1), (1, 2)],
    ],
];

fn jlstz_kicks(from: u8, to: u8) -> [(i32, i32); 5] {
    match (from, to) {
        (0, 1) => [(0, 0), (-1, 0), (-1, -1), (0, 2), (-1, 2)],
        (1, 0) => [(0, 0), (1, 0), (1, 1), (0, -2), (1, -2)],
        (1, 2) => [(0, 0), (1, 0), (1, 1), (0, -2), (1, -2)],
        (2, 1) => [(0, 0), (-1, 0), (-1, -1), (0, 2), (-1, 2)],
        (2, 3) => [(0, 0), (1, 0), (1, -1), (0, 2), (1, 2)],
        (3, 2) => [(0, 0), (-1, 0), (-1, 1), (0, -2), (-1, -2)],
        (3, 0) => [(0, 0), (-1, 0), (-1, 1), (0, -2), (-1, -2)],
        (0, 3) => [(0, 0), (1, 0), (1, -1), (0, 2), (1, 2)],
        _ => [(0, 0); 5],
    }
}

fn i_kicks(from: u8, to: u8) -> [(i32, i32); 5] {
    match (from, to) {
        (0, 1) => [(0, 0), (-2, 0), (1, 0), (-2, 1), (1, -2)],
        (1, 0) => [(0, 0), (2, 0), (-1, 0), (2, -1), (-1, 2)],
        (1, 2) => [(0, 0), (-1, 0), (2, 0), (-1, -2), (2, 1)],
        (2, 1) => [(0, 0), (1, 0), (-2, 0), (1, 2), (-2, -1)],
        (2, 3) => [(0, 0), (2, 0), (-1, 0), (2, -1), (-1, 2)],
        (3, 2) => [(0, 0), (-2, 0), (1, 0), (-2, 1), (1, -2)],
        (3, 0) => [(0, 0), (1, 0), (-2, 0), (1, 2), (-2, -1)],
        (0, 3) => [(0, 0), (-1, 0), (2, 0), (-1, -2), (2, 1)],
        _ => [(0, 0); 5],
    }
}

#[derive(Clone, Copy)]
struct ActivePiece {
    kind: u8,
    rot: u8,
    x: i32,
    y: i32,
}

impl ActivePiece {
    fn cells(self) -> [(i32, i32); 4] {
        let shape = PIECE_SHAPES[(self.kind - 1) as usize][self.rot as usize];
        [
            (self.x + shape[0].0, self.y + shape[0].1),
            (self.x + shape[1].0, self.y + shape[1].1),
            (self.x + shape[2].0, self.y + shape[2].1),
            (self.x + shape[3].0, self.y + shape[3].1),
        ]
    }
}

#[derive(Clone, Copy)]
struct TetrisOptions {
    show_ghost: bool,
    start_level: u32, // 1, 5, or 10
}

impl Default for TetrisOptions {
    fn default() -> Self {
        Self {
            show_ghost: true,
            start_level: 1,
        }
    }
}

struct TetrisGame {
    board: [[u8; COLS]; TOTAL_ROWS],
    cur: ActivePiece,
    hold: Option<u8>,
    hold_used: bool,
    queue: Vec<u8>,
    score: u32,
    best: u32,
    lines: u32,
    level: u32,
    opts: TetrisOptions,
    game_over: bool,
    rng: u64,
    last_grav_ms: u64,
    lock_since_ms: Option<u64>,
    lock_resets: u8,
    clearing: Option<([bool; TOTAL_ROWS], u64)>,
}

impl TetrisGame {
    fn new(seed: u64, best: u32, opts: TetrisOptions, now_ms: u64) -> Self {
        let mut g = Self {
            board: [[0u8; COLS]; TOTAL_ROWS],
            cur: ActivePiece {
                kind: 1,
                rot: 0,
                x: 3,
                y: 2,
            },
            hold: None,
            hold_used: false,
            queue: Vec::with_capacity(14),
            score: 0,
            best,
            lines: 0,
            level: opts.start_level.max(1),
            opts,
            game_over: false,
            rng: seed | 1,
            last_grav_ms: now_ms,
            lock_since_ms: None,
            lock_resets: 0,
            clearing: None,
        };
        g.refill_bag();
        g.refill_bag();
        g.spawn_next(now_ms);
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

    fn refill_bag(&mut self) {
        let mut bag = [1u8, 2, 3, 4, 5, 6, 7];
        for i in (1..7).rev() {
            let j = (self.next_rand() as usize) % (i + 1);
            bag.swap(i, j);
        }
        self.queue.extend_from_slice(&bag);
    }

    fn gravity_ms(&self) -> u64 {
        match self.level {
            1 => 800,
            2 => 680,
            3 => 570,
            4 => 470,
            5 => 380,
            6 => 300,
            7 => 235,
            8 => 180,
            9 => 135,
            10 => 100,
            11..=12 => 78,
            13..=15 => 60,
            16..=18 => 46,
            _ => 34,
        }
    }

    fn fits(&self, p: ActivePiece) -> bool {
        for (cx, cy) in p.cells() {
            if cx < 0 || cx >= (COLS as i32) || cy < 0 || cy >= (TOTAL_ROWS as i32) {
                return false;
            }
            if self.board[cy as usize][cx as usize] != 0 {
                return false;
            }
        }
        true
    }

    fn is_grounded(&self) -> bool {
        let mut down = self.cur;
        down.y += 1;
        !self.fits(down)
    }

    fn ghost_piece(&self) -> ActivePiece {
        let mut g = self.cur;
        while {
            let mut next = g;
            next.y += 1;
            self.fits(next)
        } {
            g.y += 1;
        }
        g
    }

    fn spawn_kind(&mut self, kind: u8, now_ms: u64) {
        let p = ActivePiece {
            kind,
            rot: 0,
            x: 3,
            y: 2,
        };
        self.cur = p;
        self.last_grav_ms = now_ms;
        self.lock_since_ms = None;
        self.lock_resets = 0;
        if !self.fits(p) {
            let mut higher = p;
            higher.y = 1;
            if self.fits(higher) {
                self.cur = higher;
            } else {
                self.game_over = true;
            }
        }
    }

    fn spawn_next(&mut self, now_ms: u64) {
        if self.queue.len() <= 7 {
            self.refill_bag();
        }
        let kind = self.queue.remove(0);
        self.hold_used = false;
        self.spawn_kind(kind, now_ms);
    }

    fn note_lock_reset(&mut self, now_ms: u64) {
        if self.is_grounded() {
            if self.lock_resets < MAX_LOCK_RESETS {
                self.lock_resets += 1;
                self.lock_since_ms = Some(now_ms);
            }
        } else {
            self.lock_since_ms = None;
        }
    }

    fn try_move(&mut self, dx: i32, dy: i32, now_ms: u64) -> bool {
        let mut cand = self.cur;
        cand.x += dx;
        cand.y += dy;
        if self.fits(cand) {
            self.cur = cand;
            if dy > 0 {
                self.last_grav_ms = now_ms;
                if self.is_grounded() && self.lock_since_ms.is_none() {
                    self.lock_since_ms = Some(now_ms);
                }
            } else {
                self.note_lock_reset(now_ms);
            }
            true
        } else {
            false
        }
    }

    fn try_rotate(&mut self, cw: bool, now_ms: u64) -> bool {
        if self.cur.kind == 2 {
            return false;
        }
        let from = self.cur.rot;
        let to = if cw { (from + 1) & 3 } else { (from + 3) & 3 };
        let kicks = if self.cur.kind == 1 {
            i_kicks(from, to)
        } else {
            jlstz_kicks(from, to)
        };
        for (kx, ky) in kicks {
            let cand = ActivePiece {
                kind: self.cur.kind,
                rot: to,
                x: self.cur.x + kx,
                y: self.cur.y + ky,
            };
            if self.fits(cand) {
                self.cur = cand;
                self.note_lock_reset(now_ms);
                return true;
            }
        }
        false
    }

    fn trigger_hold(&mut self, now_ms: u64) {
        if self.hold_used || self.clearing.is_some() || self.game_over {
            return;
        }
        let cur_kind = self.cur.kind;
        match self.hold {
            Some(prev_hold) => {
                self.hold = Some(cur_kind);
                self.hold_used = true;
                self.spawn_kind(prev_hold, now_ms);
            }
            None => {
                self.hold = Some(cur_kind);
                self.spawn_next(now_ms);
                self.hold_used = true;
            }
        }
    }

    fn hard_drop(&mut self, now_ms: u64) {
        if self.clearing.is_some() || self.game_over {
            return;
        }
        let ghost = self.ghost_piece();
        let dist = (ghost.y - self.cur.y).max(0) as u32;
        self.cur = ghost;
        self.add_score(dist * 2);
        self.lock_piece(now_ms);
    }

    fn add_score(&mut self, pts: u32) {
        self.score = self.score.saturating_add(pts);
        if self.score > self.best {
            self.best = self.score;
        }
    }

    fn lock_piece(&mut self, now_ms: u64) {
        let mut all_above_skyline = true;
        for (cx, cy) in self.cur.cells() {
            if (0..COLS as i32).contains(&cx) && (0..TOTAL_ROWS as i32).contains(&cy) {
                self.board[cy as usize][cx as usize] = self.cur.kind;
                if (cy as usize) >= HIDDEN_ROWS {
                    all_above_skyline = false;
                }
            }
        }
        if all_above_skyline {
            self.game_over = true;
            return;
        }

        let mut full_mask = [false; TOTAL_ROWS];
        let mut count = 0u32;
        for r in 0..TOTAL_ROWS {
            if self.board[r].iter().all(|&c| c != 0) {
                full_mask[r] = true;
                count += 1;
            }
        }

        if count > 0 {
            let base = match count {
                1 => 100,
                2 => 300,
                3 => 500,
                _ => 800,
            };
            self.add_score(base * self.level);
            self.lines += count;
            self.level = self.opts.start_level.max((self.lines / 10) + 1);
            self.clearing = Some((full_mask, now_ms + CLEAR_FLASH_MS));
        } else {
            self.spawn_next(now_ms);
        }
    }

    fn finish_clear(&mut self, mask: [bool; TOTAL_ROWS], now_ms: u64) {
        let mut next_board = [[0u8; COLS]; TOTAL_ROWS];
        let mut dst = TOTAL_ROWS;
        for src in (0..TOTAL_ROWS).rev() {
            if !mask[src] {
                dst -= 1;
                next_board[dst] = self.board[src];
            }
        }
        self.board = next_board;
        self.spawn_next(now_ms);
    }

    fn update(&mut self, now_ms: u64) {
        if self.game_over {
            return;
        }

        if let Some((mask, end_ms)) = self.clearing {
            if now_ms >= end_ms {
                self.clearing = None;
                self.finish_clear(mask, now_ms);
            }
            return;
        }

        let g_ms = self.gravity_ms();
        while now_ms.saturating_sub(self.last_grav_ms) >= g_ms {
            self.last_grav_ms += g_ms;
            let mut down = self.cur;
            down.y += 1;
            if self.fits(down) {
                self.cur = down;
            } else {
                break;
            }
        }

        if self.is_grounded() {
            let since = *self.lock_since_ms.get_or_insert(now_ms);
            if now_ms.saturating_sub(since) >= LOCK_DELAY_MS {
                self.lock_piece(now_ms);
            }
        } else {
            self.lock_since_ms = None;
        }
    }

    fn serialize(&self) -> String {
        if self.game_over {
            return format!(
                "BEST2 {} {} {}\n",
                self.best,
                u8::from(self.opts.show_ghost),
                self.opts.start_level
            );
        }
        let mut out = format!(
            "STATE2 {} {} {} {} {} {} {} {} {} {} {} {}\n",
            self.best,
            self.score,
            self.lines,
            self.level,
            self.cur.kind,
            self.cur.rot,
            self.cur.x,
            self.cur.y,
            self.hold.unwrap_or(0),
            u8::from(self.hold_used),
            u8::from(self.opts.show_ghost),
            self.opts.start_level
        );
        for q in self.queue.iter().take(10) {
            out.push_str(&format!("{q} "));
        }
        out.push('\n');
        for r in 0..TOTAL_ROWS {
            for c in 0..COLS {
                out.push(char::from(b'0' + self.board[r][c]));
            }
            out.push('\n');
        }
        out
    }

    fn deserialize(s: &str, seed: u64, now_ms: u64) -> Self {
        let mut lines_it = s.lines();
        let Some(first) = lines_it.next() else {
            return Self::new(seed, 0, TetrisOptions::default(), now_ms);
        };
        let parts: Vec<&str> = first.split_whitespace().collect();
        if parts.first().copied() == Some("BEST2") && parts.len() >= 4 {
            let best = parts[1].parse::<u32>().unwrap_or(0);
            let opts = TetrisOptions {
                show_ghost: parts[2].parse::<u8>().unwrap_or(1) != 0,
                start_level: parts[3].parse::<u32>().unwrap_or(1).max(1),
            };
            return Self::new(seed, best, opts, now_ms);
        }
        if parts.first().copied() == Some("BEST") {
            let best = parts.get(1).and_then(|v| v.parse::<u32>().ok()).unwrap_or(0);
            return Self::new(seed, best, TetrisOptions::default(), now_ms);
        }
        if parts.first().copied() != Some("STATE2") || parts.len() < 13 {
            return Self::new(seed, 0, TetrisOptions::default(), now_ms);
        }

        let best = parts[1].parse::<u32>().unwrap_or(0);
        let score = parts[2].parse::<u32>().unwrap_or(0);
        let lines = parts[3].parse::<u32>().unwrap_or(0);
        let level = parts[4].parse::<u32>().unwrap_or(1).max(1);
        let cur_kind = parts[5].parse::<u8>().unwrap_or(1).clamp(1, 7);
        let cur_rot = parts[6].parse::<u8>().unwrap_or(0) & 3;
        let cur_x = parts[7].parse::<i32>().unwrap_or(3);
        let cur_y = parts[8].parse::<i32>().unwrap_or(2);
        let hold_raw = parts[9].parse::<u8>().unwrap_or(0);
        let hold_used = parts[10].parse::<u8>().unwrap_or(0) != 0;
        let opts = TetrisOptions {
            show_ghost: parts[11].parse::<u8>().unwrap_or(1) != 0,
            start_level: parts[12].parse::<u32>().unwrap_or(1).max(1),
        };

        let mut queue: Vec<u8> = lines_it
            .next()
            .unwrap_or("")
            .split_whitespace()
            .filter_map(|v| v.parse::<u8>().ok())
            .filter(|&k| (1..=7).contains(&k))
            .collect();

        let mut board = [[0u8; COLS]; TOTAL_ROWS];
        for row in &mut board {
            let Some(rline) = lines_it.next() else {
                return Self::new(seed, best, opts, now_ms);
            };
            for (c, ch) in rline.bytes().take(COLS).enumerate() {
                if (b'1'..=b'7').contains(&ch) {
                    row[c] = ch - b'0';
                }
            }
        }

        let mut g = Self {
            board,
            cur: ActivePiece {
                kind: cur_kind,
                rot: cur_rot,
                x: cur_x,
                y: cur_y,
            },
            hold: if (1..=7).contains(&hold_raw) {
                Some(hold_raw)
            } else {
                None
            },
            hold_used,
            queue: Vec::new(),
            score,
            best: best.max(score),
            lines,
            level,
            opts,
            game_over: false,
            rng: seed | 1,
            last_grav_ms: now_ms,
            lock_since_ms: None,
            lock_resets: 0,
            clearing: None,
        };
        if queue.len() < 7 {
            g.refill_bag();
            queue.extend_from_slice(&g.queue);
        }
        g.queue = queue;
        if !g.fits(g.cur) {
            return Self::new(seed, g.best, opts, now_ms);
        }
        g
    }
}

fn draw_mino(ctx: &HostContext, t: PluginTheme, x: f32, y: f32, size: f32, kind: u8) {
    let p = 1.5_f32;
    let bx = x + p;
    let by = y + p;
    let bw = size - p * 2.0;
    match kind {
        1 => {
            ctx.fill_rect(bx, by, bw, bw, 4.0, t.fg);
            ctx.stroke_rect(bx + 4.0, by + 4.0, bw - 8.0, bw - 8.0, 2.0, 2.0, t.bg);
        }
        2 => {
            ctx.fill_rect(bx, by, bw, bw, 4.0, t.fg);
            ctx.fill_rect(bx + 4.0, by + 4.0, bw - 8.0, bw - 8.0, 2.0, t.bg);
            ctx.fill_rect(bx + 8.0, by + 8.0, bw - 16.0, bw - 16.0, 1.5, t.fg);
        }
        3 => {
            ctx.fill_rect(bx, by, bw, bw, 4.0, t.fg2);
            ctx.stroke_rect(bx + 1.0, by + 1.0, bw - 2.0, bw - 2.0, 3.5, 2.2, t.fg);
            ctx.fill_circle(bx + bw * 0.5, by + bw * 0.5, 3.0, t.bg);
        }
        4 | 6 => {
            ctx.fill_rect(bx, by, bw, bw, 4.0, t.fg1);
            ctx.stroke_rect(bx + 1.0, by + 1.0, bw - 2.0, bw - 2.0, 3.5, 2.5, t.fg);
            if kind == 6 {
                ctx.fill_rect(bx + 7.0, by + 7.0, bw - 14.0, bw - 14.0, 2.0, t.fg2);
            }
        }
        _ => {
            ctx.fill_rect(bx, by, bw, bw, 4.0, t.fg2);
            ctx.stroke_rect(bx + 1.0, by + 1.0, bw - 2.0, bw - 2.0, 3.5, 2.2, t.fg);
            if kind == 7 {
                ctx.fill_rect(bx + 6.0, by + 6.0, 5.0, 5.0, 1.5, t.bg);
            }
        }
    }
}

fn draw_mini_piece(ctx: &HostContext, t: PluginTheme, cx: f32, cy: f32, cell: f32, kind: u8) {
    let shape = PIECE_SHAPES[(kind - 1) as usize][0];
    let min_x = shape.iter().map(|p| p.0).min().unwrap_or(0) as f32;
    let max_x = shape.iter().map(|p| p.0).max().unwrap_or(0) as f32;
    let min_y = shape.iter().map(|p| p.1).min().unwrap_or(0) as f32;
    let max_y = shape.iter().map(|p| p.1).max().unwrap_or(0) as f32;
    let w = (max_x - min_x + 1.0) * cell;
    let h = (max_y - min_y + 1.0) * cell;
    let ox = cx - w * 0.5 - min_x * cell;
    let oy = cy - h * 0.5 - min_y * cell;
    for (sx, sy) in shape {
        draw_mino(
            ctx,
            t,
            ox + (sx as f32) * cell,
            oy + (sy as f32) * cell,
            cell,
            kind,
        );
    }
}

fn draw_tetris(ctx: &HostContext, game: &TetrisGame) {
    let t = ctx.theme();
    let title = ctx.tr("Tetris", "Тетріс");
    ctx.begin_frame(t.bg, true, title);

    ctx.stroke_rect(WELL_X, WELL_Y, WELL_W, WELL_H, 8.0, 4.5, t.fg);
    let mx0 = WELL_X + WELL_PAD;
    let my0 = WELL_Y + WELL_PAD;

    for vr in 1..VIS_ROWS {
        for c in 1..COLS {
            ctx.fill_rect(
                mx0 + (c as f32) * CELL - 1.0,
                my0 + (vr as f32) * CELL - 1.0,
                2.0,
                2.0,
                0.0,
                t.fg1,
            );
        }
    }

    for vr in 0..VIS_ROWS {
        let br = vr + HIDDEN_ROWS;
        let row_flashing = game
            .clearing
            .as_ref()
            .map(|(m, _)| m[br])
            .unwrap_or(false);
        if row_flashing {
            ctx.fill_rect(
                mx0 + 2.0,
                my0 + (vr as f32) * CELL + 3.0,
                (COLS as f32) * CELL - 4.0,
                CELL - 6.0,
                4.0,
                t.fg,
            );
            continue;
        }
        for c in 0..COLS {
            let kind = game.board[br][c];
            if kind != 0 {
                draw_mino(
                    ctx,
                    t,
                    mx0 + (c as f32) * CELL,
                    my0 + (vr as f32) * CELL,
                    CELL,
                    kind,
                );
            }
        }
    }

    if game.clearing.is_none() && !game.game_over {
        if game.opts.show_ghost {
            let ghost = game.ghost_piece();
            if ghost.y > game.cur.y {
                for (gx, gy) in ghost.cells() {
                    if gy >= (HIDDEN_ROWS as i32) {
                        let vx = mx0 + (gx as f32) * CELL + 3.0;
                        let vy = my0 + ((gy - HIDDEN_ROWS as i32) as f32) * CELL + 3.0;
                        ctx.stroke_rect(vx, vy, CELL - 6.0, CELL - 6.0, 4.0, 2.5, t.fg1);
                    }
                }
            }
        }
        for (px, py) in game.cur.cells() {
            if py >= (HIDDEN_ROWS as i32) {
                draw_mino(
                    ctx,
                    t,
                    mx0 + (px as f32) * CELL,
                    my0 + ((py - HIDDEN_ROWS as i32) as f32) * CELL,
                    CELL,
                    game.cur.kind,
                );
            }
        }
    }

    // Left column
    let lx = 32.0_f32;
    let lw = WELL_X - 24.0 - lx;

    let hold_h = 154.0_f32;
    ctx.stroke_rect(lx, WELL_Y, lw, hold_h, 10.0, 3.5, t.fg1);
    ctx.draw_text(
        lx + lw * 0.5,
        WELL_Y + 26.0,
        ctx.tr("HOLD (L / Select)", "ЗАПАС (L / Select)"),
        22.0,
        FontWeight::SemiBold,
        TextAlign::Center,
        t.fg2,
    );
    if let Some(hk) = game.hold {
        draw_mini_piece(ctx, t, lx + lw * 0.5, WELL_Y + 98.0, 24.0, hk);
    } else {
        ctx.draw_text(
            lx + lw * 0.5,
            WELL_Y + 98.0,
            "—",
            32.0,
            FontWeight::Medium,
            TextAlign::Center,
            t.fg1,
        );
    }

    let sc_y = WELL_Y + hold_h + 18.0;
    let card_h = 126.0_f32;
    ctx.stroke_rect(lx, sc_y, lw, card_h, 10.0, 3.5, t.fg1);
    ctx.draw_text(
        lx + lw * 0.5,
        sc_y + 30.0,
        ctx.tr("SCORE", "РАХУНОК"),
        23.0,
        FontWeight::SemiBold,
        TextAlign::Center,
        t.fg2,
    );
    ctx.draw_text(
        lx + lw * 0.5,
        sc_y + 82.0,
        &game.score.to_string(),
        42.0,
        FontWeight::Bold,
        TextAlign::Center,
        t.fg,
    );

    let best_y = sc_y + card_h + 18.0;
    ctx.stroke_rect(lx, best_y, lw, card_h, 10.0, 3.5, t.fg1);
    ctx.draw_text(
        lx + lw * 0.5,
        best_y + 30.0,
        ctx.tr("BEST", "РЕКОРД"),
        23.0,
        FontWeight::SemiBold,
        TextAlign::Center,
        t.fg2,
    );
    ctx.draw_text(
        lx + lw * 0.5,
        best_y + 82.0,
        &game.best.to_string(),
        42.0,
        FontWeight::Bold,
        TextAlign::Center,
        t.fg,
    );

    let lv_y = best_y + card_h + 18.0;
    let lv_h = WELL_Y + WELL_H - lv_y;
    ctx.stroke_rect(lx, lv_y, lw, lv_h, 10.0, 3.5, t.fg1);
    let lv_txt = format!(
        "{}: {}     {}: {}",
        ctx.tr("Lv", "Рів"),
        game.level,
        ctx.tr("Lines", "Лінії"),
        game.lines
    );
    ctx.draw_text(
        lx + lw * 0.5,
        lv_y + lv_h * 0.5,
        &lv_txt,
        26.0,
        FontWeight::Bold,
        TextAlign::Center,
        t.fg,
    );

    // Right column
    let rx = WELL_X + WELL_W + 24.0;
    let rw = 1024.0 - 32.0 - rx;

    let next_h = 330.0_f32;
    ctx.stroke_rect(rx, WELL_Y, rw, next_h, 10.0, 3.5, t.fg1);
    ctx.draw_text(
        rx + rw * 0.5,
        WELL_Y + 26.0,
        ctx.tr("NEXT", "ДАЛІ"),
        23.0,
        FontWeight::SemiBold,
        TextAlign::Center,
        t.fg2,
    );
    for (i, &nk) in game.queue.iter().take(3).enumerate() {
        let py = WELL_Y + 92.0 + (i as f32) * 92.0;
        draw_mini_piece(ctx, t, rx + rw * 0.5, py, 23.0, nk);
    }

    let ctrl_y = WELL_Y + next_h + 18.0;
    let ctrl_h = WELL_H - next_h - 18.0;
    ctx.stroke_rect(rx, ctrl_y, rw, ctrl_h, 10.0, 3.5, t.fg1);
    let hints = [
        ctx.tr("A : Rotate CW ↻", "A : Поворот ↻"),
        ctx.tr("B / X : Rotate CCW ↺", "B / X : Поворот ↺"),
        ctx.tr("↑ / Y / R : Hard Drop", "↑ / Y / R : Миттєво вниз"),
        ctx.tr("↓ : Soft Drop", "↓ : Прискорити"),
        ctx.tr("L / Select : Hold", "L / Select : Запас"),
        ctx.tr("Menu / Start : Menu", "Menu / Start : Меню"),
    ];
    for (i, line) in hints.iter().enumerate() {
        ctx.draw_text(
            rx + 18.0,
            ctrl_y + 30.0 + (i as f32) * 36.0,
            line,
            21.0,
            FontWeight::Medium,
            TextAlign::Left,
            t.fg2,
        );
    }

    if game.game_over {
        let title_msg = ctx.tr("GAME OVER", "ГРУ ЗАКІНЧЕНО");
        let sub_msg = ctx.tr(
            "A: New Game  •  Menu: Options",
            "A: Нова гра  •  Menu: Меню",
        );
        let bw = 460.0_f32;
        let bh = 118.0_f32;
        let bx = (1024.0 - bw) * 0.5;
        let by = WELL_Y + (WELL_H - bh) * 0.5;
        ctx.fill_rect(bx, by, bw, bh, 10.0, t.bg);
        ctx.stroke_rect(bx, by, bw, bh, 10.0, 4.5, t.fg);
        ctx.draw_text(
            512.0,
            by + 40.0,
            title_msg,
            34.0,
            FontWeight::Bold,
            TextAlign::Center,
            t.fg,
        );
        ctx.draw_text(
            512.0,
            by + 84.0,
            sub_msg,
            24.0,
            FontWeight::SemiBold,
            TextAlign::Center,
            t.fg2,
        );
    }

    ctx.present();
}

#[derive(Default)]
struct DasTracker {
    dir: i32,
    next_fire_ms: u64,
}

impl DasTracker {
    fn step(&mut self, held_left: bool, held_right: bool, now_ms: u64) -> i32 {
        let want = match (held_left, held_right) {
            (true, false) => -1,
            (false, true) => 1,
            _ => 0,
        };
        if want == 0 {
            self.dir = 0;
            return 0;
        }
        if want != self.dir {
            self.dir = want;
            self.next_fire_ms = now_ms + DAS_DELAY_MS;
            return want;
        }
        if now_ms >= self.next_fire_ms {
            self.next_fire_ms = now_ms + ARR_STEP_MS;
            return want;
        }
        0
    }
}

fn save_tetris(ctx: &HostContext, game: &TetrisGame) {
    let _ = ctx.save_plugin_data("tetris", "state.txt", game.serialize().as_bytes());
}

fn open_options_menu(ctx: &HostContext, game: &mut TetrisGame) -> bool {
    let mut sel = 0usize;
    loop {
        let ghost_lbl = if game.opts.show_ghost {
            ctx.tr("On", "Увімк")
        } else {
            ctx.tr("Off", "Вимк")
        };
        let start_lv_str = game.opts.start_level.to_string();
        let best_str = game.best.to_string();
        let items = [
            MenuItem::option(ctx.tr("Ghost Piece", "Тінь фігури"), ghost_lbl),
            MenuItem::option(ctx.tr("Start Level", "Початковий рівень"), &start_lv_str),
            MenuItem::option(ctx.tr("Reset Best Score", "Скинути рекорд"), &best_str),
        ];
        match ctx.show_menu(ctx.tr("Options", "Налаштування"), &items, sel) {
            MenuOutcome::Selected(0) => {
                sel = 0;
                game.opts.show_ghost = !game.opts.show_ghost;
                save_tetris(ctx, game);
            }
            MenuOutcome::Selected(1) => {
                sel = 1;
                game.opts.start_level = match game.opts.start_level {
                    1 => 5,
                    5 => 10,
                    _ => 1,
                };
                if game.lines == 0 && game.score == 0 {
                    game.level = game.opts.start_level;
                }
                save_tetris(ctx, game);
            }
            MenuOutcome::Selected(2) => {
                sel = 2;
                game.best = game.score;
                save_tetris(ctx, game);
                ctx.show_splash(ctx.tr("Best score reset", "Рекорд скинуто"), 700);
            }
            MenuOutcome::Selected(_) | MenuOutcome::Cancelled => return false,
            MenuOutcome::ForceExit => return true,
        }
    }
}

fn open_system_menu(ctx: &HostContext, game: &mut TetrisGame) -> bool {
    let mut sel = 0usize;
    loop {
        let title = ctx.tr("Tetris", "Тетріс");
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
                let now = ctx.ticks_ms();
                *game = TetrisGame::new(now ^ game.rng, game.best, game.opts, now);
                save_tetris(ctx, game);
                return false;
            }
            MenuOutcome::Selected(2) => {
                sel = 2;
                save_tetris(ctx, game);
                ctx.show_splash(ctx.tr("Game saved", "Гру збережено"), 700);
            }
            MenuOutcome::Selected(3) => {
                sel = 3;
                if open_options_menu(ctx, game) {
                    save_tetris(ctx, game);
                    return true;
                }
            }
            MenuOutcome::Selected(_) | MenuOutcome::ForceExit => {
                save_tetris(ctx, game);
                return true;
            }
        }
    }
}

fn run_tetris(ctx: &mut HostContext, _file: Option<&Path>) -> i32 {
    let init_now = ctx.ticks_ms();
    let seed = init_now.wrapping_mul(0x9E37_79B9_7F4A_7C15);
    let mut game = ctx
        .load_plugin_data("tetris", "state.txt")
        .and_then(|b| String::from_utf8(b).ok())
        .map(|s| TetrisGame::deserialize(&s, seed, init_now))
        .unwrap_or_else(|| TetrisGame::new(seed, 0, TetrisOptions::default(), init_now));

    let mut das = DasTracker::default();
    let mut last_soft_ms = init_now;

    loop {
        let frame_start = ctx.ticks_ms();
        let inp = ctx.poll_input(0);

        if inp.should_exit {
            save_tetris(ctx, &game);
            return 0;
        }
        if inp.menu_requested {
            if open_system_menu(ctx, &mut game) {
                return 0;
            }
            let resumed = ctx.ticks_ms();
            game.last_grav_ms = resumed;
            game.lock_since_ms = None;
            das = DasTracker::default();
            continue;
        }

        if game.game_over {
            if inp.pressed(btn::A) {
                game = TetrisGame::new(frame_start ^ game.rng, game.best, game.opts, frame_start);
                das = DasTracker::default();
            }
        } else if game.clearing.is_none() {
            let dx = das.step(inp.held(btn::LEFT), inp.held(btn::RIGHT), frame_start);
            if dx != 0 {
                game.try_move(dx, 0, frame_start);
            }

            if inp.pressed(btn::A) {
                game.try_rotate(true, frame_start);
            } else if inp.pressed(btn::B | btn::X) {
                game.try_rotate(false, frame_start);
            }

            if inp.pressed(btn::L | btn::SELECT) {
                game.trigger_hold(frame_start);
            }

            if inp.pressed(btn::UP | btn::Y | btn::R) {
                game.hard_drop(frame_start);
            } else if inp.held(btn::DOWN) && frame_start.saturating_sub(last_soft_ms) >= SOFT_DROP_MS
            {
                last_soft_ms = frame_start;
                if game.try_move(0, 1, frame_start) {
                    game.add_score(1);
                }
            }
        }

        let now = ctx.ticks_ms();
        game.update(now);
        draw_tetris(ctx, &game);

        let elapsed = ctx.ticks_ms().saturating_sub(frame_start);
        if elapsed < 16 {
            ctx.sleep_ms((16 - elapsed) as u32);
        } else {
            ctx.sleep_ms(0);
        }
    }
}

uipod_declare_plugin! {
    id: "tetris",
    name: "Tetris",
    name_uk: "Тетріс",
    author: "UIPod",
    version: "0.1.0",
    category: PluginCategory::Game,
    kind: PluginKind::MenuDriven,
    extensions: "",
    entry: run_tetris,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seven_bag_contains_every_tetromino_without_duplicates() {
        let g = TetrisGame::new(12345, 0, TetrisOptions::default(), 0);
        let mut first_bag = vec![g.cur.kind];
        first_bag.extend_from_slice(&g.queue[..6]);
        first_bag.sort_unstable();
        assert_eq!(first_bag, vec![1, 2, 3, 4, 5, 6, 7]);
    }

    #[test]
    fn srs_wall_kick_rotates_piece_flush_against_left_wall() {
        let mut g = TetrisGame::new(99, 0, TetrisOptions::default(), 0);
        g.cur = ActivePiece {
            kind: 3,
            rot: 3,
            x: 0,
            y: 10,
        };
        assert!(g.fits(g.cur));
        assert!(g.try_rotate(true, 10));
        assert_eq!(g.cur.rot, 0);
        assert!(g.fits(g.cur));
    }

    #[test]
    fn hard_drop_clears_full_line_and_awards_tetris_score() {
        let mut g = TetrisGame::new(77, 0, TetrisOptions::default(), 0);
        for c in 4..COLS {
            g.board[TOTAL_ROWS - 1][c] = 2;
        }
        g.cur = ActivePiece {
            kind: 1,
            rot: 0,
            x: 0,
            y: 5,
        };
        g.hard_drop(100);
        assert!(g.clearing.is_some());
        g.update(100 + CLEAR_FLASH_MS + 1);
        assert_eq!(g.lines, 1);
        assert!(g.score >= 100);
        assert!(g.board[TOTAL_ROWS - 1].iter().all(|&c| c == 0));
    }
}
