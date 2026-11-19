use std::collections::VecDeque;
use uipod_ui::{HeaderState, ThemeColors, UiEngine};

pub const ABOUT_MANUAL_TEXT: &str = include_str!("../../../pak/uipod/about.txt");

#[allow(dead_code)]
pub mod btn {
    pub const A: u32 = 0x0001;
    pub const B: u32 = 0x0002;
    pub const X: u32 = 0x0004;
    pub const Y: u32 = 0x0008;
    pub const UP: u32 = 0x0010;
    pub const DOWN: u32 = 0x0020;
    pub const LEFT: u32 = 0x0040;
    pub const RIGHT: u32 = 0x0080;
    pub const START: u32 = 0x0100;
    pub const SELECT: u32 = 0x0200;
    pub const L1: u32 = 0x0400;
    pub const R1: u32 = 0x0800;
    pub const L2: u32 = 0x1000;
    pub const R2: u32 = 0x2000;
    pub const MENU: u32 = 0x4000;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PluginAction {
    Continue,
    Exit,
    PlayTrackAt(usize, u32),
    EnsurePlaying,
    NextTrack,
    PrevTrack,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PluginId {
    MusicQuiz,
    SoundRacer,
    Tetris,
    Snake,
    Game2048,
    BrickBreaker,
    Stopwatch,
    TextViewer,
}

pub enum ActivePlugin {
    MusicQuiz(MusicQuizGame),
    SoundRacer(SoundRacerGame),
    Tetris(TetrisGame),
    Snake(SnakeGame),
    Game2048(Game2048),
    BrickBreaker(BrickBreakerGame),
    Stopwatch(StopwatchApp),
    TextViewer(TextViewerApp),
}

impl ActivePlugin {
    pub fn launch(id: PluginId, track_labels: &[(String, u32)]) -> Self {
        match id {
            PluginId::MusicQuiz => Self::MusicQuiz(MusicQuizGame::new(track_labels)),
            PluginId::SoundRacer => Self::SoundRacer(SoundRacerGame::new()),
            PluginId::Tetris => Self::Tetris(TetrisGame::new()),
            PluginId::Snake => Self::Snake(SnakeGame::new()),
            PluginId::Game2048 => Self::Game2048(Game2048::new()),
            PluginId::BrickBreaker => Self::BrickBreaker(BrickBreakerGame::new()),
            PluginId::Stopwatch => Self::Stopwatch(StopwatchApp::new()),
            PluginId::TextViewer => Self::TextViewer(TextViewerApp::new(ABOUT_MANUAL_TEXT)),
        }
    }

    #[allow(dead_code)]
    pub fn id(&self) -> PluginId {
        match self {
            Self::MusicQuiz(_) => PluginId::MusicQuiz,
            Self::SoundRacer(_) => PluginId::SoundRacer,
            Self::Tetris(_) => PluginId::Tetris,
            Self::Snake(_) => PluginId::Snake,
            Self::Game2048(_) => PluginId::Game2048,
            Self::BrickBreaker(_) => PluginId::BrickBreaker,
            Self::Stopwatch(_) => PluginId::Stopwatch,
            Self::TextViewer(_) => PluginId::TextViewer,
        }
    }

    pub fn title(&self, uk: bool) -> &'static str {
        match self {
            Self::MusicQuiz(_) => if uk { "Вгадай мелодію" } else { "Music Quiz" },
            Self::SoundRacer(_) => "Sound Racer",
            Self::Tetris(_) => if uk { "Тетріс" } else { "Tetris" },
            Self::Snake(_) => if uk { "Змійка" } else { "Snake" },
            Self::Game2048(_) => "2048",
            Self::BrickBreaker(_) => if uk { "Арканоїд" } else { "Brick Breaker" },
            Self::Stopwatch(_) => if uk { "Секундомір" } else { "Stopwatch" },
            Self::TextViewer(_) => if uk { "Посібник UIPod" } else { "UIPod Manual" },
        }
    }

    pub fn update(
        &mut self,
        dt: f32,
        pressed: u32,
        held: u32,
        pcm: &[i16],
        track_title: &str,
    ) -> PluginAction {
        match self {
            Self::MusicQuiz(g) => g.update(dt, pressed, pcm),
            Self::SoundRacer(g) => g.update(dt, pressed, held, pcm, track_title),
            Self::Tetris(g) => g.update(dt, pressed, held),
            Self::Snake(g) => g.update(dt, pressed, held),
            Self::Game2048(g) => g.update(dt, pressed),
            Self::BrickBreaker(g) => g.update(dt, pressed, held),
            Self::Stopwatch(a) => a.update(dt, pressed),
            Self::TextViewer(v) => v.update(dt, pressed, held),
        }
    }

    pub fn render(
        &self,
        ui: &mut UiEngine,
        theme: ThemeColors,
        header: &HeaderState,
        uk: bool,
    ) {
        ui.plugin_begin(theme, theme.bg_rgb, Some(header));
        match self {
            Self::MusicQuiz(g) => g.render(ui, theme, uk),
            Self::SoundRacer(g) => g.render(ui, theme, uk),
            Self::Tetris(g) => g.render(ui, theme, uk),
            Self::Snake(g) => g.render(ui, theme, uk),
            Self::Game2048(g) => g.render(ui, theme, uk),
            Self::BrickBreaker(g) => g.render(ui, theme, uk),
            Self::Stopwatch(a) => a.render(ui, theme, uk),
            Self::TextViewer(v) => v.render(ui, theme, uk),
        }
    }
}

struct Rng(u32);
impl Rng {
    fn new(seed: u32) -> Self {
        Self(seed.max(1))
    }
    fn next_u32(&mut self) -> u32 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
        self.0 = x;
        x
    }
    fn range(&mut self, max: usize) -> usize {
        if max == 0 {
            0
        } else {
            (self.next_u32() as usize) % max
        }
    }
}

/* ============================================================================
 * 1. SNAKE (4-Shade Arcade)
 * ========================================================================== */
#[derive(Clone, Copy, PartialEq, Eq)]
enum SnakeDir {
    Up,
    Down,
    Left,
    Right,
}

impl SnakeDir {
    fn delta(self) -> (i32, i32) {
        match self {
            Self::Up => (0, -1),
            Self::Down => (0, 1),
            Self::Left => (-1, 0),
            Self::Right => (1, 0),
        }
    }
    fn opposite(self) -> Self {
        match self {
            Self::Up => Self::Down,
            Self::Down => Self::Up,
            Self::Left => Self::Right,
            Self::Right => Self::Left,
        }
    }
}

pub struct SnakeGame {
    body: VecDeque<(i32, i32)>,
    dir: SnakeDir,
    turn_queue: VecDeque<SnakeDir>,
    food: (i32, i32),
    score: u32,
    best: u32,
    wrap_walls: bool,
    paused: bool,
    game_over: bool,
    step_accum: f32,
    rng: Rng,
}

impl SnakeGame {
    const W: i32 = 20;
    const H: i32 = 18;

    fn new() -> Self {
        let mut g = Self {
            body: VecDeque::new(),
            dir: SnakeDir::Right,
            turn_queue: VecDeque::new(),
            food: (14, 9),
            score: 0,
            best: 0,
            wrap_walls: false,
            paused: false,
            game_over: false,
            step_accum: 0.0,
            rng: Rng::new(0x5A17_93F1),
        };
        g.reset();
        g
    }

    fn reset(&mut self) {
        self.body.clear();
        self.body.push_back((10, 9));
        self.body.push_back((9, 9));
        self.body.push_back((8, 9));
        self.body.push_back((7, 9));
        self.dir = SnakeDir::Right;
        self.turn_queue.clear();
        self.score = 0;
        self.paused = false;
        self.game_over = false;
        self.step_accum = 0.0;
        self.spawn_food();
    }

    fn spawn_food(&mut self) {
        for _ in 0..256 {
            let x = self.rng.range(Self::W as usize) as i32;
            let y = self.rng.range(Self::H as usize) as i32;
            if !self.body.iter().any(|&(bx, by)| bx == x && by == y) {
                self.food = (x, y);
                return;
            }
        }
    }

    fn push_turn(&mut self, d: SnakeDir) {
        let last = self.turn_queue.back().copied().unwrap_or(self.dir);
        if d != last && d != last.opposite() && self.turn_queue.len() < 2 {
            self.turn_queue.push_back(d);
        }
    }

