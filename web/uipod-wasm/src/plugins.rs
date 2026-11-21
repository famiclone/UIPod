use std::collections::VecDeque;
use uipod_ui::layout::LIST_VISIBLE_ROWS;
use uipod_ui::{
    FileIconKind, HeaderState, ListRow, ListScreenState, RightAccessory, ThemeColors, UiEngine,
};

const CONTENT_TOP_Y: f32 = 136.0;

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
    Click,
    PlayTrackAt(usize, u32),
    SeekMs(u32),
    EnsurePlaying,
    TogglePlayPause,
    SetPlaying(bool),
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

#[derive(Debug, Clone)]
pub struct PluginMenuRow {
    pub label: String,
    pub right: Option<String>,
    pub has_chevron: bool,
}

impl PluginMenuRow {
    fn action(label: &str) -> Self {
        Self {
            label: label.to_string(),
            right: None,
            has_chevron: false,
        }
    }

    fn option(label: &str, val: &str) -> Self {
        Self {
            label: label.to_string(),
            right: Some(val.to_string()),
            has_chevron: false,
        }
    }

    fn submenu(label: &str) -> Self {
        Self {
            label: label.to_string(),
            right: None,
            has_chevron: true,
        }
    }
}

fn render_native_plugin_menu(
    ui: &mut UiEngine,
    theme: ThemeColors,
    header_base: &HeaderState<'_>,
    title: &str,
    items: &[PluginMenuRow],
    sel: usize,
) {
    let total = items.len();
    let vis = LIST_VISIBLE_ROWS;
    let sel = sel.min(total.saturating_sub(1));
    let start = if total <= vis {
        0
    } else if sel >= vis {
        (sel + 1 - vis).min(total - vis)
    } else {
        0
    };
    let end = (start + vis).min(total);
    let rows: Vec<ListRow<'_>> = items[start..end]
        .iter()
        .enumerate()
        .map(|(i, it)| ListRow {
            label: it.label.as_str(),
            selected: start + i == sel,
            file_icon: FileIconKind::None,
            right: if it.has_chevron {
                RightAccessory::Chevron
            } else if let Some(ref v) = it.right {
                RightAccessory::CycleValue(v.as_str())
            } else {
                RightAccessory::None
            },
        })
        .collect();

    let mut hdr = *header_base;
    hdr.title = title;
    let state = ListScreenState {
        theme,
        header: hdr,
        rows,
        total_items: total,
        start_item: start,
        visible_items: vis,
    };
    ui.render_list_to_fbo(&state);
}

fn render_splash_pill(ui: &mut UiEngine, theme: ThemeColors, msg: &str) {
    let w = (ui.plugin_measure_text(msg, 30.0, 2) + 72.0).clamp(280.0, 860.0);
    let h = 84.0f32;
    let x = (1024.0 - w) * 0.5;
    let y = (768.0 - h) * 0.5;
    ui.plugin_fill_rect(x, y, w, h, 14.0, theme.bg_rgb);
    ui.plugin_stroke_rect(x, y, w, h, 14.0, 4.0, theme.fg_rgb);
    ui.plugin_draw_text(512.0, y + h * 0.5, msg, 30.0, 2, 1, theme.fg_rgb);
}

#[derive(Clone, Copy)]
pub struct AudioSnapshot<'a> {
    pub playing: bool,
    pub track_title: &'a str,
    pub pos_ms: u32,
    pub dur_ms: u32,
}

pub enum ActivePlugin {
    MusicQuiz(MusicQuizGame),
    SoundRacer(SoundRacerGame),
    Tetris(TetrisPlugin),
    Snake(SnakePlugin),
    Game2048(Plugin2048),
    BrickBreaker(BrickBreakerPlugin),
    Stopwatch(StopwatchApp),
    TextViewer(TextViewerApp),
}

impl ActivePlugin {
    pub fn launch(id: PluginId, track_labels: &[(String, u32)], pos_ms: u32) -> Self {
        match id {
            PluginId::MusicQuiz => Self::MusicQuiz(MusicQuizGame::new(track_labels)),
            PluginId::SoundRacer => Self::SoundRacer(SoundRacerGame::new(pos_ms)),
            PluginId::Tetris => Self::Tetris(TetrisPlugin::new()),
            PluginId::Snake => Self::Snake(SnakePlugin::new()),
            PluginId::Game2048 => Self::Game2048(Plugin2048::new()),
            PluginId::BrickBreaker => Self::BrickBreaker(BrickBreakerPlugin::new()),
            PluginId::Stopwatch => Self::Stopwatch(StopwatchApp::new()),
            PluginId::TextViewer => Self::TextViewer(TextViewerApp::new(ABOUT_MANUAL_TEXT)),
        }
    }

    pub fn title(&self, uk: bool) -> &'static str {
        match self {
            Self::MusicQuiz(_) => if uk { "Вгадай мелодію" } else { "Music Quiz" },
            Self::SoundRacer(_) => "Sound Racer",
            Self::Tetris(_) => if uk { "Тетріс" } else { "Tetris" },
            Self::Snake(_) => if uk { "Змійка" } else { "Snake" },
            Self::Game2048(_) => "2048",
            Self::BrickBreaker(_) => if uk { "Цеглинки" } else { "Brick Breaker" },
            Self::Stopwatch(s) => s.title(uk),
            Self::TextViewer(_) => if uk { "Про UIPod" } else { "About UIPod" },
        }
    }

    pub fn update(
        &mut self,
        dt: f32,
        pressed: u32,
        held: u32,
        pcm: &[i16],
        audio: AudioSnapshot<'_>,
        uk: bool,
    ) -> PluginAction {
        match self {
            Self::MusicQuiz(g) => g.update(dt, pressed, pcm, audio, uk),
            Self::SoundRacer(g) => g.update(dt, pressed, pcm, audio, uk),
            Self::Tetris(g) => g.update(dt, pressed, held, uk),
            Self::Snake(g) => g.update(dt, pressed, held, uk),
            Self::Game2048(g) => g.update(dt, pressed, uk),
            Self::BrickBreaker(g) => g.update(dt, pressed, held, uk),
            Self::Stopwatch(a) => a.update(dt, pressed),
            Self::TextViewer(v) => v.update(dt, pressed, held, uk),
        }
    }

    pub fn render(
        &mut self,
        ui: &mut UiEngine,
        theme: ThemeColors,
        header: &HeaderState<'_>,
        uk: bool,
    ) {
        match self {
            Self::MusicQuiz(g) => g.render(ui, theme, header, uk),
            Self::SoundRacer(g) => g.render(ui, theme, header, uk),
            Self::Tetris(g) => g.render(ui, theme, header, uk),
            Self::Snake(g) => g.render(ui, theme, header, uk),
            Self::Game2048(g) => g.render(ui, theme, header, uk),
            Self::BrickBreaker(g) => g.render(ui, theme, header, uk),
            Self::Stopwatch(a) => {
                ui.plugin_begin(theme, theme.bg_rgb, Some(header));
                a.render(ui, theme, uk);
            }
            Self::TextViewer(v) => v.render(ui, theme, header, uk),
        }
    }
}

struct Rng64(u64);
impl Rng64 {
    fn new(seed: u64) -> Self {
        Self(seed | 1)
    }
    fn next_u64(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }
    fn next_u32(&mut self) -> u32 {
        (self.next_u64() >> 16) as u32
    }
    fn range(&mut self, max: usize) -> usize {
        if max <= 1 {
            0
        } else {
            (self.next_u64() as usize) % max
        }
    }
}

#[inline]
fn tr<'a>(uk: bool, en: &'a str, ua: &'a str) -> &'a str {
    if uk {
        ua
    } else {
        en
    }
}

fn nav_menu_input(pressed: u32, sel: &mut usize, count: usize) -> (bool, bool, i32) {
    if count == 0 {
        return (false, false, 0);
    }
    if (pressed & (btn::B | btn::MENU)) != 0 {
        return (true, false, 0);
    }
    if (pressed & btn::UP) != 0 {
        *sel = (*sel + count - 1) % count;
        return (false, false, 0);
    }
    if (pressed & btn::DOWN) != 0 {
        *sel = (*sel + 1) % count;
        return (false, false, 0);
    }
    if (pressed & btn::A) != 0 {
        return (false, true, 1);
    }
    if (pressed & btn::LEFT) != 0 {
        return (false, true, -1);
    }
    if (pressed & btn::RIGHT) != 0 {
        return (false, true, 1);
    }
    (false, false, 0)
}

/* ============================================================================
 * 1. TETRIS — 1:1 with `plugins/tetris/src/lib.rs`
 * ========================================================================== */
const TET_COLS: usize = 10;
const TET_VIS_ROWS: usize = 20;
const TET_HIDDEN_ROWS: usize = 4;
const TET_TOTAL_ROWS: usize = TET_VIS_ROWS + TET_HIDDEN_ROWS;
const TET_CELL: f32 = 29.0;
const TET_PAD: f32 = 6.0;
const TET_WELL_W: f32 = (TET_COLS as f32) * TET_CELL + TET_PAD * 2.0; // 302
const TET_WELL_H: f32 = (TET_VIS_ROWS as f32) * TET_CELL + TET_PAD * 2.0; // 592
const TET_WELL_X: f32 = (1024.0 - TET_WELL_W) * 0.5; // 361
const TET_WELL_Y: f32 = CONTENT_TOP_Y + 8.0; // 144

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
        let s = PIECE_SHAPES[(self.kind - 1) as usize][self.rot as usize];
        [
            (self.x + s[0].0, self.y + s[0].1),
            (self.x + s[1].0, self.y + s[1].1),
            (self.x + s[2].0, self.y + s[2].1),
            (self.x + s[3].0, self.y + s[3].1),
        ]
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum SubMenuMode {
    None,
    Main { sel: usize },
    Options { sel: usize },
}

pub struct TetrisPlugin {
    board: [[u8; TET_COLS]; TET_TOTAL_ROWS],
    cur: ActivePiece,
    hold: Option<u8>,
    hold_used: bool,
    queue: Vec<u8>,
    score: u32,
    best: u32,
    lines: u32,
    level: u32,
    show_ghost: bool,
    start_level: u32,
    game_over: bool,
    rng: Rng64,
    now_ms: u64,
    last_grav_ms: u64,
    lock_since_ms: Option<u64>,
    lock_resets: u8,
    clearing: Option<([bool; TET_TOTAL_ROWS], u64)>,
    das_dir: i32,
    das_next_ms: u64,
    last_soft_ms: u64,
    menu: SubMenuMode,
    splash: Option<(String, f32)>,
}

impl TetrisPlugin {
    fn new() -> Self {
        let mut g = Self {
            board: [[0; TET_COLS]; TET_TOTAL_ROWS],
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
            best: 0,
            lines: 0,
            level: 1,
            show_ghost: true,
            start_level: 1,
            game_over: false,
            rng: Rng64::new(0x7E74_1501_9E37_79B9),
            now_ms: 0,
            last_grav_ms: 0,
            lock_since_ms: None,
            lock_resets: 0,
            clearing: None,
            das_dir: 0,
            das_next_ms: 0,
            last_soft_ms: 0,
            menu: SubMenuMode::None,
            splash: None,
        };
        g.new_game();
        g
    }

    fn refill_bag(&mut self) {
        let mut bag = [1u8, 2, 3, 4, 5, 6, 7];
        for i in (1..7).rev() {
            let j = self.rng.range(i + 1);
            bag.swap(i, j);
        }
        self.queue.extend_from_slice(&bag);
    }