    fn update(&mut self, dt: f32, pressed: u32, _held: u32) -> PluginAction {
        if (pressed & btn::B) != 0 {
            return PluginAction::Exit;
        }
        if (pressed & btn::X) != 0 {
            self.wrap_walls = !self.wrap_walls;
        }
        if self.game_over {
            if (pressed & (btn::A | btn::START)) != 0 {
                self.reset();
            }
            return PluginAction::Continue;
        }
        if (pressed & (btn::START | btn::MENU)) != 0 {
            self.paused = !self.paused;
            return PluginAction::Continue;
        }
        if self.paused {
            return PluginAction::Continue;
        }
        if (pressed & btn::UP) != 0 {
            self.push_turn(SnakeDir::Up);
        } else if (pressed & btn::DOWN) != 0 {
            self.push_turn(SnakeDir::Down);
        } else if (pressed & btn::LEFT) != 0 {
            self.push_turn(SnakeDir::Left);
        } else if (pressed & btn::RIGHT) != 0 {
            self.push_turn(SnakeDir::Right);
        }

        let step_s = (0.145 - (self.score as f32 * 0.0022)).max(0.062);
        self.step_accum += dt;
        while self.step_accum >= step_s {
            self.step_accum -= step_s;
            if let Some(next_d) = self.turn_queue.pop_front() {
                self.dir = next_d;
            }
            let (hx, hy) = *self.body.front().unwrap();
            let (dx, dy) = self.dir.delta();
            let mut nx = hx + dx;
            let mut ny = hy + dy;
            if self.wrap_walls {
                nx = nx.rem_euclid(Self::W);
                ny = ny.rem_euclid(Self::H);
            } else if nx < 0 || nx >= Self::W || ny < 0 || ny >= Self::H {
                self.game_over = true;
                break;
            }
            let ate = (nx, ny) == self.food;
            let tail = self.body.back().copied();
            let hit_self = self
                .body
                .iter()
                .any(|&seg| seg == (nx, ny) && (ate || Some(seg) != tail));
            if hit_self {
                self.game_over = true;
                break;
            }
            self.body.push_front((nx, ny));
            if ate {
                self.score += 10;
                if self.score > self.best {
                    self.best = self.score;
                }
                self.spawn_food();
            } else {
                self.body.pop_back();
            }
        }
        PluginAction::Continue
    }

    fn render(&self, ui: &mut UiEngine, theme: ThemeColors, uk: bool) {
        let cell = 32.0f32;
        let pad = 10.0f32;
        let bx = 32.0f32;
        let by = 146.0f32;
        let bw = Self::W as f32 * cell + pad * 2.0;
        let bh = Self::H as f32 * cell + pad * 2.0;

        ui.plugin_stroke_rect(bx, by, bw, bh, 8.0, 4.0, theme.fg_rgb);

        // Subtle dot grid
        for gy in 0..Self::H {
            for gx in 0..Self::W {
                let cx = bx + pad + gx as f32 * cell + cell * 0.5;
                let cy = by + pad + gy as f32 * cell + cell * 0.5;
                ui.plugin_fill_rect(cx - 1.5, cy - 1.5, 3.0, 3.0, 1.0, theme.fg1_rgb);
            }
        }

        // Food
        let fx = bx + pad + self.food.0 as f32 * cell + cell * 0.5;
        let fy = by + pad + self.food.1 as f32 * cell + cell * 0.5;
        ui.plugin_fill_circle(fx, fy, cell * 0.36, theme.fg_rgb);
        ui.plugin_fill_circle(fx - 3.0, fy - 3.0, cell * 0.12, theme.bg_rgb);

        // Snake segments
        for (i, &(sx, sy)) in self.body.iter().enumerate() {
            let x = bx + pad + sx as f32 * cell + 2.0;
            let y = by + pad + sy as f32 * cell + 2.0;
            let col = if i == 0 {
                theme.fg_rgb
            } else if i % 2 == 0 {
                theme.fg2_rgb
            } else {
                theme.fg_rgb
            };
            ui.plugin_fill_rect(x, y, cell - 4.0, cell - 4.0, if i == 0 { 7.0 } else { 5.0 }, col);
        }

        // Right-hand HUD panel
        let px = bx + bw + 28.0;
        let pw = 1024.0 - px - 32.0;
        ui.plugin_stroke_rect(px, by, pw, 220.0, 8.0, 4.0, theme.fg1_rgb);
        ui.plugin_draw_text(px + 20.0, by + 36.0, if uk { "РАХУНОК" } else { "SCORE" }, 24.0, 2, 0, theme.fg2_rgb);
        ui.plugin_draw_text(px + 20.0, by + 80.0, &self.score.to_string(), 48.0, 3, 0, theme.fg_rgb);
        ui.plugin_draw_text(px + 20.0, by + 136.0, if uk { "РЕКОРД" } else { "BEST" }, 24.0, 2, 0, theme.fg2_rgb);
        ui.plugin_draw_text(px + 20.0, by + 178.0, &self.best.to_string(), 38.0, 3, 0, theme.fg_rgb);

        let mode_lbl = if self.wrap_walls {
            if uk { "СТІНИ: ПОРТАЛ" } else { "WALLS: WRAP" }
        } else if uk {
            "СТІНИ: КЛАСИКА"
        } else {
            "WALLS: CLASSIC"
        };
        ui.plugin_stroke_rect(px, by + 240.0, pw, 172.0, 8.0, 4.0, theme.fg1_rgb);
        ui.plugin_draw_text(px + 20.0, by + 280.0, mode_lbl, 22.0, 2, 0, theme.fg_rgb);
        ui.plugin_draw_text(px + 20.0, by + 324.0, if uk { "X — Режим стін" } else { "X — Wall mode" }, 22.0, 0, 0, theme.fg2_rgb);
        ui.plugin_draw_text(px + 20.0, by + 358.0, if uk { "START — Пауза" } else { "START — Pause" }, 22.0, 0, 0, theme.fg2_rgb);
        ui.plugin_draw_text(px + 20.0, by + 390.0, if uk { "B — Вихід" } else { "B — Exit" }, 22.0, 0, 0, theme.fg2_rgb);

        if self.paused || self.game_over {
            let ox = bx + 120.0;
            let oy = by + bh * 0.5 - 78.0;
            ui.plugin_fill_rect(ox, oy, bw - 240.0, 156.0, 10.0, theme.bg_rgb);
            ui.plugin_stroke_rect(ox, oy, bw - 240.0, 156.0, 10.0, 4.0, theme.fg_rgb);
            let msg = if self.game_over {
                if uk { "ГРА ЗАКІНЧЕНА" } else { "GAME OVER" }
            } else if uk {
                "ПАУЗА"
            } else {
                "PAUSED"
            };
            let sub = if self.game_over {
                if uk { "A — Нова гра  •  B — Вихід" } else { "A — Retry  •  B — Exit" }
            } else if uk {
                "START — Продовжити"
            } else {
                "START — Resume"
            };
            ui.plugin_draw_text(bx + bw * 0.5, oy + 58.0, msg, 38.0, 3, 1, theme.fg_rgb);
            ui.plugin_draw_text(bx + bw * 0.5, oy + 112.0, sub, 24.0, 1, 1, theme.fg2_rgb);
        }
    }
}

/* ============================================================================
 * 2. TETRIS (7-Bag Guideline + Ghost Piece)
 * ========================================================================== */
const TETRO_SHAPES: [[[(i32, i32); 4]; 4]; 7] = [
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

pub struct TetrisGame {
    board: [[u8; 10]; 20],
    piece: usize,
    next_piece: usize,
    rot: usize,
    px: i32,
    py: i32,
    bag: Vec<usize>,
    score: u32,
    lines: u32,
    level: u32,
    paused: bool,
    game_over: bool,
    fall_accum: f32,
    das_accum: f32,
    rng: Rng,
}

impl TetrisGame {
    fn new() -> Self {
        let mut g = Self {
            board: [[0; 10]; 20],
            piece: 0,
            next_piece: 1,
            rot: 0,
            px: 3,
            py: 0,
            bag: Vec::new(),
            score: 0,
            lines: 0,
            level: 1,
            paused: false,
            game_over: false,
            fall_accum: 0.0,
            das_accum: 0.0,
            rng: Rng::new(0x7E74_1501),
        };
        g.reset();
        g
    }

    fn pop_bag(&mut self) -> usize {
        if self.bag.is_empty() {
            let mut b = vec![0, 1, 2, 3, 4, 5, 6];
            for i in (1..b.len()).rev() {
                let j = self.rng.range(i + 1);
                b.swap(i, j);
            }
            self.bag = b;
        }
        self.bag.pop().unwrap_or(0)
    }

    fn reset(&mut self) {
        self.board = [[0; 10]; 20];
        self.bag.clear();
        self.score = 0;
        self.lines = 0;
        self.level = 1;
        self.paused = false;
        self.game_over = false;
        self.fall_accum = 0.0;
        self.next_piece = self.pop_bag();
        self.spawn_piece();
    }

    fn spawn_piece(&mut self) {
        self.piece = self.next_piece;
        self.next_piece = self.pop_bag();
        self.rot = 0;
        self.px = 3;
        self.py = 0;
        if !self.fits(self.piece, self.rot, self.px, self.py) {
            self.game_over = true;
        }
    }

    fn fits(&self, piece: usize, rot: usize, px: i32, py: i32) -> bool {
        for &(cx, cy) in &TETRO_SHAPES[piece][rot] {
            let x = px + cx;
            let y = py + cy;
            if x < 0 || x >= 10 || y >= 20 {
                return false;
            }
            if y >= 0 && self.board[y as usize][x as usize] != 0 {
                return false;
            }
        }
        true
    }

    fn lock_piece(&mut self) {
        for &(cx, cy) in &TETRO_SHAPES[self.piece][self.rot] {
            let x = self.px + cx;
            let y = self.py + cy;
            if (0..20).contains(&y) && (0..10).contains(&x) {
                self.board[y as usize][x as usize] = (self.piece as u8) + 1;
            }
        }
        let mut cleared = 0u32;
        let mut dst = 19isize;
        for src in (0..20).rev() {
            if self.board[src].iter().all(|&v| v != 0) {
                cleared += 1;
            } else {
                self.board[dst as usize] = self.board[src];
                dst -= 1;
            }
        }
        while dst >= 0 {
            self.board[dst as usize] = [0; 10];
            dst -= 1;
        }
        if cleared > 0 {
            self.lines += cleared;
            self.level = 1 + self.lines / 10;
            let pts = match cleared {
                1 => 100,
                2 => 300,
                3 => 500,
                _ => 800,
            };
            self.score += pts * self.level;
        }
        self.spawn_piece();
    }

    fn ghost_y(&self) -> i32 {
        let mut gy = self.py;
        while self.fits(self.piece, self.rot, self.px, gy + 1) {
            gy += 1;
        }
        gy
    }

    fn update(&mut self, dt: f32, pressed: u32, held: u32) -> PluginAction {
        if (pressed & btn::B) != 0 {
            return PluginAction::Exit;
        }
        if self.game_over {
            if (pressed & (btn::A | btn::START)) != 0 {
                self.reset();
            }
            return PluginAction::Continue;
        }
        if (pressed & (btn::START | btn::MENU)) != 0 {
            self.paused = !self.paused;
            return PluginAction::Continue;
        }
        if self.paused {
            return PluginAction::Continue;
        }

        if (pressed & btn::LEFT) != 0 && self.fits(self.piece, self.rot, self.px - 1, self.py) {
            self.px -= 1;
            self.das_accum = 0.0;
        }
        if (pressed & btn::RIGHT) != 0 && self.fits(self.piece, self.rot, self.px + 1, self.py) {
            self.px += 1;
            self.das_accum = 0.0;
        }
        if (held & (btn::LEFT | btn::RIGHT)) != 0 {
            self.das_accum += dt;
            if self.das_accum > 0.17 {
                self.das_accum -= 0.055;
                let dir = if (held & btn::LEFT) != 0 { -1 } else { 1 };
                if self.fits(self.piece, self.rot, self.px + dir, self.py) {
                    self.px += dir;
                }
            }
        } else {
            self.das_accum = 0.0;
        }

        // Rotate CW (A or X)
        if (pressed & (btn::A | btn::X)) != 0 {
            let nr = (self.rot + 1) & 3;
            for kick in [0, -1, 1, -2, 2] {
                if self.fits(self.piece, nr, self.px + kick, self.py) {
                    self.rot = nr;
                    self.px += kick;
                    break;
                }
            }
        }
        // Hard drop (UP)
        if (pressed & btn::UP) != 0 {
            let gy = self.ghost_y();
            self.score += ((gy - self.py).max(0) as u32) * 2;
            self.py = gy;
            self.lock_piece();
            return PluginAction::Continue;
        }

        let grav = (0.72 * (0.86f32).powi((self.level.saturating_sub(1)) as i32)).max(0.08);
        let speed = if (held & btn::DOWN) != 0 { 0.045 } else { grav };
        self.fall_accum += dt;
        if self.fall_accum >= speed {
            self.fall_accum = 0.0;
            if self.fits(self.piece, self.rot, self.px, self.py + 1) {
                self.py += 1;
                if (held & btn::DOWN) != 0 {
                    self.score += 1;
                }
            } else {
                self.lock_piece();
            }
        }
        PluginAction::Continue
    }

    fn render(&self, ui: &mut UiEngine, theme: ThemeColors, uk: bool) {
        let cell = 29.0f32;
        let pad = 6.0f32;
        let ww = 10.0 * cell + pad * 2.0;
        let wh = 20.0 * cell + pad * 2.0;
        let wx = (1024.0 - ww) * 0.5;
        let wy = 144.0f32;

        ui.plugin_stroke_rect(wx, wy, ww, wh, 8.0, 4.0, theme.fg_rgb);

        // Board cells
        for r in 0..20 {
            for c in 0..10 {
                let x = wx + pad + c as f32 * cell;
                let y = wy + pad + r as f32 * cell;
                let v = self.board[r][c];
                if v != 0 {
                    let col = if v % 2 == 0 { theme.fg2_rgb } else { theme.fg_rgb };
                    ui.plugin_fill_rect(x + 2.0, y + 2.0, cell - 4.0, cell - 4.0, 4.0, col);
                } else {
                    ui.plugin_fill_rect(x + cell * 0.5 - 1.0, y + cell * 0.5 - 1.0, 2.0, 2.0, 0.5, theme.fg1_rgb);
                }
            }
        }

        // Ghost piece
        let gy = self.ghost_y();
        for &(cx, cy) in &TETRO_SHAPES[self.piece][self.rot] {
            let x = wx + pad + (self.px + cx) as f32 * cell;
            let y = wy + pad + (gy + cy) as f32 * cell;
            if gy + cy >= 0 {
                ui.plugin_stroke_rect(x + 3.0, y + 3.0, cell - 6.0, cell - 6.0, 4.0, 2.0, theme.fg1_rgb);
            }
        }

        // Active piece
        for &(cx, cy) in &TETRO_SHAPES[self.piece][self.rot] {
            let x = wx + pad + (self.px + cx) as f32 * cell;
            let y = wy + pad + (self.py + cy) as f32 * cell;
            if self.py + cy >= 0 {
                ui.plugin_fill_rect(x + 2.0, y + 2.0, cell - 4.0, cell - 4.0, 4.0, theme.fg_rgb);
                ui.plugin_fill_rect(x + 8.0, y + 8.0, cell - 16.0, cell - 16.0, 2.0, theme.fg1_rgb);
            }
        }

        // Left stats card
        let lx = 36.0f32;
        let lw = wx - 64.0;
        ui.plugin_stroke_rect(lx, wy, lw, 290.0, 8.0, 4.0, theme.fg1_rgb);
        ui.plugin_draw_text(lx + 20.0, wy + 38.0, if uk { "РАХУНОК" } else { "SCORE" }, 22.0, 2, 0, theme.fg2_rgb);
        ui.plugin_draw_text(lx + 20.0, wy + 80.0, &self.score.to_string(), 42.0, 3, 0, theme.fg_rgb);
        ui.plugin_draw_text(lx + 20.0, wy + 142.0, if uk { "ЛІНІЇ" } else { "LINES" }, 22.0, 2, 0, theme.fg2_rgb);
        ui.plugin_draw_text(lx + 20.0, wy + 180.0, &self.lines.to_string(), 36.0, 3, 0, theme.fg_rgb);
        ui.plugin_draw_text(lx + 20.0, wy + 236.0, if uk { "РІВЕНЬ" } else { "LEVEL" }, 22.0, 2, 0, theme.fg2_rgb);
        ui.plugin_draw_text(lx + 150.0, wy + 236.0, &self.level.to_string(), 32.0, 3, 0, theme.fg_rgb);

        // Right Next + Controls card
        let rx = wx + ww + 28.0;
        let rw = 1024.0 - rx - 36.0;
        ui.plugin_stroke_rect(rx, wy, rw, 190.0, 8.0, 4.0, theme.fg1_rgb);
        ui.plugin_draw_text(rx + 20.0, wy + 34.0, if uk { "ДАЛІ" } else { "NEXT" }, 22.0, 2, 0, theme.fg2_rgb);
        for &(cx, cy) in &TETRO_SHAPES[self.next_piece][0] {
            let x = rx + 64.0 + cx as f32 * 30.0;
            let y = wy + 68.0 + cy as f32 * 30.0;
            ui.plugin_fill_rect(x, y, 26.0, 26.0, 4.0, theme.fg_rgb);
        }

        ui.plugin_stroke_rect(rx, wy + 214.0, rw, 210.0, 8.0, 4.0, theme.fg1_rgb);
        ui.plugin_draw_text(rx + 20.0, wy + 252.0, if uk { "A — Обертати" } else { "A — Rotate" }, 22.0, 0, 0, theme.fg2_rgb);
        ui.plugin_draw_text(rx + 20.0, wy + 292.0, if uk { "↑ — Скинути" } else { "↑ — Hard Drop" }, 22.0, 0, 0, theme.fg2_rgb);
        ui.plugin_draw_text(rx + 20.0, wy + 332.0, if uk { "↓ — Прискорити" } else { "↓ — Soft Drop" }, 22.0, 0, 0, theme.fg2_rgb);
        ui.plugin_draw_text(rx + 20.0, wy + 372.0, if uk { "B — Вихід" } else { "B — Exit" }, 22.0, 0, 0, theme.fg2_rgb);

        if self.paused || self.game_over {
            let ox = wx - 36.0;
            let oy = wy + wh * 0.5 - 70.0;
            ui.plugin_fill_rect(ox, oy, ww + 72.0, 140.0, 10.0, theme.bg_rgb);
            ui.plugin_stroke_rect(ox, oy, ww + 72.0, 140.0, 10.0, 4.0, theme.fg_rgb);
            let msg = if self.game_over {
                if uk { "ГРА ЗАКІНЧЕНА" } else { "GAME OVER" }
            } else if uk {
                "ПАУЗА"
            } else {
                "PAUSED"
            };
            ui.plugin_draw_text(wx + ww * 0.5, oy + 52.0, msg, 34.0, 3, 1, theme.fg_rgb);
            ui.plugin_draw_text(
                wx + ww * 0.5,
                oy + 102.0,
                if self.game_over { "A — Retry" } else { "START — Resume" },
                22.0,
                1,
                1,
                theme.fg2_rgb,
            );
        }
    }
}

/* ============================================================================
 * 3. 2048
 * ========================================================================== */
pub struct Game2048 {
    grid: [[u32; 4]; 4],
    score: u32,
    best: u32,
    over: bool,
    rng: Rng,
}

impl Game2048 {
    fn new() -> Self {
        let mut g = Self {
            grid: [[0; 4]; 4],
            score: 0,
            best: 0,
            over: false,
            rng: Rng::new(0x2048_CAFE),
        };
        g.reset();
        g
    }

    fn reset(&mut self) {
        self.grid = [[0; 4]; 4];
        self.score = 0;
        self.over = false;
        self.spawn();
        self.spawn();
    }

    fn spawn(&mut self) {
        let mut empty = Vec::new();
        for r in 0..4 {
            for c in 0..4 {
                if self.grid[r][c] == 0 {
                    empty.push((r, c));
                }
            }
        }
        if let Some(&(r, c)) = empty.get(self.rng.range(empty.len())) {
            self.grid[r][c] = if self.rng.range(10) == 0 { 4 } else { 2 };
        }
    }

    fn slide_line(line: [u32; 4]) -> ([u32; 4], u32, bool) {
        let mut vals: Vec<u32> = line.into_iter().filter(|&v| v != 0).collect();
        let mut out = [0u32; 4];
        let mut gained = 0u32;
        let mut dst = 0;
        let mut i = 0;
        while i < vals.len() {
            if i + 1 < vals.len() && vals[i] == vals[i + 1] {
                let merged = vals[i] * 2;
                out[dst] = merged;
                gained += merged;
                i += 2;
            } else {
                out[dst] = vals[i];
                i += 1;
            }
            dst += 1;
        }
        vals.clear();
        (out, gained, out != line)
    }

    fn move_dir(&mut self, dir: u8) {
        let mut moved = false;
        for i in 0..4 {
            let line = match dir {
                0 => self.grid[i],
                1 => [self.grid[i][3], self.grid[i][2], self.grid[i][1], self.grid[i][0]],
                2 => [self.grid[0][i], self.grid[1][i], self.grid[2][i], self.grid[3][i]],
                _ => [self.grid[3][i], self.grid[2][i], self.grid[1][i], self.grid[0][i]],
            };
            let (out, gained, ch) = Self::slide_line(line);
            if ch {
                moved = true;
                self.score += gained;
                self.best = self.best.max(self.score);
                match dir {
                    0 => self.grid[i] = out,
                    1 => {
                        for c in 0..4 {
                            self.grid[i][3 - c] = out[c];
                        }
                    }
                    2 => {
                        for r in 0..4 {
                            self.grid[r][i] = out[r];
                        }
                    }
                    _ => {
                        for r in 0..4 {
                            self.grid[3 - r][i] = out[r];
                        }
                    }
                }
            }
        }
        if moved {
            self.spawn();
        }
    }

    fn update(&mut self, _dt: f32, pressed: u32) -> PluginAction {
        if (pressed & btn::B) != 0 {
            return PluginAction::Exit;
        }
        if (pressed & (btn::START | btn::X)) != 0 {
            self.reset();
            return PluginAction::Continue;
        }
        if (pressed & btn::LEFT) != 0 {
            self.move_dir(0);
        } else if (pressed & btn::RIGHT) != 0 {
            self.move_dir(1);
        } else if (pressed & btn::UP) != 0 {
            self.move_dir(2);
        } else if (pressed & btn::DOWN) != 0 {
            self.move_dir(3);
        }
        PluginAction::Continue
    }

    fn render(&self, ui: &mut UiEngine, theme: ThemeColors, uk: bool) {
        let board_s = 540.0f32;
        let bx = 48.0f32;
        let by = 166.0f32;
        ui.plugin_stroke_rect(bx, by, board_s, board_s, 12.0, 4.0, theme.fg_rgb);

        let gap = 14.0f32;
        let cell = (board_s - gap * 5.0) / 4.0;
        for r in 0..4 {
            for c in 0..4 {
                let x = bx + gap + c as f32 * (cell + gap);
                let y = by + gap + r as f32 * (cell + gap);
                let v = self.grid[r][c];
                if v == 0 {
                    ui.plugin_stroke_rect(x, y, cell, cell, 8.0, 2.0, theme.fg1_rgb);
                } else {
                    let (fill, text_col) = match v {
                        2 | 4 => (theme.fg1_rgb, theme.fg_rgb),
                        8 | 16 | 32 => (theme.fg2_rgb, theme.bg_rgb),
                        _ => (theme.fg_rgb, theme.bg_rgb),
                    };
                    ui.plugin_fill_rect(x, y, cell, cell, 8.0, fill);
                    let fsz = if v >= 1024 { 36.0 } else if v >= 128 { 44.0 } else { 52.0 };
                    ui.plugin_draw_text(
                        x + cell * 0.5,
                        y + cell * 0.5,
                        &v.to_string(),
                        fsz,
                        3,
                        1,
                        text_col,
                    );
                }
            }
        }

        let rx = bx + board_s + 36.0;
        let rw = 1024.0 - rx - 48.0;
        ui.plugin_stroke_rect(rx, by, rw, 220.0, 10.0, 4.0, theme.fg1_rgb);
        ui.plugin_draw_text(rx + 24.0, by + 40.0, if uk { "РАХУНОК" } else { "SCORE" }, 24.0, 2, 0, theme.fg2_rgb);
        ui.plugin_draw_text(rx + 24.0, by + 88.0, &self.score.to_string(), 48.0, 3, 0, theme.fg_rgb);
        ui.plugin_draw_text(rx + 24.0, by + 144.0, if uk { "РЕКОРД" } else { "BEST" }, 24.0, 2, 0, theme.fg2_rgb);
        ui.plugin_draw_text(rx + 24.0, by + 188.0, &self.best.to_string(), 38.0, 3, 0, theme.fg_rgb);

        ui.plugin_draw_text(rx + 8.0, by + 280.0, if uk { "D-Pad — Зсув плиток" } else { "D-Pad — Slide tiles" }, 24.0, 0, 0, theme.fg2_rgb);
        ui.plugin_draw_text(rx + 8.0, by + 320.0, if uk { "X / START — Заново" } else { "X / START — Restart" }, 24.0, 0, 0, theme.fg2_rgb);
        ui.plugin_draw_text(rx + 8.0, by + 360.0, if uk { "B — Вихід" } else { "B — Exit" }, 24.0, 0, 0, theme.fg2_rgb);
    }
}

/* ============================================================================
 * 4. BRICK BREAKER
 * ========================================================================== */
pub struct BrickBreakerGame {
    paddle_x: f32,
    ball_x: f32,
    ball_y: f32,
    vx: f32,
    vy: f32,
    bricks: [[u8; 10]; 5],
    score: u32,
    lives: u8,
    launched: bool,
}

impl BrickBreakerGame {
    fn new() -> Self {
        let mut g = Self {
            paddle_x: 512.0,
            ball_x: 512.0,
            ball_y: 660.0,
            vx: 260.0,
            vy: -360.0,
            bricks: [[1; 10]; 5],
            score: 0,
            lives: 3,
            launched: false,
        };
        g.reset();
        g
    }

    fn reset(&mut self) {
        for r in 0..5 {
            for c in 0..10 {
                self.bricks[r][c] = if r < 2 { 2 } else { 1 };
            }
        }
        self.score = 0;
        self.lives = 3;
        self.paddle_x = 512.0;
        self.reset_ball();
    }

    fn reset_ball(&mut self) {
        self.launched = false;
        self.ball_x = self.paddle_x;
        self.ball_y = 668.0;
        self.vx = 250.0;
        self.vy = -370.0;
    }

    fn update(&mut self, dt: f32, pressed: u32, held: u32) -> PluginAction {
        if (pressed & btn::B) != 0 {
            return PluginAction::Exit;
        }
        if (held & (btn::LEFT | btn::L1)) != 0 {
            self.paddle_x -= 560.0 * dt;
        }
        if (held & (btn::RIGHT | btn::R1)) != 0 {
            self.paddle_x += 560.0 * dt;
        }
        self.paddle_x = self.paddle_x.clamp(124.0, 900.0);

        if !self.launched {
            self.ball_x = self.paddle_x;
            self.ball_y = 668.0;
            if (pressed & (btn::A | btn::START)) != 0 {
                self.launched = true;
            }
            return PluginAction::Continue;
        }

        self.ball_x += self.vx * dt;
        self.ball_y += self.vy * dt;

        if self.ball_x < 46.0 {
            self.ball_x = 46.0;
            self.vx = self.vx.abs();
        } else if self.ball_x > 978.0 {
            self.ball_x = 978.0;
            self.vx = -self.vx.abs();
        }
        if self.ball_y < 156.0 {
            self.ball_y = 156.0;
            self.vy = self.vy.abs();
        }

        // Paddle bounce
        if self.vy > 0.0
            && self.ball_y >= 668.0
            && self.ball_y <= 696.0
            && (self.ball_x - self.paddle_x).abs() <= 94.0
        {
            let hit = (self.ball_x - self.paddle_x) / 94.0;
            self.vx = hit * 420.0;
            self.vy = -self.vy.abs().max(340.0);
        }

        // Brick collisions
        let bw = 88.0f32;
        let bh = 32.0f32;
        'bricks: for r in 0..5 {
            for c in 0..10 {
                if self.bricks[r][c] == 0 {
                    continue;
                }
                let x = 52.0 + c as f32 * (bw + 4.0);
                let y = 180.0 + r as f32 * (bh + 8.0);
                if self.ball_x + 10.0 >= x
                    && self.ball_x - 10.0 <= x + bw
                    && self.ball_y + 10.0 >= y
                    && self.ball_y - 10.0 <= y + bh
                {
                    self.bricks[r][c] -= 1;
                    self.score += 50;
                    self.vy = -self.vy;
                    break 'bricks;
                }
            }
        }

        if self.ball_y > 748.0 {
            if self.lives > 1 {
                self.lives -= 1;
                self.reset_ball();
            } else {
                self.reset();
            }
        }
        PluginAction::Continue
    }

    fn render(&self, ui: &mut UiEngine, theme: ThemeColors, uk: bool) {
        ui.plugin_stroke_rect(32.0, 144.0, 960.0, 592.0, 8.0, 4.0, theme.fg1_rgb);
        let bw = 88.0f32;
        let bh = 32.0f32;
        for r in 0..5 {
            for c in 0..10 {
                let hp = self.bricks[r][c];
                if hp == 0 {
                    continue;
                }
                let x = 52.0 + c as f32 * (bw + 4.0);
                let y = 180.0 + r as f32 * (bh + 8.0);
                let col = if hp >= 2 { theme.fg_rgb } else { theme.fg2_rgb };
                ui.plugin_fill_rect(x, y, bw, bh, 5.0, col);
            }
        }
        // Paddle & Ball
        ui.plugin_fill_rect(self.paddle_x - 84.0, 680.0, 168.0, 22.0, 8.0, theme.fg_rgb);
        ui.plugin_fill_circle(self.ball_x, self.ball_y, 11.0, theme.fg_rgb);

        let hud = format!(
            "{}: {}   •   {}: {}",
            if uk { "РАХУНОК" } else { "SCORE" },
            self.score,
            if uk { "ЖИТТЯ" } else { "LIVES" },
            self.lives
        );
        ui.plugin_draw_text(512.0, 720.0, &hud, 24.0, 2, 1, theme.fg2_rgb);
        if !self.launched {
            ui.plugin_draw_text(
                512.0,
                480.0,
                if uk { "Натисніть A для запуску" } else { "Press A to launch ball" },
                30.0,
                2,
                1,
                theme.fg_rgb,
            );
        }
    }
}