    fn new_game(&mut self) {
        self.board = [[0; TET_COLS]; TET_TOTAL_ROWS];
        self.hold = None;
        self.hold_used = false;
        self.queue.clear();
        self.score = 0;
        self.lines = 0;
        self.level = self.start_level.max(1);
        self.game_over = false;
        self.last_grav_ms = self.now_ms;
        self.lock_since_ms = None;
        self.lock_resets = 0;
        self.clearing = None;
        self.das_dir = 0;
        self.refill_bag();
        self.refill_bag();
        self.spawn_next();
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
            if cx < 0 || cx >= TET_COLS as i32 || cy < 0 || cy >= TET_TOTAL_ROWS as i32 {
                return false;
            }
            if self.board[cy as usize][cx as usize] != 0 {
                return false;
            }
        }
        true
    }

    fn is_grounded(&self) -> bool {
        let mut d = self.cur;
        d.y += 1;
        !self.fits(d)
    }

    fn ghost_piece(&self) -> ActivePiece {
        let mut g = self.cur;
        while {
            let mut n = g;
            n.y += 1;
            self.fits(n)
        } {
            g.y += 1;
        }
        g
    }

    fn spawn_kind(&mut self, kind: u8) {
        self.cur = ActivePiece {
            kind,
            rot: 0,
            x: 3,
            y: 2,
        };
        self.last_grav_ms = self.now_ms;
        self.lock_since_ms = None;
        self.lock_resets = 0;
        if !self.fits(self.cur) {
            self.cur.y = 1;
            if !self.fits(self.cur) {
                self.game_over = true;
            }
        }
    }

    fn spawn_next(&mut self) {
        if self.queue.len() < 7 {
            self.refill_bag();
        }
        let kind = self.queue.remove(0);
        self.hold_used = false;
        self.spawn_kind(kind);
    }

    fn add_score(&mut self, pts: u32) {
        self.score = self.score.saturating_add(pts);
        if self.score > self.best {
            self.best = self.score;
        }
    }

    fn refresh_lock(&mut self) {
        if self.is_grounded() {
            if self.lock_since_ms.is_some() && self.lock_resets < 12 {
                self.lock_since_ms = Some(self.now_ms);
                self.lock_resets += 1;
            } else if self.lock_since_ms.is_none() {
                self.lock_since_ms = Some(self.now_ms);
            }
        } else {
            self.lock_since_ms = None;
        }
    }

    fn try_move(&mut self, dx: i32, dy: i32) -> bool {
        let mut cand = self.cur;
        cand.x += dx;
        cand.y += dy;
        if self.fits(cand) {
            self.cur = cand;
            if dy > 0 {
                self.last_grav_ms = self.now_ms;
            }
            self.refresh_lock();
            true
        } else {
            false
        }
    }

    fn try_rotate(&mut self, cw: bool) -> bool {
        if self.cur.kind == 2 {
            return true;
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
                self.refresh_lock();
                return true;
            }
        }
        false
    }

    fn lock_piece(&mut self) {
        let mut above = true;
        for (cx, cy) in self.cur.cells() {
            if (0..TET_COLS as i32).contains(&cx) && (0..TET_TOTAL_ROWS as i32).contains(&cy) {
                self.board[cy as usize][cx as usize] = self.cur.kind;
                if cy >= TET_HIDDEN_ROWS as i32 {
                    above = false;
                }
            }
        }
        if above {
            self.game_over = true;
            return;
        }
        let mut mask = [false; TET_TOTAL_ROWS];
        let mut any = false;
        for r in 0..TET_TOTAL_ROWS {
            if self.board[r].iter().all(|&v| v != 0) {
                mask[r] = true;
                any = true;
            }
        }
        if any {
            self.clearing = Some((mask, self.now_ms + 150));
        } else {
            self.spawn_next();
        }
    }

    fn hard_drop(&mut self) {
        let g = self.ghost_piece();
        let dist = (g.y - self.cur.y).max(0) as u32;
        self.cur = g;
        self.add_score(dist * 2);
        self.lock_piece();
    }

    fn trigger_hold(&mut self) {
        if self.hold_used || self.game_over {
            return;
        }
        let k = self.cur.kind;
        if let Some(prev) = self.hold.replace(k) {
            self.hold_used = true;
            self.spawn_kind(prev);
        } else {
            self.spawn_next();
            self.hold_used = true;
        }
    }

    fn step_das(&mut self, left: bool, right: bool) -> i32 {
        let want = match (left, right) {
            (true, false) => -1,
            (false, true) => 1,
            _ => 0,
        };
        if want == 0 {
            self.das_dir = 0;
            return 0;
        }
        if want != self.das_dir {
            self.das_dir = want;
            self.das_next_ms = self.now_ms + 145;
            return want;
        }
        if self.now_ms >= self.das_next_ms {
            self.das_next_ms = self.now_ms + 36;
            return want;
        }
        0
    }

    fn draw_mino(ui: &mut UiEngine, t: ThemeColors, x: f32, y: f32, size: f32, kind: u8) {
        let p = 1.5f32;
        let bx = x + p;
        let by = y + p;
        let bw = size - p * 2.0;
        match kind {
            1 => {
                ui.plugin_fill_rect(bx, by, bw, bw, 4.0, t.fg_rgb);
                ui.plugin_stroke_rect(bx + 4.0, by + 4.0, bw - 8.0, bw - 8.0, 2.0, 2.0, t.bg_rgb);
            }
            2 => {
                ui.plugin_fill_rect(bx, by, bw, bw, 4.0, t.fg_rgb);
                ui.plugin_fill_rect(bx + 4.0, by + 4.0, bw - 8.0, bw - 8.0, 2.0, t.bg_rgb);
                ui.plugin_fill_rect(bx + 8.0, by + 8.0, bw - 16.0, bw - 16.0, 1.5, t.fg_rgb);
            }
            3 => {
                ui.plugin_fill_rect(bx, by, bw, bw, 4.0, t.fg2_rgb);
                ui.plugin_stroke_rect(bx + 1.0, by + 1.0, bw - 2.0, bw - 2.0, 3.5, 2.2, t.fg_rgb);
                ui.plugin_fill_circle(bx + bw * 0.5, by + bw * 0.5, 3.0, t.bg_rgb);
            }
            4 | 6 => {
                ui.plugin_fill_rect(bx, by, bw, bw, 4.0, t.fg1_rgb);
                ui.plugin_stroke_rect(bx + 1.0, by + 1.0, bw - 2.0, bw - 2.0, 3.5, 2.5, t.fg_rgb);
                if kind == 6 {
                    ui.plugin_fill_rect(bx + 7.0, by + 7.0, bw - 14.0, bw - 14.0, 2.0, t.fg2_rgb);
                }
            }
            _ => {
                ui.plugin_fill_rect(bx, by, bw, bw, 4.0, t.fg2_rgb);
                ui.plugin_stroke_rect(bx + 1.0, by + 1.0, bw - 2.0, bw - 2.0, 3.5, 2.2, t.fg_rgb);
                if kind == 7 {
                    ui.plugin_fill_rect(bx + 6.0, by + 6.0, 5.0, 5.0, 1.5, t.bg_rgb);
                }
            }
        }
    }

    fn draw_mini_piece(ui: &mut UiEngine, t: ThemeColors, cx: f32, cy: f32, cell: f32, kind: u8) {
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
            Self::draw_mino(ui, t, ox + sx as f32 * cell, oy + sy as f32 * cell, cell, kind);
        }
    }

    fn update(&mut self, dt: f32, pressed: u32, held: u32, uk: bool) -> PluginAction {
        if let Some((_, ref mut rem)) = self.splash {
            *rem -= dt;
            if *rem <= 0.0 {
                self.splash = None;
            }
        }

        match self.menu {
            SubMenuMode::Main { mut sel } => {
                let (back, act, _) = nav_menu_input(pressed, &mut sel, 5);
                self.menu = SubMenuMode::Main { sel };
                if back || (pressed & btn::START) != 0 {
                    self.menu = SubMenuMode::None;
                    self.last_grav_ms = self.now_ms;
                    self.lock_since_ms = None;
                    self.das_dir = 0;
                    return PluginAction::Click;
                }
                if act && (pressed & btn::A) != 0 {
                    match sel {
                        0 => {
                            self.menu = SubMenuMode::None;
                            self.last_grav_ms = self.now_ms;
                        }
                        1 => {
                            self.new_game();
                            self.menu = SubMenuMode::None;
                        }
                        2 => {
                            self.splash = Some((tr(uk, "Game saved", "Гру збережено").into(), 0.75));
                            self.menu = SubMenuMode::None;
                        }
                        3 => self.menu = SubMenuMode::Options { sel: 0 },
                        _ => return PluginAction::Exit,
                    }
                    return PluginAction::Click;
                }
                return PluginAction::Continue;
            }
            SubMenuMode::Options { mut sel } => {
                let (back, act, _) = nav_menu_input(pressed, &mut sel, 3);
                self.menu = SubMenuMode::Options { sel };
                if back {
                    self.menu = SubMenuMode::Main { sel: 3 };
                    return PluginAction::Click;
                }
                if act {
                    match sel {
                        0 => self.show_ghost = !self.show_ghost,
                        1 => {
                            self.start_level = match self.start_level {
                                1 => 5,
                                5 => 10,
                                _ => 1,
                            };
                            if self.lines == 0 && self.score == 0 {
                                self.level = self.start_level;
                            }
                        }
                        _ => {
                            self.best = self.score;
                            self.splash =
                                Some((tr(uk, "Best score reset", "Рекорд скинуто").into(), 0.75));
                        }
                    }
                    return PluginAction::Click;
                }
                return PluginAction::Continue;
            }
            SubMenuMode::None => {}
        }

        self.now_ms = self.now_ms.saturating_add((dt * 1000.0).round() as u64);

        if (pressed & (btn::START | btn::MENU)) != 0 {
            self.menu = SubMenuMode::Main { sel: 0 };
            return PluginAction::Click;
        }

        if self.game_over {
            if (pressed & btn::A) != 0 {
                self.new_game();
                return PluginAction::Click;
            }
            return PluginAction::Continue;
        }

        if let Some((mask, until)) = self.clearing {
            if self.now_ms >= until {
                self.clearing = None;
                let mut cleared = 0u32;
                let mut write = TET_TOTAL_ROWS;
                let mut next_board = [[0u8; TET_COLS]; TET_TOTAL_ROWS];
                for r in (0..TET_TOTAL_ROWS).rev() {
                    if mask[r] {
                        cleared += 1;
                    } else {
                        write -= 1;
                        next_board[write] = self.board[r];
                    }
                }
                self.board = next_board;
                self.lines += cleared;
                let new_lv = (self.start_level + self.lines / 10).max(1);
                if new_lv > self.level {
                    self.level = new_lv;
                }
                let base = match cleared {
                    1 => 100,
                    2 => 300,
                    3 => 500,
                    _ => 800,
                };
                self.add_score(base * self.level);
                self.spawn_next();
            }
            return PluginAction::Continue;
        }

        let dx = self.step_das((held & btn::LEFT) != 0, (held & btn::RIGHT) != 0);
        if dx != 0 {
            self.try_move(dx, 0);
        }
        if (pressed & btn::A) != 0 {
            self.try_rotate(true);
        } else if (pressed & (btn::B | btn::X)) != 0 {
            self.try_rotate(false);
        }
        if (pressed & (btn::L1 | btn::SELECT)) != 0 {
            self.trigger_hold();
        }
        if (pressed & (btn::UP | btn::Y | btn::R1)) != 0 {
            self.hard_drop();
            return PluginAction::Continue;
        } else if (held & btn::DOWN) != 0 && self.now_ms.saturating_sub(self.last_soft_ms) >= 42 {
            self.last_soft_ms = self.now_ms;
            if self.try_move(0, 1) {
                self.add_score(1);
            }
        }

        let grav = self.gravity_ms();
        while self.now_ms.saturating_sub(self.last_grav_ms) >= grav {
            self.last_grav_ms += grav;
            let mut down = self.cur;
            down.y += 1;
            if self.fits(down) {
                self.cur = down;
                if self.is_grounded() && self.lock_since_ms.is_none() {
                    self.lock_since_ms = Some(self.now_ms);
                }
            } else {
                if self.lock_since_ms.is_none() {
                    self.lock_since_ms = Some(self.now_ms);
                }
                break;
            }
        }
        if self.is_grounded() {
            let since = *self.lock_since_ms.get_or_insert(self.now_ms);
            if self.now_ms.saturating_sub(since) >= 480 {
                self.lock_piece();
            }
        } else {
            self.lock_since_ms = None;
        }

        PluginAction::Continue
    }

    fn render(&self, ui: &mut UiEngine, t: ThemeColors, header: &HeaderState<'_>, uk: bool) {
        match self.menu {
            SubMenuMode::Main { sel } => {
                let rows = [
                    PluginMenuRow::action(tr(uk, "Continue", "Продовжити")),
                    PluginMenuRow::action(tr(uk, "New Game", "Нова гра")),
                    PluginMenuRow::action(tr(uk, "Save Game", "Зберегти гру")),
                    PluginMenuRow::submenu(tr(uk, "Options", "Налаштування")),
                    PluginMenuRow::action(tr(uk, "Exit", "Вихід")),
                ];
                render_native_plugin_menu(ui, t, header, tr(uk, "Tetris", "Тетріс"), &rows, sel);
                return;
            }
            SubMenuMode::Options { sel } => {
                let ghost = if self.show_ghost {
                    tr(uk, "On", "Увімк")
                } else {
                    tr(uk, "Off", "Вимк")
                };
                let rows = [
                    PluginMenuRow::option(tr(uk, "Ghost Piece", "Тінь фігури"), ghost),
                    PluginMenuRow::option(
                        tr(uk, "Start Level", "Початковий рівень"),
                        &self.start_level.to_string(),
                    ),
                    PluginMenuRow::option(
                        tr(uk, "Reset Best Score", "Скинути рекорд"),
                        &self.best.to_string(),
                    ),
                ];
                render_native_plugin_menu(
                    ui,
                    t,
                    header,
                    tr(uk, "Options", "Налаштування"),
                    &rows,
                    sel,
                );
                if let Some((ref msg, _)) = self.splash {
                    render_splash_pill(ui, t, msg);
                }
                return;
            }
            SubMenuMode::None => {}
        }

        ui.plugin_begin(t, t.bg_rgb, Some(header));
        ui.plugin_stroke_rect(TET_WELL_X, TET_WELL_Y, TET_WELL_W, TET_WELL_H, 8.0, 4.5, t.fg_rgb);
        let mx0 = TET_WELL_X + TET_PAD;
        let my0 = TET_WELL_Y + TET_PAD;

        for vr in 1..TET_VIS_ROWS {
            for c in 1..TET_COLS {
                ui.plugin_fill_rect(
                    mx0 + (c as f32) * TET_CELL - 1.0,
                    my0 + (vr as f32) * TET_CELL - 1.0,
                    2.0,
                    2.0,
                    0.0,
                    t.fg1_rgb,
                );
            }
        }

        for vr in 0..TET_VIS_ROWS {
            let br = vr + TET_HIDDEN_ROWS;
            let flashing = self.clearing.as_ref().map(|(m, _)| m[br]).unwrap_or(false);
            if flashing {
                ui.plugin_fill_rect(
                    mx0 + 2.0,
                    my0 + (vr as f32) * TET_CELL + 3.0,
                    (TET_COLS as f32) * TET_CELL - 4.0,
                    TET_CELL - 6.0,
                    4.0,
                    t.fg_rgb,
                );
                continue;
            }
            for c in 0..TET_COLS {
                let kind = self.board[br][c];
                if kind != 0 {
                    Self::draw_mino(
                        ui,
                        t,
                        mx0 + (c as f32) * TET_CELL,
                        my0 + (vr as f32) * TET_CELL,
                        TET_CELL,
                        kind,
                    );
                }
            }
        }

        if self.clearing.is_none() && !self.game_over {
            if self.show_ghost {
                let g = self.ghost_piece();
                if g.y > self.cur.y {
                    for (gx, gy) in g.cells() {
                        if gy >= TET_HIDDEN_ROWS as i32 {
                            let vx = mx0 + (gx as f32) * TET_CELL + 3.0;
                            let vy = my0 + ((gy - TET_HIDDEN_ROWS as i32) as f32) * TET_CELL + 3.0;
                            ui.plugin_stroke_rect(
                                vx,
                                vy,
                                TET_CELL - 6.0,
                                TET_CELL - 6.0,
                                4.0,
                                2.5,
                                t.fg1_rgb,
                            );
                        }
                    }
                }
            }
            for (px, py) in self.cur.cells() {
                if py >= TET_HIDDEN_ROWS as i32 {
                    Self::draw_mino(
                        ui,
                        t,
                        mx0 + (px as f32) * TET_CELL,
                        my0 + ((py - TET_HIDDEN_ROWS as i32) as f32) * TET_CELL,
                        TET_CELL,
                        self.cur.kind,
                    );
                }
            }
        }

        // Left column: HOLD, SCORE, BEST, Lv / Lines
        let lx = 32.0f32;
        let lw = TET_WELL_X - 24.0 - lx;
        let hold_h = 154.0f32;
        ui.plugin_stroke_rect(lx, TET_WELL_Y, lw, hold_h, 10.0, 3.5, t.fg1_rgb);
        ui.plugin_draw_text(
            lx + lw * 0.5,
            TET_WELL_Y + 26.0,
            tr(uk, "HOLD (L / Select)", "ЗАПАС (L / Select)"),
            22.0,
            2,
            1,
            t.fg2_rgb,
        );
        if let Some(hk) = self.hold {
            Self::draw_mini_piece(ui, t, lx + lw * 0.5, TET_WELL_Y + 98.0, 24.0, hk);
        } else {
            ui.plugin_draw_text(lx + lw * 0.5, TET_WELL_Y + 98.0, "—", 32.0, 1, 1, t.fg1_rgb);
        }

        let sc_y = TET_WELL_Y + hold_h + 18.0;
        let card_h = 126.0f32;
        ui.plugin_stroke_rect(lx, sc_y, lw, card_h, 10.0, 3.5, t.fg1_rgb);
        ui.plugin_draw_text(
            lx + lw * 0.5,
            sc_y + 30.0,
            tr(uk, "SCORE", "РАХУНОК"),
            23.0,
            2,
            1,
            t.fg2_rgb,
        );
        ui.plugin_draw_text(
            lx + lw * 0.5,
            sc_y + 82.0,
            &self.score.to_string(),
            42.0,
            3,
            1,
            t.fg_rgb,
        );

        let best_y = sc_y + card_h + 18.0;
        ui.plugin_stroke_rect(lx, best_y, lw, card_h, 10.0, 3.5, t.fg1_rgb);
        ui.plugin_draw_text(
            lx + lw * 0.5,
            best_y + 30.0,
            tr(uk, "BEST", "РЕКОРД"),
            23.0,
            2,
            1,
            t.fg2_rgb,
        );
        ui.plugin_draw_text(
            lx + lw * 0.5,
            best_y + 82.0,
            &self.best.to_string(),
            42.0,
            3,
            1,
            t.fg_rgb,
        );

        let lv_y = best_y + card_h + 18.0;
        let lv_h = TET_WELL_Y + TET_WELL_H - lv_y;
        ui.plugin_stroke_rect(lx, lv_y, lw, lv_h, 10.0, 3.5, t.fg1_rgb);
        let lv_txt = format!(
            "{}: {}     {}: {}",
            tr(uk, "Lv", "Рів"),
            self.level,
            tr(uk, "Lines", "Лінії"),
            self.lines
        );
        ui.plugin_draw_text(
            lx + lw * 0.5,
            lv_y + lv_h * 0.5,
            &lv_txt,
            26.0,
            3,
            1,
            t.fg_rgb,
        );

        // Right column: 3-piece NEXT + Controls
        let rx = TET_WELL_X + TET_WELL_W + 24.0;
        let rw = 1024.0 - 32.0 - rx;
        let next_h = 330.0f32;
        ui.plugin_stroke_rect(rx, TET_WELL_Y, rw, next_h, 10.0, 3.5, t.fg1_rgb);
        ui.plugin_draw_text(
            rx + rw * 0.5,
            TET_WELL_Y + 26.0,
            tr(uk, "NEXT", "ДАЛІ"),
            23.0,
            2,
            1,
            t.fg2_rgb,
        );
        for (i, &nk) in self.queue.iter().take(3).enumerate() {
            let py = TET_WELL_Y + 92.0 + (i as f32) * 92.0;
            Self::draw_mini_piece(ui, t, rx + rw * 0.5, py, 23.0, nk);
        }

        let ctrl_y = TET_WELL_Y + next_h + 18.0;
        let ctrl_h = TET_WELL_H - next_h - 18.0;
        ui.plugin_stroke_rect(rx, ctrl_y, rw, ctrl_h, 10.0, 3.5, t.fg1_rgb);
        let hints = [
            tr(uk, "A : Rotate CW ↻", "A : Поворот ↻"),
            tr(uk, "B / X : Rotate CCW ↺", "B / X : Поворот ↺"),
            tr(uk, "↑ / Y / R : Hard Drop", "↑ / Y / R : Миттєво вниз"),
            tr(uk, "↓ : Soft Drop", "↓ : Прискорити"),
            tr(uk, "L / Select : Hold", "L / Select : Запас"),
            tr(uk, "Menu / Start : Menu", "Menu / Start : Меню"),
        ];
        for (i, line) in hints.iter().enumerate() {
            ui.plugin_draw_text(
                rx + 18.0,
                ctrl_y + 30.0 + (i as f32) * 36.0,
                line,
                21.0,
                1,
                0,
                t.fg2_rgb,
            );
        }

        if self.game_over {
            let bw = 460.0f32;
            let bh = 118.0f32;
            let bx = (1024.0 - bw) * 0.5;
            let by = TET_WELL_Y + (TET_WELL_H - bh) * 0.5;
            ui.plugin_fill_rect(bx, by, bw, bh, 10.0, t.bg_rgb);
            ui.plugin_stroke_rect(bx, by, bw, bh, 10.0, 4.5, t.fg_rgb);
            ui.plugin_draw_text(
                512.0,
                by + 40.0,
                tr(uk, "GAME OVER", "ГРУ ЗАКІНЧЕНО"),
                34.0,
                3,
                1,
                t.fg_rgb,
            );
            ui.plugin_draw_text(
                512.0,
                by + 84.0,
                tr(uk, "A: New Game  •  Menu: Options", "A: Нова гра  •  Menu: Меню"),
                24.0,
                2,
                1,
                t.fg2_rgb,
            );
        }

        if let Some((ref msg, _)) = self.splash {
            render_splash_pill(ui, t, msg);
        }
    }
}

/* ============================================================================
 * 2. SNAKE — 1:1 with `plugins/snake/src/lib.rs`
 * ========================================================================== */
const SNK_W: i32 = 20;
const SNK_H: i32 = 17;
const SNK_CELL: f32 = 32.0;
const SNK_PAD: f32 = 12.0;
const SNK_BX: f32 = 32.0;
const SNK_BY: f32 = CONTENT_TOP_Y + 14.0;
const SNK_OUTER_W: f32 = (SNK_W as f32) * SNK_CELL + SNK_PAD * 2.0; // 664
const SNK_OUTER_H: f32 = (SNK_H as f32) * SNK_CELL + SNK_PAD * 2.0; // 568

#[derive(Clone, Copy, PartialEq, Eq)]
enum SnkDir {
    Up,
    Down,
    Left,
    Right,
}

impl SnkDir {
    fn delta(self) -> (i32, i32) {
        match self {
            Self::Up => (0, -1),
            Self::Down => (0, 1),
            Self::Left => (-1, 0),
            Self::Right => (1, 0),
        }
    }
    fn is_opposite(self, o: Self) -> bool {
        matches!(
            (self, o),
            (Self::Up, Self::Down)
                | (Self::Down, Self::Up)
                | (Self::Left, Self::Right)
                | (Self::Right, Self::Left)
        )
    }
}

pub struct SnakePlugin {
    body: VecDeque<(i32, i32)>,
    dir: SnkDir,
    turn_q: [Option<SnkDir>; 2],
    food: (i32, i32),
    bonus: Option<((i32, i32), u64)>,
    score: u32,
    best_walls: u32,
    best_wrap: u32,
    apples: u32,
    wrap_mode: bool,
    speed_preset: u8, // 0=Chill, 1=Normal, 2=Fast
    started: bool,
    game_over: bool,
    now_ms: u64,
    last_step_ms: u64,
    rng: Rng64,
    menu: SubMenuMode,
    splash: Option<(String, f32)>,
}

impl SnakePlugin {
    fn new() -> Self {
        let mut s = Self {
            body: VecDeque::new(),
            dir: SnkDir::Right,
            turn_q: [None, None],
            food: (14, 8),
            bonus: None,
            score: 0,
            best_walls: 0,
            best_wrap: 0,
            apples: 0,
            wrap_mode: false,
            speed_preset: 1,
            started: false,
            game_over: false,
            now_ms: 0,
            last_step_ms: 0,
            rng: Rng64::new(0x5A17_93F1_CAFE_BEEF),
            menu: SubMenuMode::None,
            splash: None,
        };
        s.reset_round();
        s
    }

    fn reset_round(&mut self) {
        self.body.clear();
        self.body.push_back((10, 8));
        self.body.push_back((9, 8));
        self.body.push_back((8, 8));
        self.body.push_back((7, 8));
        self.dir = SnkDir::Right;
        self.turn_q = [None, None];
        self.bonus = None;
        self.score = 0;
        self.apples = 0;
        self.started = false;
        self.game_over = false;
        self.last_step_ms = self.now_ms;
        if let Some(f) = self.spawn_free(None) {
            self.food = f;
        }
    }

    fn best(&self) -> u32 {
        if self.wrap_mode {
            self.best_wrap
        } else {
            self.best_walls
        }
    }

    fn record_score(&mut self) {
        if self.wrap_mode {
            self.best_wrap = self.best_wrap.max(self.score);
        } else {
            self.best_walls = self.best_walls.max(self.score);
        }
    }

    fn spawn_free(&mut self, avoid: Option<(i32, i32)>) -> Option<(i32, i32)> {
        let mut free = Vec::new();
        for y in 0..SNK_H {
            for x in 0..SNK_W {
                let p = (x, y);
                if avoid == Some(p) || self.body.contains(&p) {
                    continue;
                }
                if self.bonus.map(|(bp, _)| bp == p).unwrap_or(false) {
                    continue;
                }
                free.push(p);
            }
        }
        if free.is_empty() {
            None
        } else {
            Some(free[self.rng.range(free.len())])
        }
    }

    fn queue_turn(&mut self, d: SnkDir) {
        let eff = self.turn_q[1].or(self.turn_q[0]).unwrap_or(self.dir);
        if d == eff || d.is_opposite(eff) {
            return;
        }
        if self.turn_q[0].is_none() {
            self.turn_q[0] = Some(d);
        } else if self.turn_q[1].is_none() {
            self.turn_q[1] = Some(d);
        }
    }

    fn pop_turn(&mut self) -> SnkDir {
        if let Some(d) = self.turn_q[0].take() {
            self.turn_q[0] = self.turn_q[1].take();
            d
        } else {
            self.dir
        }
    }

    fn speed_level(&self) -> u32 {
        (self.apples / 4) + 1 + (self.speed_preset as u32) * 2
    }

    fn step_interval_ms(&self, boost: bool) -> u64 {
        let base_start = match self.speed_preset {
            0 => 132u64,
            1 => 104u64,
            _ => 80u64,
        };
        let ramp = ((self.apples / 4) as u64) * 7;
        let base = base_start.saturating_sub(ramp).max(56);
        if boost {
            base.min(50)
        } else {
            base
        }
    }