/* ============================================================================
 * 5. STOPWATCH
 * ========================================================================== */
pub struct StopwatchApp {
    elapsed_s: f64,
    running: bool,
    laps: Vec<f64>,
}

impl StopwatchApp {
    fn new() -> Self {
        Self {
            elapsed_s: 0.0,
            running: false,
            laps: Vec::new(),
        }
    }

    fn format_time(t: f64) -> String {
        let total_ms = (t * 1000.0) as u64;
        let mins = total_ms / 60_000;
        let secs = (total_ms / 1000) % 60;
        let centis = (total_ms % 1000) / 10;
        format!("{mins:02}:{secs:02}.{centis:02}")
    }

    fn update(&mut self, dt: f32, pressed: u32) -> PluginAction {
        if (pressed & btn::B) != 0 {
            return PluginAction::Exit;
        }
        if (pressed & (btn::A | btn::START)) != 0 {
            self.running = !self.running;
        }
        if (pressed & btn::X) != 0 && self.elapsed_s > 0.0 {
            self.laps.insert(0, self.elapsed_s);
            self.laps.truncate(5);
        }
        if (pressed & btn::Y) != 0 {
            self.elapsed_s = 0.0;
            self.running = false;
            self.laps.clear();
        }
        if self.running {
            self.elapsed_s += dt as f64;
        }
        PluginAction::Continue
    }

    fn render(&self, ui: &mut UiEngine, theme: ThemeColors, uk: bool) {
        let main = Self::format_time(self.elapsed_s);
        ui.plugin_stroke_rect(112.0, 176.0, 800.0, 180.0, 12.0, 4.0, theme.fg_rgb);
        ui.plugin_draw_text(512.0, 266.0, &main, 96.0, 3, 1, theme.fg_rgb);

        for (i, &lap) in self.laps.iter().enumerate() {
            let y = 410.0 + i as f32 * 52.0;
            let label = format!(
                "{} {}   —   {}",
                if uk { "Коло" } else { "Lap" },
                self.laps.len() - i,
                Self::format_time(lap)
            );
            ui.plugin_draw_text(512.0, y, &label, 30.0, 1, 1, theme.fg2_rgb);
        }

        let hint = if uk {
            "A — Старт/Стоп   •   X — Коло   •   Y — Скинути   •   B — Вихід"
        } else {
            "A — Start/Stop   •   X — Lap   •   Y — Reset   •   B — Exit"
        };
        ui.plugin_draw_text(512.0, 718.0, hint, 22.0, 1, 1, theme.fg2_rgb);
    }
}

/* ============================================================================
 * 6. TEXT VIEWER (Renders the real `pak/uipod/about.txt` manual)
 * ========================================================================== */
pub struct TextViewerApp {
    lines: Vec<String>,
    scroll: usize,
    font_idx: usize,
    hold_scroll_accum: f32,
}

impl TextViewerApp {
    fn new(raw: &str) -> Self {
        let mut lines = Vec::new();
        for para in raw.lines() {
            if para.trim().is_empty() {
                lines.push(String::new());
                continue;
            }
            let max_chars = 62usize;
            let mut cur = String::new();
            for word in para.split_whitespace() {
                if !cur.is_empty() && cur.chars().count() + 1 + word.chars().count() > max_chars {
                    lines.push(std::mem::take(&mut cur));
                }
                if !cur.is_empty() {
                    cur.push(' ');
                }
                cur.push_str(word);
            }
            if !cur.is_empty() {
                lines.push(cur);
            }
        }
        Self {
            lines,
            scroll: 0,
            font_idx: 1,
            hold_scroll_accum: 0.0,
        }
    }

    fn visible_rows(&self) -> usize {
        match self.font_idx {
            0 => 18,
            1 => 14,
            _ => 11,
        }
    }

    fn update(&mut self, dt: f32, pressed: u32, held: u32) -> PluginAction {
        if (pressed & btn::B) != 0 {
            return PluginAction::Exit;
        }
        if (pressed & btn::X) != 0 {
            self.font_idx = (self.font_idx + 1) % 3;
        }
        let vis = self.visible_rows();
        let max_scroll = self.lines.len().saturating_sub(vis);

        if (pressed & btn::UP) != 0 {
            self.scroll = self.scroll.saturating_sub(1);
            self.hold_scroll_accum = 0.0;
        }
        if (pressed & btn::DOWN) != 0 {
            self.scroll = (self.scroll + 1).min(max_scroll);
            self.hold_scroll_accum = 0.0;
        }
        if (pressed & (btn::LEFT | btn::L1)) != 0 {
            self.scroll = self.scroll.saturating_sub(vis);
        }
        if (pressed & (btn::RIGHT | btn::R1)) != 0 {
            self.scroll = (self.scroll + vis).min(max_scroll);
        }
        if (held & (btn::UP | btn::DOWN)) != 0 {
            self.hold_scroll_accum += dt;
            if self.hold_scroll_accum > 0.24 {
                self.hold_scroll_accum -= 0.06;
                if (held & btn::UP) != 0 {
                    self.scroll = self.scroll.saturating_sub(1);
                } else {
                    self.scroll = (self.scroll + 1).min(max_scroll);
                }
            }
        } else {
            self.hold_scroll_accum = 0.0;
        }
        PluginAction::Continue
    }

    fn render(&self, ui: &mut UiEngine, theme: ThemeColors, _uk: bool) {
        let vis = self.visible_rows();
        let (fsz, step) = match self.font_idx {
            0 => (24.0f32, 33.0f32),
            1 => (28.0f32, 41.0f32),
            _ => (34.0f32, 51.0f32),
        };
        let top_y = 160.0f32;
        for (i, line) in self.lines.iter().skip(self.scroll).take(vis).enumerate() {
            let y = top_y + i as f32 * step;
            let is_heading = line.starts_with('#');
            let clean = line.trim_start_matches('#').trim();
            ui.plugin_draw_text(
                36.0,
                y,
                clean,
                if is_heading { fsz + 4.0 } else { fsz },
                if is_heading { 3 } else { 0 },
                0,
                if is_heading { theme.fg_rgb } else { theme.fg2_rgb },
            );
        }
        ui.plugin_draw_scrollbar(
            theme,
            976.0,
            148.0,
            16.0,
            584.0,
            self.lines.len(),
            self.scroll,
            vis,
        );
    }
}