    fn tick_board(&mut self) {
        if !self.started || self.game_over {
            return;
        }
        if let Some((_, exp)) = self.bonus {
            if self.now_ms >= exp {
                self.bonus = None;
            }
        }
        let nd = self.pop_turn();
        if !nd.is_opposite(self.dir) {
            self.dir = nd;
        }
        let (hx, hy) = *self.body.front().unwrap_or(&(0, 0));
        let (dx, dy) = self.dir.delta();
        let mut nx = hx + dx;
        let mut ny = hy + dy;
        if self.wrap_mode {
            nx = nx.rem_euclid(SNK_W);
            ny = ny.rem_euclid(SNK_H);
        } else if !(0..SNK_W).contains(&nx) || !(0..SNK_H).contains(&ny) {
            self.game_over = true;
            return;
        }
        let nh = (nx, ny);
        let eat_f = nh == self.food;
        let eat_b = self.bonus.map(|(bp, _)| bp == nh).unwrap_or(false);
        let grow = eat_f || eat_b;
        let hit = self
            .body
            .iter()
            .take(if grow {
                self.body.len()
            } else {
                self.body.len().saturating_sub(1)
            })
            .any(|&s| s == nh);
        if hit {
            self.game_over = true;
            return;
        }
        self.body.push_front(nh);
        if !grow {
            self.body.pop_back();
        }
        if eat_f {
            self.apples += 1;
            self.score += 10 + (self.speed_level() - 1) * 2;
            self.record_score();
            if let Some(c) = self.spawn_free(None) {
                self.food = c;
            } else {
                self.game_over = true;
                return;
            }
            if self.apples % 5 == 0 && self.bonus.is_none() {
                if let Some(bc) = self.spawn_free(Some(self.food)) {
                    self.bonus = Some((bc, self.now_ms + 7_000));
                }
            }
        } else if eat_b {
            self.bonus = None;
            self.score += 50;
            self.record_score();
        }
    }

    fn update(&mut self, dt: f32, pressed: u32, held: u32, uk: bool) -> PluginAction {
        if let Some((_, ref mut rem)) = self.splash {
            *rem -= dt;
            if *rem <= 0.0 {
                self.splash = None;
            }
        }

        match self.menu {
            SubMenuMode::Main { mut sel } => {
                let (back, act, _) = nav_menu_input(pressed, &mut sel, 5);
                self.menu = SubMenuMode::Main { sel };
                if back || (pressed & btn::START) != 0 {
                    self.menu = SubMenuMode::None;
                    self.last_step_ms = self.now_ms;
                    return PluginAction::Click;
                }
                if act && (pressed & btn::A) != 0 {
                    match sel {
                        0 => {
                            self.menu = SubMenuMode::None;
                            self.last_step_ms = self.now_ms;
                        }
                        1 => {
                            self.reset_round();
                            self.menu = SubMenuMode::None;
                        }
                        2 => {
                            self.splash = Some((tr(uk, "Game saved", "Гру збережено").into(), 0.75));
                            self.menu = SubMenuMode::None;
                        }
                        3 => self.menu = SubMenuMode::Options { sel: 0 },
                        _ => return PluginAction::Exit,
                    }
                    return PluginAction::Click;
                }
                return PluginAction::Continue;
            }
            SubMenuMode::Options { mut sel } => {
                let (back, act, _) = nav_menu_input(pressed, &mut sel, 3);
                self.menu = SubMenuMode::Options { sel };
                if back {
                    self.menu = SubMenuMode::Main { sel: 3 };
                    return PluginAction::Click;
                }
                if act {
                    match sel {
                        0 => {
                            self.wrap_mode = !self.wrap_mode;
                            self.reset_round();
                        }
                        1 => self.speed_preset = (self.speed_preset + 1) % 3,
                        _ => {
                            self.best_walls = 0;
                            self.best_wrap = 0;
                            self.record_score();
                            self.splash = Some((
                                tr(uk, "Best scores reset", "Рекорди скинуто").into(),
                                0.75,
                            ));
                        }
                    }
                    return PluginAction::Click;
                }
                return PluginAction::Continue;
            }
            SubMenuMode::None => {}
        }

        self.now_ms = self.now_ms.saturating_add((dt * 1000.0).round() as u64);

        if (pressed & (btn::START | btn::MENU)) != 0 {
            self.menu = SubMenuMode::Main { sel: 0 };
            return PluginAction::Click;
        }
        if (pressed & btn::X) != 0 {
            self.wrap_mode = !self.wrap_mode;
            self.reset_round();
            return PluginAction::Click;
        }

        if self.game_over {
            if (pressed & btn::A) != 0 {
                self.reset_round();
                self.started = true;
                return PluginAction::Click;
            }
            return PluginAction::Continue;
        }

        let mut dir_pressed = false;
        if (pressed & btn::UP) != 0 {
            self.queue_turn(SnkDir::Up);
            dir_pressed = true;
        } else if (pressed & btn::DOWN) != 0 {
            self.queue_turn(SnkDir::Down);
            dir_pressed = true;
        } else if (pressed & btn::LEFT) != 0 {
            self.queue_turn(SnkDir::Left);
            dir_pressed = true;
        } else if (pressed & btn::RIGHT) != 0 {
            self.queue_turn(SnkDir::Right);
            dir_pressed = true;
        }
        if !self.started && (dir_pressed || (pressed & btn::A) != 0) {
            self.started = true;
            self.last_step_ms = self.now_ms;
        }

        let step_ms = self.step_interval_ms((held & btn::A) != 0);
        while self.started && self.now_ms.saturating_sub(self.last_step_ms) >= step_ms {
            self.last_step_ms += step_ms;
            self.tick_board();
            if self.game_over {
                break;
            }
        }

        PluginAction::Continue
    }

    fn render(&self, ui: &mut UiEngine, t: ThemeColors, header: &HeaderState<'_>, uk: bool) {
        match self.menu {
            SubMenuMode::Main { sel } => {
                let rows = [
                    PluginMenuRow::action(tr(uk, "Continue", "Продовжити")),
                    PluginMenuRow::action(tr(uk, "New Game", "Нова гра")),
                    PluginMenuRow::action(tr(uk, "Save Game", "Зберегти гру")),
                    PluginMenuRow::submenu(tr(uk, "Options", "Налаштування")),
                    PluginMenuRow::action(tr(uk, "Exit", "Вихід")),
                ];
                render_native_plugin_menu(ui, t, header, tr(uk, "Snake", "Змійка"), &rows, sel);
                return;
            }
            SubMenuMode::Options { sel } => {
                let wall_lbl = if self.wrap_mode {
                    tr(uk, "Wrap", "Без стін")
                } else {
                    tr(uk, "Walls", "Стіни")
                };
                let spd_lbl = match self.speed_preset {
                    0 => tr(uk, "Chill", "Повільно"),
                    1 => tr(uk, "Normal", "Нормально"),
                    _ => tr(uk, "Fast", "Швидко"),
                };
                let rows = [
                    PluginMenuRow::option(tr(uk, "Wall Mode", "Режим стін"), wall_lbl),
                    PluginMenuRow::option(tr(uk, "Speed", "Швидкість"), spd_lbl),
                    PluginMenuRow::option(
                        tr(uk, "Reset Best Score", "Скинути рекорд"),
                        &self.best().to_string(),
                    ),
                ];
                render_native_plugin_menu(
                    ui,
                    t,
                    header,
                    tr(uk, "Options", "Налаштування"),
                    &rows,
                    sel,
                );
                if let Some((ref msg, _)) = self.splash {
                    render_splash_pill(ui, t, msg);
                }
                return;
            }
            SubMenuMode::None => {}
        }

        ui.plugin_begin(t, t.bg_rgb, Some(header));

        ui.plugin_stroke_rect(
            SNK_BX,
            SNK_BY,
            SNK_OUTER_W,
            SNK_OUTER_H,
            10.0,
            if self.wrap_mode { 2.5 } else { 5.0 },
            if self.wrap_mode { t.fg1_rgb } else { t.fg_rgb },
        );
        if !self.wrap_mode {
            ui.plugin_stroke_rect(
                SNK_BX + 6.0,
                SNK_BY + 6.0,
                SNK_OUTER_W - 12.0,
                SNK_OUTER_H - 12.0,
                6.0,
                2.0,
                t.fg1_rgb,
            );
        }

        let gx0 = SNK_BX + SNK_PAD;
        let gy0 = SNK_BY + SNK_PAD;

        for gy in (1..SNK_H).step_by(2) {
            for gx in (1..SNK_W).step_by(2) {
                let px = gx0 + gx as f32 * SNK_CELL;
                let py = gy0 + gy as f32 * SNK_CELL;
                ui.plugin_fill_rect(px - 1.5, py - 1.5, 3.0, 3.0, 0.0, t.fg1_rgb);
            }
        }

        // Apple with stem & highlight
        let fx = gx0 + (self.food.0 as f32 + 0.5) * SNK_CELL;
        let fy = gy0 + (self.food.1 as f32 + 0.5) * SNK_CELL;
        ui.plugin_fill_circle(fx, fy + 1.0, 11.0, t.fg_rgb);
        ui.plugin_fill_circle(fx - 3.5, fy - 2.5, 3.0, t.bg_rgb);
        ui.plugin_fill_rect(fx - 1.5, fy - 14.0, 4.0, 5.0, 1.5, t.fg2_rgb);

        if let Some(((bx, by), exp)) = self.bonus {
            let cx = gx0 + (bx as f32 + 0.5) * SNK_CELL;
            let cy = gy0 + (by as f32 + 0.5) * SNK_CELL;
            let blink = exp.saturating_sub(self.now_ms) > 1_800 || ((self.now_ms / 120) % 2 == 0);
            if blink {
                ui.plugin_stroke_rect(cx - 12.0, cy - 12.0, 24.0, 24.0, 7.0, 3.5, t.fg_rgb);
                ui.plugin_fill_circle(cx, cy, 6.5, t.fg2_rgb);
            }
        }

        let len = self.body.len();
        for (idx, &(sx, sy)) in self.body.iter().enumerate().rev() {
            let x = gx0 + sx as f32 * SNK_CELL;
            let y = gy0 + sy as f32 * SNK_CELL;
            if idx == 0 {
                ui.plugin_fill_rect(x + 2.0, y + 2.0, SNK_CELL - 4.0, SNK_CELL - 4.0, 8.0, t.fg_rgb);
                let (e1, e2) = match self.dir {
                    SnkDir::Right => ((x + 21.0, y + 10.0), (x + 21.0, y + 22.0)),
                    SnkDir::Left => ((x + 11.0, y + 10.0), (x + 11.0, y + 22.0)),
                    SnkDir::Up => ((x + 10.0, y + 11.0), (x + 22.0, y + 11.0)),
                    SnkDir::Down => ((x + 10.0, y + 21.0), (x + 22.0, y + 21.0)),
                };
                ui.plugin_fill_circle(e1.0, e1.1, 2.8, t.bg_rgb);
                ui.plugin_fill_circle(e2.0, e2.1, 2.8, t.bg_rgb);
            } else {
                let pad = if idx + 1 == len { 5.5 } else { 3.5 };
                let shade = if idx % 2 == 0 { t.fg_rgb } else { t.fg2_rgb };
                ui.plugin_fill_rect(
                    x + pad,
                    y + pad,
                    SNK_CELL - pad * 2.0,
                    SNK_CELL - pad * 2.0,
                    6.0,
                    shade,
                );
            }
        }

        // Right sidebar
        let side_x = SNK_BX + SNK_OUTER_W + 24.0;
        let side_w = 1024.0 - side_x - 32.0;

        ui.plugin_stroke_rect(side_x, SNK_BY, side_w, 112.0, 10.0, 3.5, t.fg1_rgb);
        ui.plugin_draw_text(
            side_x + side_w * 0.5,
            SNK_BY + 30.0,
            tr(uk, "SCORE", "РАХУНОК"),
            24.0,
            2,
            1,
            t.fg2_rgb,
        );
        ui.plugin_draw_text(
            side_x + side_w * 0.5,
            SNK_BY + 76.0,
            &self.score.to_string(),
            44.0,
            3,
            1,
            t.fg_rgb,
        );

        let best_y = SNK_BY + 128.0;
        ui.plugin_stroke_rect(side_x, best_y, side_w, 112.0, 10.0, 3.5, t.fg1_rgb);
        ui.plugin_draw_text(
            side_x + side_w * 0.5,
            best_y + 30.0,
            tr(uk, "BEST", "РЕКОРД"),
            24.0,
            2,
            1,
            t.fg2_rgb,
        );
        ui.plugin_draw_text(
            side_x + side_w * 0.5,
            best_y + 76.0,
            &self.best().to_string(),
            44.0,
            3,
            1,
            t.fg_rgb,
        );

        let info_y = best_y + 128.0;
        ui.plugin_stroke_rect(side_x, info_y, side_w, 116.0, 10.0, 3.5, t.fg1_rgb);
        let mode_lbl = if self.wrap_mode {
            tr(uk, "Mode: Wrap", "Режим: Без стін")
        } else {
            tr(uk, "Mode: Walls", "Режим: Стіни")
        };
        ui.plugin_draw_text(
            side_x + side_w * 0.5,
            info_y + 36.0,
            mode_lbl,
            23.0,
            2,
            1,
            t.fg_rgb,
        );
        let sp_lbl = format!("{}: {}", tr(uk, "Speed", "Швидкість"), self.speed_level());
        ui.plugin_draw_text(
            side_x + side_w * 0.5,
            info_y + 80.0,
            &sp_lbl,
            23.0,
            1,
            1,
            t.fg2_rgb,
        );

        if let Some((_, exp)) = self.bonus {
            let rem = exp.saturating_sub(self.now_ms) as f32 / 7_000.0;
            ui.plugin_draw_progress_bar(t, side_x, info_y + 132.0, side_w, 16.0, rem);
        }

        let ctrl_y = info_y + 162.0;
        ui.plugin_draw_text(
            side_x + 6.0,
            ctrl_y,
            tr(uk, "Hold A: Speed boost", "Утрим. A: Прискорення"),
            21.0,
            1,
            0,
            t.fg2_rgb,
        );
        ui.plugin_draw_text(
            side_x + 6.0,
            ctrl_y + 34.0,
            tr(uk, "X: Wall mode", "X: Режим стін"),
            21.0,
            1,
            0,
            t.fg2_rgb,
        );
        ui.plugin_draw_text(
            side_x + 6.0,
            ctrl_y + 68.0,
            tr(uk, "Menu / Start: Menu", "Menu / Start: Меню"),
            21.0,
            1,
            0,
            t.fg2_rgb,
        );

        if !self.started || self.game_over {
            let bw = 460.0f32;
            let bh = 118.0f32;
            let bx = SNK_BX + (SNK_OUTER_W - bw) * 0.5;
            let by = SNK_BY + (SNK_OUTER_H - bh) * 0.5;
            ui.plugin_fill_rect(bx, by, bw, bh, 10.0, t.bg_rgb);
            ui.plugin_stroke_rect(bx, by, bw, bh, 10.0, 4.5, t.fg_rgb);
            if self.game_over {
                ui.plugin_draw_text(
                    bx + bw * 0.5,
                    by + 40.0,
                    tr(uk, "GAME OVER", "ГРУ ЗАКІНЧЕНО"),
                    34.0,
                    3,
                    1,
                    t.fg_rgb,
                );
                ui.plugin_draw_text(
                    bx + bw * 0.5,
                    by + 84.0,
                    tr(uk, "A: New Game  •  Menu: Options", "A: Нова гра  •  Menu: Меню"),
                    23.0,
                    2,
                    1,
                    t.fg2_rgb,
                );
            } else {
                ui.plugin_draw_text(
                    bx + bw * 0.5,
                    by + 58.0,
                    tr(uk, "Press D-Pad or A to start", "Натисни D-Pad або A"),
                    28.0,
                    3,
                    1,
                    t.fg_rgb,
                );
            }
        }

        if let Some((ref msg, _)) = self.splash {
            render_splash_pill(ui, t, msg);
        }
    }
}

/* ============================================================================
 * 3. 2048 — 1:1 with `plugins/uipod-2048/src/lib.rs` (`B` = Undo!)
 * ========================================================================== */
pub struct Plugin2048 {
    grid: [[u32; 4]; 4],
    prev_grid: Option<([[u32; 4]; 4], u32)>,
    score: u32,
    best: u32,
    allow_undo: bool,
    game_over: bool,
    rng: Rng64,
    menu: SubMenuMode,
    splash: Option<(String, f32)>,
}

impl Plugin2048 {
    fn new() -> Self {
        let mut g = Self {
            grid: [[0; 4]; 4],
            prev_grid: None,
            score: 0,
            best: 0,
            allow_undo: true,
            game_over: false,
            rng: Rng64::new(0x2048_CAFE_D1B5_4A32),
            menu: SubMenuMode::None,
            splash: None,
        };
        g.reset();
        g
    }

    fn reset(&mut self) {
        self.grid = [[0; 4]; 4];
        self.prev_grid = None;
        self.score = 0;
        self.game_over = false;
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
        if !empty.is_empty() {
            let (r, c) = empty[self.rng.range(empty.len())];
            self.grid[r][c] = if self.rng.range(10) == 0 { 4 } else { 2 };
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

    fn slide_line(line: [u32; 4]) -> ([u32; 4], u32, bool) {
        let vals: Vec<u32> = line.into_iter().filter(|&v| v != 0).collect();
        let mut out = [0u32; 4];
        let mut gained = 0u32;
        let mut dst = 0;
        let mut i = 0;
        while i < vals.len() {
            if i + 1 < vals.len() && vals[i] == vals[i + 1] {
                let m = vals[i] * 2;
                out[dst] = m;
                gained += m;
                i += 2;
            } else {
                out[dst] = vals[i];
                i += 1;
            }
            dst += 1;
        }
        (out, gained, out != line)
    }

    fn apply_move(&mut self, dir: u8) -> bool {
        let snapshot = (self.grid, self.score);
        let mut moved = false;
        for i in 0..4 {
            let line = match dir {
                0 => self.grid[i],
                1 => [
                    self.grid[i][3],
                    self.grid[i][2],
                    self.grid[i][1],
                    self.grid[i][0],
                ],
                2 => [
                    self.grid[0][i],
                    self.grid[1][i],
                    self.grid[2][i],
                    self.grid[3][i],
                ],
                _ => [
                    self.grid[3][i],
                    self.grid[2][i],
                    self.grid[1][i],
                    self.grid[0][i],
                ],
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
            if self.allow_undo {
                self.prev_grid = Some(snapshot);
            }
            self.spawn();
            self.game_over = !self.can_move();
        }
        moved
    }

    fn undo(&mut self) -> bool {
        if !self.allow_undo {
            return false;
        }
        if let Some((pg, ps)) = self.prev_grid.take() {
            self.grid = pg;
            self.score = ps;
            self.game_over = !self.can_move();
            true
        } else {
            false
        }
    }

    fn update(&mut self, dt: f32, pressed: u32, uk: bool) -> PluginAction {
        if let Some((_, ref mut rem)) = self.splash {
            *rem -= dt;
            if *rem <= 0.0 {
                self.splash = None;
            }
        }

        match self.menu {
            SubMenuMode::Main { mut sel } => {
                let (back, act, _) = nav_menu_input(pressed, &mut sel, 5);
                self.menu = SubMenuMode::Main { sel };
                if back || (pressed & btn::START) != 0 {
                    self.menu = SubMenuMode::None;
                    return PluginAction::Click;
                }
                if act && (pressed & btn::A) != 0 {
                    match sel {
                        0 => self.menu = SubMenuMode::None,
                        1 => {
                            self.reset();
                            self.menu = SubMenuMode::None;
                        }
                        2 => {
                            self.splash = Some((tr(uk, "Game saved", "Гру збережено").into(), 0.75));
                            self.menu = SubMenuMode::None;
                        }
                        3 => self.menu = SubMenuMode::Options { sel: 0 },
                        _ => return PluginAction::Exit,
                    }
                    return PluginAction::Click;
                }
                return PluginAction::Continue;
            }
            SubMenuMode::Options { mut sel } => {
                let (back, act, _) = nav_menu_input(pressed, &mut sel, 2);
                self.menu = SubMenuMode::Options { sel };
                if back {
                    self.menu = SubMenuMode::Main { sel: 3 };
                    return PluginAction::Click;
                }
                if act {
                    match sel {
                        0 => {
                            self.allow_undo = !self.allow_undo;
                            if !self.allow_undo {
                                self.prev_grid = None;
                            }
                        }
                        _ => {
                            self.best = self.score;
                            self.splash =
                                Some((tr(uk, "Best score reset", "Рекорд скинуто").into(), 0.75));
                        }
                    }
                    return PluginAction::Click;
                }
                return PluginAction::Continue;
            }
            SubMenuMode::None => {}
        }

        if (pressed & (btn::START | btn::MENU)) != 0 {
            self.menu = SubMenuMode::Main { sel: 0 };
            return PluginAction::Click;
        }
        if (pressed & btn::B) != 0 {
            if self.undo() {
                return PluginAction::Click;
            }
            return PluginAction::Continue;
        }
        if self.game_over && (pressed & btn::A) != 0 {
            self.reset();
            return PluginAction::Click;
        }
        if (pressed & btn::LEFT) != 0 {
            self.apply_move(0);
        } else if (pressed & btn::RIGHT) != 0 {
            self.apply_move(1);
        } else if (pressed & btn::UP) != 0 {
            self.apply_move(2);
        } else if (pressed & btn::DOWN) != 0 {
            self.apply_move(3);
        }
        PluginAction::Continue
    }

    fn render(&self, ui: &mut UiEngine, t: ThemeColors, header: &HeaderState<'_>, uk: bool) {
        match self.menu {
            SubMenuMode::Main { sel } => {
                let rows = [
                    PluginMenuRow::action(tr(uk, "Continue", "Продовжити")),
                    PluginMenuRow::action(tr(uk, "New Game", "Нова гра")),
                    PluginMenuRow::action(tr(uk, "Save Game", "Зберегти гру")),
                    PluginMenuRow::submenu(tr(uk, "Options", "Налаштування")),
                    PluginMenuRow::action(tr(uk, "Exit", "Вихід")),
                ];
                render_native_plugin_menu(ui, t, header, "2048", &rows, sel);
                return;
            }
            SubMenuMode::Options { sel } => {
                let undo_lbl = if self.allow_undo {
                    tr(uk, "On", "Увімк")
                } else {
                    tr(uk, "Off", "Вимк")
                };
                let rows = [
                    PluginMenuRow::option(
                        tr(uk, "Undo Move (B)", "Скасування ходу (B)"),
                        undo_lbl,
                    ),
                    PluginMenuRow::option(
                        tr(uk, "Reset Best Score", "Скинути рекорд"),
                        &self.best.to_string(),
                    ),
                ];
                render_native_plugin_menu(
                    ui,
                    t,
                    header,
                    tr(uk, "Options", "Налаштування"),
                    &rows,
                    sel,
                );
                if let Some((ref msg, _)) = self.splash {
                    render_splash_pill(ui, t, msg);
                }
                return;
            }
            SubMenuMode::None => {}
        }

        ui.plugin_begin(t, t.bg_rgb, Some(header));

        let board_size = 556.0f32;
        let board_x = 40.0f32;
        let board_y = CONTENT_TOP_Y + 20.0;
        ui.plugin_stroke_rect(board_x, board_y, board_size, board_size, 12.0, 4.0, t.fg1_rgb);

        let gap = 14.0f32;
        let cell = (board_size - gap * 5.0) / 4.0;
        for r in 0..4 {
            for c in 0..4 {
                let x = board_x + gap + (c as f32) * (cell + gap);
                let y = board_y + gap + (r as f32) * (cell + gap);
                let val = self.grid[r][c];
                if val == 0 {
                    ui.plugin_stroke_rect(x, y, cell, cell, 8.0, 3.0, t.fg1_rgb);
                } else {
                    let (fill_c, txt_c) = match val {
                        2 | 4 => (t.fg1_rgb, t.fg_rgb),
                        8 | 16 | 32 => (t.fg2_rgb, t.bg_rgb),
                        _ => (t.fg_rgb, t.bg_rgb),
                    };
                    ui.plugin_fill_rect(x, y, cell, cell, 8.0, fill_c);
                    if val >= 512 {
                        ui.plugin_stroke_rect(
                            x + 6.0,
                            y + 6.0,
                            cell - 12.0,
                            cell - 12.0,
                            5.0,
                            3.0,
                            t.fg1_rgb,
                        );
                    }
                    let fsz = if val >= 1024 {
                        34.0
                    } else if val >= 128 {
                        40.0
                    } else {
                        46.0
                    };
                    ui.plugin_draw_text(
                        x + cell * 0.5,
                        y + cell * 0.5,
                        &val.to_string(),
                        fsz,
                        3,
                        1,
                        txt_c,
                    );
                }
            }
        }

        let side_x = board_x + board_size + 32.0;
        let side_w = 1024.0 - side_x - 32.0;

        ui.plugin_stroke_rect(side_x, board_y, side_w, 124.0, 10.0, 4.0, t.fg1_rgb);
        ui.plugin_draw_text(
            side_x + side_w * 0.5,
            board_y + 34.0,
            tr(uk, "SCORE", "РАХУНОК"),
            26.0,
            2,
            1,
            t.fg2_rgb,
        );
        ui.plugin_draw_text(
            side_x + side_w * 0.5,
            board_y + 84.0,
            &self.score.to_string(),
            46.0,
            3,
            1,
            t.fg_rgb,
        );

        let best_y = board_y + 144.0;
        ui.plugin_stroke_rect(side_x, best_y, side_w, 124.0, 10.0, 4.0, t.fg1_rgb);
        ui.plugin_draw_text(
            side_x + side_w * 0.5,
            best_y + 34.0,
            tr(uk, "BEST", "РЕКОРД"),
            26.0,
            2,
            1,
            t.fg2_rgb,
        );
        ui.plugin_draw_text(
            side_x + side_w * 0.5,
            best_y + 84.0,
            &self.best.to_string(),
            46.0,
            3,
            1,
            t.fg_rgb,
        );

        let hint_y = best_y + 156.0;
        ui.plugin_draw_text(
            side_x + 8.0,
            hint_y,
            tr(uk, "D-Pad: Slide tiles", "Хрестовина: Рух"),
            25.0,
            1,
            0,
            t.fg2_rgb,
        );
        if self.allow_undo {
            ui.plugin_draw_text(
                side_x + 8.0,
                hint_y + 42.0,
                tr(uk, "B: Undo last move", "B: Скасувати хід"),
                25.0,
                1,
                0,
                t.fg2_rgb,
            );
        }
        ui.plugin_draw_text(
            side_x + 8.0,
            hint_y + if self.allow_undo { 84.0 } else { 42.0 },
            tr(uk, "Menu / Start: Menu", "Menu / Start: Меню"),
            25.0,
            1,
            0,
            t.fg2_rgb,
        );

        if self.game_over {
            let banner_y = hint_y + 138.0;
            ui.plugin_fill_rect(side_x, banner_y, side_w, 72.0, 8.0, t.fg_rgb);
            ui.plugin_draw_text(
                side_x + side_w * 0.5,
                banner_y + 36.0,
                tr(uk, "No moves! Press Menu", "Кінець! Натисни Menu"),
                26.0,
                3,
                1,
                t.bg_rgb,
            );
        }

        if let Some((ref msg, _)) = self.splash {
            render_splash_pill(ui, t, msg);
        }
    }
}

/* ============================================================================
 * 4. BRICK BREAKER — 1:1 with `plugins/brick-breaker/src/lib.rs`
 * ========================================================================== */
const BRK_COLS: usize = 11;
const BRK_ROWS: usize = 7;
const BRK_AX: f32 = 32.0;
const BRK_AY: f32 = CONTENT_TOP_Y + 44.0;
const BRK_AW: f32 = 1024.0 - 64.0;
const BRK_AH: f32 = 768.0 - BRK_AY - 24.0;

pub struct BrickBreakerPlugin {
    bricks: [[u8; BRK_COLS]; BRK_ROWS],
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
    fast_ball: bool,
    wide_paddle: bool,
    step_accum: f32,
    menu: SubMenuMode,
    splash: Option<(String, f32)>,
}

impl BrickBreakerPlugin {
    fn new() -> Self {
        let mut g = Self {
            bricks: [[0; BRK_COLS]; BRK_ROWS],
            paddle_x: 0.0,
            paddle_w: 152.0,
            ball_x: 0.0,
            ball_y: 0.0,
            ball_vx: 5.5,
            ball_vy: -7.8,
            ball_stuck: true,
            lives: 3,
            level: 1,
            score: 0,
            high_score: 0,
            game_over: false,
            fast_ball: false,
            wide_paddle: false,
            step_accum: 0.0,
            menu: SubMenuMode::None,
            splash: None,
        };
        g.new_game();
        g
    }

    fn base_paddle_w(&self) -> f32 {
        let base = if self.wide_paddle { 196.0f32 } else { 152.0f32 };
        (base - ((self.level.saturating_sub(1)) as f32) * 8.0).max(96.0)
    }

    fn ball_mult(&self) -> f32 {
        if self.fast_ball { 1.28 } else { 1.0 }
    }

    fn new_game(&mut self) {
        self.lives = 3;
        self.level = 1;
        self.score = 0;
        self.game_over = false;
        self.populate_level();
        self.reset_ball();
    }

    fn populate_level(&mut self) {
        let pat = (self.level - 1) % 3;
        for r in 0..BRK_ROWS {
            for c in 0..BRK_COLS {
                let hp = match pat {
                    0 => match r {
                        0 => 3,
                        1 | 2 => 2,
                        _ => 1,
                    },
                    1 => {
                        if (r + c) % 2 == 0 {
                            if r < 2 { 3 } else { 2 }
                        } else if r < 4 {
                            1
                        } else {
                            0
                        }
                    }
                    _ => {
                        let d = (c as i32 - 5).unsigned_abs() as usize;
                        if r + d <= 6 {
                            if r < 2 { 3 } else { 2 }
                        } else {
                            1
                        }
                    }
                };
                self.bricks[r][c] = hp;
            }
        }
    }

    fn reset_ball(&mut self) {
        self.paddle_w = self.base_paddle_w();
        self.paddle_x = BRK_AX + (BRK_AW - self.paddle_w) * 0.5;
        self.ball_stuck = true;
        self.ball_x = self.paddle_x + self.paddle_w * 0.5;
        self.ball_y = BRK_AY + BRK_AH - 38.0;
        let spd = (8.6 + (self.level as f32) * 0.65).min(13.5) * self.ball_mult();
        self.ball_vx = spd * 0.58;
        self.ball_vy = -spd * 0.81;
    }

    fn apply_opts(&mut self) {
        self.paddle_w = self.base_paddle_w();
        self.paddle_x = self
            .paddle_x
            .clamp(BRK_AX + 8.0, BRK_AX + BRK_AW - self.paddle_w - 8.0);
        if self.ball_stuck {
            self.ball_x = self.paddle_x + self.paddle_w * 0.5;
        }
    }

    fn step_60hz(&mut self, held: u32) {
        if self.game_over {
            return;
        }
        let move_spd = if (held & btn::B) != 0 { 16.5f32 } else { 12.5f32 };
        if (held & (btn::LEFT | btn::L1)) != 0 {
            self.paddle_x -= move_spd;
        }
        if (held & (btn::RIGHT | btn::R1)) != 0 {
            self.paddle_x += move_spd;
        }
        self.paddle_x = self
            .paddle_x
            .clamp(BRK_AX + 8.0, BRK_AX + BRK_AW - self.paddle_w - 8.0);

        let ball_r = 9.0f32;
        let paddle_y = BRK_AY + BRK_AH - 26.0;
        if self.ball_stuck {
            self.ball_x = self.paddle_x + self.paddle_w * 0.5;
            self.ball_y = paddle_y - ball_r - 2.0;
            return;
        }

        self.ball_x += self.ball_vx;
        self.ball_y += self.ball_vy;

        if self.ball_x - ball_r < BRK_AX + 4.0 {
            self.ball_x = BRK_AX + 4.0 + ball_r;
            self.ball_vx = self.ball_vx.abs();
        } else if self.ball_x + ball_r > BRK_AX + BRK_AW - 4.0 {
            self.ball_x = BRK_AX + BRK_AW - 4.0 - ball_r;
            self.ball_vx = -self.ball_vx.abs();
        }
        if self.ball_y - ball_r < BRK_AY + 4.0 {
            self.ball_y = BRK_AY + 4.0 + ball_r;
            self.ball_vy = self.ball_vy.abs();
        }

        if self.ball_vy > 0.0
            && self.ball_y + ball_r >= paddle_y
            && self.ball_y - ball_r <= paddle_y + 16.0
            && self.ball_x >= self.paddle_x - ball_r * 0.6
            && self.ball_x <= self.paddle_x + self.paddle_w + ball_r * 0.6
        {
            let rel = ((self.ball_x - (self.paddle_x + self.paddle_w * 0.5))
                / (self.paddle_w * 0.5))
                .clamp(-1.0, 1.0);
            let min_spd = 8.6 * self.ball_mult();
            let spd = (self.ball_vx * self.ball_vx + self.ball_vy * self.ball_vy)
                .sqrt()
                .clamp(min_spd, 15.8);
            self.ball_vx = rel * spd * 0.78;
            let vy_sq = (spd * spd - self.ball_vx * self.ball_vx).max(16.0);
            self.ball_vy = -vy_sq.sqrt();
            self.ball_y = paddle_y - ball_r - 1.0;
        }

        let pad_x = 18.0f32;
        let pad_top = 22.0f32;
        let gap = 8.0f32;
        let bw = (BRK_AW - pad_x * 2.0 - gap * ((BRK_COLS - 1) as f32)) / (BRK_COLS as f32);
        let bh = 28.0f32;
        'outer: for r in 0..BRK_ROWS {
            for c in 0..BRK_COLS {
                let hp = self.bricks[r][c];
                if hp == 0 {
                    continue;
                }
                let bx = BRK_AX + pad_x + (c as f32) * (bw + gap);
                let by = BRK_AY + pad_top + (r as f32) * (bh + gap);
                if self.ball_x + ball_r >= bx
                    && self.ball_x - ball_r <= bx + bw
                    && self.ball_y + ball_r >= by
                    && self.ball_y - ball_r <= by + bh
                {
                    self.bricks[r][c] = hp - 1;
                    self.score += if self.bricks[r][c] == 0 { 25 } else { 10 };
                    self.high_score = self.high_score.max(self.score);
                    let ol = (self.ball_x + ball_r) - bx;
                    let or_ = (bx + bw) - (self.ball_x - ball_r);
                    let ot = (self.ball_y + ball_r) - by;
                    let ob = (by + bh) - (self.ball_y - ball_r);
                    if ol.min(or_) < ot.min(ob) {
                        self.ball_vx = -self.ball_vx;
                    } else {
                        self.ball_vy = -self.ball_vy;
                    }
                    break 'outer;
                }
            }
        }

        let rem: usize = self
            .bricks
            .iter()
            .flat_map(|r| r.iter())
            .filter(|&&v| v > 0)
            .count();
        if rem == 0 {
            self.level += 1;
            self.populate_level();
            self.reset_ball();
        } else if self.ball_y - ball_r > BRK_AY + BRK_AH {
            if self.lives > 1 {
                self.lives -= 1;
                self.reset_ball();
            } else {
                self.lives = 0;
                self.game_over = true;
            }
        }
    }

    fn update(&mut self, dt: f32, pressed: u32, held: u32, uk: bool) -> PluginAction {
        if let Some((_, ref mut rem)) = self.splash {
            *rem -= dt;
            if *rem <= 0.0 {
                self.splash = None;
            }
        }

        match self.menu {
            SubMenuMode::Main { mut sel } => {
                let (back, act, _) = nav_menu_input(pressed, &mut sel, 5);
                self.menu = SubMenuMode::Main { sel };
                if back || (pressed & btn::START) != 0 {
                    self.menu = SubMenuMode::None;
                    return PluginAction::Click;
                }
                if act && (pressed & btn::A) != 0 {
                    match sel {
                        0 => self.menu = SubMenuMode::None,
                        1 => {
                            self.new_game();
                            self.menu = SubMenuMode::None;
                        }
                        2 => {
                            self.splash = Some((tr(uk, "Game saved", "Гру збережено").into(), 0.75));
                            self.menu = SubMenuMode::None;
                        }
                        3 => self.menu = SubMenuMode::Options { sel: 0 },
                        _ => return PluginAction::Exit,
                    }
                    return PluginAction::Click;
                }
                return PluginAction::Continue;
            }
            SubMenuMode::Options { mut sel } => {
                let (back, act, _) = nav_menu_input(pressed, &mut sel, 3);
                self.menu = SubMenuMode::Options { sel };
                if back {
                    self.menu = SubMenuMode::Main { sel: 3 };
                    return PluginAction::Click;
                }
                if act {
                    match sel {
                        0 => {
                            self.fast_ball = !self.fast_ball;
                            self.apply_opts();
                        }
                        1 => {
                            self.wide_paddle = !self.wide_paddle;
                            self.apply_opts();
                        }
                        _ => {
                            self.high_score = self.score;
                            self.splash =
                                Some((tr(uk, "Best score reset", "Рекорд скинуто").into(), 0.75));
                        }
                    }
                    return PluginAction::Click;
                }
                return PluginAction::Continue;
            }
            SubMenuMode::None => {}
        }

        if (pressed & (btn::START | btn::MENU)) != 0 {
            self.menu = SubMenuMode::Main { sel: 0 };
            return PluginAction::Click;
        }
        if (pressed & btn::A) != 0 {
            if self.game_over {
                self.new_game();
            } else if self.ball_stuck {
                self.ball_stuck = false;
            }
        }

        self.step_accum += dt;
        while self.step_accum >= 1.0 / 60.0 {
            self.step_accum -= 1.0 / 60.0;
            self.step_60hz(held);
        }
        PluginAction::Continue
    }

    fn render(&self, ui: &mut UiEngine, t: ThemeColors, header: &HeaderState<'_>, uk: bool) {
        match self.menu {
            SubMenuMode::Main { sel } => {
                let rows = [
                    PluginMenuRow::action(tr(uk, "Continue", "Продовжити")),
                    PluginMenuRow::action(tr(uk, "New Game", "Нова гра")),
                    PluginMenuRow::action(tr(uk, "Save Game", "Зберегти гру")),
                    PluginMenuRow::submenu(tr(uk, "Options", "Налаштування")),
                    PluginMenuRow::action(tr(uk, "Exit", "Вихід")),
                ];
                render_native_plugin_menu(
                    ui,
                    t,
                    header,
                    tr(uk, "Brick Breaker", "Цеглинки"),
                    &rows,
                    sel,
                );
                return;
            }
            SubMenuMode::Options { sel } => {
                let spd_val = if self.fast_ball {
                    tr(uk, "Fast", "Швидка")
                } else {
                    tr(uk, "Normal", "Звичайна")
                };
                let pad_val = if self.wide_paddle {
                    tr(uk, "Wide", "Широка")
                } else {
                    tr(uk, "Normal", "Звичайна")
                };
                let rows = [
                    PluginMenuRow::option(tr(uk, "Ball Speed", "Швидкість м'яча"), spd_val),
                    PluginMenuRow::option(tr(uk, "Paddle Size", "Розмір платформи"), pad_val),
                    PluginMenuRow::option(
                        tr(uk, "Reset Best Score", "Скинути рекорд"),
                        &self.high_score.to_string(),
                    ),
                ];
                render_native_plugin_menu(
                    ui,
                    t,
                    header,
                    tr(uk, "Options", "Налаштування"),
                    &rows,
                    sel,
                );
                if let Some((ref msg, _)) = self.splash {
                    render_splash_pill(ui, t, msg);
                }
                return;
            }
            SubMenuMode::None => {}
        }

        ui.plugin_begin(t, t.bg_rgb, Some(header));

        let hud_y = CONTENT_TOP_Y + 16.0;
        let score_lbl = format!(
            "{}: {}   {}: {}   {}: {}",
            tr(uk, "Score", "Рахунок"),
            self.score,
            tr(uk, "Best", "Рекорд"),
            self.high_score,
            tr(uk, "Lv", "Рів"),
            self.level
        );
        ui.plugin_draw_text(BRK_AX, hud_y, &score_lbl, 28.0, 2, 0, t.fg_rgb);

        for i in 0..3u8 {
            let cx = BRK_AX + BRK_AW - 12.0 - (i as f32) * 28.0;
            if i < self.lives {
                ui.plugin_fill_circle(cx, hud_y, 9.0, t.fg_rgb);
            } else {
                ui.plugin_stroke_rect(cx - 9.0, hud_y - 9.0, 18.0, 18.0, 9.0, 3.0, t.fg1_rgb);
            }
        }

        ui.plugin_stroke_rect(BRK_AX, BRK_AY, BRK_AW, BRK_AH, 8.0, 4.0, t.fg1_rgb);

        let pad_x = 18.0f32;
        let pad_top = 22.0f32;
        let gap = 8.0f32;
        let bw = (BRK_AW - pad_x * 2.0 - gap * ((BRK_COLS - 1) as f32)) / (BRK_COLS as f32);
        let bh = 28.0f32;
        for r in 0..BRK_ROWS {
            for c in 0..BRK_COLS {
                let hp = self.bricks[r][c];
                if hp == 0 {
                    continue;
                }
                let bx = BRK_AX + pad_x + (c as f32) * (bw + gap);
                let by = BRK_AY + pad_top + (r as f32) * (bh + gap);
                let col = match hp {
                    3 => t.fg_rgb,
                    2 => t.fg2_rgb,
                    _ => t.fg1_rgb,
                };
                ui.plugin_fill_rect(bx, by, bw, bh, 5.0, col);
            }
        }

        let paddle_y = BRK_AY + BRK_AH - 26.0;
        ui.plugin_fill_rect(self.paddle_x, paddle_y, self.paddle_w, 16.0, 8.0, t.fg_rgb);
        ui.plugin_fill_circle(self.ball_x, self.ball_y, 9.0, t.fg2_rgb);

        if self.game_over || self.ball_stuck {
            let msg = if self.game_over {
                tr(
                    uk,
                    "GAME OVER  —  A: Retry  •  Menu: Options",
                    "ГРУ ЗАКІНЧЕНО  —  A: Ще раз  •  Menu: Меню",
                )
            } else {
                tr(
                    uk,
                    "A: Launch Ball  •  Menu: Game Menu",
                    "A: Запуск м'яча  •  Menu: Меню гри",
                )
            };
            let box_w = (ui.plugin_measure_text(msg, 30.0, 2) + 56.0).min(900.0);
            let bx = BRK_AX + (BRK_AW - box_w) * 0.5;
            let by = BRK_AY + BRK_AH * 0.52;
            ui.plugin_fill_rect(bx, by, box_w, 64.0, 8.0, t.bg_rgb);
            ui.plugin_stroke_rect(bx, by, box_w, 64.0, 8.0, 4.0, t.fg1_rgb);
            ui.plugin_draw_text(BRK_AX + BRK_AW * 0.5, by + 32.0, msg, 30.0, 2, 1, t.fg_rgb);
        }

        if let Some((ref msg, _)) = self.splash {
            render_splash_pill(ui, t, msg);
        }
    }
}

/* ============================================================================
 * 5. STOPWATCH & TIMER — 1:1 with `plugins/stopwatch/src/lib.rs`
 * ========================================================================== */
fn format_clock_ms(ms: u64, show_centis: bool) -> String {
    let total_sec = ms / 1000;
    let mins = total_sec / 60;
    let secs = total_sec % 60;
    let centis = (ms % 1000) / 10;
    if mins >= 60 {
        let hrs = mins / 60;
        let rem_m = mins % 60;
        if show_centis {
            format!("{hrs:02}:{rem_m:02}:{secs:02}.{centis:02}")
        } else {
            format!("{hrs:02}:{rem_m:02}:{secs:02}")
        }
    } else if show_centis {
        format!("{mins:02}:{secs:02}.{centis:02}")
    } else {
        format!("{mins:02}:{secs:02}")
    }
}

pub struct StopwatchApp {
    timer_mode: bool,
    sw_running: bool,
    sw_ms: u64,
    laps: Vec<(u64, u64)>, // (split_ms, total_ms)
    lap_scroll: usize,
    tm_running: bool,
    tm_preset_sec: u64,
    tm_rem_ms: u64,
    tm_finished: bool,
    blink_ms: u64,
}

impl StopwatchApp {
    fn new() -> Self {
        Self {
            timer_mode: false,
            sw_running: false,
            sw_ms: 0,
            laps: Vec::new(),
            lap_scroll: 0,
            tm_running: false,
            tm_preset_sec: 5 * 60,
            tm_rem_ms: 5 * 60 * 1000,
            tm_finished: false,
            blink_ms: 0,
        }
    }

    fn title(&self, uk: bool) -> &'static str {
        if self.timer_mode {
            tr(uk, "Timer", "Таймер")
        } else {
            tr(uk, "Stopwatch", "Секундомір")
        }
    }

    fn update(&mut self, dt: f32, pressed: u32) -> PluginAction {
        if (pressed & btn::B) != 0 {
            return PluginAction::Exit;
        }
        let dt_ms = (dt * 1000.0).round() as u64;
        self.blink_ms = self.blink_ms.wrapping_add(dt_ms);

        if self.sw_running {
            self.sw_ms = self.sw_ms.saturating_add(dt_ms);
        }
        if self.tm_running {
            if dt_ms >= self.tm_rem_ms {
                self.tm_rem_ms = 0;
                self.tm_running = false;
                self.tm_finished = true;
            } else {
                self.tm_rem_ms -= dt_ms;
            }
        }

        if (pressed & (btn::L1 | btn::R1 | btn::SELECT)) != 0 {
            self.timer_mode = !self.timer_mode;
            return PluginAction::Click;
        }

        if !self.timer_mode {
            if (pressed & (btn::A | btn::START)) != 0 {
                self.sw_running = !self.sw_running;
                return PluginAction::Click;
            } else if (pressed & (btn::X | btn::Y)) != 0 {
                if self.sw_running {
                    let prev = self.laps.first().map(|&(_, tot)| tot).unwrap_or(0);
                    let split = self.sw_ms.saturating_sub(prev);
                    self.laps.insert(0, (split, self.sw_ms));
                    self.lap_scroll = 0;
                } else {
                    self.sw_ms = 0;
                    self.laps.clear();
                    self.lap_scroll = 0;
                }
                return PluginAction::Click;
            } else if (pressed & btn::UP) != 0 && self.lap_scroll > 0 {
                self.lap_scroll -= 1;
            } else if (pressed & btn::DOWN) != 0 && self.lap_scroll + 5 < self.laps.len() {
                self.lap_scroll += 1;
            }
        } else if (pressed & (btn::A | btn::START)) != 0 {
            if self.tm_finished {
                self.tm_finished = false;
                self.tm_rem_ms = self.tm_preset_sec * 1000;
            } else if self.tm_running {
                self.tm_running = false;
            } else if self.tm_rem_ms > 0 {
                self.tm_running = true;
            }
            return PluginAction::Click;
        } else if (pressed & (btn::X | btn::Y)) != 0 {
            self.tm_running = false;
            self.tm_finished = false;
            self.tm_rem_ms = self.tm_preset_sec * 1000;
            return PluginAction::Click;
        } else if !self.tm_running {
            if (pressed & (btn::UP | btn::RIGHT)) != 0 {
                self.tm_preset_sec = (self.tm_preset_sec + 60).min(180 * 60);
                self.tm_rem_ms = self.tm_preset_sec * 1000;
                self.tm_finished = false;
            } else if (pressed & (btn::DOWN | btn::LEFT)) != 0 {
                self.tm_preset_sec = self.tm_preset_sec.saturating_sub(60).max(60);
                self.tm_rem_ms = self.tm_preset_sec * 1000;
                self.tm_finished = false;
            }
        }

        PluginAction::Continue
    }

    fn render(&self, ui: &mut UiEngine, t: ThemeColors, uk: bool) {
        let w = 1024.0f32;
        let tab_y = CONTENT_TOP_Y + 4.0;
        let tab_w = 220.0f32;
        let tab_h = 44.0f32;
        let tab0_x = w * 0.5 - tab_w - 8.0;
        let tab1_x = w * 0.5 + 8.0;

        if !self.timer_mode {
            ui.plugin_fill_rect(tab0_x, tab_y, tab_w, tab_h, 8.0, t.fg_rgb);
            ui.plugin_stroke_rect(tab1_x, tab_y, tab_w, tab_h, 8.0, 3.0, t.fg1_rgb);
        } else {
            ui.plugin_stroke_rect(tab0_x, tab_y, tab_w, tab_h, 8.0, 3.0, t.fg1_rgb);
            ui.plugin_fill_rect(tab1_x, tab_y, tab_w, tab_h, 8.0, t.fg_rgb);
        }
        ui.plugin_draw_text(
            tab0_x + tab_w * 0.5,
            tab_y + tab_h * 0.5,
            tr(uk, "Stopwatch", "Секундомір"),
            26.0,
            2,
            1,
            if !self.timer_mode { t.bg_rgb } else { t.fg2_rgb },
        );
        ui.plugin_draw_text(
            tab1_x + tab_w * 0.5,
            tab_y + tab_h * 0.5,
            tr(uk, "Timer", "Таймер"),
            26.0,
            2,
            1,
            if self.timer_mode { t.bg_rgb } else { t.fg2_rgb },
        );

        if !self.timer_mode {
            let main_str = format_clock_ms(self.sw_ms, true);
            ui.plugin_stroke_rect(32.0, tab_y + 62.0, w - 64.0, 152.0, 12.0, 4.0, t.fg1_rgb);
            ui.plugin_draw_text(w * 0.5, tab_y + 138.0, &main_str, 92.0, 3, 1, t.fg_rgb);

            let hint = if self.sw_running {
                tr(
                    uk,
                    "A: Pause   •   X: Lap   •   L/R: Timer",
                    "A: Пауза   •   X: Коло   •   L/R: Таймер",
                )
            } else {
                tr(
                    uk,
                    "A: Start   •   X: Reset   •   L/R: Timer",
                    "A: Старт   •   X: Скинути   •   L/R: Таймер",
                )
            };
            ui.plugin_draw_text(w * 0.5, tab_y + 244.0, hint, 26.0, 1, 1, t.fg2_rgb);

            let list_top = tab_y + 276.0;
            let row_h = 54.0f32;
            let vis = 5usize;
            if self.laps.is_empty() {
                ui.plugin_draw_text(
                    w * 0.5,
                    list_top + 90.0,
                    tr(uk, "(No laps recorded)", "(Кіл ще немає)"),
                    30.0,
                    0,
                    1,
                    t.fg1_rgb,
                );
            } else {
                let list_w = if self.laps.len() > vis {
                    w - 64.0 - 28.0
                } else {
                    w - 64.0
                };
                for i in 0..vis {
                    let idx = self.lap_scroll + i;
                    if idx >= self.laps.len() {
                        break;
                    }
                    let lap_num = self.laps.len() - idx;
                    let (split_ms, tot_ms) = self.laps[idx];
                    let ry = list_top + (i as f32) * (row_h + 6.0);
                    ui.plugin_stroke_rect(32.0, ry, list_w, row_h, 8.0, 3.0, t.fg1_rgb);
                    let left_txt = format!("{} #{lap_num}", tr(uk, "Lap", "Коло"));
                    let right_txt = format!(
                        "+{}   ({})",
                        format_clock_ms(split_ms, true),
                        format_clock_ms(tot_ms, true)
                    );
                    ui.plugin_draw_text(52.0, ry + row_h * 0.5, &left_txt, 28.0, 2, 0, t.fg_rgb);
                    ui.plugin_draw_text(
                        32.0 + list_w - 20.0,
                        ry + row_h * 0.5,
                        &right_txt,
                        28.0,
                        1,
                        2,
                        t.fg2_rgb,
                    );
                }
                if self.laps.len() > vis {
                    ui.plugin_draw_scrollbar(
                        t,
                        w - 46.0,
                        list_top,
                        14.0,
                        (vis as f32) * (row_h + 6.0) - 6.0,
                        self.laps.len(),
                        self.lap_scroll,
                        vis,
                    );
                }
            }
        } else {
            let main_str = format_clock_ms(self.tm_rem_ms, false);
            let card_y = tab_y + 78.0;
            if self.tm_finished && ((self.blink_ms / 300) % 2 == 0) {
                ui.plugin_fill_rect(32.0, card_y, w - 64.0, 180.0, 12.0, t.fg_rgb);
                ui.plugin_draw_text(
                    w * 0.5,
                    card_y + 90.0,
                    tr(uk, "TIME UP!", "ЧАС ВИЙШОВ!"),
                    74.0,
                    3,
                    1,
                    t.bg_rgb,
                );
            } else {
                ui.plugin_stroke_rect(32.0, card_y, w - 64.0, 180.0, 12.0, 4.0, t.fg1_rgb);
                ui.plugin_draw_text(w * 0.5, card_y + 90.0, &main_str, 98.0, 3, 1, t.fg_rgb);
            }

            let frac = if self.tm_preset_sec > 0 {
                (self.tm_rem_ms as f32) / ((self.tm_preset_sec * 1000) as f32)
            } else {
                0.0
            };
            ui.plugin_draw_progress_bar(t, 32.0, card_y + 206.0, w - 64.0, 26.0, frac);

            let hint = if self.tm_running {
                tr(uk, "A: Pause   •   X: Reset", "A: Пауза   •   X: Скинути")
            } else {
                tr(
                    uk,
                    "Up/Down: ±1 min   •   A: Start   •   X: Reset",
                    "Вгору/Вниз: ±1 хв   •   A: Старт   •   X: Скинути",
                )
            };
            ui.plugin_draw_text(w * 0.5, card_y + 276.0, hint, 28.0, 1, 1, t.fg2_rgb);
        }
    }
}

/* ============================================================================
 * 6. TEXT VIEWER — 1:1 with `plugins/text-viewer/src/lib.rs`
 * ========================================================================== */
const TV_FONTS: [f32; 3] = [26.0, 32.0, 40.0];
const TV_SPACINGS: [f32; 3] = [1.22, 1.38, 1.56];

pub struct TextViewerApp {
    raw: String,
    lines: Vec<(String, bool)>,
    scroll: usize,
    bookmark: usize,
    size_idx: usize,
    spacing_idx: usize,
    wrapped_for_size: Option<usize>,
    hold_accum: f32,
    menu: SubMenuMode,
    splash: Option<(String, f32)>,
}

impl TextViewerApp {
    fn new(raw: &str) -> Self {
        Self {
            raw: raw.to_string(),
            lines: Vec::new(),
            scroll: 0,
            bookmark: 0,
            size_idx: 1,
            spacing_idx: 1,
            wrapped_for_size: None,
            hold_accum: 0.0,
            menu: SubMenuMode::None,
            splash: None,
        }
    }