/* ============================================================================
 * 7. MUSIC QUIZ ("Name That Tune" — 4 choices, mid-song snippet, live 12-bar EQ)
 * ========================================================================== */
pub struct MusicQuizGame {
    pool: Vec<(String, u32)>,
    choices: Vec<usize>,
    correct_slot: usize,
    cursor: usize,
    score: u32,
    best: u32,
    round: u32,
    round_timer: f32,
    reveal_timer: f32,
    picked_slot: Option<usize>,
    last_won: bool,
    eq_bars: [f32; 12],
    needs_snippet: bool,
    rng: Rng,
}

impl MusicQuizGame {
    fn new(track_labels: &[(String, u32)]) -> Self {
        let pool = if !track_labels.is_empty() {
            track_labels.to_vec()
        } else {
            vec![
                ("Cyberia 99 - AquaBIOS 2000".into(), 23_220),
                ("NetRunner 2000 - Chrome Breakbeat (Y2K Mix)".into(), 23_325),
            ]
        };
        let mut g = Self {
            pool,
            choices: Vec::new(),
            correct_slot: 0,
            cursor: 0,
            score: 0,
            best: 0,
            round: 1,
            round_timer: 15.0,
            reveal_timer: 0.0,
            picked_slot: None,
            last_won: false,
            eq_bars: [0.16; 12],
            needs_snippet: true,
            rng: Rng::new(0x01C0_5123),
        };
        g.setup_round();
        g
    }

    fn setup_round(&mut self) {
        let n = self.pool.len().max(1);
        let choice_cnt = n.min(4);
        let target = self.rng.range(n);
        let mut picked = vec![target];
        let mut guard = 0;
        while picked.len() < choice_cnt && guard < 128 {
            guard += 1;
            let cand = self.rng.range(n);
            if !picked.contains(&cand) {
                picked.push(cand);
            }
        }
        for i in (1..picked.len()).rev() {
            let j = self.rng.range(i + 1);
            picked.swap(i, j);
        }
        self.correct_slot = picked.iter().position(|&idx| idx == target).unwrap_or(0);
        self.choices = picked;
        self.cursor = 0;
        self.round_timer = 15.0;
        self.reveal_timer = 0.0;
        self.picked_slot = None;
        self.needs_snippet = true;
    }

    fn snippet_offset_ms(&mut self, track_idx: usize) -> u32 {
        let dur = self.pool.get(track_idx).map(|t| t.1).unwrap_or(23_000);
        let lo = (dur as f32 * 0.15) as u32;
        let span = ((dur as f32 * 0.45) as u32).max(1000);
        lo + (self.rng.next_u32() % span)
    }

    fn update(&mut self, dt: f32, pressed: u32, pcm: &[i16]) -> PluginAction {
        if (pressed & btn::B) != 0 {
            return PluginAction::Exit;
        }

        // Update 12-bar live PCM mystery equalizer
        let frames = pcm.len() / 2;
        if frames >= 24 {
            let chunk = frames / 12;
            for (b, slot) in self.eq_bars.iter_mut().enumerate() {
                let s0 = b * chunk;
                let s1 = ((b + 1) * chunk).min(frames);
                let mut peak = 0.0f32;
                for f in s0..s1 {
                    let l = (pcm[2 * f] as f32).abs() / 32768.0;
                    let r = (pcm[2 * f + 1] as f32).abs() / 32768.0;
                    peak = peak.max(0.5 * (l + r));
                }
                let tgt = (peak * 2.5).clamp(0.10, 1.0);
                *slot += (tgt - *slot) * 0.36;
            }
        } else {
            for slot in &mut self.eq_bars {
                *slot = (*slot * 0.90).max(0.10);
            }
        }

        if self.needs_snippet {
            self.needs_snippet = false;
            let trk = self.choices.get(self.correct_slot).copied().unwrap_or(0);
            let pos = self.snippet_offset_ms(trk);
            return PluginAction::PlayTrackAt(trk, pos);
        }

        if self.reveal_timer > 0.0 {
            self.reveal_timer -= dt;
            if self.reveal_timer <= 0.0 || (pressed & btn::A) != 0 {
                self.round += 1;
                self.setup_round();
            }
            return PluginAction::Continue;
        }

        let n_choices = self.choices.len().max(1);
        if (pressed & btn::UP) != 0 {
            self.cursor = (self.cursor + n_choices - 1) % n_choices;
        } else if (pressed & btn::DOWN) != 0 {
            self.cursor = (self.cursor + 1) % n_choices;
        } else if (pressed & btn::X) != 0 {
            let trk = self.choices.get(self.correct_slot).copied().unwrap_or(0);
            let pos = self.snippet_offset_ms(trk);
            return PluginAction::PlayTrackAt(trk, pos);
        } else if (pressed & btn::A) != 0 {
            let won = self.cursor == self.correct_slot;
            self.picked_slot = Some(self.cursor);
            self.last_won = won;
            if won {
                self.score += 1;
                self.best = self.best.max(self.score);
            }
            self.reveal_timer = 1.45;
            return PluginAction::Continue;
        }

        self.round_timer = (self.round_timer - dt).max(0.0);
        if self.round_timer <= 0.0 {
            self.picked_slot = None;
            self.last_won = false;
            self.reveal_timer = 1.45;
        }
        PluginAction::Continue
    }

    fn render(&self, ui: &mut UiEngine, theme: ThemeColors, uk: bool) {
        // Top HUD card: Round, Score, Best + Live 12-bar Mystery Equalizer
        let hx = 36.0f32;
        let hy = 148.0f32;
        let hw = 952.0f32;
        let hh = 158.0f32;
        ui.plugin_stroke_rect(hx, hy, hw, hh, 10.0, 4.0, theme.fg1_rgb);

        let left_stat = format!(
            "{} {}   •   {}: {}   •   {}: {}",
            if uk { "РАУНД" } else { "ROUND" },
            self.round,
            if uk { "ОЧКИ" } else { "SCORE" },
            self.score,
            if uk { "РЕКОРД" } else { "BEST" },
            self.best
        );
        ui.plugin_draw_text(hx + 26.0, hy + 42.0, &left_stat, 26.0, 2, 0, theme.fg2_rgb);

        let prompt = if self.reveal_timer > 0.0 {
            if self.last_won {
                if uk { "Правильно! +1" } else { "Correct! +1" }
            } else if uk {
                "Не вгадав — правильна відповідь підсвічена"
            } else {
                "Missed — correct track highlighted"
            }
        } else if uk {
            "Який це трек?  (X — інший фрагмент)"
        } else {
            "Name that tune!  (X — another snippet)"
        };
        ui.plugin_draw_text(hx + 26.0, hy + 96.0, prompt, 30.0, 3, 0, theme.fg_rgb);

        // Live 12-bar audio-reactive mini equalizer on right side of HUD
        let eq_x = hx + hw - 246.0;
        let eq_base_y = hy + hh - 24.0;
        for (i, &lvl) in self.eq_bars.iter().enumerate() {
            let bh = (lvl * 98.0).clamp(10.0, 98.0);
            let bx = eq_x + i as f32 * 18.0;
            ui.plugin_fill_rect(bx, eq_base_y - bh, 12.0, bh, 3.0, theme.fg_rgb);
        }

        // Countdown timer bar
        ui.plugin_draw_progress_bar(theme, hx, hy + hh + 16.0, hw, 18.0, self.round_timer / 15.0);

        // Choice rows (2..4 depending on library size)
        let list_y = hy + hh + 54.0;
        let row_h = 84.0f32;
        let gap = 14.0f32;
        for (i, &song_idx) in self.choices.iter().enumerate() {
            let y = list_y + i as f32 * (row_h + gap);
            let is_sel = i == self.cursor;
            let is_correct = i == self.correct_slot;
            let revealing = self.reveal_timer > 0.0;

            let (fill, text_col, stroke) = if revealing && is_correct {
                (Some(theme.fg_rgb), theme.bg_rgb, theme.fg_rgb)
            } else if revealing && self.picked_slot == Some(i) && !is_correct {
                (Some(theme.fg1_rgb), theme.fg_rgb, theme.fg2_rgb)
            } else if is_sel {
                (Some(theme.fg_rgb), theme.bg_rgb, theme.fg_rgb)
            } else {
                (None, theme.fg_rgb, theme.fg1_rgb)
            };

            if let Some(bg) = fill {
                ui.plugin_fill_rect(hx, y, hw, row_h, 10.0, bg);
            } else {
                ui.plugin_stroke_rect(hx, y, hw, row_h, 10.0, 4.0, stroke);
            }

            let badge = format!("{}.", i + 1);
            ui.plugin_draw_text(hx + 28.0, y + row_h * 0.5, &badge, 32.0, 3, 0, text_col);

            let lbl = self
                .pool
                .get(song_idx)
                .map(|t| t.0.as_str())
                .unwrap_or("Track");
            ui.plugin_draw_text(hx + 84.0, y + row_h * 0.5, lbl, 30.0, 2, 0, text_col);
        }
    }
}