    fn ensure_wrapped(&mut self, ui: &mut UiEngine) {
        if self.wrapped_for_size == Some(self.size_idx) && !self.lines.is_empty() {
            return;
        }
        let font_px = TV_FONTS[self.size_idx];
        let max_w = 1024.0 - 64.0 - 26.0;
        let mut out = Vec::new();
        for raw_line in self.raw.lines() {
            let trimmed_end = raw_line.trim_end_matches('\r');
            if trimmed_end.trim().is_empty() {
                out.push((String::new(), false));
                continue;
            }
            let (clean, heading) = if let Some(rest) = trimmed_end.strip_prefix("# ") {
                (rest.trim(), true)
            } else if let Some(rest) = trimmed_end.strip_prefix("## ") {
                (rest.trim(), true)
            } else {
                (trimmed_end, false)
            };
            let wt = if heading { 3 } else { 0 };
            let mut cur = String::new();
            for word in clean.split_whitespace() {
                let cand = if cur.is_empty() {
                    word.to_string()
                } else {
                    format!("{cur} {word}")
                };
                if ui.plugin_measure_text(&cand, font_px, wt) <= max_w {
                    cur = cand;
                } else {
                    if !cur.is_empty() {
                        out.push((std::mem::take(&mut cur), heading));
                    }
                    cur = word.to_string();
                }
            }
            if !cur.is_empty() {
                out.push((cur, heading));
            }
        }
        if out.is_empty() {
            out.push((String::from("(Empty file)"), false));
        }
        self.lines = out;
        self.wrapped_for_size = Some(self.size_idx);
        let vis = self.visible_rows();
        self.scroll = self.scroll.min(self.lines.len().saturating_sub(vis));
    }

    fn row_step(&self) -> f32 {
        (TV_FONTS[self.size_idx] * TV_SPACINGS[self.spacing_idx]).round()
    }

    fn visible_rows(&self) -> usize {
        let usable_h = 768.0 - (CONTENT_TOP_Y + 8.0) - 24.0;
        ((usable_h / self.row_step()).floor() as usize).max(3)
    }

    fn update(&mut self, dt: f32, pressed: u32, held: u32, uk: bool) -> PluginAction {
        if let Some((_, ref mut rem)) = self.splash {
            *rem -= dt;
            if *rem <= 0.0 {
                self.splash = None;
            }
        }

        match self.menu {
            SubMenuMode::Main { mut sel } => {
                let (back, act, _) = nav_menu_input(pressed, &mut sel, 6);
                self.menu = SubMenuMode::Main { sel };
                if back {
                    self.menu = SubMenuMode::None;
                    return PluginAction::Click;
                }
                if act && (pressed & btn::A) != 0 {
                    let vis = self.visible_rows();
                    let max_s = self.lines.len().saturating_sub(vis);
                    match sel {
                        0 => self.menu = SubMenuMode::None,
                        1 => {
                            self.scroll = 0;
                            self.menu = SubMenuMode::None;
                        }
                        2 => {
                            self.scroll = max_s;
                            self.menu = SubMenuMode::None;
                        }
                        3 => {
                            self.bookmark = self.scroll;
                            self.splash = Some((
                                tr(uk, "Bookmark saved", "Закладку збережено").into(),
                                0.75,
                            ));
                            self.menu = SubMenuMode::None;
                        }
                        4 => self.menu = SubMenuMode::Options { sel: 0 },
                        _ => return PluginAction::Exit,
                    }
                    return PluginAction::Click;
                }
                return PluginAction::Continue;
            }
            SubMenuMode::Options { mut sel } => {
                let (back, act, _) = nav_menu_input(pressed, &mut sel, 2);
                self.menu = SubMenuMode::Options { sel };
                if back {
                    self.menu = SubMenuMode::Main { sel: 4 };
                    return PluginAction::Click;
                }
                if act {
                    match sel {
                        0 => {
                            self.size_idx = (self.size_idx + 1) % TV_FONTS.len();
                            self.wrapped_for_size = None;
                        }
                        _ => {
                            self.spacing_idx = (self.spacing_idx + 1) % TV_SPACINGS.len();
                        }
                    }
                    return PluginAction::Click;
                }
                return PluginAction::Continue;
            }
            SubMenuMode::None => {}
        }

        if (pressed & btn::B) != 0 {
            return PluginAction::Exit;
        }
        if (pressed & btn::MENU) != 0 {
            self.menu = SubMenuMode::Main { sel: 0 };
            return PluginAction::Click;
        }
        if (pressed & btn::START) != 0 {
            return PluginAction::TogglePlayPause;
        }
        if (pressed & btn::X) != 0 {
            self.size_idx = (self.size_idx + 1) % TV_FONTS.len();
            self.wrapped_for_size = None;
            return PluginAction::Click;
        }

        let vis = self.visible_rows();
        let max_scroll = self.lines.len().saturating_sub(vis);
        if (pressed & btn::UP) != 0 {
            self.scroll = self.scroll.saturating_sub(1);
            self.hold_accum = 0.0;
        } else if (pressed & btn::DOWN) != 0 {
            self.scroll = (self.scroll + 1).min(max_scroll);
            self.hold_accum = 0.0;
        } else if (pressed & (btn::LEFT | btn::L1)) != 0 {
            self.scroll = self.scroll.saturating_sub(vis);
        } else if (pressed & (btn::RIGHT | btn::R1)) != 0 {
            self.scroll = (self.scroll + vis).min(max_scroll);
        }

        if (held & (btn::UP | btn::DOWN)) != 0 {
            self.hold_accum += dt;
            if self.hold_accum > 0.24 {
                self.hold_accum -= 0.055;
                if (held & btn::UP) != 0 {
                    self.scroll = self.scroll.saturating_sub(1);
                } else {
                    self.scroll = (self.scroll + 1).min(max_scroll);
                }
            }
        } else {
            self.hold_accum = 0.0;
        }

        PluginAction::Continue
    }