/* ============================================================================
 * 8. SOUND RACER (3-Lane 3D Perspective Rhythm Highway + Collector Tray)
 * ========================================================================== */
#[derive(Clone, Copy, PartialEq, Eq)]
enum RacerBlockKind {
    Heavy,  // fg (darkest / solid)
    Medium, // fg2
    Light,  // fg1
    Spike,  // hazard to jump with A
}

#[derive(Clone, Copy)]
struct RacerBlock {
    lane: usize, // 0, 1, 2
    z: f32,      // 1.0 = horizon, 0.0 = ship line
    kind: RacerBlockKind,
}

pub struct SoundRacerGame {
    lane: usize,
    ship_x: f32,
    jump_t: f32,
    blocks: Vec<RacerBlock>,
    tray: [[u8; 3]; 7], // 0=empty, 1=Heavy, 2=Medium, 3=Light
    score: u32,
    best: u32,
    combo: u32,
    energy_avg: f32,
    spawn_cd: f32,
    grid_phase: f32,
    started_audio: bool,
    paused: bool,
    track_name: String,
    callout: Option<(String, f32)>,
    rng: Rng,
}

impl SoundRacerGame {
    fn new() -> Self {
        Self {
            lane: 1,
            ship_x: 1.0,
            jump_t: 0.0,
            blocks: Vec::new(),
            tray: [[0; 3]; 7],
            score: 0,
            best: 0,
            combo: 0,
            energy_avg: 0.08,
            spawn_cd: 0.25,
            grid_phase: 0.0,
            started_audio: false,
            paused: false,
            track_name: String::new(),
            callout: None,
            rng: Rng::new(0x50AD_4ACE),
        }
    }

    fn push_tray(&mut self, lane: usize, kind: RacerBlockKind) {
        let code = match kind {
            RacerBlockKind::Heavy => 1,
            RacerBlockKind::Medium => 2,
            RacerBlockKind::Light => 3,
            RacerBlockKind::Spike => return,
        };
        for r in (0..7).rev() {
            if self.tray[r][lane] == 0 {
                self.tray[r][lane] = code;
                return;
            }
        }
        // Column full -> overflow clear with penalty
        for r in 0..7 {
            self.tray[r][lane] = 0;
        }
        self.combo = 0;
        self.callout = Some(("OVERFLOW".into(), 0.7));
    }

    fn cash_in_clusters(&mut self) -> u32 {
        let mut visited = [[false; 3]; 7];
        let mut total_pts = 0u32;
        let mut to_clear = Vec::new();
        for r in 0..7 {
            for c in 0..3 {
                let v = self.tray[r][c];
                if v == 0 || visited[r][c] {
                    continue;
                }
                let mut stack = vec![(r, c)];
                let mut comp = Vec::new();
                visited[r][c] = true;
                while let Some((cr, cc)) = stack.pop() {
                    comp.push((cr, cc));
                    for (dr, dc) in [(-1i32, 0), (1, 0), (0, -1), (0, 1)] {
                        let nr = cr as i32 + dr;
                        let nc = cc as i32 + dc;
                        if (0..7).contains(&nr) && (0..3).contains(&nc) {
                            let (ur, uc) = (nr as usize, nc as usize);
                            if !visited[ur][uc] && self.tray[ur][uc] == v {
                                visited[ur][uc] = true;
                                stack.push((ur, uc));
                            }
                        }
                    }
                }
                if comp.len() >= 3 {
                    total_pts += (comp.len() as u32) * (comp.len() as u32) * 35;
                    to_clear.extend(comp);
                }
            }
        }
        if to_clear.is_empty() {
            return 0;
        }
        for (r, c) in to_clear {
            self.tray[r][c] = 0;
        }
        // Gravity per column
        for c in 0..3 {
            let mut dst = 6isize;
            for r in (0..7).rev() {
                if self.tray[r][c] != 0 {
                    let v = self.tray[r][c];
                    self.tray[r][c] = 0;
                    self.tray[dst as usize][c] = v;
                    dst -= 1;
                }
            }
        }
        self.score += total_pts;
        self.best = self.best.max(self.score);
        self.callout = Some((format!("CLUSTER +{total_pts}"), 0.85));
        total_pts
    }

    fn update(
        &mut self,
        dt: f32,
        pressed: u32,
        _held: u32,
        pcm: &[i16],
        track_title: &str,
    ) -> PluginAction {
        if self.track_name != track_title {
            self.track_name = track_title.to_string();
        }
        if !self.started_audio {
            self.started_audio = true;
            return PluginAction::EnsurePlaying;
        }
        if (pressed & btn::L2) != 0 {
            return PluginAction::PrevTrack;
        }
        if (pressed & btn::R2) != 0 {
            return PluginAction::NextTrack;
        }
        if (pressed & (btn::START | btn::MENU)) != 0 {
            self.paused = !self.paused;
            return PluginAction::Continue;
        }
        if self.paused {
            if (pressed & btn::B) != 0 {
                return PluginAction::Exit;
            }
            return PluginAction::Continue;
        }

        // B cashes in clusters if available, otherwise exits to menu cleanly!
        if (pressed & btn::B) != 0 {
            if self.cash_in_clusters() == 0 {
                return PluginAction::Exit;
            }
            return PluginAction::Continue;
        }

        if (pressed & (btn::LEFT | btn::L1)) != 0 {
            self.lane = self.lane.saturating_sub(1);
        }
        if (pressed & (btn::RIGHT | btn::R1)) != 0 {
            self.lane = (self.lane + 1).min(2);
        }
        if (pressed & (btn::A | btn::UP)) != 0 && self.jump_t <= 0.0 {
            self.jump_t = 0.38;
        }
        if (pressed & btn::X) != 0 {
            self.cash_in_clusters();
        }

        // Smooth ship lane interpolation
        let target_x = self.lane as f32;
        self.ship_x += (target_x - self.ship_x) * (dt * 18.0).min(1.0);
        if self.jump_t > 0.0 {
            self.jump_t = (self.jump_t - dt).max(0.0);
        }
        if let Some((_, ref mut t)) = self.callout {
            *t -= dt;
            if *t <= 0.0 {
                self.callout = None;
            }
        }

        // Live stereo PCM energy + left/right balance analysis to generate highway blocks
        let frames = (pcm.len() / 2).min(512);
        let (mut sum_l, mut sum_r) = (0.0f32, 0.0f32);
        for i in 0..frames {
            let l = (pcm[2 * i] as f32) / 32768.0;
            let r = (pcm[2 * i + 1] as f32) / 32768.0;
            sum_l += l * l;
            sum_r += r * r;
        }
        let rms_l = if frames > 0 { (sum_l / frames as f32).sqrt() } else { 0.0 };
        let rms_r = if frames > 0 { (sum_r / frames as f32).sqrt() } else { 0.0 };
        let energy = 0.5 * (rms_l + rms_r);
        self.energy_avg += (energy - self.energy_avg) * (dt * 2.5).min(1.0);

        let speed = 0.72 + energy.clamp(0.0, 0.5) * 0.65;
        self.grid_phase = (self.grid_phase + dt * speed * 4.0).fract();

        self.spawn_cd -= dt;
        let beat_hit = energy > (self.energy_avg * 1.18).max(0.06);
        if self.spawn_cd <= 0.0 && (beat_hit || self.spawn_cd < -0.35) {
            self.spawn_cd = if beat_hit { 0.24 } else { 0.34 };
            let pan = rms_r - rms_l;
            let lane = if pan < -0.025 {
                0
            } else if pan > 0.025 {
                2
            } else {
                self.rng.range(3)
            };
            let kind = if energy > self.energy_avg * 1.55 && self.rng.range(4) == 0 {
                RacerBlockKind::Spike
            } else if energy > self.energy_avg * 1.32 {
                RacerBlockKind::Heavy
            } else if energy > self.energy_avg * 1.05 {
                RacerBlockKind::Medium
            } else {
                RacerBlockKind::Light
            };
            self.blocks.push(RacerBlock { lane, z: 1.0, kind });
        }

        // Advance blocks & resolve collisions at ship line (z <= 0.06)
        let mut remaining = Vec::with_capacity(self.blocks.len());
        let mut collected = Vec::new();
        for mut b in self.blocks.drain(..) {
            b.z -= dt * speed;
            if b.z <= 0.07 && b.z >= -0.04 {
                if b.lane == self.lane {
                    if b.kind == RacerBlockKind::Spike {
                        if self.jump_t > 0.04 {
                            self.score += 60;
                            self.combo += 1;
                            self.callout = Some(("JUMP +60".into(), 0.55));
                        } else {
                            self.combo = 0;
                            self.callout = Some(("SPIKE!".into(), 0.6));
                        }
                    } else {
                        collected.push((b.lane, b.kind));
                    }
                    continue;
                }
            }
            if b.z > -0.08 {
                remaining.push(b);
            }
        }
        self.blocks = remaining;

        for (lane, kind) in collected {
            let base = match kind {
                RacerBlockKind::Heavy => 35,
                RacerBlockKind::Medium => 25,
                RacerBlockKind::Light => 15,
                RacerBlockKind::Spike => 0,
            };
            self.combo += 1;
            let mult = 1 + (self.combo / 8).min(4);
            self.score += base * mult;
            self.best = self.best.max(self.score);
            self.push_tray(lane, kind);
        }

        PluginAction::Continue
    }