    fn render(&mut self, ui: &mut UiEngine, t: ThemeColors, header: &HeaderState<'_>, uk: bool) {
        self.ensure_wrapped(ui);

        match self.menu {
            SubMenuMode::Main { sel } => {
                let rows = [
                    PluginMenuRow::action(tr(uk, "Continue Reading", "Продовжити читання")),
                    PluginMenuRow::action(tr(uk, "Jump to Top", "На початок")),
                    PluginMenuRow::action(tr(uk, "Jump to Bottom", "У кінець")),
                    PluginMenuRow::action(tr(uk, "Save Bookmark", "Зберегти закладку")),
                    PluginMenuRow::submenu(tr(uk, "Options", "Налаштування")),
                    PluginMenuRow::action(tr(uk, "Exit", "Вихід")),
                ];
                render_native_plugin_menu(
                    ui,
                    t,
                    header,
                    tr(uk, "About UIPod", "Про UIPod"),
                    &rows,
                    sel,
                );
                return;
            }
            SubMenuMode::Options { sel } => {
                let sz = match self.size_idx {
                    0 => tr(uk, "Small", "Малий"),
                    1 => tr(uk, "Medium", "Середній"),
                    _ => tr(uk, "Large", "Великий"),
                };
                let sp = match self.spacing_idx {
                    0 => tr(uk, "Compact", "Щільний"),
                    1 => tr(uk, "Normal", "Звичайний"),
                    _ => tr(uk, "Relaxed", "Широкий"),
                };
                let rows = [
                    PluginMenuRow::option(tr(uk, "Font Size", "Розмір шрифту"), sz),
                    PluginMenuRow::option(tr(uk, "Line Spacing", "Міжрядковий інтервал"), sp),
                ];
                render_native_plugin_menu(
                    ui,
                    t,
                    header,
                    tr(uk, "Options", "Налаштування"),
                    &rows,
                    sel,
                );
                return;
            }
            SubMenuMode::None => {}
        }

        let total = self.lines.len();
        let vis = self.visible_rows();
        let pct = if total <= vis {
            100
        } else {
            ((self.scroll * 100) / (total - vis)).min(100)
        };
        let pct_str = format!("{pct}%");
        let mut hdr = *header;
        hdr.legend = &pct_str;

        ui.plugin_begin(t, t.bg_rgb, Some(&hdr));

        let font_px = TV_FONTS[self.size_idx];
        let step = self.row_step();
        let top_y = CONTENT_TOP_Y + 8.0;
        for (i, (txt, is_h)) in self.lines.iter().skip(self.scroll).take(vis).enumerate() {
            if txt.is_empty() {
                continue;
            }
            let cy = top_y + (i as f32 + 0.5) * step;
            ui.plugin_draw_text(
                32.0,
                cy,
                txt,
                font_px,
                if *is_h { 3 } else { 0 },
                0,
                t.fg_rgb,
            );
        }

        if total > vis {
            ui.plugin_draw_scrollbar(
                t,
                1024.0 - 46.0,
                top_y,
                14.0,
                (vis as f32) * step,
                total,
                self.scroll,
                vis,
            );
        }

        if let Some((ref msg, _)) = self.splash {
            render_splash_pill(ui, t, msg);
        }
    }
}

/* ============================================================================
 * 7. MUSIC QUIZ — 1:1 with `plugins/music-quiz/src/lib.rs`
 * ========================================================================== */
#[derive(Clone, Copy, PartialEq, Eq)]
enum QuizTimer {
    Sec15,
    Sec10,
    Off,
}

impl QuizTimer {
    fn sec(self) -> Option<f32> {
        match self {
            Self::Sec15 => Some(15.0),
            Self::Sec10 => Some(10.0),
            Self::Off => None,
        }
    }
    fn next(self) -> Self {
        match self {
            Self::Sec15 => Self::Sec10,
            Self::Sec10 => Self::Off,
            Self::Off => Self::Sec15,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum QuizLength {
    Endless,
    Rounds10,
    Rounds20,
}

impl QuizLength {
    fn limit(self) -> Option<u32> {
        match self {
            Self::Endless => None,
            Self::Rounds10 => Some(10),
            Self::Rounds20 => Some(20),
        }
    }
    fn next(self) -> Self {
        match self {
            Self::Endless => Self::Rounds10,
            Self::Rounds10 => Self::Rounds20,
            Self::Rounds20 => Self::Endless,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum QuizPhase {
    Guessing,
    Reveal { picked: Option<usize>, won: bool },
    Summary,
}

pub struct MusicQuizGame {
    pool: Vec<(String, u32)>,
    choices: [usize; 4],
    correct_slot: usize,
    cursor: usize,
    score: u32,
    best: u32,
    round: u32,
    timer_cfg: QuizTimer,
    len_cfg: QuizLength,
    elapsed_sec: f32,
    reveal_rem: f32,
    phase: QuizPhase,
    eq_bars: [f32; 12],
    needs_snippet: bool,
    menu_sel: Option<usize>,
    was_playing_before_menu: bool,
    rng: Rng64,
}

impl MusicQuizGame {
    fn new(track_labels: &[(String, u32)]) -> Self {
        let mut pool = track_labels.to_vec();
        let fallbacks = [
            ("Cyberia 99 - AquaBIOS 2000", 23_220u32),
            ("NetRunner 2000 - Chrome Breakbeat (Y2K Mix)", 23_325),
            ("Vector 99 - No Other Possibility (2000)", 52_723),
            ("subplaid - distortedspaceship", 65_907),
        ];
        for (lbl, dur) in fallbacks {
            if pool.len() >= 4 {
                break;
            }
            if !pool.iter().any(|p| p.0 == lbl) {
                pool.push((lbl.to_string(), dur));
            }
        }

        let mut g = Self {
            pool,
            choices: [0, 1, 2, 3],
            correct_slot: 0,
            cursor: 0,
            score: 0,
            best: 0,
            round: 1,
            timer_cfg: QuizTimer::Sec15,
            len_cfg: QuizLength::Endless,
            elapsed_sec: 0.0,
            reveal_rem: 0.0,
            phase: QuizPhase::Guessing,
            eq_bars: [0.12; 12],
            needs_snippet: true,
            menu_sel: None,
            was_playing_before_menu: false,
            rng: Rng64::new(0x01C0_5123_9E37_79B9),
        };
        g.setup_round();
        g
    }

    fn setup_round(&mut self) {
        let n = self.pool.len().max(4);
        let target = self.rng.range(n);
        let mut chosen = [target; 4];
        let mut cnt = 1usize;
        for _ in 0..128 {
            if cnt >= 4 {
                break;
            }
            let cand = self.rng.range(n);
            if !chosen[..cnt].contains(&cand) {
                chosen[cnt] = cand;
                cnt += 1;
            }
        }
        for cand in 0..n {
            if cnt >= 4 {
                break;
            }
            if !chosen[..cnt].contains(&cand) {
                chosen[cnt] = cand;
                cnt += 1;
            }
        }
        for i in (1..4).rev() {
            let j = self.rng.range(i + 1);
            chosen.swap(i, j);
        }
        self.correct_slot = chosen.iter().position(|&x| x == target).unwrap_or(0);
        self.choices = chosen;
        self.cursor = 0;
        self.elapsed_sec = 0.0;
        self.reveal_rem = 0.0;
        self.phase = QuizPhase::Guessing;
        self.needs_snippet = true;
    }

    fn snippet_offset_ms(&mut self, idx: usize) -> u32 {
        let dur = self.pool.get(idx).map(|t| t.1).unwrap_or(23_000);
        let lo = (dur as f32 * 0.20) as u32;
        let span = ((dur as f32 * 0.45) as u32).max(1_000);
        lo + (self.rng.next_u32() % span)
    }

    fn update(
        &mut self,
        dt: f32,
        pressed: u32,
        pcm: &[i16],
        audio: AudioSnapshot<'_>,
        _uk: bool,
    ) -> PluginAction {
        if let Some(mut sel) = self.menu_sel {
            let (back, act, _) = nav_menu_input(pressed, &mut sel, 5);
            self.menu_sel = Some(sel);
            if back || (pressed & btn::START) != 0 {
                self.menu_sel = None;
                if self.was_playing_before_menu {
                    return PluginAction::SetPlaying(true);
                }
                return PluginAction::Click;
            }
            if act && (pressed & btn::A) != 0 {
                match sel {
                    0 => {
                        self.menu_sel = None;
                        if self.was_playing_before_menu {
                            return PluginAction::SetPlaying(true);
                        }
                    }
                    1 => {
                        self.menu_sel = None;
                        self.score = 0;
                        self.round = 1;
                        self.setup_round();
                    }
                    2 => self.timer_cfg = self.timer_cfg.next(),
                    3 => self.len_cfg = self.len_cfg.next(),
                    _ => return PluginAction::Exit,
                }
                return PluginAction::Click;
            }
            return PluginAction::Continue;
        }

        if (pressed & (btn::START | btn::MENU)) != 0 {
            self.was_playing_before_menu = audio.playing;
            self.menu_sel = Some(0);
            if audio.playing {
                return PluginAction::SetPlaying(false);
            }
            return PluginAction::Click;
        }

        // Animate 12-bar live audio mystery equalizer
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
                let tgt = (peak * 2.6).clamp(0.10, 1.0);
                *slot += (tgt - *slot) * 0.38;
            }
        } else {
            for slot in &mut self.eq_bars {
                *slot = (*slot * 0.88).max(0.08);
            }
        }

        if matches!(self.phase, QuizPhase::Summary) {
            if (pressed & btn::A) != 0 {
                self.score = 0;
                self.round = 1;
                self.setup_round();
                return PluginAction::Click;
            }
            return PluginAction::Continue;
        }

        if self.needs_snippet {
            self.needs_snippet = false;
            let trk = self.choices[self.correct_slot];
            let pos = self.snippet_offset_ms(trk);
            return PluginAction::PlayTrackAt(trk, pos);
        }

        if matches!(self.phase, QuizPhase::Reveal { .. }) {
            self.reveal_rem -= dt;
            if self.reveal_rem <= 0.0 || (pressed & btn::A) != 0 {
                if let Some(lim) = self.len_cfg.limit() {
                    if self.round >= lim {
                        self.phase = QuizPhase::Summary;
                        return PluginAction::Continue;
                    }
                }
                self.round += 1;
                self.setup_round();
            }
            return PluginAction::Continue;
        }

        if (pressed & btn::UP) != 0 {
            self.cursor = (self.cursor + 3) % 4;
        } else if (pressed & btn::DOWN) != 0 {
            self.cursor = (self.cursor + 1) % 4;
        } else if (pressed & btn::X) != 0 {
            let trk = self.choices[self.correct_slot];
            let pos = self.snippet_offset_ms(trk);
            return PluginAction::PlayTrackAt(trk, pos);
        } else if (pressed & btn::A) != 0 {
            let won = self.cursor == self.correct_slot;
            if won {
                self.score += 1;
                self.best = self.best.max(self.score);
            }
            self.phase = QuizPhase::Reveal {
                picked: Some(self.cursor),
                won,
            };
            self.reveal_rem = 1.5;
            return PluginAction::Click;
        }

        self.elapsed_sec += dt;
        if let Some(lim_s) = self.timer_cfg.sec() {
            if self.elapsed_sec >= lim_s {
                self.phase = QuizPhase::Reveal {
                    picked: None,
                    won: false,
                };
                self.reveal_rem = 1.5;
            }
        }
        PluginAction::Continue
    }

    fn render(&self, ui: &mut UiEngine, t: ThemeColors, header: &HeaderState<'_>, uk: bool) {
        if let Some(sel) = self.menu_sel {
            let timer_lbl = match self.timer_cfg {
                QuizTimer::Sec15 => tr(uk, "15s", "15с"),
                QuizTimer::Sec10 => tr(uk, "10s", "10с"),
                QuizTimer::Off => tr(uk, "Off", "Вимк."),
            };
            let len_lbl = match self.len_cfg {
                QuizLength::Endless => tr(uk, "Endless", "Нескінченно"),
                QuizLength::Rounds10 => "10",
                QuizLength::Rounds20 => "20",
            };
            let rows = [
                PluginMenuRow::action(tr(uk, "Continue", "Продовжити")),
                PluginMenuRow::action(tr(uk, "New Game", "Нова гра")),
                PluginMenuRow::option(tr(uk, "Round Timer", "Таймер раунду"), timer_lbl),
                PluginMenuRow::option(tr(uk, "Game Length", "Кількість раундів"), len_lbl),
                PluginMenuRow::action(tr(uk, "Exit", "Вихід")),
            ];
            render_native_plugin_menu(
                ui,
                t,
                header,
                tr(uk, "Music Quiz", "Вгадай мелодію"),
                &rows,
                sel,
            );
            return;
        }

        ui.plugin_begin(t, t.bg_rgb, Some(header));
        let sw = 1024.0f32;
        let cx = sw * 0.5;
        let inner_w = sw - 64.0;

        if matches!(self.phase, QuizPhase::Summary) {
            let max_r = self.len_cfg.limit().unwrap_or(self.round);
            let sum_w = 832.0f32;
            let sum_x = cx - sum_w * 0.5;
            ui.plugin_fill_rect(sum_x, 184.0, sum_w, 460.0, 24.0, t.bg_rgb);
            ui.plugin_stroke_rect(sum_x, 184.0, sum_w, 460.0, 24.0, 4.0, t.fg_rgb);
            ui.plugin_draw_text(
                cx,
                256.0,
                tr(uk, "Quiz Complete!", "Гру завершено!"),
                42.0,
                3,
                1,
                t.fg_rgb,
            );
            let res_str = format!("{}: {} / {}", tr(uk, "Score", "Рахунок"), self.score, max_r);
            ui.plugin_draw_text(cx, 355.0, &res_str, 52.0, 3, 1, t.fg_rgb);
            let best_str = format!("{}: {}", tr(uk, "Best Score", "Рекорд"), self.best);
            ui.plugin_draw_text(cx, 435.0, &best_str, 28.0, 2, 1, t.fg2_rgb);
            ui.plugin_fill_rect(cx - 250.0, 515.0, 500.0, 74.0, 16.0, t.fg_rgb);
            ui.plugin_draw_text(
                cx,
                552.0,
                tr(
                    uk,
                    "[A] Play Again    [START] Menu",
                    "[A] Ще раз    [START] Меню",
                ),
                26.0,
                2,
                1,
                t.bg_rgb,
            );
            return;
        }

        let hud_x = 32.0f32;
        let hud_y = CONTENT_TOP_Y;
        let hud_h = 110.0f32;
        ui.plugin_stroke_rect(hud_x, hud_y, inner_w, hud_h, 18.0, 3.0, t.fg1_rgb);

        let round_lbl = match self.len_cfg.limit() {
            Some(lim) => format!("{} {}/{}", tr(uk, "Round", "Раунд"), self.round, lim),
            None => format!("{} {}", tr(uk, "Round", "Раунд"), self.round),
        };
        ui.plugin_draw_text(hud_x + 26.0, hud_y + 36.0, &round_lbl, 30.0, 3, 0, t.fg_rgb);

        let (status_txt, status_col, status_wt) = match self.phase {
            QuizPhase::Guessing => (
                tr(uk, "[X] Another snippet", "[X] Інший уривок"),
                t.fg2_rgb,
                1,
            ),
            QuizPhase::Reveal { won: true, .. } => (
                tr(uk, "Correct! +1 point", "Правильно! +1 очко"),
                t.fg_rgb,
                3,
            ),
            QuizPhase::Reveal { picked: None, .. } => {
                (tr(uk, "Time's up!", "Час вийшов!"), t.fg_rgb, 3)
            }
            QuizPhase::Reveal { won: false, .. } => {
                (tr(uk, "Wrong answer!", "Неправильно!"), t.fg_rgb, 3)
            }
            QuizPhase::Summary => ("", t.fg_rgb, 1),
        };
        ui.plugin_draw_text(
            hud_x + 26.0,
            hud_y + 78.0,
            status_txt,
            22.0,
            status_wt,
            0,
            status_col,
        );

        // Center 12-bar live audio equalizer pill
        let eq_total_w = 12.0 * 10.0 + 11.0 * 6.0;
        let eq_x0 = (sw - eq_total_w) * 0.5;
        let eq_base_y = hud_y + hud_h - 18.0;
        for (i, &amp) in self.eq_bars.iter().enumerate() {
            let bx = eq_x0 + (i as f32) * 16.0;
            let bh = (amp * 74.0).max(8.0);
            let col = if amp > 0.62 {
                t.fg_rgb
            } else if amp > 0.30 {
                t.fg2_rgb
            } else {
                t.fg1_rgb
            };
            ui.plugin_fill_rect(bx, eq_base_y - bh, 10.0, bh, 4.0, col);
        }

        let score_lbl = format!("{}: {}", tr(uk, "Score", "Рахунок"), self.score);
        ui.plugin_draw_text(
            hud_x + inner_w - 26.0,
            hud_y + 36.0,
            &score_lbl,
            32.0,
            3,
            2,
            t.fg_rgb,
        );
        let best_lbl = format!("{}: {}", tr(uk, "Best", "Рекорд"), self.best);
        ui.plugin_draw_text(
            hud_x + inner_w - 26.0,
            hud_y + 78.0,
            &best_lbl,
            22.0,
            1,
            2,
            t.fg2_rgb,
        );

        let bar_y = hud_y + hud_h + 14.0;
        let progress = match (self.phase, self.timer_cfg.sec()) {
            (QuizPhase::Guessing, Some(lim)) => (1.0 - self.elapsed_sec / lim).clamp(0.0, 1.0),
            (QuizPhase::Reveal { .. }, Some(_)) => 0.0,
            _ => (self.elapsed_sec / 10.0).fract(),
        };
        ui.plugin_draw_progress_bar(t, 32.0, bar_y, inner_w, 22.0, progress);

        let cards_top = bar_y + 36.0;
        let card_h = 96.0f32;
        let card_gap = 14.0f32;
        for slot in 0..4usize {
            let cy = cards_top + (slot as f32) * (card_h + card_gap);
            let song_lbl = self
                .pool
                .get(self.choices[slot])
                .map(|p| p.0.as_str())
                .unwrap_or("Track");

            let (fill_c, border_c, border_w, txt_c, badge_bg, badge_fg, right_tag) =
                match self.phase {
                    QuizPhase::Guessing => {
                        if slot == self.cursor {
                            (t.fg_rgb, t.fg_rgb, 0.0, t.bg_rgb, t.bg_rgb, t.fg_rgb, None)
                        } else {
                            (t.bg_rgb, t.fg1_rgb, 3.0, t.fg_rgb, t.fg1_rgb, t.bg_rgb, None)
                        }
                    }
                    QuizPhase::Reveal { picked, won } => {
                        if slot == self.correct_slot {
                            (
                                t.fg_rgb,
                                t.fg2_rgb,
                                4.0,
                                t.bg_rgb,
                                t.bg_rgb,
                                t.fg_rgb,
                                Some(if won { "+1" } else { "✓" }),
                            )
                        } else if picked == Some(slot) {
                            (
                                t.fg1_rgb,
                                t.fg2_rgb,
                                3.0,
                                t.fg_rgb,
                                t.fg2_rgb,
                                t.bg_rgb,
                                Some("✕"),
                            )
                        } else {
                            (t.bg_rgb, t.fg1_rgb, 2.0, t.fg2_rgb, t.fg1_rgb, t.bg_rgb, None)
                        }
                    }
                    QuizPhase::Summary => {
                        (t.bg_rgb, t.fg1_rgb, 2.0, t.fg_rgb, t.fg1_rgb, t.bg_rgb, None)
                    }
                };

            ui.plugin_fill_rect(32.0, cy, inner_w, card_h, 18.0, fill_c);
            if border_w > 0.0 {
                ui.plugin_stroke_rect(32.0, cy, inner_w, card_h, 18.0, border_w, border_c);
            }

            ui.plugin_fill_rect(54.0, cy + 23.0, 50.0, 50.0, 12.0, badge_bg);
            ui.plugin_draw_text(
                79.0,
                cy + card_h * 0.5,
                &(slot + 1).to_string(),
                28.0,
                3,
                1,
                badge_fg,
            );

            ui.plugin_draw_text(124.0, cy + card_h * 0.5, song_lbl, 28.0, 2, 0, txt_c);
            if let Some(tag) = right_tag {
                ui.plugin_draw_text(
                    32.0 + inner_w - 28.0,
                    cy + card_h * 0.5,
                    tag,
                    30.0,
                    3,
                    2,
                    txt_c,
                );
            }
        }
    }
}

/* ============================================================================
 * 8. SOUND RACER — 1:1 with `plugins/sound-racer/src/lib.rs`
 * ========================================================================== */
const SR_VX: f32 = 32.0;
const SR_VY: f32 = CONTENT_TOP_Y + 42.0; // 178
const SR_VW: f32 = 700.0;
const SR_VH: f32 = 562.0;
const SR_SHIP_Y: f32 = SR_VY + SR_VH - 58.0; // 682

#[derive(Clone, Copy, PartialEq, Eq)]
enum SrBlockKind {
    Heavy,
    Medium,
    Light,
    Spike,
}

#[derive(Clone, Copy)]
struct SrBlock {
    lane: usize,
    z: f32,
    kind: SrBlockKind,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum SrMenu {
    Launch { sel: usize },
    None,
    Main { sel: usize },
    Options { sel: usize },
}

pub struct SoundRacerGame {
    lane: usize,
    ship_pos: f32,
    jump_rem: f32,
    blocks: Vec<SrBlock>,
    tray: [[u8; 7]; 3],
    matched_mask: [[bool; 7]; 3],
    fuse_rem: Option<f32>,
    score: u32,
    hi_score: u32,
    chain: u32,
    streak_hits: u32,
    cluster_mode: bool,
    difficulty: u8, // 0=Chill, 1=Normal, 2=Pro
    energy_avg: f32,
    spawn_cd: f32,
    scroll_phase: f32,
    road_curve: f32,
    target_curve: f32,
    intensity: f32,
    live_bars: [f32; 16],
    live_kick: f32,
    now_ms: u64,
    launch_pos_ms: u32,
    track_display: String,
    cur_pos_ms: u32,
    cur_dur_ms: u32,
    was_playing_before_menu: bool,
    menu: SrMenu,
    callout: Option<(String, f32)>,
    splash: Option<(String, f32)>,
    rng: Rng64,
}

impl SoundRacerGame {
    fn new(pos_ms: u32) -> Self {
        Self {
            lane: 1,
            ship_pos: 1.0,
            jump_rem: 0.0,
            blocks: Vec::new(),
            tray: [[0; 7]; 3],
            matched_mask: [[false; 7]; 3],
            fuse_rem: None,
            score: 0,
            hi_score: 0,
            chain: 1,
            streak_hits: 0,
            cluster_mode: false,
            difficulty: 1,
            energy_avg: 0.08,
            spawn_cd: 0.22,
            scroll_phase: 0.0,
            road_curve: 0.0,
            target_curve: 0.0,
            intensity: 0.35,
            live_bars: [0.0; 16],
            live_kick: 0.0,
            now_ms: 0,
            launch_pos_ms: pos_ms,
            track_display: String::new(),
            cur_pos_ms: pos_ms,
            cur_dur_ms: 0,
            was_playing_before_menu: true,
            menu: SrMenu::Launch { sel: 0 },
            callout: None,
            splash: None,
            rng: Rng64::new(0x50AD_4ACE_CAFE_1234),
        }
    }

    fn reset_run(&mut self) {
        self.score = 0;
        self.chain = 1;
        self.streak_hits = 0;
        self.blocks.clear();
        self.tray = [[0; 7]; 3];
        self.matched_mask = [[false; 7]; 3];
        self.fuse_rem = None;
    }

    fn recompute_matches(&mut self) {
        self.matched_mask = [[false; 7]; 3];
        let mut visited = [[false; 7]; 3];
        let mut any = false;
        for c in 0..3 {
            for r in 0..7 {
                let v = self.tray[c][r];
                if v == 0 || v == 4 || visited[c][r] {
                    continue;
                }
                let mut st = vec![(c, r)];
                let mut comp = Vec::new();
                visited[c][r] = true;
                while let Some((cc, cr)) = st.pop() {
                    comp.push((cc, cr));
                    for (dc, dr) in [(-1i32, 0), (1, 0), (0, -1), (0, 1)] {
                        let nc = cc as i32 + dc;
                        let nr = cr as i32 + dr;
                        if (0..3).contains(&nc) && (0..7).contains(&nr) {
                            let (uc, ur) = (nc as usize, nr as usize);
                            if !visited[uc][ur] && self.tray[uc][ur] == v {
                                visited[uc][ur] = true;
                                st.push((uc, ur));
                            }
                        }
                    }
                }
                if comp.len() >= 3 {
                    any = true;
                    for (mc, mr) in comp {
                        self.matched_mask[mc][mr] = true;
                    }
                }
            }
        }
        if any {
            if self.fuse_rem.is_none() {
                self.fuse_rem = Some(0.32);
            }
        } else {
            self.fuse_rem = None;
        }
    }

    fn push_tray(&mut self, col: usize, kind: SrBlockKind) -> bool {
        let code = match kind {
            SrBlockKind::Heavy => 1,
            SrBlockKind::Medium => 2,
            SrBlockKind::Light => 3,
            SrBlockKind::Spike => 4,
        };
        for r in 0..7 {
            if self.tray[col][r] == 0 {
                self.tray[col][r] = code;
                self.recompute_matches();
                return false;
            }
        }
        self.tray[col] = [0; 7];
        self.recompute_matches();
        true
    }

    fn poll_cash_in(&mut self, dt: f32, force: bool) -> Option<(usize, u32)> {
        if let Some(ref mut rem) = self.fuse_rem {
            *rem -= dt;
            if !force && *rem > 0.0 {
                return None;
            }
        } else if !force {
            return None;
        }

        let mut cleared = 0usize;
        for c in 0..3 {
            for r in 0..7 {
                if self.matched_mask[c][r] {
                    self.tray[c][r] = 0;
                    cleared += 1;
                }
            }
        }
        if cleared == 0 {
            self.fuse_rem = None;
            return None;
        }
        for c in 0..3 {
            let mut next_col = [0u8; 7];
            let mut dst = 0usize;
            for r in 0..7 {
                if self.tray[c][r] != 0 {
                    next_col[dst] = self.tray[c][r];
                    dst += 1;
                }
            }
            self.tray[c] = next_col;
        }
        self.fuse_rem = None;
        self.recompute_matches();
        let pts = (cleared as u32) * (cleared as u32) * 35;
        Some((cleared, pts))
    }

    fn project_road(z: f32, lane_offset: f32, curve: f32, intensity: f32) -> (f32, f32, f32) {
        let vp_y = SR_VY + 74.0 + (0.5 - intensity) * 28.0;
        let depth = (1.0 - z.clamp(0.0, 1.0)).powf(1.62);
        let y = vp_y + depth * (SR_SHIP_Y - vp_y);
        let scale = 0.16 + depth * 0.84;
        let curve_x = curve * (1.0 - depth).powi(2) * 145.0;
        let lane_w = 206.0 * scale;
        let x = SR_VX + SR_VW * 0.5 + curve_x + lane_offset * lane_w;
        (x, y, scale)
    }

    fn draw_highway_block(
        ui: &mut UiEngine,
        t: ThemeColors,
        cx: f32,
        cy: f32,
        scale: f32,
        kind: SrBlockKind,
    ) {
        let w = 132.0 * scale;
        let h = 34.0 * scale;
        let lift = 10.0 * scale;
        let x = cx - w * 0.5;
        let y_top = cy - h - lift;
        let r = (5.0 * scale).max(1.5);
        match kind {
            SrBlockKind::Spike => {
                let sw = w * 0.82;
                let sx = cx - sw * 0.5;
                ui.plugin_stroke_rect(sx, cy - h, sw, h, r, (3.2 * scale).max(1.5), t.fg_rgb);
                let tip_y = cy - h - 18.0 * scale;
                ui.plugin_draw_line(
                    sx + sw * 0.15,
                    cy - 2.0,
                    cx,
                    tip_y,
                    (3.5 * scale).max(1.5),
                    t.fg_rgb,
                );
                ui.plugin_draw_line(
                    sx + sw * 0.85,
                    cy - 2.0,
                    cx,
                    tip_y,
                    (3.5 * scale).max(1.5),
                    t.fg_rgb,
                );
            }
            SrBlockKind::Heavy => {
                ui.plugin_fill_rect(x, y_top, w, h + lift, r, t.fg_rgb);
                if w > 18.0 {
                    ui.plugin_fill_rect(
                        x + w * 0.18,
                        y_top + h * 0.36,
                        w * 0.64,
                        (4.5 * scale).max(1.5),
                        0.0,
                        t.bg_rgb,
                    );
                }
            }
            SrBlockKind::Medium => {
                ui.plugin_fill_rect(x, y_top, w, h + lift, r, t.fg2_rgb);
                ui.plugin_stroke_rect(x, y_top, w, h + lift, r, (2.4 * scale).max(1.2), t.fg_rgb);
                if w > 18.0 {
                    ui.plugin_fill_rect(
                        x + w * 0.22,
                        y_top + h * 0.45,
                        w * 0.56,
                        (4.0 * scale).max(1.5),
                        0.0,
                        t.bg_rgb,
                    );
                }
            }
            SrBlockKind::Light => {
                ui.plugin_fill_rect(x, y_top, w, h + lift, r, t.fg1_rgb);
                ui.plugin_stroke_rect(x, y_top, w, h + lift, r, (2.4 * scale).max(1.2), t.fg_rgb);
                if w > 18.0 {
                    let ps = (9.0 * scale).max(3.0);
                    ui.plugin_fill_rect(
                        cx - ps * 0.5,
                        y_top + (h - ps) * 0.5,
                        ps,
                        ps,
                        2.0,
                        t.fg_rgb,
                    );
                }
            }
        }
    }

    fn update(
        &mut self,
        dt: f32,
        pressed: u32,
        pcm: &[i16],
        audio: AudioSnapshot<'_>,
        uk: bool,
    ) -> PluginAction {
        if self.track_display != audio.track_title {
            self.track_display = audio.track_title.to_string();
        }
        self.cur_pos_ms = audio.pos_ms;
        self.cur_dur_ms = audio.dur_ms;

        if let Some((_, ref mut rem)) = self.splash {
            *rem -= dt;
            if *rem <= 0.0 {
                self.splash = None;
            }
        }

        match self.menu {
            SrMenu::Launch { mut sel } => {
                self.launch_pos_ms = audio.pos_ms;
                let (back, act, _) = nav_menu_input(pressed, &mut sel, 2);
                self.menu = SrMenu::Launch { sel };
                if back {
                    return PluginAction::Exit;
                }
                if act && (pressed & btn::A) != 0 {
                    self.menu = SrMenu::None;
                    self.reset_run();
                    if sel == 0 {
                        return PluginAction::SeekMs(0);
                    } else {
                        return PluginAction::EnsurePlaying;
                    }
                }
                return PluginAction::Continue;
            }
            SrMenu::Main { mut sel } => {
                let (back, act, _) = nav_menu_input(pressed, &mut sel, 6);
                self.menu = SrMenu::Main { sel };
                if back || (pressed & btn::START) != 0 {
                    self.menu = SrMenu::None;
                    if self.was_playing_before_menu {
                        return PluginAction::SetPlaying(true);
                    }
                    return PluginAction::Click;
                }
                if act && (pressed & btn::A) != 0 {
                    match sel {
                        0 => {
                            self.menu = SrMenu::None;
                            if self.was_playing_before_menu {
                                return PluginAction::SetPlaying(true);
                            }
                        }
                        1 => {
                            self.menu = SrMenu::None;
                            self.reset_run();
                            return PluginAction::SeekMs(0);
                        }
                        2 => {
                            self.menu = SrMenu::None;
                            self.reset_run();
                            return PluginAction::NextTrack;
                        }
                        3 => {
                            self.menu = SrMenu::None;
                            self.reset_run();
                            return PluginAction::PrevTrack;
                        }
                        4 => self.menu = SrMenu::Options { sel: 0 },
                        _ => return PluginAction::Exit,
                    }
                    return PluginAction::Click;
                }
                return PluginAction::Continue;
            }
            SrMenu::Options { mut sel } => {
                let (back, act, _) = nav_menu_input(pressed, &mut sel, 3);
                self.menu = SrMenu::Options { sel };
                if back {
                    self.menu = SrMenu::Main { sel: 4 };
                    return PluginAction::Click;
                }
                if act {
                    match sel {
                        0 => {
                            self.cluster_mode = !self.cluster_mode;
                            self.reset_run();
                        }
                        1 => self.difficulty = (self.difficulty + 1) % 3,
                        _ => {
                            self.hi_score = 0;
                            self.splash = Some((
                                tr(uk, "Track Hi-Score reset", "Рекорд треку скинуто").into(),
                                0.75,
                            ));
                        }
                    }
                    return PluginAction::Click;
                }
                return PluginAction::Continue;
            }
            SrMenu::None => {}
        }

        self.now_ms = self.now_ms.saturating_add((dt * 1000.0).round() as u64);

        if (pressed & (btn::START | btn::MENU)) != 0 {
            self.was_playing_before_menu = audio.playing;
            self.menu = SrMenu::Main { sel: 0 };
            if audio.playing {
                return PluginAction::SetPlaying(false);
            }
            return PluginAction::Click;
        }
        if (pressed & btn::L2) != 0 {
            self.reset_run();
            return PluginAction::PrevTrack;
        }
        if (pressed & btn::R2) != 0 {
            self.reset_run();
            return PluginAction::NextTrack;
        }
        if (pressed & (btn::LEFT | btn::L1)) != 0 && self.lane > 0 {
            self.lane -= 1;
        } else if (pressed & (btn::RIGHT | btn::R1)) != 0 && self.lane < 2 {
            self.lane += 1;
        }

        if (pressed & (btn::A | btn::UP)) != 0 {
            if !audio.playing {
                return PluginAction::EnsurePlaying;
            } else if self.jump_rem <= 0.0 {
                self.jump_rem = 0.36;
            }
        }
        if (pressed & (btn::B | btn::X)) != 0 {
            if let Some((cnt, base_pts)) = self.poll_cash_in(0.0, true) {
                let gained = base_pts * self.chain;
                self.score += gained;
                self.hi_score = self.hi_score.max(self.score);
                self.chain = (self.chain + 1).min(8);
                self.callout = Some((
                    format!("{} x{}  +{}", tr(uk, "CASH IN", "КАСКАД"), cnt, gained),
                    0.95,
                ));
            }
        }

        self.ship_pos += ((self.lane as f32) - self.ship_pos) * (dt * 20.0).min(1.0);
        self.road_curve += (self.target_curve - self.road_curve) * (dt * 3.2).min(1.0);
        if self.jump_rem > 0.0 {
            self.jump_rem = (self.jump_rem - dt).max(0.0);
        }
        if let Some((_, ref mut rem)) = self.callout {
            *rem -= dt;
            if *rem <= 0.0 {
                self.callout = None;
            }
        }

        // Analyze live stereo PCM into 16 spectrum bars + low/high/pan features
        let frames = (pcm.len() / 2).min(512);
        if audio.playing && frames >= 32 {
            let chunk = (frames / 16).max(1);
            let mut sum_l = 0.0f32;
            let mut sum_r = 0.0f32;
            let mut low_acc = 0.0f32;
            let mut high_acc = 0.0f32;
            let mut prev_m = 0.0f32;
            for i in 0..frames {
                let l = pcm[2 * i] as f32 / 32768.0;
                let r = pcm[2 * i + 1] as f32 / 32768.0;
                let m = 0.5 * (l + r);
                sum_l += l * l;
                sum_r += r * r;
                high_acc += (m - prev_m).abs();
                prev_m = m;
            }
            for b in 0..16 {
                let s0 = b * chunk;
                let s1 = ((b + 1) * chunk).min(frames);
                let mut pk = 0.0f32;
                for i in s0..s1 {
                    let m =
                        0.5 * ((pcm[2 * i] as f32).abs() + (pcm[2 * i + 1] as f32).abs()) / 32768.0;
                    pk = pk.max(m);
                }
                let src = (pk * 2.2).clamp(0.0, 1.0);
                if b < 4 {
                    low_acc += src * 0.25;
                }
                let dst = &mut self.live_bars[b];
                *dst = if src > *dst {
                    *dst * 0.35 + src * 0.65
                } else {
                    *dst * 0.82 + src * 0.18
                };
            }
            let rms_l = (sum_l / frames as f32).sqrt();
            let rms_r = (sum_r / frames as f32).sqrt();
            let energy = 0.5 * (rms_l + rms_r);
            let high = (high_acc / frames as f32 * 3.5).clamp(0.0, 1.0);
            let pan = ((rms_r - rms_l) * 3.0).clamp(-1.0, 1.0);

            let apu = uipod_codecs::get_nsf_apu_telemetry();
            let (apu_lead, apu_bass, apu_perc, apu_pan) =
                if uipod_codecs::get_nsf_active_chip_flags().is_some() && apu.channel_count > 0 {
                    let vol = |idx: usize| -> f32 {
                        let ch = &apu.channels[idx];
                        if ch.active != 0 && ch.muted == 0 {
                            ch.volume.clamp(0.0, 1.0)
                        } else {
                            0.0
                        }
                    };
                    let trig = |idx: usize| -> bool {
                        let ch = &apu.channels[idx];
                        ch.active != 0 && ch.muted == 0 && ch.onset_trigger != 0
                    };
                    let p1 = vol(0);
                    let p2 = vol(1);
                    let exp1 = vol(5);
                    let exp2 = vol(6);
                    let lead = trig(0) || trig(1) || trig(5) || trig(6);
                    let bass = trig(2) || trig(7) || vol(2) > 0.65;
                    let perc = trig(3) || trig(4);
                    let pan_est = ((p2 + exp2) - (p1 + exp1)).clamp(-1.0, 1.0);
                    for (c, ch) in apu.channels[..(apu.channel_count as usize).min(8)]
                        .iter()
                        .enumerate()
                    {
                        if ch.muted == 0 && ch.active != 0 {
                            let b0 = (c * 2).min(15);
                            let b1 = (c * 2 + 1).min(15);
                            let v = ch.volume.clamp(0.0, 1.0);
                            self.live_bars[b0] = self.live_bars[b0].max(v);
                            self.live_bars[b1] = self.live_bars[b1].max(v * 0.85);
                        }
                    }
                    (lead, bass, perc, Some(pan_est))
                } else {
                    (false, false, false, None)
                };

            self.energy_avg = self.energy_avg * 0.94 + energy * 0.06;
            let rel_int = (energy / (self.energy_avg.max(0.02) * 1.45)).clamp(0.05, 1.0);
            self.intensity = self.intensity * 0.88 + rel_int * 0.12;
            let eff_pan = apu_pan.unwrap_or(pan);
            self.target_curve = (eff_pan * 1.4 + (high - low_acc) * 0.6).clamp(-1.0, 1.0);
            let mut kick_src = (low_acc * 1.25).clamp(0.0, 1.0);
            if apu_perc || apu_bass {
                kick_src = kick_src.max(0.88);
            }
            self.live_kick = (self.live_kick * 0.72).max(kick_src);

            let speed = 0.86 + self.intensity * 0.32;
            self.scroll_phase = (self.scroll_phase + dt * (3.0 + self.intensity * 4.2)).fract();

            self.spawn_cd -= dt;
            let min_gap = match self.difficulty {
                0 => 0.28,
                1 => 0.19,
                _ => 0.13,
            };
            let beat = apu_lead || apu_bass || apu_perc || energy > (self.energy_avg * 1.15).max(0.045);
            if self.spawn_cd <= 0.0 && (beat || self.spawn_cd < -0.32) {
                self.spawn_cd = if beat { min_gap } else { min_gap + 0.12 };
                let lane = if apu_bass && !apu_lead {
                    1
                } else if eff_pan < -0.08 {
                    0
                } else if eff_pan > 0.08 {
                    2
                } else {
                    self.rng.range(3)
                };
                let kind = if (apu_perc && self.rng.range(3) == 0)
                    || (energy > self.energy_avg * 1.55 && self.rng.range(4) == 0)
                {
                    SrBlockKind::Spike
                } else if apu_bass || !self.cluster_mode || energy > self.energy_avg * 1.28 {
                    SrBlockKind::Heavy
                } else if apu_lead || energy > self.energy_avg * 1.02 {
                    SrBlockKind::Medium
                } else {
                    SrBlockKind::Light
                };
                self.blocks.push(SrBlock { lane, z: 1.0, kind });
            }

            let airborne = self.jump_rem > 0.04;
            let ship_lane = (self.ship_pos.round() as i32).clamp(0, 2) as usize;
            let old_blocks = std::mem::take(&mut self.blocks);
            let mut rem_blocks = Vec::with_capacity(old_blocks.len());
            for mut b in old_blocks {
                b.z -= dt * speed;
                if (-0.04..=0.06).contains(&b.z) && b.lane == ship_lane && !airborne {
                    if b.kind == SrBlockKind::Spike {
                        self.chain = 1;
                        self.streak_hits = 0;
                        let _ = self.push_tray(ship_lane, SrBlockKind::Spike);
                        self.callout = Some((
                            tr(uk, "SPIKE HIT!  Chain x1", "ШИП!  Комбо скинуто").into(),
                            0.80,
                        ));
                    } else {
                        self.streak_hits += 1;
                        if self.streak_hits % 6 == 0 {
                            self.chain = (self.chain + 1).min(8);
                        }
                        self.score += 15 * self.chain;
                        self.hi_score = self.hi_score.max(self.score);
                        if self.push_tray(ship_lane, b.kind) {
                            self.chain = 1;
                            self.streak_hits = 0;
                            self.callout = Some((
                                tr(uk, "COLUMN OVERFILL!", "ПЕРЕПОВНЕННЯ!").into(),
                                0.85,
                            ));
                        }
                    }
                    continue;
                }
                if b.z > -0.08 {
                    rem_blocks.push(b);
                }
            }
            self.blocks = rem_blocks;

            if let Some((cnt, base_pts)) = self.poll_cash_in(dt, false) {
                let gained = base_pts * self.chain;
                self.score += gained;
                self.hi_score = self.hi_score.max(self.score);
                self.chain = (self.chain + 1).min(8);
                self.callout = Some((
                    format!("{} x{}  +{}", tr(uk, "CLUSTER", "КЛАСТЕР"), cnt, gained),
                    0.95,
                ));
            }
        } else {
            self.live_kick *= 0.85;
            for dst in &mut self.live_bars {
                *dst *= 0.88;
            }
        }

        PluginAction::Continue
    }

    fn render(
        &self,
        ui: &mut UiEngine,
        t: ThemeColors,
        header: &HeaderState<'_>,
        uk: bool,
    ) {
        match self.menu {
            SrMenu::Launch { sel } => {
                let s = self.launch_pos_ms / 1000;
                let cont_badge = format!("{:02}:{:02}", s / 60, s % 60);
                let rows = [
                    if self.hi_score > 0 {
                        PluginMenuRow::option(
                            tr(uk, "Start song from beginning", "Почати трек спочатку"),
                            &format!("Hi: {}", self.hi_score),
                        )
                    } else {
                        PluginMenuRow::action(tr(
                            uk,
                            "Start song from beginning",
                            "Почати трек спочатку",
                        ))
                    },
                    PluginMenuRow::option(tr(uk, "Continue", "Продовжити"), &cont_badge),
                ];
                render_native_plugin_menu(ui, t, header, "Sound Racer", &rows, sel);
                return;
            }
            SrMenu::Main { sel } => {
                let rows = [
                    PluginMenuRow::action(tr(uk, "Continue", "Продовжити")),
                    PluginMenuRow::action(tr(
                        uk,
                        "Start song from beginning",
                        "Почати трек спочатку",
                    )),
                    PluginMenuRow::action(tr(uk, "Next Track", "Наступний трек")),
                    PluginMenuRow::action(tr(uk, "Previous Track", "Попередній трек")),
                    PluginMenuRow::submenu(tr(uk, "Options", "Налаштування")),
                    PluginMenuRow::action(tr(uk, "Exit", "Вихід")),
                ];
                render_native_plugin_menu(ui, t, header, "Sound Racer", &rows, sel);
                return;
            }
            SrMenu::Options { sel } => {
                let mode_lbl = if self.cluster_mode {
                    tr(uk, "Cluster (3-Tone)", "Кластери")
                } else {
                    tr(uk, "Mono", "Моно")
                };
                let diff_lbl = match self.difficulty {
                    0 => tr(uk, "Chill", "Легка"),
                    1 => tr(uk, "Normal", "Звичайна"),
                    _ => tr(uk, "Pro", "Профі"),
                };
                let hi_lbl = if self.hi_score > 0 {
                    self.hi_score.to_string()
                } else {
                    String::from("—")
                };
                let rows = [
                    PluginMenuRow::option(tr(uk, "Game Mode", "Режим гри"), mode_lbl),
                    PluginMenuRow::option(tr(uk, "Difficulty", "Складність"), diff_lbl),
                    PluginMenuRow::option(
                        tr(uk, "Reset Track Hi-Score", "Скинути рекорд треку"),
                        &hi_lbl,
                    ),
                ];
                render_native_plugin_menu(
                    ui,
                    t,
                    header,
                    tr(uk, "Options", "Налаштування"),
                    &rows,
                    sel,
                );
                if let Some((ref msg, _)) = self.splash {
                    render_splash_pill(ui, t, msg);
                }
                return;
            }
            SrMenu::None => {}
        }

        ui.plugin_begin(t, t.bg_rgb, Some(header));

        // Top Live Track Ribbon (`y = 132..170`, 1:1 with `plugins/sound-racer/src/lib.rs`)
        if !self.track_display.is_empty() {
            ui.plugin_draw_text(
                SR_VX,
                148.0,
                &self.track_display,
                25.0,
                2,
                0,
                t.fg_rgb,
            );
            let ps = self.cur_pos_ms / 1000;
            let ds = self.cur_dur_ms / 1000;
            let time_lbl = format!("{:02}:{:02} / {:02}:{:02}", ps / 60, ps % 60, ds / 60, ds % 60);
            ui.plugin_draw_text(992.0, 142.0, &time_lbl, 22.0, 1, 2, t.fg2_rgb);
            let prog = if self.cur_dur_ms > 0 {
                (self.cur_pos_ms as f32 / self.cur_dur_ms as f32).clamp(0.0, 1.0)
            } else {
                0.0
            };
            ui.plugin_draw_progress_bar(t, 752.0, 158.0, 240.0, 10.0, prog);
        } else {
            ui.plugin_draw_text(
                SR_VX,
                148.0,
                tr(
                    uk,
                    "No track playing  —  Press A to start music",
                    "Музика на паузі  —  Натисни A, щоб увімкнути трек",
                ),
                25.0,
                2,
                0,
                t.fg2_rgb,
            );
        }

        // Main 3D Highway Viewport (`700 x 562`)
        ui.plugin_stroke_rect(SR_VX, SR_VY, SR_VW, SR_VH, 10.0, 3.5, t.fg1_rgb);
        let (vp_x, vp_y, _) = Self::project_road(1.0, 0.0, self.road_curve, self.intensity);

        let sun_r = 16.0 + self.live_kick * 20.0;
        ui.plugin_fill_circle(vp_x, vp_y - 10.0, sun_r, t.fg1_rgb);
        ui.plugin_fill_circle(vp_x, vp_y - 10.0, sun_r * 0.56, t.bg_rgb);

        let sky_span = 460.0f32;
        let bar_step = sky_span / 16.0;
        let bar_w = bar_step - 6.0;
        for (i, &lvl) in self.live_bars.iter().enumerate() {
            let bx = (vp_x - sky_span * 0.5 + (i as f32) * bar_step)
                .clamp(SR_VX + 10.0, SR_VX + SR_VW - bar_w - 10.0);
            let bh = (4.0 + lvl * 54.0).min(vp_y - SR_VY - 10.0).max(3.0);
            let shade = if lvl > 0.62 {
                t.fg_rgb
            } else if lvl > 0.30 {
                t.fg2_rgb
            } else {
                t.fg1_rgb
            };
            ui.plugin_fill_rect(bx, vp_y - bh, bar_w, bh, 0.0, shade);
        }

        ui.plugin_draw_line(SR_VX + 8.0, vp_y, SR_VX + SR_VW - 8.0, vp_y, 2.0, t.fg1_rgb);

        for i in 0..9 {
            let z = ((i as f32 + self.scroll_phase) / 9.0).fract();
            let (lx, ly, sc) = Self::project_road(z, -1.5, self.road_curve, self.intensity);
            let (rx, ry, _) = Self::project_road(z, 1.5, self.road_curve, self.intensity);
            ui.plugin_draw_line(lx, ly, rx, ry, (2.0 * sc).max(1.0), t.fg1_rgb);
        }

        for &lane_edge in &[-1.5f32, -0.5, 0.5, 1.5] {
            let is_outer = lane_edge.abs() > 1.0;
            let color = if is_outer { t.fg_rgb } else { t.fg2_rgb };
            let base_w = if is_outer { 4.8 } else { 2.2 };
            let (mut px, mut py, mut psc) =
                Self::project_road(0.0, lane_edge, self.road_curve, self.intensity);
            for s in 1..=8 {
                let z1 = s as f32 / 8.0;
                let (nx, ny, nsc) =
                    Self::project_road(z1, lane_edge, self.road_curve, self.intensity);
                ui.plugin_draw_line(px, py, nx, ny, (base_w * psc).max(1.3), color);
                px = nx;
                py = ny;
                psc = nsc;
            }
        }

        let mut sorted = self.blocks.clone();
        sorted.sort_by(|a, b| b.z.partial_cmp(&a.z).unwrap_or(std::cmp::Ordering::Equal));
        for b in &sorted {
            let (bx, by, sc) = Self::project_road(
                b.z.clamp(0.0, 1.0),
                b.lane as f32 - 1.0,
                self.road_curve,
                self.intensity,
            );
            Self::draw_highway_block(ui, t, bx, by, sc, b.kind);
        }

        // Player Hovercraft Ship (`z = 0.0`)
        let (ship_x, ship_ground_y, _) =
            Self::project_road(0.0, self.ship_pos - 1.0, self.road_curve, self.intensity);
        let jump_frac = if self.jump_rem > 0.0 {
            let u = 1.0 - self.jump_rem / 0.36;
            (4.0 * u * (1.0 - u)).clamp(0.0, 1.0)
        } else {
            0.0
        };
        let jump_px = jump_frac * 58.0;
        if jump_px > 2.0 {
            let sh_w = 88.0 * (1.0 - jump_frac * 0.35);
            ui.plugin_fill_rect(
                ship_x - sh_w * 0.5,
                ship_ground_y + 6.0,
                sh_w,
                12.0,
                5.0,
                t.fg1_rgb,
            );
        }
        let sy = ship_ground_y - jump_px;
        ui.plugin_draw_line(ship_x, sy - 26.0, ship_x - 46.0, sy + 14.0, 4.5, t.fg_rgb);
        ui.plugin_draw_line(ship_x, sy - 26.0, ship_x + 46.0, sy + 14.0, 4.5, t.fg_rgb);
        ui.plugin_draw_line(ship_x - 46.0, sy + 14.0, ship_x + 46.0, sy + 14.0, 4.5, t.fg_rgb);
        ui.plugin_fill_rect(ship_x - 20.0, sy - 8.0, 40.0, 22.0, 5.0, t.fg_rgb);
        ui.plugin_fill_circle(ship_x, sy + 14.0, 5.5 + self.live_kick * 6.0, t.fg2_rgb);
        ui.plugin_fill_rect(ship_x - 4.0, sy - 1.0, 8.0, 8.0, 2.0, t.bg_rgb);

        if let Some((ref txt, _)) = self.callout {
            ui.plugin_draw_text(
                SR_VX + SR_VW * 0.5,
                SR_VY + 34.0,
                txt,
                28.0,
                3,
                1,
                t.fg_rgb,
            );
        }

        // Right Sidebar (`x = 752..992`)
        let rx = 752.0f32;
        let rw = 240.0f32;
        ui.plugin_stroke_rect(rx, SR_VY, rw, 134.0, 10.0, 3.5, t.fg1_rgb);
        let hdr_txt = format!(
            "{}  ×{}",
            tr(uk, "SCORE", "РАХУНОК"),
            self.chain
        );
        ui.plugin_draw_text(rx + rw * 0.5, SR_VY + 26.0, &hdr_txt, 22.0, 3, 1, t.fg2_rgb);
        ui.plugin_draw_text(
            rx + rw * 0.5,
            SR_VY + 68.0,
            &self.score.to_string(),
            38.0,
            3,
            1,
            t.fg_rgb,
        );
        let hi_txt = format!("Hi: {}", self.hi_score);
        ui.plugin_draw_text(rx + rw * 0.5, SR_VY + 110.0, &hi_txt, 20.0, 2, 1, t.fg2_rgb);

        // 3 x 7 Collector Matrix
        let tray_y = SR_VY + 148.0;
        let tray_h = 292.0f32;
        ui.plugin_stroke_rect(rx, tray_y, rw, tray_h, 10.0, 3.5, t.fg1_rgb);
        ui.plugin_draw_text(
            rx + rw * 0.5,
            tray_y + 22.0,
            tr(uk, "COLLECTOR (3+)", "КОЛЕКТОР (3+)"),
            20.0,
            2,
            1,
            t.fg2_rgb,
        );

        let gx = rx + 18.0;
        let gy = tray_y + 42.0;
        let gw = rw - 36.0;
        let gh = tray_h - 58.0;
        let cw = gw / 3.0;
        let ch = gh / 7.0;
        ui.plugin_stroke_rect(gx, gy, gw, gh, 4.0, 2.0, t.fg1_rgb);
        for c in 1..3 {
            let lx = gx + (c as f32) * cw;
            ui.plugin_draw_line(lx, gy, lx, gy + gh, 1.5, t.fg1_rgb);
        }
        for r in 1..7 {
            let ly = gy + (r as f32) * ch;
            ui.plugin_draw_line(gx, ly, gx + gw, ly, 1.5, t.fg1_rgb);
        }
        let marker_x = gx + (self.lane as f32) * cw + cw * 0.22;
        ui.plugin_fill_rect(marker_x, tray_y + tray_h - 11.0, cw * 0.56, 5.0, 2.0, t.fg_rgb);

        let flash_on = (self.now_ms / 70) % 2 == 0;
        for col in 0..3 {
            let cx = gx + (col as f32) * cw + 3.0;
            let cell_w = cw - 6.0;
            for row in 0..7 {
                let v = self.tray[col][row];
                if v == 0 {
                    continue;
                }
                let cy = gy + ((6 - row) as f32) * ch + 2.5;
                let cell_h = ch - 5.0;
                if self.matched_mask[col][row] && flash_on {
                    ui.plugin_fill_rect(cx, cy, cell_w, cell_h, 3.0, t.fg_rgb);
                    ui.plugin_fill_rect(
                        cx + 4.0,
                        cy + 4.0,
                        cell_w - 8.0,
                        cell_h - 8.0,
                        1.5,
                        t.bg_rgb,
                    );
                } else {
                    match v {
                        1 => ui.plugin_fill_rect(cx, cy, cell_w, cell_h, 3.0, t.fg_rgb),
                        2 => ui.plugin_fill_rect(cx, cy, cell_w, cell_h, 3.0, t.fg2_rgb),
                        3 => {
                            ui.plugin_fill_rect(cx, cy, cell_w, cell_h, 3.0, t.fg1_rgb);
                            ui.plugin_stroke_rect(cx, cy, cell_w, cell_h, 3.0, 2.0, t.fg_rgb);
                        }
                        _ => {
                            ui.plugin_draw_line(
                                cx + 4.0,
                                cy + 3.0,
                                cx + cell_w - 4.0,
                                cy + cell_h - 3.0,
                                2.2,
                                t.fg_rgb,
                            );
                            ui.plugin_draw_line(
                                cx + cell_w - 4.0,
                                cy + 3.0,
                                cx + 4.0,
                                cy + cell_h - 3.0,
                                2.2,
                                t.fg_rgb,
                            );
                        }
                    }
                }
            }
        }

        let bot_y = tray_y + tray_h + 14.0;
        let bot_h = SR_VY + SR_VH - bot_y;
        ui.plugin_stroke_rect(rx, bot_y, rw, bot_h, 10.0, 3.5, t.fg1_rgb);
        let slope_lbl = if self.intensity > 0.68 {
            tr(uk, "Slope: ▼ RUSH!", "Схил: ▼ ДРАЙВ!")
        } else if self.intensity > 0.36 {
            tr(uk, "Slope: ► Groove", "Схил: ► Ритм")
        } else {
            tr(uk, "Slope: ▲ Climb", "Схил: ▲ Підйом")
        };
        ui.plugin_draw_text(rx + rw * 0.5, bot_y + 26.0, slope_lbl, 22.0, 3, 1, t.fg_rgb);
        ui.plugin_draw_text(
            rx + rw * 0.5,
            bot_y + 58.0,
            tr(uk, "←/→: Lane  •  A: Jump", "←/→: Смуга  •  A: Стрибок"),
            19.0,
            1,
            1,
            t.fg2_rgb,
        );
        ui.plugin_draw_text(
            rx + rw * 0.5,
            bot_y + 84.0,
            tr(uk, "Start / Menu: Pause", "Start / Menu: Пауза"),
            19.0,
            1,
            1,
            t.fg2_rgb,
        );

        if let Some((ref msg, _)) = self.splash {
            render_splash_pill(ui, t, msg);
        }
    }
}