    fn project_lane(lane_f: f32, z: f32) -> (f32, f32, f32) {
        // z=1.0 horizon, z=0.0 ship
        let vx = 32.0f32;
        let vy = 176.0f32;
        let vw = 684.0f32;
        let vh = 556.0f32;
        let persp = (1.0 - z).powf(1.55);
        let y = vy + 64.0 + persp * (vh - 110.0);
        let half_w = 56.0 + persp * (vw * 0.44 - 56.0);
        let cx = vx + vw * 0.5 + (lane_f - 1.0) * (half_w * 0.66);
        let scale = 0.22 + persp * 0.78;
        (cx, y, scale)
    }

    fn render(&self, ui: &mut UiEngine, theme: ThemeColors, uk: bool) {
        let vx = 32.0f32;
        let vy = 176.0f32;
        let vw = 684.0f32;
        let vh = 556.0f32;

        // Track title bar above highway
        ui.plugin_draw_text(vx + 6.0, 152.0, &self.track_name, 26.0, 2, 0, theme.fg2_rgb);

        // Highway frame
        ui.plugin_stroke_rect(vx, vy, vw, vh, 10.0, 4.0, theme.fg_rgb);

        // Horizon line & perspective lane rails
        let hor_y = vy + 64.0;
        ui.plugin_draw_line(vx + 6.0, hor_y, vx + vw - 6.0, hor_y, 2.0, theme.fg1_rgb);
        for rail in [-1.5f32, -0.5, 0.5, 1.5] {
            let (tx, ty, _) = Self::project_lane(1.0 + rail, 1.0);
            let (bx, by, _) = Self::project_lane(1.0 + rail, 0.0);
            ui.plugin_draw_line(tx, ty, bx, by, if rail.abs() > 1.0 { 4.0 } else { 2.0 }, theme.fg2_rgb);
        }

        // Scrolling horizontal speed bars
        for i in 0..8 {
            let z = ((i as f32 + 1.0 - self.grid_phase) / 8.0).clamp(0.0, 1.0);
            let (lx, y, _) = Self::project_lane(-0.5, z);
            let (rx, _, _) = Self::project_lane(2.5, z);
            ui.plugin_draw_line(lx, y, rx, y, 2.0, theme.fg1_rgb);
        }

        // Highway blocks sorted far -> near
        let mut sorted = self.blocks.clone();
        sorted.sort_by(|a, b| b.z.partial_cmp(&a.z).unwrap_or(std::cmp::Ordering::Equal));
        for b in &sorted {
            let (cx, cy, sc) = Self::project_lane(b.lane as f32, b.z.clamp(0.0, 1.0));
            let bw = 112.0 * sc;
            let bh = 38.0 * sc;
            match b.kind {
                RacerBlockKind::Spike => {
                    ui.plugin_stroke_rect(cx - bw * 0.45, cy - bh, bw * 0.9, bh, 4.0, 4.0, theme.fg_rgb);
                    ui.plugin_draw_line(cx - bw * 0.38, cy - 4.0, cx, cy - bh - 12.0 * sc, 4.0, theme.fg_rgb);
                    ui.plugin_draw_line(cx + bw * 0.38, cy - 4.0, cx, cy - bh - 12.0 * sc, 4.0, theme.fg_rgb);
                }
                RacerBlockKind::Heavy => {
                    ui.plugin_fill_rect(cx - bw * 0.5, cy - bh, bw, bh, 6.0 * sc, theme.fg_rgb);
                }
                RacerBlockKind::Medium => {
                    ui.plugin_fill_rect(cx - bw * 0.5, cy - bh, bw, bh, 6.0 * sc, theme.fg2_rgb);
                    ui.plugin_stroke_rect(cx - bw * 0.5, cy - bh, bw, bh, 6.0 * sc, 2.0, theme.fg_rgb);
                }
                RacerBlockKind::Light => {
                    ui.plugin_fill_rect(cx - bw * 0.5, cy - bh, bw, bh, 6.0 * sc, theme.fg1_rgb);
                    ui.plugin_stroke_rect(cx - bw * 0.5, cy - bh, bw, bh, 6.0 * sc, 2.0, theme.fg2_rgb);
                }
            }
        }

        // Player Ship (jumps upward when jump_t > 0)
        let (sx, sy_base, _) = Self::project_lane(self.ship_x, 0.0);
        let jump_arc = if self.jump_t > 0.0 {
            let p = 1.0 - (self.jump_t / 0.38);
            (p * std::f32::consts::PI).sin() * 68.0
        } else {
            0.0
        };
        let sy = sy_base - jump_arc;
        if jump_arc > 2.0 {
            ui.plugin_fill_rect(sx - 44.0, sy_base - 6.0, 88.0, 10.0, 5.0, theme.fg1_rgb);
        }
        ui.plugin_fill_rect(sx - 54.0, sy - 26.0, 108.0, 26.0, 8.0, theme.fg_rgb);
        ui.plugin_fill_rect(sx - 24.0, sy - 38.0, 48.0, 16.0, 5.0, theme.fg2_rgb);
        ui.plugin_fill_rect(sx - 10.0, sy - 18.0, 20.0, 8.0, 3.0, theme.bg_rgb);

        if let Some((ref msg, _)) = self.callout {
            ui.plugin_draw_text(vx + vw * 0.5, vy + 128.0, msg, 34.0, 3, 1, theme.fg_rgb);
        }

        // Right-hand Collector Tray + Score HUD
        let rx = vx + vw + 24.0;
        let rw = 1024.0 - rx - 32.0;
        ui.plugin_stroke_rect(rx, vy, rw, 186.0, 10.0, 4.0, theme.fg1_rgb);
        ui.plugin_draw_text(rx + 18.0, vy + 34.0, if uk { "РАХУНОК" } else { "SCORE" }, 20.0, 2, 0, theme.fg2_rgb);
        ui.plugin_draw_text(rx + 18.0, vy + 74.0, &self.score.to_string(), 40.0, 3, 0, theme.fg_rgb);
        ui.plugin_draw_text(
            rx + 18.0,
            vy + 124.0,
            &format!("{} ×{}", if uk { "КОМБО" } else { "COMBO" }, 1 + (self.combo / 8).min(4)),
            22.0,
            2,
            0,
            theme.fg2_rgb,
        );
        ui.plugin_draw_text(
            rx + 18.0,
            vy + 158.0,
            &format!("{}: {}", if uk { "РЕК." } else { "BEST" }, self.best),
            20.0,
            1,
            0,
            theme.fg2_rgb,
        );

        // 3x7 Collector Tray
        let ty = vy + 202.0;
        let th = 270.0;
        ui.plugin_stroke_rect(rx, ty, rw, th, 10.0, 4.0, theme.fg_rgb);
        let cw = (rw - 24.0) / 3.0;
        let ch = (th - 24.0) / 7.0;
        for r in 0..7 {
            for c in 0..3 {
                let x = rx + 12.0 + c as f32 * cw + 3.0;
                let y = ty + 12.0 + r as f32 * ch + 3.0;
                let v = self.tray[r][c];
                if v == 0 {
                    ui.plugin_stroke_rect(x, y, cw - 6.0, ch - 6.0, 4.0, 1.5, theme.fg1_rgb);
                } else {
                    let col = match v {
                        1 => theme.fg_rgb,
                        2 => theme.fg2_rgb,
                        _ => theme.fg1_rgb,
                    };
                    ui.plugin_fill_rect(x, y, cw - 6.0, ch - 6.0, 4.0, col);
                }
            }
        }

        ui.plugin_draw_text(rx + 6.0, ty + th + 28.0, if uk { "A — Стрибок  •  X — Кластер" } else { "A — Jump  •  X — Cash In" }, 18.0, 0, 0, theme.fg2_rgb);
        ui.plugin_draw_text(rx + 6.0, ty + th + 56.0, if uk { "L2/R2 — Трек  •  B — Вихід" } else { "L2/R2 — Track  •  B — Exit" }, 18.0, 0, 0, theme.fg2_rgb);

        if self.paused {
            let ox = vx + 140.0;
            let oy = vy + vh * 0.5 - 64.0;
            ui.plugin_fill_rect(ox, oy, vw - 280.0, 128.0, 10.0, theme.bg_rgb);
            ui.plugin_stroke_rect(ox, oy, vw - 280.0, 128.0, 10.0, 4.0, theme.fg_rgb);
            ui.plugin_draw_text(vx + vw * 0.5, oy + 48.0, if uk { "ПАУЗА" } else { "PAUSED" }, 34.0, 3, 1, theme.fg_rgb);
            ui.plugin_draw_text(vx + vw * 0.5, oy + 92.0, if uk { "START — Далі  •  B — Вихід" } else { "START — Resume  •  B — Exit" }, 22.0, 1, 1, theme.fg2_rgb);
        }
    }
}
