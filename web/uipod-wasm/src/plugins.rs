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
    BeatStack,
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

fn flush_plugin_splash_to_hud(ui: &mut UiEngine, splash: &mut Option<(String, f32)>) {
    if let Some((msg, _)) = splash.take() {
        let now_ms = ui.hud_clock_ms();
        ui.hud_toast("plugin_toast", &msg, 1400, now_ms);
    }
}

#[derive(Clone, Copy)]
pub struct AudioSnapshot<'a> {
    pub playing: bool,
    pub track_title: &'a str,
    pub pos_ms: u32,
    pub dur_ms: u32,
    pub sample_rate: u32,
}

pub enum ActivePlugin {
    BeatStack(BeatStackPlugin),
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
            PluginId::BeatStack => Self::BeatStack(BeatStackPlugin::new(pos_ms as u64)),
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
            Self::BeatStack(_) => if uk { "Біт Стек" } else { "Beat Stack" },
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

    /// Returns `true` when the active plugin uses `B` directly as an immediate gameplay button
    /// (suppressing `650 ms` Hold-`B` Home exit and `B`-on-release deferral outside Pause Menu).
    pub fn captures_b_button(&self) -> bool {
        matches!(self, Self::BeatStack(g) if !g.menu_open)
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
            Self::BeatStack(g) => g.update(dt, pressed, held, pcm, audio, uk),
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
        win_w: u32,
        win_h: u32,
    ) {
        match self {
            Self::BeatStack(g) => {
                flush_plugin_splash_to_hud(ui, &mut g.splash);
                g.render(ui, theme, header, uk, win_w, win_h);
            }
            Self::MusicQuiz(g) => g.render(ui, theme, header, uk),
            Self::SoundRacer(g) => {
                flush_plugin_splash_to_hud(ui, &mut g.splash);
                g.render(ui, theme, header, uk);
            }
            Self::Tetris(g) => {
                flush_plugin_splash_to_hud(ui, &mut g.splash);
                g.render(ui, theme, header, uk);
            }
            Self::Snake(g) => {
                flush_plugin_splash_to_hud(ui, &mut g.splash);
                g.render(ui, theme, header, uk);
            }
            Self::Game2048(g) => {
                flush_plugin_splash_to_hud(ui, &mut g.splash);
                g.render(ui, theme, header, uk);
            }
            Self::BrickBreaker(g) => {
                flush_plugin_splash_to_hud(ui, &mut g.splash);
                g.render(ui, theme, header, uk);
            }
            Self::Stopwatch(a) => {
                ui.plugin_begin(theme, theme.bg_rgb, Some(header));
                a.render(ui, theme, uk);
            }
            Self::TextViewer(v) => {
                flush_plugin_splash_to_hud(ui, &mut v.splash);
                v.render(ui, theme, header, uk);
            }
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
                let _ = &self.splash;
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

        let _ = &self.splash;
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
                let _ = &self.splash;
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

        let _ = &self.splash;
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
                let _ = &self.splash;
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

        let _ = &self.splash;
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
                let _ = &self.splash;
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

        let _ = &self.splash;
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

        let _ = &self.splash;
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
            let live_intel = uipod_track_intel::read_live_frame();
            if live_intel.status == 1 {
                self.live_kick = self.live_kick.max(live_intel.kick_impulse);
                for b in 0..16 {
                    let k0 = b * 4;
                    let sp = live_intel.spectrum_64[k0]
                        .max(live_intel.spectrum_64[k0 + 1])
                        .max(live_intel.spectrum_64[k0 + 2])
                        .max(live_intel.spectrum_64[k0 + 3]);
                    let sv = (sp as f32) * (1.0 / 255.0);
                    self.live_bars[b] = self.live_bars[b].max(sv * 0.85);
                }
                if live_intel.has_analysis != 0 {
                    self.intensity =
                        (self.intensity * 0.90 + live_intel.structural_energy * 0.10).clamp(0.08, 1.0);
                }
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
                let _ = &self.splash;
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

        let _ = &self.splash;
    }
}


/* ============================================================================
 * 8. BEAT STACK — 1:1 with `plugins/beat-stack/src/{lib,rhythm}.rs`
 * ========================================================================== */

const BS_COLS: usize = 12;
const BS_ROWS: usize = 7;
const BS_MAX_CRANES: usize = 4;
const BS_SYNC_OFFSETS_MS: [i32; 5] = [0, 25, 50, 100, 150];
const BS_MIN_ARM_BEATS: f32 = 0.48;
const BS_MIN_ARM_SEC: f32 = 0.15;
const BS_ENV_RATE_HZ: f32 = 100.0;
const BS_BIN_SEC: f32 = 1.0 / BS_ENV_RATE_HZ;
const BS_RING_BINS: usize = 600;
const BS_EST_WINDOW: usize = 520;

struct BsLayoutSpec {
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
    crane_zone_ratio: f32,
    floor_plate_ratio: f32,
    rail_top_ratio: f32,
    rail_h_ratio: f32,
    crate_inset_ratio: f32,
    crate_radius_ratio: f32,
    min_tile: f32,
    max_tile: f32,
    font_banner_lg: f32,
    font_banner_sm: f32,
    font_hud_val: f32,
    font_hud_lbl: f32,
    font_callout: f32,
    font_hint_main: f32,
    font_hint_sub: f32,
}

const BS_SPEC: BsLayoutSpec = BsLayoutSpec {
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

#[derive(Clone, Copy)]
struct BsComputedLayout {
    tile: f32,
    shaft_x: f32,
    shaft_y: f32,
    shaft_w: f32,
    shaft_h: f32,
    floor_plate_h: f32,
    grid_x: f32,
    grid_y: f32,
    grid_w: f32,
    grid_h: f32,
    hud_x: f32,
    hud_y: f32,
    hud_w: f32,
    hud_h: f32,
}

impl BsComputedLayout {
    fn compute(win_w: f32, win_h: f32) -> Self {
        let top_y = CONTENT_TOP_Y + BS_SPEC.outer_top_offset;
        let avail_w = (win_w - BS_SPEC.outer_pad_x * 2.0).max(280.0);
        let avail_h = (win_h
            - top_y
            - BS_SPEC.outer_bottom_pad
            - BS_SPEC.hud_gap_y
            - BS_SPEC.hud_bar_h)
            .max(220.0);
        let total_w_tiles = (BS_COLS as f32) + 0.25;
        let total_h_tiles =
            (BS_ROWS as f32) + BS_SPEC.crane_zone_ratio + BS_SPEC.floor_plate_ratio;

        let tile = (avail_w / total_w_tiles)
            .min(avail_h / total_h_tiles)
            .floor()
            .clamp(BS_SPEC.min_tile, BS_SPEC.max_tile);

        let crane_zone_h = (tile * BS_SPEC.crane_zone_ratio).round();
        let floor_plate_h = (tile * BS_SPEC.floor_plate_ratio).round().max(10.0);
        let grid_w = tile * (BS_COLS as f32);
        let grid_h = tile * (BS_ROWS as f32);
        let wall_pad = BS_SPEC.wall_stroke;

        let shaft_w = grid_w + wall_pad * 2.0;
        let shaft_h = crane_zone_h + grid_h + floor_plate_h;
        let total_block_h = shaft_h + BS_SPEC.hud_gap_y + BS_SPEC.hud_bar_h;

        let shaft_x = ((win_w - shaft_w) * 0.5).floor();
        let extra_y = ((win_h - top_y - BS_SPEC.outer_bottom_pad - total_block_h) * 0.5).max(0.0);
        let shaft_y = (top_y + extra_y).floor();
        let grid_x = shaft_x + wall_pad;
        let grid_y = shaft_y + crane_zone_h;
        let hud_y = shaft_y + shaft_h + BS_SPEC.hud_gap_y;

        Self {
            tile,
            shaft_x,
            shaft_y,
            shaft_w,
            shaft_h,
            floor_plate_h,
            grid_x,
            grid_y,
            grid_w,
            grid_h,
            hud_x: shaft_x,
            hud_y,
            hud_w: shaft_w,
            hud_h: BS_SPEC.hud_bar_h,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum BsCrateKind {
    Light = 1,
    Checker = 2,
    Medium = 3,
    Heavy = 4,
}

#[derive(Clone, Copy)]
struct BsGroundedCrate {
    kind: BsCrateKind,
    armed_square: bool,
    armed_row: bool,
    armed_sec: f32,
    armed_beats: f32,
    settle_px_frac: u8,
}

impl BsGroundedCrate {
    const fn new(kind: BsCrateKind) -> Self {
        Self {
            kind,
            armed_square: false,
            armed_row: false,
            armed_sec: 0.0,
            armed_beats: 0.0,
            settle_px_frac: 0,
        }
    }
    const fn is_armed(self) -> bool {
        self.armed_square || self.armed_row
    }
    fn ready_to_detonate(self) -> bool {
        self.is_armed()
            && self.armed_beats >= BS_MIN_ARM_BEATS
            && self.armed_sec >= BS_MIN_ARM_SEC
    }
}

#[derive(Clone, Copy)]
struct BsFallingCrate {
    col: usize,
    row: usize,
    kind: BsCrateKind,
    snap_anim: f32,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum BsCraneMode {
    Carrying,
    Telegraph,
    EmptyReturn,
}

#[derive(Clone, Copy)]
struct BsCrane {
    active: bool,
    col: i32,
    vis_col: f32,
    dir: i32,
    target_col: usize,
    cargo: Option<BsCrateKind>,
    mode: BsCraneMode,
    telegraph_beats: f32,
    rail_steps_loaded: u8,
    open_flash: f32,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum BsLoseReason {
    Crushed,
    ShaftJammed,
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum BsWorkerAction {
    Idle,
    Walking { from_col: i32, progress: f32 },
    Pushing { dir: i32, progress: f32 },
    Jumping {
        from_col: f32,
        from_row: f32,
        to_col: i32,
        to_row: i32,
        progress: f32,
        blocked_wall: bool,
        steered_apex: bool,
    },
    Falling { vis_row: f32 },
}

#[derive(Clone, Copy)]
struct BsBurst {
    x: f32,
    y: f32,
    vx: f32,
    vy: f32,
    life: f32,
    shade: u8,
}

struct BsBeatClock {
    raw_beats: f64,
    comp_beats: f64,
    audio_latency_sec: f64,
    live_bpm: f32,
    tempo_locked: bool,
    parity_shift: u64,

    hp40: f32,
    lp140_a: f32,
    lp140_b: f32,
    kick_env: f32,
    prev_kick_env: f32,
    flux_ema: f32,
    since_onset: f32,

    bin_accum_sec: f32,
    onset_ring: [f32; BS_RING_BINS],
    kick_ring: [f32; BS_RING_BINS],
    ring_pos: usize,
    ring_len: usize,
    bins_since_est: usize,

    silence_sec: f32,
    pll_trim: f32,
    pending_jumps: u8,
    last_jump_cand: f32,
    initialized: bool,
}

impl BsBeatClock {
    fn new(latency_sec: f64) -> Self {
        Self {
            raw_beats: 0.0,
            comp_beats: 0.0,
            audio_latency_sec: latency_sec.clamp(0.0, 0.30),
            live_bpm: 120.0,
            tempo_locked: false,
            parity_shift: 0,
            hp40: 0.0,
            lp140_a: 0.0,
            lp140_b: 0.0,
            kick_env: 0.0,
            prev_kick_env: 0.0,
            flux_ema: 0.02,
            since_onset: 1.0,
            bin_accum_sec: 0.0,
            onset_ring: [0.0; BS_RING_BINS],
            kick_ring: [0.0; BS_RING_BINS],
            ring_pos: 0,
            ring_len: 0,
            bins_since_est: 0,
            silence_sec: 2.0,
            pll_trim: 0.0,
            pending_jumps: 0,
            last_jump_cand: 120.0,
            initialized: false,
        }
    }

    fn reset_full(&mut self) {
        self.raw_beats = 0.0;
        self.comp_beats = 0.0;
        self.live_bpm = 120.0;
        self.parity_shift = 0;
        self.silence_sec = 2.0;
        self.initialized = false;
        self.on_track_change();
    }

    fn on_track_change(&mut self) {
        self.tempo_locked = false;
        self.hp40 = 0.0;
        self.lp140_a = 0.0;
        self.lp140_b = 0.0;
        self.kick_env = 0.0;
        self.prev_kick_env = 0.0;
        self.flux_ema = 0.02;
        self.since_onset = 1.0;
        self.bin_accum_sec = 0.0;
        self.ring_pos = 0;
        self.ring_len = 0;
        self.bins_since_est = 0;
        self.pll_trim = 0.0;
        self.pending_jumps = 0;
    }

    fn fold_bpm(mut bpm: f32) -> f32 {
        if !bpm.is_finite() || bpm <= 0.0 {
            return 120.0;
        }
        bpm = bpm.clamp(30.0, 360.0);
        while bpm < 75.0 {
            bpm *= 2.0;
        }
        while bpm > 180.0 {
            bpm *= 0.5;
        }
        bpm.clamp(75.0, 180.0)
    }

    fn ingest_audio(&mut self, dt: f32, pcm: &[i16], sample_rate: u32, apu_pulse: f32) -> bool {
        self.since_onset += dt;
        let n_frames = pcm.len() / 2;
        let sr = sample_rate.clamp(16_000, 96_000) as f32;
        const INV_I16: f32 = 1.0 / 32768.0;

        let mut full_sq = 0.0f32;
        if n_frames > 0 {
            let st = (n_frames / 128).max(1);
            let mut cnt = 0usize;
            let mut i = 0usize;
            while i < n_frames {
                let m = ((pcm[i * 2] as f32) + (pcm[i * 2 + 1] as f32)) * (0.5 * INV_I16);
                full_sq += m * m;
                cnt += 1;
                i += st;
            }
            full_sq /= cnt.max(1) as f32;
        }
        if full_sq.sqrt() < 0.004 && apu_pulse < 0.03 {
            self.silence_sec = (self.silence_sec + dt).min(10.0);
        } else {
            self.silence_sec = 0.0;
        }

        self.bin_accum_sec += dt;
        let bins_due = (self.bin_accum_sec / BS_BIN_SEC).floor() as usize;
        if bins_due == 0 {
            return false;
        }
        let k = bins_due.min(8);
        self.bin_accum_sec =
            (self.bin_accum_sec - (bins_due as f32) * BS_BIN_SEC).clamp(0.0, BS_BIN_SEC);

        let a_hp40 = (1.0 - (-2.0 * core::f32::consts::PI * 40.0 / sr).exp()).clamp(0.001, 0.25);
        let a_lp140 =
            (1.0 - (-2.0 * core::f32::consts::PI * 140.0 / sr).exp()).clamp(0.003, 0.45);

        let hop_frames = ((sr * BS_BIN_SEC).round() as usize).max(32);
        let tail_frames = (k * hop_frames).min(n_frames);
        let start_frame = n_frames.saturating_sub(tail_frames);
        let apu_clamped = apu_pulse.clamp(0.0, 1.0);

        let mut onset = false;
        for b in 0..k {
            let k_rms = if tail_frames >= k && n_frames > 0 {
                let f0 = start_frame + (b * tail_frames) / k;
                let f1 = start_frame + ((b + 1) * tail_frames) / k;
                let mut sq = 0.0f32;
                let cnt = f1.saturating_sub(f0).max(1);
                for f in f0..f1 {
                    let m = ((pcm[f * 2] as f32) + (pcm[f * 2 + 1] as f32)) * (0.5 * INV_I16);
                    self.hp40 += a_hp40 * (m - self.hp40);
                    let hp = m - self.hp40;
                    self.lp140_a += a_lp140 * (hp - self.lp140_a);
                    self.lp140_b += a_lp140 * (self.lp140_a - self.lp140_b);
                    sq += self.lp140_b * self.lp140_b;
                }
                (sq / (cnt as f32)).sqrt()
            } else {
                0.0
            };

            let comb_k = k_rms.max(apu_clamped * 0.25);
            self.kick_env = self.kick_env * 0.35 + comb_k * 0.65;
            let k_flux = (self.kick_env - self.prev_kick_env).max(0.0);
            self.prev_kick_env = self.kick_env;

            self.onset_ring[self.ring_pos] = k_flux;
            self.kick_ring[self.ring_pos] = self.kick_env;
            self.ring_pos = (self.ring_pos + 1) % BS_RING_BINS;
            if self.ring_len < BS_RING_BINS {
                self.ring_len += 1;
            }
            self.bins_since_est += 1;

            let thr = (self.flux_ema * 1.65).max(0.006);
            if k_flux > thr && self.since_onset >= 0.24 {
                onset = true;
                self.since_onset = 0.0;
            }
            self.flux_ema += (k_flux - self.flux_ema) * 0.08;
        }

        if self.silence_sec < 1.0 && self.ring_len >= 160 && self.bins_since_est >= 20 {
            self.bins_since_est = 0;
            self.estimate_tempo_and_phase();
        }
        onset
    }

    fn estimate_tempo_and_phase(&mut self) {
        let w = self.ring_len.min(BS_EST_WINDOW);
        if w < 160 {
            return;
        }
        let mut onset_win = [0.0f32; BS_EST_WINDOW];
        let mut kick_win = [0.0f32; BS_EST_WINDOW];
        let oldest = (self.ring_pos + BS_RING_BINS - w) % BS_RING_BINS;
        let mut sum = 0.0f32;
        let mut max_v = 0.0f32;
        for i in 0..w {
            let idx = (oldest + i) % BS_RING_BINS;
            let v = self.onset_ring[idx];
            onset_win[i] = v;
            kick_win[i] = self.kick_ring[idx];
            sum += v;
            if v > max_v {
                max_v = v;
            }
        }
        let mean = sum / (w as f32);
        if max_v < 1e-4 || max_v / (mean + 1e-6) < 1.75 {
            return;
        }

        let mut centered = [0.0f32; BS_EST_WINDOW];
        let mut var_sum = 1e-12f32;
        for i in 0..w {
            let sm = if i > 0 && i + 1 < w {
                0.25 * onset_win[i - 1] + 0.50 * onset_win[i] + 0.25 * onset_win[i + 1]
            } else {
                onset_win[i]
            };
            let c = sm - mean;
            centered[i] = c;
            var_sum += c * c;
        }

        let min_lag = 33usize;
        let max_lag = 80usize;
        let corr_max = (max_lag * 2 + 4).min(w - 1);
        if min_lag >= corr_max {
            return;
        }

        let mut acf = [0.0f32; 170];
        let acf_top = corr_max.min(acf.len() - 1);
        for lag in (min_lag / 2)..=acf_top {
            let cnt = w - lag;
            let mut dot = 0.0f32;
            for i in 0..cnt {
                dot += centered[i] * centered[i + lag];
            }
            let sc = (w as f32) / ((cnt as f32) * var_sum);
            acf[lag] = (dot * sc).clamp(-1.0, 1.0);
        }

        let mut best_lag = 50usize;
        let mut best_sc = f32::NEG_INFINITY;
        for lag in min_lag..=max_lag.min(acf_top) {
            let r1 = acf[lag].max(0.0);
            let r2 = if lag * 2 <= acf_top {
                acf[lag * 2].max(0.0)
            } else {
                r1
            };
            let r4 = if lag * 4 < w && lag * 4 <= acf_top {
                acf[lag * 4].max(0.0)
            } else {
                r2
            };
            let sc = 0.58 * r1 + 0.34 * r2 + 0.08 * r4;
            if sc > best_sc {
                best_sc = sc;
                best_lag = lag;
            }
        }
        if best_sc < 0.08 {
            return;
        }

        if best_lag >= 66 && best_lag / 2 >= min_lag {
            let half = best_lag / 2;
            let r_half = acf[half].max(acf[(half + 1).min(acf_top)]);
            if r_half >= 0.72 * acf[best_lag].max(0.05) {
                best_lag = if acf[(half + 1).min(acf_top)] > acf[half] {
                    half + 1
                } else {
                    half
                };
            }
        }

        let interp_pk = |idx: usize| -> f32 {
            if idx > 1 && idx + 1 <= acf_top {
                let ym = acf[idx - 1];
                let y0 = acf[idx];
                let yp = acf[idx + 1];
                let den = ym - 2.0 * y0 + yp;
                if den.abs() > 1e-6 {
                    (idx as f32) + (0.5 * (ym - yp) / den).clamp(-0.5, 0.5)
                } else {
                    idx as f32
                }
            } else {
                idx as f32
            }
        };

        let lag1 = interp_pk(best_lag);
        let mut l2 = (best_lag * 2).min(acf_top);
        if l2 > 2 && l2 + 1 <= acf_top {
            if acf[l2 - 1] > acf[l2] && acf[l2 - 1] >= acf[l2 + 1] {
                l2 -= 1;
            } else if acf[l2 + 1] > acf[l2] {
                l2 += 1;
            }
        }
        let exact_lag = if acf[best_lag] < 0.12 && l2 <= acf_top && acf[l2] > 0.18 {
            interp_pk(l2) * 0.5
        } else if l2 <= acf_top && acf[l2] > 0.25 {
            let l2h = interp_pk(l2) * 0.5;
            if (lag1 - l2h).abs() < 0.85 {
                0.40 * lag1 + 0.60 * l2h
            } else {
                lag1
            }
        } else {
            lag1
        };

        let det_bpm = Self::fold_bpm(BS_ENV_RATE_HZ * 60.0 / exact_lag.max(20.0));
        if !self.tempo_locked {
            self.live_bpm = det_bpm;
            self.tempo_locked = true;
        } else {
            let diff = (det_bpm - self.live_bpm).abs();
            if diff > 5.0 {
                if (det_bpm - self.last_jump_cand).abs() < 3.0 {
                    self.pending_jumps = self.pending_jumps.saturating_add(1);
                } else {
                    self.pending_jumps = 1;
                    self.last_jump_cand = det_bpm;
                }
                if self.pending_jumps >= 2 {
                    self.live_bpm += (det_bpm - self.live_bpm) * 0.70;
                    self.pending_jumps = 0;
                }
            } else {
                self.pending_jumps = 0;
                self.live_bpm += (det_bpm - self.live_bpm) * 0.28;
            }
        }

        let period_bins = (BS_ENV_RATE_HZ * 60.0 / self.live_bpm).clamp(30.0, 90.0);
        let two_beat = (period_bins * 2.0).round() as usize;
        let newest = w - 1;
        let mut best_age = 0usize;
        let mut best_pe = f32::NEG_INFINITY;
        for age in 0..two_beat.max(1) {
            let mut sc = 0.0f32;
            let mut k = 0usize;
            loop {
                let back = age + ((k as f32) * period_bins * 2.0).round() as usize;
                if back >= w {
                    break;
                }
                let idx = newest - back;
                let mut es = onset_win[idx] + kick_win[idx] * 0.45;
                if idx > 0 {
                    es += 0.45 * onset_win[idx - 1];
                }
                if idx + 1 < w {
                    es += 0.45 * onset_win[idx + 1];
                }
                let wb = back + (period_bins.round() as usize);
                let ew = if wb < w {
                    onset_win[newest - wb] * 0.35
                } else {
                    0.0
                };
                sc += es + ew;
                k += 1;
            }
            if sc > best_pe {
                best_pe = sc;
                best_age = age;
            }
        }

        let lat_bins = (self.audio_latency_sec as f32) * BS_ENV_RATE_HZ;
        let target_2b = (((best_age as f32) - lat_bins) / period_bins).rem_euclid(2.0);
        let cur_floor = self.comp_beats.floor().max(0.0) as u64;
        let cur_par = ((cur_floor.wrapping_add(self.parity_shift)) & 1) as f32;
        let cur_frac = (self.comp_beats - self.comp_beats.floor()) as f32;
        let cur_2b = cur_par + cur_frac;

        let mut q_err = target_2b.rem_euclid(1.0) - cur_frac;
        if q_err > 0.5 {
            q_err -= 1.0;
        } else if q_err < -0.5 {
            q_err += 1.0;
        }
        self.pll_trim = (q_err * 0.36).clamp(-0.14, 0.14);

        let two_err = (target_2b - cur_2b).rem_euclid(2.0);
        if (0.65..=1.35).contains(&two_err) {
            self.parity_shift ^= 1;
        }
    }

    fn step(
        &mut self,
        dt: f32,
        playing: bool,
        pcm: &[i16],
        sample_rate: u32,
        apu_pulse: f32,
    ) -> (f32, f32, f32, bool, bool, bool, bool) {
        let dt = dt.clamp(0.0, 0.1);
        let prev_comp = self.comp_beats;

        if playing {
            let live_intel = uipod_track_intel::read_live_frame();
            if live_intel.status == 1 && live_intel.has_analysis != 0 {
                if let Some(rec) = uipod_track_intel::read_active_analysis() {
                    if rec.key_hash == live_intel.track_key_hash {
                        let folded = Self::fold_bpm(rec.bpm);
                        self.live_bpm = folded;
                        self.tempo_locked = true;
                        self.silence_sec = 0.0;
                        self.since_onset += dt;
                        let raw_onset =
                            if live_intel.kick_impulse.max(apu_pulse) >= 0.56 && self.since_onset >= 0.18
                            {
                                self.since_onset = 0.0;
                                true
                            } else {
                                false
                            };
                        let trim_ms = (self.audio_latency_sec * 1000.0).clamp(0.0, 300.0);
                        let target_b = (((live_intel.playhead_ms as f64)
                            - trim_ms
                            - (rec.first_beat_offset_ms as f64))
                            * (folded as f64 / 60_000.0))
                            .max(0.0);
                        let nom_step = (dt as f64) * (folded as f64 / 60.0);
                        if !self.initialized {
                            self.comp_beats = target_b;
                            self.raw_beats = target_b;
                            self.parity_shift = 0;
                        } else {
                            let err = target_b - self.comp_beats;
                            if err.abs() > 3.0 {
                                let tf = target_b.rem_euclid(2.0);
                                let cb = (self.comp_beats * 0.5).floor() * 2.0;
                                let mut nb = cb + tf;
                                if nb < self.comp_beats {
                                    nb += 2.0;
                                }
                                self.comp_beats = nb;
                                self.parity_shift = 0;
                            } else {
                                let rm = (1.0 + err * 1.8).clamp(0.84, 1.18);
                                self.comp_beats += nom_step * rm;
                            }
                            self.raw_beats = self.comp_beats;
                        }
                        let (fire_beat, fire_half) = if !self.initialized {
                            self.initialized = true;
                            (false, false)
                        } else {
                            (
                                (self.comp_beats.floor() as i64) > (prev_comp.floor() as i64),
                                ((self.comp_beats * 2.0).floor() as i64)
                                    > ((prev_comp * 2.0).floor() as i64),
                            )
                        };
                        let bf_floor = self.comp_beats.floor().max(0.0) as u64;
                        let beat_index = bf_floor.wrapping_add(self.parity_shift);
                        let fire_strong = fire_beat && (beat_index & 1) == 0;
                        let bf =
                            ((self.comp_beats - (bf_floor as f64)) as f32).clamp(0.0, 0.999_99);
                        let actual_step = (self.comp_beats - prev_comp).max(0.0) as f32;
                        return (
                            folded,
                            bf,
                            actual_step,
                            fire_beat,
                            fire_strong,
                            fire_half,
                            raw_onset,
                        );
                    }
                }
            }
        }

        let raw_onset = if playing {
            self.ingest_audio(dt, pcm, sample_rate, apu_pulse)
        } else {
            self.silence_sec = (self.silence_sec + dt).min(10.0);
            self.pll_trim *= (1.0 - dt * 6.0).clamp(0.0, 1.0);
            false
        };

        let fallback = !playing || self.silence_sec >= 1.8;
        if fallback {
            self.tempo_locked = false;
            self.live_bpm += (120.0 - self.live_bpm) * (dt * 3.0).clamp(0.0, 1.0);
        }
        let bpm = self.live_bpm.clamp(75.0, 180.0);
        self.pll_trim *= (1.0 - dt * 4.5).clamp(0.0, 1.0);
        let rate = if fallback {
            1.0
        } else {
            (1.0 + self.pll_trim).clamp(0.86, 1.14)
        };
        let step_b = (dt * bpm * rate / 60.0) as f64;
        self.raw_beats += step_b;

        let lat_b = if fallback {
            0.0
        } else {
            self.audio_latency_sec * (bpm as f64 / 60.0)
        };
        let desired = (self.raw_beats - lat_b).max(0.0);
        if !self.initialized {
            self.comp_beats = desired;
        } else if desired > self.comp_beats {
            self.comp_beats = desired;
        } else {
            self.comp_beats += step_b * 0.15;
        }

        let (fire_beat, fire_half) = if !self.initialized {
            self.initialized = true;
            (false, false)
        } else {
            (
                (self.comp_beats.floor() as i64) > (prev_comp.floor() as i64),
                ((self.comp_beats * 2.0).floor() as i64) > ((prev_comp * 2.0).floor() as i64),
            )
        };

        let bf_floor = self.comp_beats.floor().max(0.0) as u64;
        let beat_index = bf_floor.wrapping_add(self.parity_shift);
        let fire_strong = fire_beat && (beat_index & 1) == 0;
        let bf = ((self.comp_beats - (bf_floor as f64)) as f32).clamp(0.0, 0.999_99);
        let actual_step = (self.comp_beats - prev_comp).max(0.0) as f32;
        (bpm, bf, actual_step, fire_beat, fire_strong, fire_half, raw_onset)
    }
}

pub struct BeatStackPlugin {
    grid: [[Option<BsGroundedCrate>; BS_COLS]; BS_ROWS],
    falling: Vec<BsFallingCrate>,
    cranes: [BsCrane; BS_MAX_CRANES],
    worker_col: i32,
    worker_row: i32,
    worker_facing: i32,
    worker_action: BsWorkerAction,
    worker_walk_phase: u8,
    worker_danger: bool,

    score: u32,
    hi_score: u32,
    chain: u32,
    max_chain: u32,
    chain_active: bool,
    cleared_crates: u32,
    wave: u32,
    drops_done: u32,

    difficulty: u8, // 0 Chill, 1 Normal, 2 Overtime
    latency_idx: usize,
    clock: BsBeatClock,
    effective_bpm: f32,
    beat_frac: f32,
    bpm_pulse: f32,
    strong_pulse: f32,
    show_hint: bool,
    hint_pinned: bool,
    music_playing: bool,
    track_title: String,

    lose_reason: Option<BsLoseReason>,
    pub(crate) menu_open: bool,
    menu_sel: usize,

    bag: [BsCrateKind; 8],
    bag_pos: usize,
    rng: Rng64,
    gravity_accum: f32,
    move_cooldown: f32,
    particles: Vec<BsBurst>,
    callout: Option<(String, f32, f32, f32)>,
    pub splash: Option<(String, f32)>,
}

impl BeatStackPlugin {
    pub fn new(seed: u64) -> Self {
        let mut p = Self {
            grid: [[None; BS_COLS]; BS_ROWS],
            falling: Vec::with_capacity(8),
            cranes: [BsCrane {
                active: false,
                col: 0,
                vis_col: 0.0,
                dir: 1,
                target_col: 4,
                cargo: None,
                mode: BsCraneMode::EmptyReturn,
                telegraph_beats: 0.0,
                rail_steps_loaded: 0,
                open_flash: 0.0,
            }; BS_MAX_CRANES],
            worker_col: 5,
            worker_row: (BS_ROWS - 1) as i32,
            worker_facing: 1,
            worker_action: BsWorkerAction::Idle,
            worker_walk_phase: 0,
            worker_danger: false,
            score: 0,
            hi_score: 0,
            chain: 1,
            max_chain: 1,
            chain_active: false,
            cleared_crates: 0,
            wave: 1,
            drops_done: 0,
            difficulty: 1,
            latency_idx: 1,
            clock: BsBeatClock::new(0.025),
            effective_bpm: 120.0,
            beat_frac: 0.0,
            bpm_pulse: 0.0,
            strong_pulse: 0.0,
            show_hint: true,
            hint_pinned: false,
            music_playing: false,
            track_title: String::new(),
            lose_reason: None,
            menu_open: false,
            menu_sel: 0,
            bag: [
                BsCrateKind::Light,
                BsCrateKind::Light,
                BsCrateKind::Checker,
                BsCrateKind::Checker,
                BsCrateKind::Medium,
                BsCrateKind::Medium,
                BsCrateKind::Heavy,
                BsCrateKind::Heavy,
            ],
            bag_pos: 8,
            rng: Rng64::new(seed ^ 0x57AC_CA7E),
            gravity_accum: 0.0,
            move_cooldown: 0.0,
            particles: Vec::with_capacity(64),
            callout: None,
            splash: None,
        };
        p.reset_round();
        p
    }

    fn reset_round(&mut self) {
        self.grid = [[None; BS_COLS]; BS_ROWS];
        self.falling.clear();
        for c in &mut self.cranes {
            c.active = false;
        }
        self.worker_col = 5;
        self.worker_row = (BS_ROWS - 1) as i32;
        self.worker_facing = 1;
        self.worker_action = BsWorkerAction::Idle;
        self.worker_walk_phase = 0;
        self.worker_danger = false;
        self.score = 0;
        self.chain = 1;
        self.max_chain = 1;
        self.chain_active = false;
        self.cleared_crates = 0;
        self.wave = 1;
        self.drops_done = 0;
        self.show_hint = true;
        self.hint_pinned = false;
        self.lose_reason = None;
        self.gravity_accum = 0.0;
        self.move_cooldown = 0.0;
        self.particles.clear();
        self.callout = None;
        self.clock.reset_full();

        let bottom = BS_ROWS - 1;
        for col in [0usize, 1, 3, 8, 10, 11] {
            let k = self.draw_kind();
            self.grid[bottom][col] = Some(BsGroundedCrate::new(k));
        }
        self.recompute_armed();
        self.ensure_cranes();
    }

    fn draw_kind(&mut self) -> BsCrateKind {
        if self.bag_pos >= 8 {
            self.bag = [
                BsCrateKind::Light,
                BsCrateKind::Light,
                BsCrateKind::Checker,
                BsCrateKind::Checker,
                BsCrateKind::Medium,
                BsCrateKind::Medium,
                BsCrateKind::Heavy,
                BsCrateKind::Heavy,
            ];
            for i in (1..8).rev() {
                let j = self.rng.range(i + 1);
                self.bag.swap(i, j);
            }
            self.bag_pos = 0;
        }
        let k = self.bag[self.bag_pos];
        self.bag_pos += 1;
        k
    }

    fn max_cranes(&self) -> usize {
        if self.drops_done < 2 && self.difficulty != 2 {
            return 1;
        }
        let base = match self.wave {
            1..=2 => 2,
            3..=5 => 3,
            _ => 4,
        };
        match self.difficulty {
            0 => base.min(3),
            1 => base.min(BS_MAX_CRANES),
            _ => (base + 1).min(BS_MAX_CRANES),
        }
    }

    fn col_height(&self, col: usize) -> usize {
        let mut h = 0;
        for r in (0..BS_ROWS).rev() {
            if self.grid[r][col].is_some() {
                h = BS_ROWS - r;
            }
        }
        h
    }

    fn col_has_telegraph_other(&self, col: usize, self_idx: Option<usize>) -> bool {
        self.cranes.iter().enumerate().any(|(j, c)| {
            Some(j) != self_idx
                && c.active
                && c.mode == BsCraneMode::Telegraph
                && c.col.clamp(0, (BS_COLS - 1) as i32) as usize == col
        })
    }

    fn col_targeted_by_other(&self, col: usize, self_idx: Option<usize>) -> bool {
        self.cranes.iter().enumerate().any(|(j, c)| {
            Some(j) != self_idx
                && c.active
                && c.mode != BsCraneMode::EmptyReturn
                && c.target_col == col
        })
    }

    fn pick_drop_col(&mut self, from: i32, self_idx: Option<usize>) -> usize {
        let seed_off = self.rng.range(BS_COLS);
        let mut best = None;
        let mut best_s = i32::MIN;
        for step in 0..BS_COLS {
            let cand = (seed_off + step * 5) % BS_COLS;
            if self.grid[0][cand].is_some() {
                continue;
            }
            let h = self.col_height(cand) as i32;
            let dist = ((cand as i32) - from).abs();
            let high_drop = self.falling.iter().any(|f| f.col == cand && f.row <= 2);
            let pen = (if self.col_has_telegraph_other(cand, self_idx) { 50 } else { 0 })
                + (if high_drop { 28 } else { 0 })
                + (if self.col_targeted_by_other(cand, self_idx) { 22 } else { 0 });
            let jit = (self.rng.range(7) as i32) - 3;
            let s = (dist.min(6) * 3) - (h * 6) - pen + jit;
            if s > best_s {
                best_s = s;
                best = Some(cand);
            }
        }
        best.unwrap_or_else(|| from.clamp(0, (BS_COLS - 1) as i32) as usize)
    }

    fn resolve_free_drop_col(&self, preferred: usize, self_idx: usize) -> usize {
        let pref = preferred.min(BS_COLS - 1);
        if self.grid[0][pref].is_none() && !self.col_has_telegraph_other(pref, Some(self_idx)) {
            return pref;
        }
        for d in 1..BS_COLS {
            for sign in [-1i32, 1i32] {
                let c = (pref as i32) + sign * (d as i32);
                if (0..BS_COLS as i32).contains(&c) {
                    let cu = c as usize;
                    if self.grid[0][cu].is_none()
                        && !self.col_has_telegraph_other(cu, Some(self_idx))
                    {
                        return cu;
                    }
                }
            }
        }
        for cu in 0..BS_COLS {
            if self.grid[0][cu].is_none() {
                return cu;
            }
        }
        pref
    }

    fn ensure_cranes(&mut self) {
        let wanted = self.max_cranes();
        if self.cranes.iter().filter(|c| c.active).count() >= wanted {
            return;
        }
        for i in 0..wanted {
            if !self.cranes[i].active {
                let left = i % 2 == 0;
                let sc = if left { 0 } else { (BS_COLS - 1) as i32 };
                let cargo = self.draw_kind();
                let tgt = self.pick_drop_col(sc, Some(i));
                let dir = if (tgt as i32) >= sc { 1 } else { -1 };
                self.cranes[i] = BsCrane {
                    active: true,
                    col: sc,
                    vis_col: sc as f32,
                    dir,
                    target_col: tgt,
                    cargo: Some(cargo),
                    mode: BsCraneMode::Carrying,
                    telegraph_beats: 0.0,
                    rail_steps_loaded: 0,
                    open_flash: 0.0,
                };
                break;
            }
        }
    }

    fn is_solid(&self, c: i32, r: i32) -> bool {
        if c < 0 || c >= (BS_COLS as i32) || r >= (BS_ROWS as i32) {
            return true;
        }
        if r < 0 {
            return false;
        }
        self.grid[r as usize][c as usize].is_some()
    }

    fn has_falling_at(&self, c: i32, r: i32) -> bool {
        if c < 0 || c >= (BS_COLS as i32) || r < 0 || r >= (BS_ROWS as i32) {
            return false;
        }
        self.falling
            .iter()
            .any(|f| f.col == c as usize && f.row == r as usize)
    }

    fn has_unsupported_crates(&self) -> bool {
        for c in 0..BS_COLS {
            let mut seen_empty = false;
            for r in (0..BS_ROWS).rev() {
                if self.grid[r][c].is_none() {
                    seen_empty = true;
                } else if seen_empty {
                    return true;
                }
            }
        }
        false
    }

    fn any_crate_armed(&self) -> bool {
        for r in 0..BS_ROWS {
            for c in 0..BS_COLS {
                if let Some(cell) = self.grid[r][c] {
                    if cell.is_armed() {
                        return true;
                    }
                }
            }
        }
        false
    }

    fn trigger_lose(&mut self, reason: BsLoseReason) {
        self.lose_reason = Some(reason);
        self.hi_score = self.hi_score.max(self.score);
        let cx = self.worker_col as f32 + 0.5;
        let cy = self.worker_row as f32 + 0.5;
        self.spawn_burst(cx, cy, 12);
    }

    fn spawn_burst(&mut self, cx: f32, cy: f32, count: usize) {
        for i in 0..count {
            if self.particles.len() >= 64 {
                break;
            }
            let ang = (i as f32 / count.max(1) as f32) * core::f32::consts::TAU;
            let spd = 2.8 + (self.rng.range(100) as f32) * 0.035;
            self.particles.push(BsBurst {
                x: cx,
                y: cy,
                vx: ang.cos() * spd,
                vy: ang.sin() * spd - 2.4,
                life: 1.0,
                shade: ((i % 2) + 2) as u8,
            });
        }
    }

    fn drop_unsupported_once(&mut self) -> bool {
        let mut moved = false;
        for c in 0..BS_COLS {
            for r in (0..BS_ROWS - 1).rev() {
                if self.grid[r][c].is_some() && self.grid[r + 1][c].is_none() {
                    let mut cell = self.grid[r][c].take().unwrap();
                    cell.settle_px_frac = 100;
                    self.grid[r + 1][c] = Some(cell);
                    moved = true;
                }
            }
        }
        if moved {
            self.recompute_armed();
        }
        moved
    }

    fn recompute_armed(&mut self) {
        let mut sq = [[false; BS_COLS]; BS_ROWS];
        for r in 0..(BS_ROWS - 1) {
            for c in 0..(BS_COLS - 1) {
                if let (Some(a), Some(b), Some(d), Some(e)) = (
                    self.grid[r][c],
                    self.grid[r][c + 1],
                    self.grid[r + 1][c],
                    self.grid[r + 1][c + 1],
                ) {
                    if a.kind == b.kind && a.kind == d.kind && a.kind == e.kind {
                        sq[r][c] = true;
                        sq[r][c + 1] = true;
                        sq[r + 1][c] = true;
                        sq[r + 1][c + 1] = true;
                    }
                }
            }
        }
        let fr = BS_ROWS - 1;
        let full_row = (0..BS_COLS).all(|c| self.grid[fr][c].is_some());
        for r in 0..BS_ROWS {
            for c in 0..BS_COLS {
                if let Some(ref mut cell) = self.grid[r][c] {
                    let was_armed = cell.is_armed();
                    cell.armed_square = sq[r][c];
                    cell.armed_row = r == fr && full_row;
                    if !cell.is_armed() || !was_armed {
                        cell.armed_sec = 0.0;
                        cell.armed_beats = 0.0;
                    }
                }
            }
        }
    }

    fn try_walk_or_push(&mut self, dir: i32) -> bool {
        self.worker_facing = dir;
        let c = self.worker_col;
        let r = self.worker_row;
        let nx = c + dir;
        if !(0..BS_COLS as i32).contains(&nx) {
            return false;
        }
        if !self.is_solid(nx, r) {
            if self.has_falling_at(nx, r) {
                self.worker_col = nx;
                self.trigger_lose(BsLoseReason::Crushed);
                return true;
            }
            self.worker_col = nx;
            self.worker_action = BsWorkerAction::Walking {
                from_col: c,
                progress: 0.0,
            };
            self.move_cooldown = 0.11;
            return true;
        }
        let nnx = nx + dir;
        if !(0..BS_COLS as i32).contains(&nnx)
            || self.is_solid(nnx, r)
            || self.has_falling_at(nnx, r)
        {
            return false;
        }
        if r > 0 && (self.is_solid(nx, r - 1) || self.has_falling_at(nx, r - 1)) {
            return false;
        }
        let mut moved = self.grid[r as usize][nx as usize].take().unwrap();
        moved.armed_sec = 0.0;
        moved.armed_beats = 0.0;
        self.grid[r as usize][nnx as usize] = Some(moved);
        self.worker_col = nx;
        self.worker_action = BsWorkerAction::Pushing { dir, progress: 0.0 };
        self.move_cooldown = 0.135;
        if (r as usize) + 1 < BS_ROWS && self.grid[(r as usize) + 1][nnx as usize].is_none() {
            let _ = self.drop_unsupported_once();
        } else {
            self.recompute_armed();
        }
        true
    }

    fn try_vertical_jump(&mut self) -> bool {
        let c = self.worker_col;
        let r = self.worker_row;
        if r <= 0 || self.is_solid(c, r - 1) {
            return false;
        }
        if self.has_falling_at(c, r - 1) {
            self.worker_row = r - 1;
            self.trigger_lose(BsLoseReason::Crushed);
            return true;
        }
        self.worker_action = BsWorkerAction::Jumping {
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

    fn try_diagonal_jump(&mut self, dir: i32) -> bool {
        self.worker_facing = dir;
        let c = self.worker_col;
        let r = self.worker_row;
        if r <= 0 || self.is_solid(c, r - 1) {
            return false;
        }
        let nx = c + dir;
        if !(0..BS_COLS as i32).contains(&nx) {
            return self.try_vertical_jump();
        }
        if self.has_falling_at(c, r - 1) {
            self.worker_row = r - 1;
            self.trigger_lose(BsLoseReason::Crushed);
            return true;
        }
        let blocked = self.is_solid(nx, r - 1);
        if !blocked && self.has_falling_at(nx, r - 1) {
            self.worker_col = nx;
            self.worker_row = r - 1;
            self.trigger_lose(BsLoseReason::Crushed);
            return true;
        }
        self.worker_action = BsWorkerAction::Jumping {
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

    fn step_strong_beat_detonation(&mut self, uk: bool) -> u32 {
        let mut visited = [[false; BS_COLS]; BS_ROWS];
        let mut clear = [[false; BS_COLS]; BS_ROWS];
        let mut groups = 0u32;
        let mut mono_row = false;
        let mut center = (6.0f32, 4.0f32);

        for r in 0..BS_ROWS {
            for c in 0..BS_COLS {
                if visited[r][c] {
                    continue;
                }
                let Some(seed) = self.grid[r][c] else {
                    continue;
                };
                if !seed.armed_square {
                    continue;
                }
                let mut st = vec![(c, r)];
                let mut mem = Vec::with_capacity(8);
                let mut min_c = c;
                let mut max_c = c;
                let mut ready = false;
                visited[r][c] = true;
                while let Some((cx, cy)) = st.pop() {
                    mem.push((cx, cy));
                    min_c = min_c.min(cx);
                    max_c = max_c.max(cx);
                    if let Some(cell) = self.grid[cy][cx] {
                        if cell.ready_to_detonate() {
                            ready = true;
                        }
                    }
                    for (dx, dy) in [(-1i32, 0i32), (1, 0), (0, -1), (0, 1)] {
                        let nx = cx as i32 + dx;
                        let ny = cy as i32 + dy;
                        if (0..BS_COLS as i32).contains(&nx) && (0..BS_ROWS as i32).contains(&ny) {
                            let ux = nx as usize;
                            let uy = ny as usize;
                            if !visited[uy][ux] {
                                if let Some(nb) = self.grid[uy][ux] {
                                    if nb.armed_square && nb.kind == seed.kind {
                                        visited[uy][ux] = true;
                                        st.push((ux, uy));
                                    }
                                }
                            }
                        }
                    }
                }
                if ready {
                    for &(mx, my) in &mem {
                        clear[my][mx] = true;
                    }
                    groups += 1;
                    center = ((min_c + max_c) as f32 * 0.5 + 0.5, mem[0].1 as f32);
                }
            }
        }

        let fr = BS_ROWS - 1;
        let floor_armed =
            (0..BS_COLS).all(|c| self.grid[fr][c].map(|x| x.armed_row).unwrap_or(false));
        if floor_armed {
            let row_ready = (0..BS_COLS).any(|c| {
                self.grid[fr][c]
                    .map(|x| x.ready_to_detonate())
                    .unwrap_or(false)
            });
            if row_ready {
                let fk = self.grid[fr][0].map(|x| x.kind);
                let is_mono =
                    fk.is_some() && (0..BS_COLS).all(|c| self.grid[fr][c].map(|x| x.kind) == fk);
                for c in 0..BS_COLS {
                    clear[fr][c] = true;
                }
                groups += 1;
                mono_row = is_mono;
                center = (6.0, fr as f32);
            }
        }

        if groups > 0 {
            let mut crates = 0u32;
            for r in 0..BS_ROWS {
                for c in 0..BS_COLS {
                    if clear[r][c] && self.grid[r][c].take().is_some() {
                        crates += 1;
                        self.spawn_burst(c as f32 + 0.5, r as f32 + 0.5, 6);
                    }
                }
            }
            if crates > 0 {
                if self.chain_active {
                    self.chain = (self.chain + 1).min(16);
                } else {
                    self.chain = 1;
                    self.chain_active = true;
                }
                self.max_chain = self.max_chain.max(self.chain);
                let mono_mult = if mono_row { 3 } else { 1 };
                let gained =
                    (crates * 50 + groups.saturating_sub(1) * 100) * mono_mult * self.chain;
                self.score = self.score.saturating_add(gained);
                self.hi_score = self.hi_score.max(self.score);
                self.cleared_crates += crates;
                self.wave = 1 + (self.cleared_crates / 12);
                if !self.hint_pinned {
                    self.show_hint = false;
                }
                let txt = if mono_row && self.chain > 1 {
                    format!(
                        "+{}  MONO ×3 · {} ×{}",
                        gained,
                        tr(uk, "CHAIN", "ЛАНЦЮГ"),
                        self.chain
                    )
                } else if mono_row {
                    format!("+{}  {}", gained, tr(uk, "MONO ROW ×3", "МОНО РЯД ×3"))
                } else if self.chain > 1 {
                    format!(
                        "+{}  {} ×{}",
                        gained,
                        tr(uk, "CHAIN", "ЛАНЦЮГ"),
                        self.chain
                    )
                } else {
                    format!("+{}", gained)
                };
                self.callout = Some((
                    txt,
                    center.0.clamp(2.4, 9.6),
                    center.1.clamp(1.2, 5.8),
                    1.10,
                ));
                let _ = self.drop_unsupported_once();
                self.recompute_armed();
                return crates;
            }
        }

        if !self.any_crate_armed() && !self.has_unsupported_crates() {
            self.chain_active = false;
            self.chain = 1;
        }
        0
    }

    fn step_cranes(&mut self, step_beats: f32, fire_beat: bool, fire_half: bool) {
        self.ensure_cranes();
        for i in 0..BS_MAX_CRANES {
            if !self.cranes[i].active {
                continue;
            }
            if self.cranes[i].mode != BsCraneMode::EmptyReturn && self.cranes[i].cargo.is_none() {
                self.cranes[i].mode = BsCraneMode::EmptyReturn;
            }
            match self.cranes[i].mode {
                BsCraneMode::Telegraph => {
                    self.cranes[i].telegraph_beats += step_beats;
                    if (fire_beat && self.cranes[i].telegraph_beats >= 0.72)
                        || self.cranes[i].telegraph_beats >= 1.25
                    {
                        let dc = (self.cranes[i].col.clamp(0, (BS_COLS - 1) as i32)) as usize;
                        if let Some(kind) = self.cranes[i].cargo.take() {
                            if self.grid[0][dc].is_some() {
                                self.trigger_lose(BsLoseReason::ShaftJammed);
                                return;
                            }
                            if self.worker_col == dc as i32 && self.worker_row == 0 {
                                self.trigger_lose(BsLoseReason::Crushed);
                                return;
                            }
                            self.falling.push(BsFallingCrate {
                                col: dc,
                                row: 0,
                                kind,
                                snap_anim: -0.65,
                            });
                            self.drops_done += 1;
                        }
                        self.cranes[i].mode = BsCraneMode::EmptyReturn;
                        self.cranes[i].telegraph_beats = 0.0;
                        self.cranes[i].rail_steps_loaded = 0;
                        self.cranes[i].open_flash = 1.0;
                        self.cranes[i].dir = if dc < BS_COLS / 2 { -1 } else { 1 };
                    }
                }
                BsCraneMode::Carrying => {
                    let tgt = self.cranes[i].target_col as i32;
                    let reached = if self.cranes[i].dir >= 0 {
                        self.cranes[i].col >= tgt
                    } else {
                        self.cranes[i].col <= tgt
                    };
                    let watchdog = self.cranes[i].rail_steps_loaded >= (BS_COLS as u8 + 2);
                    if (reached && (fire_beat || fire_half)) || watchdog {
                        let vc = self.resolve_free_drop_col(self.cranes[i].target_col, i);
                        if vc == self.cranes[i].target_col
                            || watchdog
                            || self.cranes[i].col == vc as i32
                        {
                            self.cranes[i].col = vc as i32;
                            self.cranes[i].target_col = vc;
                            self.cranes[i].mode = BsCraneMode::Telegraph;
                            self.cranes[i].telegraph_beats = 0.0;
                            self.cranes[i].rail_steps_loaded = 0;
                        } else {
                            self.cranes[i].target_col = vc;
                            self.cranes[i].dir =
                                if (vc as i32) >= self.cranes[i].col { 1 } else { -1 };
                        }
                    } else if fire_half {
                        let dir = if tgt >= self.cranes[i].col { 1 } else { -1 };
                        self.cranes[i].dir = dir;
                        self.cranes[i].col =
                            (self.cranes[i].col + dir).clamp(0, (BS_COLS - 1) as i32);
                        self.cranes[i].rail_steps_loaded =
                            self.cranes[i].rail_steps_loaded.saturating_add(1);
                    }
                }
                BsCraneMode::EmptyReturn => {
                    if fire_half {
                        let dir = if self.cranes[i].dir >= 0 { 1 } else { -1 };
                        let next_c = (self.cranes[i].col + dir).clamp(0, (BS_COLS - 1) as i32);
                        if next_c <= 0 || next_c >= (BS_COLS as i32 - 1) {
                            let bay = if next_c <= 0 { 0 } else { (BS_COLS - 1) as i32 };
                            let nk = self.draw_kind();
                            let tgt = self.pick_drop_col(bay, Some(i));
                            let nd = if (tgt as i32) >= bay { 1 } else { -1 };
                            self.cranes[i].col = bay;
                            self.cranes[i].cargo = Some(nk);
                            self.cranes[i].target_col = tgt;
                            self.cranes[i].dir = nd;
                            self.cranes[i].telegraph_beats = 0.0;
                            self.cranes[i].rail_steps_loaded = 0;
                            self.cranes[i].mode = BsCraneMode::Carrying;
                        } else {
                            self.cranes[i].col = next_c;
                        }
                    }
                }
            }
        }
        self.ensure_cranes();
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
        let dt = dt.clamp(1.0 / 240.0, 0.05);
        self.music_playing = audio.playing;

        if self.track_title != audio.track_title {
            let had_prev = !self.track_title.is_empty();
            self.track_title = audio.track_title.to_string();
            self.clock.on_track_change();
            if had_prev && !self.track_title.is_empty() {
                self.splash = Some((format!("♪ {}", self.track_title), 1.4));
            }
        }

        if self.menu_open {
            let (back, act, dir) = nav_menu_input(pressed, &mut self.menu_sel, 7);
            if back || (pressed & btn::START) != 0 {
                self.menu_open = false;
                return PluginAction::Click;
            }
            if act {
                match self.menu_sel {
                    0 if dir > 0 && (pressed & btn::A) != 0 => {
                        self.menu_open = false;
                    }
                    1 if (pressed & btn::A) != 0 => {
                        self.reset_round();
                        self.menu_open = false;
                    }
                    2 if (pressed & btn::A) != 0 => {
                        return PluginAction::TogglePlayPause;
                    }
                    3 => {
                        self.difficulty = if dir < 0 {
                            (self.difficulty + 2) % 3
                        } else {
                            (self.difficulty + 1) % 3
                        };
                    }
                    4 => {
                        let n = BS_SYNC_OFFSETS_MS.len();
                        self.latency_idx = if dir < 0 {
                            (self.latency_idx + n - 1) % n
                        } else {
                            (self.latency_idx + 1) % n
                        };
                        self.clock.audio_latency_sec =
                            (BS_SYNC_OFFSETS_MS[self.latency_idx] as f64) * 0.001;
                    }
                    5 => {
                        self.show_hint = !self.show_hint;
                        self.hint_pinned = self.show_hint;
                    }
                    6 if (pressed & btn::A) != 0 => {
                        return PluginAction::Exit;
                    }
                    _ => {}
                }
                return PluginAction::Click;
            }
            return PluginAction::Continue;
        }

        // `START` / `MENU` opens Pause Menu (`B` & `Hold B` NEVER exit gameplay!)
        if (pressed & (btn::START | btn::MENU)) != 0 {
            self.menu_open = true;
            self.menu_sel = 0;
            return PluginAction::Click;
        }
        if (pressed & btn::L2) != 0 {
            return PluginAction::PrevTrack;
        }
        if (pressed & btn::R2) != 0 {
            return PluginAction::NextTrack;
        }
        if (pressed & btn::X) != 0 {
            self.show_hint = !self.show_hint;
            self.hint_pinned = self.show_hint;
        }

        if self.lose_reason.is_some() && (pressed & btn::A) != 0 {
            self.reset_round();
            return PluginAction::Click;
        }

        let apu = uipod_codecs::get_nsf_apu_telemetry();
        let apu_pulse = if audio.playing
            && uipod_codecs::get_nsf_active_chip_flags().is_some()
            && apu.channel_count > 0
        {
            let ch_trig = |idx: usize, boost: f32| -> f32 {
                let c = &apu.channels[idx];
                if c.active != 0 && c.muted == 0 {
                    let mut v = c.volume.clamp(0.0, 1.0);
                    if c.onset_trigger != 0 {
                        v = (v + boost).min(1.0);
                    }
                    v
                } else {
                    0.0
                }
            };
            ch_trig(3, 0.45)
                .max(ch_trig(4, 0.50))
                .max(ch_trig(2, 0.35) * 0.85)
        } else {
            0.0
        };

        let (bpm, bf, step_beats, fire_beat, fire_strong, fire_half, raw_onset) =
            self.clock
                .step(dt, audio.playing, pcm, audio.sample_rate.max(44_100), apu_pulse);
        self.effective_bpm = bpm;
        self.beat_frac = bf;
        if fire_strong {
            self.strong_pulse = 1.0;
            self.bpm_pulse = 1.0;
        } else if fire_beat || raw_onset {
            self.bpm_pulse = self.bpm_pulse.max(0.75);
            self.strong_pulse = (self.strong_pulse - dt * 3.4).max(0.0);
        } else {
            self.bpm_pulse = (self.bpm_pulse - dt * 4.5).max(0.0);
            self.strong_pulse = (self.strong_pulse - dt * 3.4).max(0.0);
        }

        for r in 0..BS_ROWS {
            for c in 0..BS_COLS {
                if let Some(ref mut cell) = self.grid[r][c] {
                    if cell.settle_px_frac > 0 {
                        cell.settle_px_frac =
                            cell.settle_px_frac.saturating_sub(((dt * 1200.0) as u8).max(1));
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
        if let Some((_, _, ref mut ry, ref mut rem)) = self.callout {
            *ry -= dt * 0.9;
            *rem -= dt;
            if *rem <= 0.0 {
                self.callout = None;
            }
        }

        if self.lose_reason.is_some() {
            return PluginAction::Continue;
        }

        for r in 0..BS_ROWS {
            for c in 0..BS_COLS {
                if let Some(ref mut cell) = self.grid[r][c] {
                    if cell.is_armed() {
                        cell.armed_sec += dt;
                        cell.armed_beats += step_beats;
                    }
                }
            }
        }

        // Settled / beat cascade gravity
        if fire_half && self.has_unsupported_crates() {
            let _ = self.drop_unsupported_once();
            self.gravity_accum = 0.0;
        } else {
            self.gravity_accum += dt;
            while self.gravity_accum >= 0.085 {
                self.gravity_accum -= 0.085;
                if !self.drop_unsupported_once() {
                    self.gravity_accum = 0.0;
                    break;
                }
            }
        }

        // Worker input & physics (`B` = Jump, `B + Left/Right` & `L1/R1` = Diag Jump, ZERO `X`/`Y` jump)
        let mut move_dir = 0;
        if (held & btn::LEFT) != 0 {
            move_dir -= 1;
        }
        if (held & btn::RIGHT) != 0 {
            move_dir += 1;
        }
        if self.move_cooldown > 0.0 {
            self.move_cooldown = (self.move_cooldown - dt).max(0.0);
        }

        if matches!(self.worker_action, BsWorkerAction::Idle)
            && self.worker_row < (BS_ROWS as i32 - 1)
            && !self.is_solid(self.worker_col, self.worker_row + 1)
        {
            self.worker_action = BsWorkerAction::Falling {
                vis_row: self.worker_row as f32,
            };
        }

        match self.worker_action {
            BsWorkerAction::Walking {
                from_col,
                mut progress,
            } => {
                progress += dt / 0.090;
                if progress >= 1.0 {
                    self.worker_walk_phase = self.worker_walk_phase.wrapping_add(1);
                    if self.worker_row < (BS_ROWS as i32 - 1)
                        && !self.is_solid(self.worker_col, self.worker_row + 1)
                    {
                        self.worker_action = BsWorkerAction::Falling {
                            vis_row: self.worker_row as f32,
                        };
                    } else {
                        self.worker_action = BsWorkerAction::Idle;
                    }
                } else {
                    self.worker_action = BsWorkerAction::Walking { from_col, progress };
                }
            }
            BsWorkerAction::Pushing { dir, mut progress } => {
                progress += dt / 0.115;
                if progress >= 1.0 {
                    self.worker_walk_phase = self.worker_walk_phase.wrapping_add(1);
                    if self.worker_row < (BS_ROWS as i32 - 1)
                        && !self.is_solid(self.worker_col, self.worker_row + 1)
                    {
                        self.worker_action = BsWorkerAction::Falling {
                            vis_row: self.worker_row as f32,
                        };
                    } else {
                        self.worker_action = BsWorkerAction::Idle;
                    }
                } else {
                    self.worker_action = BsWorkerAction::Pushing { dir, progress };
                }
            }
            BsWorkerAction::Jumping {
                from_col,
                from_row,
                mut to_col,
                to_row,
                mut progress,
                blocked_wall,
                mut steered_apex,
            } => {
                if !steered_apex
                    && !blocked_wall
                    && (to_col as f32 - from_col).abs() < 0.01
                    && (0.20..=0.78).contains(&progress)
                    && move_dir != 0
                {
                    let cand = self.worker_col + move_dir;
                    if (0..BS_COLS as i32).contains(&cand)
                        && !self.is_solid(cand, to_row)
                        && !self.has_falling_at(cand, to_row)
                    {
                        self.worker_facing = move_dir;
                        to_col = cand;
                        steered_apex = true;
                    }
                }
                progress += dt / 0.195;
                if progress >= 1.0 {
                    if !blocked_wall {
                        self.worker_col = to_col;
                        self.worker_row = to_row;
                    }
                    if self.has_falling_at(self.worker_col, self.worker_row) {
                        self.trigger_lose(BsLoseReason::Crushed);
                        return PluginAction::Continue;
                    }
                    if self.worker_row < (BS_ROWS as i32 - 1)
                        && !self.is_solid(self.worker_col, self.worker_row + 1)
                    {
                        self.worker_action = BsWorkerAction::Falling {
                            vis_row: self.worker_row as f32,
                        };
                    } else {
                        self.worker_action = BsWorkerAction::Idle;
                    }
                } else {
                    self.worker_action = BsWorkerAction::Jumping {
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
            BsWorkerAction::Falling { mut vis_row } => {
                vis_row += dt * 10.5;
                let nr = vis_row.floor() as i32;
                while self.worker_row < nr && self.worker_row < (BS_ROWS as i32 - 1) {
                    if self.is_solid(self.worker_col, self.worker_row + 1) {
                        self.worker_action = BsWorkerAction::Idle;
                        break;
                    }
                    self.worker_row += 1;
                    if self.has_falling_at(self.worker_col, self.worker_row) {
                        self.trigger_lose(BsLoseReason::Crushed);
                        return PluginAction::Continue;
                    }
                }
                if self.worker_row >= (BS_ROWS as i32 - 1)
                    || self.is_solid(self.worker_col, self.worker_row + 1)
                {
                    self.worker_row = self.worker_row.min(BS_ROWS as i32 - 1);
                    self.worker_action = BsWorkerAction::Idle;
                } else if matches!(self.worker_action, BsWorkerAction::Falling { .. }) {
                    self.worker_action = BsWorkerAction::Falling { vis_row };
                }
            }
            BsWorkerAction::Idle => {
                let jump_b = (pressed & btn::B) != 0;
                let held_b = (held & btn::B) != 0;
                let press_l = (pressed & btn::LEFT) != 0;
                let press_r = (pressed & btn::RIGHT) != 0;
                let diag_l = (pressed & btn::L1) != 0
                    || (jump_b && move_dir < 0)
                    || (held_b && press_l);
                let diag_r = (pressed & btn::R1) != 0
                    || (jump_b && move_dir > 0)
                    || (held_b && press_r);
                if diag_l {
                    self.try_diagonal_jump(-1);
                } else if diag_r {
                    self.try_diagonal_jump(1);
                } else if jump_b {
                    self.try_vertical_jump();
                } else {
                    let dir = if press_l {
                        -1
                    } else if press_r {
                        1
                    } else if self.move_cooldown <= 0.0 {
                        move_dir
                    } else {
                        0
                    };
                    if dir != 0 {
                        self.try_walk_or_push(dir);
                    }
                }
            }
        }
        if self.lose_reason.is_some() {
            return PluginAction::Continue;
        }

        // Strong-Beat Detonation (`Beats 1 & 3`)
        if fire_strong {
            self.step_strong_beat_detonation(uk);
        }

        // Falling crates on beat / half-beat
        let fast_fall =
            (self.difficulty == 1 && self.wave >= 8) || (self.difficulty == 2 && self.wave >= 4);
        if if fast_fall { fire_half } else { fire_beat } {
            self.falling.sort_by(|a, b| b.row.cmp(&a.row));
            let mut i = 0;
            while i < self.falling.len() {
                let col = self.falling[i].col;
                let row = self.falling[i].row;
                let kind = self.falling[i].kind;
                if self.worker_col == col as i32 && self.worker_row == row as i32 {
                    self.trigger_lose(BsLoseReason::Crushed);
                    return PluginAction::Continue;
                }
                if row + 1 >= BS_ROWS || self.grid[row + 1][col].is_some() {
                    self.falling.remove(i);
                    self.grid[row][col] = Some(BsGroundedCrate::new(kind));
                    self.recompute_armed();
                    continue;
                }
                let next_r = row + 1;
                if self.worker_col == col as i32 && self.worker_row == next_r as i32 {
                    self.falling[i].row = next_r;
                    self.trigger_lose(BsLoseReason::Crushed);
                    return PluginAction::Continue;
                }
                if next_r + 1 >= BS_ROWS || self.grid[next_r + 1][col].is_some() {
                    self.falling.remove(i);
                    let mut landed = BsGroundedCrate::new(kind);
                    landed.settle_px_frac = 65;
                    self.grid[next_r][col] = Some(landed);
                    self.recompute_armed();
                } else {
                    self.falling[i].row = next_r;
                    self.falling[i].snap_anim = -1.0;
                    i += 1;
                }
            }
        }

        for fc in &mut self.falling {
            fc.snap_anim = (fc.snap_anim + dt * 13.0).min(0.0);
        }
        for cr in &mut self.cranes {
            if cr.active {
                cr.vis_col += ((cr.col as f32) - cr.vis_col) * (dt * 18.0).min(1.0);
                cr.open_flash = (cr.open_flash - dt * 3.2).max(0.0);
            }
        }

        self.step_cranes(step_beats, fire_beat, fire_half);
        if self.lose_reason.is_some() {
            return PluginAction::Continue;
        }

        let wc = self.worker_col as usize;
        let wr = self.worker_row as usize;
        self.worker_danger = self.falling.iter().any(|f| f.col == wc && f.row <= wr)
            || self
                .cranes
                .iter()
                .any(|c| c.active && c.mode == BsCraneMode::Telegraph && c.col as usize == wc);

        PluginAction::Continue
    }

    fn worker_vis(&self) -> (f32, f32, bool, i32) {
        match self.worker_action {
            BsWorkerAction::Idle => (self.worker_col as f32, self.worker_row as f32, false, 0),
            BsWorkerAction::Walking { from_col, progress } => {
                let x = from_col as f32 + (self.worker_col as f32 - from_col as f32) * progress;
                let hop = (progress * core::f32::consts::PI).sin() * 0.08;
                (x, self.worker_row as f32 - hop, false, 0)
            }
            BsWorkerAction::Pushing { dir, progress } => {
                let fc = (self.worker_col - dir) as f32;
                (fc + (dir as f32) * progress, self.worker_row as f32, false, dir)
            }
            BsWorkerAction::Jumping {
                from_col,
                from_row,
                to_col,
                to_row,
                progress,
                blocked_wall,
                ..
            } => {
                let x = if blocked_wall {
                    from_col
                        + (progress * core::f32::consts::PI).sin()
                            * (self.worker_facing as f32)
                            * 0.28
                } else {
                    from_col + (to_col as f32 - from_col) * progress
                };
                let ly = from_row + (to_row as f32 - from_row) * progress;
                let arc = 4.0 * progress * (1.0 - progress) * 0.85;
                (x, ly - arc, true, 0)
            }
            BsWorkerAction::Falling { vis_row } => (self.worker_col as f32, vis_row, true, 0),
        }
    }

    fn draw_crate_wasm(
        ui: &mut UiEngine,
        t: ThemeColors,
        x: f32,
        y: f32,
        tile: f32,
        kind: BsCrateKind,
        armed: bool,
        telegraph_flash: bool,
        beat_frac: f32,
    ) {
        let inset = (tile * BS_SPEC.crate_inset_ratio).round().max(1.5);
        let bx = x + inset;
        let by = y + inset;
        let bw = (tile - inset * 2.0).max(8.0);
        let bh = bw;
        let rad = (tile * BS_SPEC.crate_radius_ratio).round().max(2.0);
        let border = (tile * 0.065).clamp(2.0, 4.0);

        match kind {
            BsCrateKind::Light => {
                ui.plugin_fill_rect(bx, by, bw, bh, rad, t.bg_rgb);
                ui.plugin_stroke_rect(bx, by, bw, bh, rad, border, t.fg_rgb);
                let in_pad = border + 2.0;
                ui.plugin_stroke_rect(
                    bx + in_pad,
                    by + in_pad,
                    bw - in_pad * 2.0,
                    bh - in_pad * 2.0,
                    1.5,
                    border,
                    t.fg2_rgb,
                );
                let rv = (tile * 0.11).clamp(3.0, 6.5);
                let roff = in_pad + border + 2.0;
                for (rx, ry) in [
                    (bx + roff, by + roff),
                    (bx + bw - roff - rv, by + roff),
                    (bx + roff, by + bh - roff - rv),
                    (bx + bw - roff - rv, by + bh - roff - rv),
                ] {
                    ui.plugin_fill_rect(rx, ry, rv, rv, 1.0, t.fg2_rgb);
                }
            }
            BsCrateKind::Checker => {
                ui.plugin_fill_rect(bx, by, bw, bh, rad, t.fg1_rgb);
                let inner = border;
                let iw = (bw - inner * 2.0).max(4.0);
                let cell = iw / 4.0;
                for sr in 0..4 {
                    for sc in 0..4 {
                        if (sr + sc) % 2 == 1 {
                            ui.plugin_fill_rect(
                                bx + inner + (sc as f32) * cell,
                                by + inner + (sr as f32) * cell,
                                cell.ceil(),
                                cell.ceil(),
                                0.0,
                                t.fg2_rgb,
                            );
                        }
                    }
                }
                ui.plugin_draw_line(
                    bx + inner + 2.0,
                    by + bh - inner - 2.0,
                    bx + bw - inner - 2.0,
                    by + inner + 2.0,
                    border * 1.15,
                    t.fg_rgb,
                );
                ui.plugin_stroke_rect(bx, by, bw, bh, rad, border, t.fg_rgb);
            }
            BsCrateKind::Medium => {
                ui.plugin_fill_rect(bx, by, bw, bh, rad, t.fg2_rgb);
                let sh = (bh * 0.13).clamp(3.0, 7.0);
                let px = (bw * 0.18).round();
                let sw = (bw - px * 2.0).max(4.0);
                ui.plugin_fill_rect(bx + px, by + bh * 0.28, sw, sh, 1.0, t.bg_rgb);
                ui.plugin_fill_rect(bx + px, by + bh - bh * 0.28 - sh, sw, sh, 1.0, t.bg_rgb);
                ui.plugin_stroke_rect(bx, by, bw, bh, rad, border, t.fg_rgb);
            }
            BsCrateKind::Heavy => {
                ui.plugin_fill_rect(bx, by, bw, bh, rad, t.fg_rgb);
                let pad = (bw * 0.22).round();
                ui.plugin_draw_line(
                    bx + pad,
                    by + pad,
                    bx + bw - pad,
                    by + bh - pad,
                    border * 1.1,
                    t.bg_rgb,
                );
                ui.plugin_draw_line(
                    bx + bw - pad,
                    by + pad,
                    bx + pad,
                    by + bh - pad,
                    border * 1.1,
                    t.bg_rgb,
                );
                ui.plugin_stroke_rect(
                    bx + border,
                    by + border,
                    bw - border * 2.0,
                    bh - border * 2.0,
                    1.5,
                    1.5,
                    t.fg2_rgb,
                );
            }
        }

        if telegraph_flash {
            ui.plugin_stroke_rect(bx - 1.0, by - 1.0, bw + 2.0, bh + 2.0, rad, border + 1.5, t.fg_rgb);
            ui.plugin_stroke_rect(
                bx + border,
                by + border,
                bw - border * 2.0,
                bh - border * 2.0,
                2.0,
                border,
                t.bg_rgb,
            );
        } else if armed {
            let pulse = beat_frac < 0.52;
            ui.plugin_stroke_rect(
                bx + border * 0.8,
                by + border * 0.8,
                bw - border * 1.6,
                bh - border * 1.6,
                2.0,
                border,
                if pulse { t.bg_rgb } else { t.fg1_rgb },
            );
            let core = if pulse {
                (bw * 0.26).max(6.0)
            } else {
                (bw * 0.18).max(4.5)
            };
            ui.plugin_fill_rect(
                bx + (bw - core) * 0.5,
                by + (bh - core) * 0.5,
                core,
                core,
                2.0,
                t.bg_rgb,
            );
            ui.plugin_stroke_rect(
                bx + (bw - core) * 0.5,
                by + (bh - core) * 0.5,
                core,
                core,
                2.0,
                2.0,
                t.fg_rgb,
            );
        }
    }

    fn draw_armed_highlights(&self, ui: &mut UiEngine, t: ThemeColors, lay: &BsComputedLayout) {
        let tile = lay.tile;
        let pulse_bright = self.beat_frac < 0.52;
        let outer_col = if pulse_bright { t.fg_rgb } else { t.fg2_rgb };
        let inner_col = if pulse_bright { t.bg_rgb } else { t.fg_rgb };
        let w_outer = 5.0;
        let w_inner = 2.2;

        for r in 0..BS_ROWS {
            for c in 0..BS_COLS {
                let Some(cell) = self.grid[r][c] else {
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
                    if !(0..BS_COLS as i32).contains(&nc) || !(0..BS_ROWS as i32).contains(&nr) {
                        return false;
                    }
                    self.grid[nr as usize][nc as usize]
                        .map(|nb| nb.armed_square && nb.kind == cell.kind)
                        .unwrap_or(false)
                };

                if !same_at(c as i32, r as i32 - 1) {
                    ui.plugin_draw_line(x0, y0, x1, y0, w_outer, outer_col);
                    ui.plugin_draw_line(x0, y0, x1, y0, w_inner, inner_col);
                }
                if !same_at(c as i32, r as i32 + 1) {
                    ui.plugin_draw_line(x0, y1, x1, y1, w_outer, outer_col);
                    ui.plugin_draw_line(x0, y1, x1, y1, w_inner, inner_col);
                }
                if !same_at(c as i32 - 1, r as i32) {
                    ui.plugin_draw_line(x0, y0, x0, y1, w_outer, outer_col);
                    ui.plugin_draw_line(x0, y0, x0, y1, w_inner, inner_col);
                }
                if !same_at(c as i32 + 1, r as i32) {
                    ui.plugin_draw_line(x1, y0, x1, y1, w_outer, outer_col);
                    ui.plugin_draw_line(x1, y0, x1, y1, w_inner, inner_col);
                }
            }
        }

        let fr = BS_ROWS - 1;
        let floor_armed =
            (0..BS_COLS).all(|c| self.grid[fr][c].map(|x| x.armed_row).unwrap_or(false));
        if floor_armed {
            let rx = lay.grid_x + 1.0;
            let ry = lay.grid_y + (fr as f32) * tile + 1.0;
            let rw = lay.grid_w - 2.0;
            let rh = tile - 2.0;
            ui.plugin_stroke_rect(rx, ry, rw, rh, 4.0, w_outer, outer_col);
            ui.plugin_stroke_rect(rx + 2.0, ry + 2.0, rw - 4.0, rh - 4.0, 3.0, w_inner, inner_col);
        }
    }

    pub fn render(
        &self,
        ui: &mut UiEngine,
        t: ThemeColors,
        header: &HeaderState<'_>,
        uk: bool,
        win_w: u32,
        win_h: u32,
    ) {
        if self.menu_open {
            let music_lbl = if self.music_playing {
                tr(uk, "Pause Music", "Пауза музики")
            } else {
                tr(uk, "Play Music", "Увімкнути музику")
            };
            let diff_lbl = match self.difficulty {
                0 => tr(uk, "Chill", "Легка"),
                2 => tr(uk, "Overtime", "Хардкор"),
                _ => tr(uk, "Normal", "Стандарт"),
            };
            let sync_lbl = format!("+{} ms", BS_SYNC_OFFSETS_MS[self.latency_idx]);
            let hint_lbl = if self.show_hint {
                tr(uk, "On", "Увімк.")
            } else {
                tr(uk, "Off", "Вимк.")
            };
            let items = [
                PluginMenuRow::action(tr(uk, "Resume", "Продовжити")),
                PluginMenuRow::action(tr(uk, "Restart", "Почати знову")),
                PluginMenuRow::action(music_lbl),
                PluginMenuRow::option(tr(uk, "Difficulty", "Складність"), diff_lbl),
                PluginMenuRow::option(tr(uk, "Audio Sync", "Синхронізація"), &sync_lbl),
                PluginMenuRow::option(tr(uk, "Rules Hint", "Підказка"), hint_lbl),
                PluginMenuRow::action(tr(uk, "Quit", "Вихід")),
            ];
            render_native_plugin_menu(
                ui,
                t,
                header,
                tr(uk, "Beat Stack", "Біт Стек"),
                &items,
                self.menu_sel,
            );
            return;
        }

        let (lw, lh) = ui.plugin_begin_ex(t, t.bg_rgb, Some(header), win_w, win_h, true);
        let lay = BsComputedLayout::compute(lw, lh);
        let tile = lay.tile;
        let bf = self.beat_frac;
        let strong_hi = self.strong_pulse > 0.35;

        // 1. Symmetric Outer Warehouse Shaft Frame (ZERO left girder protrusion & ZERO Sweep Line!)
        ui.plugin_stroke_rect(
            lay.shaft_x,
            lay.shaft_y,
            lay.shaft_w,
            lay.shaft_h,
            BS_SPEC.frame_radius,
            BS_SPEC.wall_stroke,
            t.fg_rgb,
        );

        // 2. Gantry Rail + Strong-Beat Rhythm Tick Markers & Column Guides
        let rail_y = lay.shaft_y + tile * BS_SPEC.rail_top_ratio;
        let rail_h = tile * BS_SPEC.rail_h_ratio;
        ui.plugin_fill_rect(lay.grid_x, rail_y, lay.grid_w, rail_h, 0.0, t.fg2_rgb);
        ui.plugin_draw_line(lay.grid_x, rail_y, lay.grid_x + lay.grid_w, rail_y, 2.5, t.fg_rgb);
        ui.plugin_draw_line(
            lay.grid_x,
            rail_y + rail_h,
            lay.grid_x + lay.grid_w,
            rail_y + rail_h,
            2.5,
            t.fg_rgb,
        );
        for c in 0..BS_COLS {
            let tx = lay.grid_x + (c as f32 + 0.5) * tile;
            let tick_pad = if strong_hi { 1.0 } else { rail_h * 0.24 };
            ui.plugin_draw_line(
                tx,
                rail_y + tick_pad,
                tx,
                rail_y + rail_h - tick_pad,
                if strong_hi { 2.5 } else { 1.5 },
                if strong_hi { t.bg_rgb } else { t.fg1_rgb },
            );
        }

        ui.plugin_draw_line(
            lay.grid_x + 2.0,
            lay.grid_y,
            lay.grid_x + lay.grid_w - 2.0,
            lay.grid_y,
            if strong_hi { 2.2 } else { 1.5 },
            if strong_hi { t.fg2_rgb } else { t.fg1_rgb },
        );
        for c in 1..BS_COLS {
            let gx = lay.grid_x + (c as f32) * tile;
            ui.plugin_draw_line(
                gx,
                lay.grid_y + 4.0,
                gx,
                lay.grid_y + lay.grid_h - 4.0,
                1.0,
                t.fg1_rgb,
            );
        }

        // 3. Roller Floor Plate
        let floor_y = lay.grid_y + lay.grid_h;
        ui.plugin_fill_rect(lay.grid_x, floor_y, lay.grid_w, lay.floor_plate_h, 0.0, t.fg2_rgb);
        ui.plugin_draw_line(
            lay.grid_x,
            floor_y,
            lay.grid_x + lay.grid_w,
            floor_y,
            BS_SPEC.wall_stroke,
            t.fg_rgb,
        );
        let rr = (lay.floor_plate_h * 0.28).clamp(2.5, 5.5);
        for c in 0..BS_COLS {
            ui.plugin_fill_circle(
                lay.grid_x + (c as f32 + 0.5) * tile,
                floor_y + lay.floor_plate_h * 0.54,
                rr,
                t.bg_rgb,
            );
        }

        // 4. Grounded Crates + Pulsing Armed Group Highlights & Falling Crates
        for r in 0..BS_ROWS {
            for c in 0..BS_COLS {
                if let Some(cell) = self.grid[r][c] {
                    let x = lay.grid_x + (c as f32) * tile;
                    let off = (cell.settle_px_frac as f32 * 0.01) * tile * 0.65;
                    let y = lay.grid_y + (r as f32) * tile - off;
                    Self::draw_crate_wasm(ui, t, x, y, tile, cell.kind, cell.is_armed(), false, bf);
                }
            }
        }
        self.draw_armed_highlights(ui, t, &lay);

        for fc in &self.falling {
            let x = lay.grid_x + (fc.col as f32) * tile;
            let y = lay.grid_y + ((fc.row as f32) + fc.snap_anim * 0.45) * tile;
            Self::draw_crate_wasm(ui, t, x, y, tile, fc.kind, false, false, bf);
        }

        // 5. Cranes
        for crane in &self.cranes {
            if !crane.active {
                continue;
            }
            let cx = lay.grid_x + (crane.vis_col + 0.5) * tile;
            let hw = tile * 0.58;
            let hh = tile * 0.34;
            let hx = cx - hw * 0.5;
            let hy = rail_y - 2.0;
            let flash = crane.mode == BsCraneMode::Telegraph && bf < 0.5;
            ui.plugin_fill_rect(hx, hy, hw, hh, 3.0, if flash { t.bg_rgb } else { t.fg_rgb });
            ui.plugin_stroke_rect(hx, hy, hw, hh, 3.0, 2.5, t.fg_rgb);
            let ec = if flash { t.fg_rgb } else { t.bg_rgb };
            let es = (tile * 0.08).max(3.0);
            ui.plugin_fill_rect(cx - hw * 0.24, hy + hh * 0.34, es, es, 1.0, ec);
            ui.plugin_fill_rect(cx + hw * 0.24 - es, hy + hh * 0.34, es, es, 1.0, ec);
            let st_top = hy + hh;
            let st_bot = st_top + tile * 0.16;
            ui.plugin_draw_line(cx, st_top, cx, st_bot, 3.5, t.fg_rgb);
            let sp = if crane.cargo.is_some() {
                if flash { tile * 0.54 } else { tile * 0.48 }
            } else {
                tile * (0.56 + crane.open_flash * 0.12)
            };
            let jb = st_bot + tile * 0.44;
            ui.plugin_draw_line(cx - sp, st_bot, cx + sp, st_bot, 3.0, t.fg_rgb);
            ui.plugin_draw_line(cx - sp, st_bot, cx - sp, jb, 3.0, t.fg_rgb);
            ui.plugin_draw_line(cx + sp, st_bot, cx + sp, jb, 3.0, t.fg_rgb);
            if let Some(kind) = crane.cargo {
                let cy = st_bot + tile * 0.04;
                Self::draw_crate_wasm(ui, t, cx - tile * 0.5, cy, tile, kind, false, flash, bf);
                if crane.mode == BsCraneMode::Telegraph {
                    let ay = cy + tile + 3.0;
                    let aw = tile * 0.22;
                    let col = if flash { t.fg_rgb } else { t.fg2_rgb };
                    ui.plugin_draw_line(cx - aw, ay, cx, ay + aw * 0.65, 3.0, col);
                    ui.plugin_draw_line(cx + aw, ay, cx, ay + aw * 0.65, 3.0, col);
                }
            }
        }

        // 6. Worker
        let (vx, vy, airborne, push_dir) = self.worker_vis();
        let dir = self.worker_facing as f32;
        let wx = lay.grid_x + vx * tile;
        let wy = lay.grid_y + vy * tile;
        let wcx = wx + tile * 0.5 + (push_dir as f32) * tile * 0.08;
        if !airborne {
            ui.plugin_fill_rect(
                wcx - tile * 0.28,
                wy + tile - 4.0,
                tile * 0.56,
                4.0,
                2.0,
                t.fg2_rgb,
            );
        }
        let leg_y = wy + tile * 0.72;
        let leg_h = if airborne { tile * 0.18 } else { tile * 0.25 };
        let leg_w = (tile * 0.15).max(4.0);
        let stride = if matches!(self.worker_action, BsWorkerAction::Walking { .. })
            || self.worker_walk_phase % 2 == 1
        {
            tile * 0.06
        } else {
            0.0
        };
        ui.plugin_fill_rect(wcx - tile * 0.20 - stride, leg_y, leg_w, leg_h, 2.0, t.fg_rgb);
        ui.plugin_fill_rect(wcx + tile * 0.06 + stride, leg_y, leg_w, leg_h, 2.0, t.fg_rgb);
        let bw = tile * 0.44;
        let bh = tile * 0.36;
        let bx = wcx - bw * 0.5;
        let by = wy + tile * 0.40;
        ui.plugin_fill_rect(bx, by, bw, bh, 3.0, t.fg_rgb);
        let stw = (tile * 0.08).max(2.5);
        ui.plugin_fill_rect(bx + tile * 0.07, by + 3.0, stw, bh * 0.52, 1.0, t.bg_rgb);
        ui.plugin_fill_rect(bx + bw - tile * 0.07 - stw, by + 3.0, stw, bh * 0.52, 1.0, t.bg_rgb);
        if push_dir != 0 {
            let ax = if push_dir > 0 {
                bx + bw - 2.0
            } else {
                bx - tile * 0.20
            };
            ui.plugin_fill_rect(ax, by + tile * 0.05, tile * 0.22, (tile * 0.11).max(3.5), 2.0, t.fg_rgb);
        }
        let hdw = tile * 0.36;
        let hdh = tile * 0.24;
        let hdx = wcx - hdw * 0.5;
        let hdy = wy + tile * 0.18;
        ui.plugin_fill_rect(hdx, hdy, hdw, hdh, 2.0, t.bg_rgb);
        ui.plugin_stroke_rect(hdx, hdy, hdw, hdh, 2.0, 2.2, t.fg_rgb);
        ui.plugin_fill_rect(
            wcx + dir * tile * 0.06 - 1.5,
            hdy + hdh * 0.32,
            3.5,
            3.5,
            1.0,
            t.fg_rgb,
        );
        let nx = if dir > 0.0 {
            hdx + hdw - 1.0
        } else {
            hdx - tile * 0.09
        };
        ui.plugin_fill_rect(nx, hdy + hdh * 0.38, tile * 0.10, (tile * 0.08).max(2.5), 1.0, t.fg_rgb);
        let hat_w = tile * 0.44;
        let hat_h = tile * 0.16;
        let hat_x = wcx - hat_w * 0.5;
        let hat_y = wy + tile * 0.05;
        ui.plugin_fill_rect(hat_x, hat_y, hat_w, hat_h, 3.0, t.fg2_rgb);
        ui.plugin_stroke_rect(hat_x, hat_y, hat_w, hat_h, 3.0, 2.2, t.fg_rgb);
        let brim_x = if dir > 0.0 {
            hat_x
        } else {
            hat_x - tile * 0.10
        };
        ui.plugin_fill_rect(brim_x, hat_y + hat_h - 3.0, hat_w + tile * 0.10, 3.5, 1.0, t.fg_rgb);
        if self.worker_danger && bf < 0.72 {
            let sy = hat_y - tile * 0.16;
            ui.plugin_draw_line(wcx, sy - 6.0, wcx, sy + 1.0, 3.0, t.fg_rgb);
            ui.plugin_fill_circle(wcx, sy + 5.0, 2.0, t.fg_rgb);
        }

        // 7. Shatter Particles & Combo Callouts
        for p in &self.particles {
            let px = lay.grid_x + p.x * tile;
            let py = lay.grid_y + p.y * tile;
            let sz = (p.life * tile * 0.17).max(2.5);
            ui.plugin_fill_rect(
                px - sz * 0.5,
                py - sz * 0.5,
                sz,
                sz,
                1.5,
                if p.shade >= 3 { t.fg_rgb } else { t.fg2_rgb },
            );
        }
        if let Some((ref txt, cc, ry, _)) = self.callout {
            let cx = lay.grid_x + cc * tile;
            let cy = lay.grid_y + ry * tile;
            let tw = ui.plugin_measure_text(txt, BS_SPEC.font_callout, 3) + 20.0;
            ui.plugin_fill_rect(cx - tw * 0.5, cy - 16.0, tw, 30.0, 7.0, t.fg_rgb);
            ui.plugin_draw_text(cx, cy - 1.0, txt, BS_SPEC.font_callout, 3, 1, t.bg_rgb);
        }

        // 8. Concise 2-Line Onboarding Rules Hint Banner
        if self.show_hint && self.lose_reason.is_none() {
            let hw = (lay.grid_w - 20.0).max(260.0);
            let hx = lay.grid_x + (lay.grid_w - hw) * 0.5;
            let hy = lay.grid_y + 6.0;
            ui.plugin_fill_rect(hx, hy, hw, BS_SPEC.hint_card_h, BS_SPEC.hint_radius, t.bg_rgb);
            ui.plugin_stroke_rect(
                hx,
                hy,
                hw,
                BS_SPEC.hint_card_h,
                BS_SPEC.hint_radius,
                BS_SPEC.thin_stroke,
                t.fg2_rgb,
            );
            ui.plugin_draw_text(
                hx + hw * 0.5,
                hy + 16.0,
                tr(
                    uk,
                    "Push crates into 2×2 matching squares or a full floor row — matches explode on the strong beat!",
                    "Складай ящики у квадрати 2×2 або повний ряд підлоги — вибух на сильну долю!",
                ),
                BS_SPEC.font_hint_main,
                2,
                1,
                t.fg_rgb,
            );
            ui.plugin_draw_text(
                hx + hw * 0.5,
                hy + 35.0,
                tr(
                    uk,
                    "B: Jump · L1/R1: Diag Jump · START: Pause",
                    "B: Стрибок · L1/R1: Діаг. стрибок · START: Пауза",
                ),
                BS_SPEC.font_hint_sub,
                1,
                1,
                t.fg2_rgb,
            );
        }

        // 9. Floor HUD Strip
        ui.plugin_fill_rect(
            lay.hud_x,
            lay.hud_y,
            lay.hud_w,
            lay.hud_h,
            BS_SPEC.hud_radius,
            t.bg_rgb,
        );
        ui.plugin_stroke_rect(
            lay.hud_x,
            lay.hud_y,
            lay.hud_w,
            lay.hud_h,
            BS_SPEC.hud_radius,
            if strong_hi {
                BS_SPEC.thin_stroke + 0.8
            } else {
                BS_SPEC.thin_stroke
            },
            if strong_hi { t.fg_rgb } else { t.fg2_rgb },
        );
        let col_w = lay.hud_w / 4.0;
        let lbl_y = lay.hud_y + 16.0;
        let val_y = lay.hud_y + 39.0;

        let c0 = lay.hud_x + col_w * 0.5;
        ui.plugin_draw_text(
            c0,
            lbl_y,
            tr(uk, "SCORE", "РАХУНОК"),
            BS_SPEC.font_hud_lbl,
            2,
            1,
            t.fg2_rgb,
        );
        ui.plugin_draw_text(
            c0,
            val_y,
            &self.score.to_string(),
            BS_SPEC.font_hud_val,
            3,
            1,
            t.fg_rgb,
        );

        let c1 = lay.hud_x + col_w * 1.5;
        ui.plugin_draw_text(
            c1,
            lbl_y,
            tr(uk, "HIGH", "РЕКОРД"),
            BS_SPEC.font_hud_lbl,
            2,
            1,
            t.fg2_rgb,
        );
        ui.plugin_draw_text(
            c1,
            val_y,
            &self.hi_score.max(self.score).to_string(),
            BS_SPEC.font_hud_val,
            3,
            1,
            t.fg_rgb,
        );

        let c2 = lay.hud_x + col_w * 2.5;
        let w_lbl = format!("{} {} · CHAIN", tr(uk, "W", "ХВ"), self.wave);
        ui.plugin_draw_text(c2, lbl_y, &w_lbl, BS_SPEC.font_hud_lbl, 2, 1, t.fg2_rgb);
        ui.plugin_draw_text(
            c2,
            val_y,
            &format!("×{}", self.chain),
            BS_SPEC.font_hud_val,
            3,
            1,
            t.fg_rgb,
        );

        let c3 = lay.hud_x + col_w * 3.5;
        let src_lbl = if self.music_playing {
            tr(uk, "LIVE AUDIO", "ЖИВИЙ ЗВУК")
        } else {
            tr(uk, "INTERNAL CLOCK", "ВНУТР. РИТМ")
        };
        ui.plugin_draw_text(c3, lbl_y, src_lbl, BS_SPEC.font_hud_lbl, 2, 1, t.fg2_rgb);
        let bpm_s = format!("{:.0} BPM", self.effective_bpm.round());
        let bpm_w = ui.plugin_measure_text(&bpm_s, BS_SPEC.font_hud_val, 3);
        ui.plugin_fill_circle(
            c3 - bpm_w * 0.5 - 12.0,
            val_y,
            4.5 + self.bpm_pulse * 2.0 + self.strong_pulse * 2.8,
            if self.strong_pulse > 0.35 || self.bpm_pulse > 0.55 {
                t.fg_rgb
            } else {
                t.fg2_rgb
            },
        );
        ui.plugin_draw_text(
            c3 + 4.0,
            val_y,
            &bpm_s,
            BS_SPEC.font_hud_val + self.strong_pulse * 1.5,
            3,
            1,
            t.fg_rgb,
        );

        for d in 1..4 {
            let dx = lay.hud_x + col_w * (d as f32);
            ui.plugin_draw_line(dx, lay.hud_y + 10.0, dx, lay.hud_y + lay.hud_h - 10.0, 1.5, t.fg1_rgb);
        }

        if let Some(reason) = self.lose_reason {
            let cw = (lay.grid_w * 0.76).clamp(320.0, 520.0);
            let ch = 168.0;
            let cx = lay.grid_x + (lay.grid_w - cw) * 0.5;
            let cy = lay.grid_y + (lay.grid_h - ch) * 0.5;
            ui.plugin_fill_rect(cx, cy, cw, ch, 14.0, t.bg_rgb);
            ui.plugin_stroke_rect(cx, cy, cw, ch, 14.0, 4.0, t.fg_rgb);
            let head = match reason {
                BsLoseReason::Crushed => tr(uk, "CRUSHED!", "ПРИДАВИЛО!"),
                BsLoseReason::ShaftJammed => tr(uk, "SHAFT JAMMED!", "ШАХТУ ЗАБЛОКОВАНО!"),
            };
            ui.plugin_draw_text(
                cx + cw * 0.5,
                cy + 44.0,
                head,
                BS_SPEC.font_banner_lg,
                3,
                1,
                t.fg_rgb,
            );
            let st = format!(
                "{}: {}   ·   {}: ×{}",
                tr(uk, "Score", "Рахунок"),
                self.score,
                tr(uk, "Max Chain", "Макс. ланцюг"),
                self.max_chain
            );
            ui.plugin_draw_text(
                cx + cw * 0.5,
                cy + 92.0,
                &st,
                BS_SPEC.font_banner_sm,
                2,
                1,
                t.fg2_rgb,
            );
            ui.plugin_draw_text(
                cx + cw * 0.5,
                cy + 134.0,
                tr(
                    uk,
                    "A: Play Again  ·  START: Pause / Quit",
                    "A: Ще раз  ·  START: Пауза / Вихід",
                ),
                15.0,
                1,
                1,
                t.fg_rgb,
            );
        }
    }
}

#[cfg(test)]
mod beat_stack_wasm_tests {
    use super::*;

    fn silent_snap() -> AudioSnapshot<'static> {
        AudioSnapshot {
            playing: false,
            track_title: "",
            pos_ms: 0,
            dur_ms: 0,
            sample_rate: 48_000,
        }
    }

    #[test]
    fn wasm_beat_stack_140_bpm_cranes_strong_beat_and_b_jump_parity() {
        // 1. Verify 140 BPM lock within ±1 BPM with loud off-beat hats
        let sr = 48_000u32;
        let total = (6.0 * (sr as f32)) as usize;
        let mut pcm = vec![0i16; total * 2];
        let beat_sec = 60.0 / 140.0;
        for f in 0..total {
            let t = (f as f32) / (sr as f32);
            let bp = t / beat_sec;
            let in_b = (bp - bp.floor()) * beat_sec;
            let kick = if in_b < 0.075 {
                (2.0 * core::f32::consts::PI * 62.0 * in_b).sin()
                    * (1.0 - in_b / 0.075).powi(2)
                    * 0.80
            } else {
                0.0
            };
            let eighth = t % (beat_sec * 0.5);
            let hat = if eighth < 0.025 {
                (2.0 * core::f32::consts::PI * 2600.0 * t).sin() * (1.0 - eighth / 0.025) * 0.45
            } else {
                0.0
            };
            let s = ((kick + hat).clamp(-0.98, 0.98) * 32767.0) as i16;
            pcm[f * 2] = s;
            pcm[f * 2 + 1] = s;
        }
        let mut p = BeatStackPlugin::new(99);
        let mut cur = 0.0f32;
        while cur < 5.5 {
            cur += 1.0 / 60.0;
            let ef = ((cur * (sr as f32)) as usize).min(total);
            let sf = ef.saturating_sub(1024);
            let _ = p.update(
                1.0 / 60.0,
                0,
                0,
                &pcm[sf * 2..ef * 2],
                AudioSnapshot {
                    playing: true,
                    track_title: "140 Test",
                    pos_ms: (cur * 1000.0) as u32,
                    dur_ms: 6000,
                    sample_rate: sr,
                },
                false,
            );
        }
        assert!(
            (p.effective_bpm - 140.0).abs() <= 1.0,
            "expected 140 BPM ± 1, got {:.2}",
            p.effective_bpm
        );

        // 2. Verify B jumps, Hold B never exits, and X/Y never jump
        let mut g = BeatStackPlugin::new(42);
        g.grid = [[None; BS_COLS]; BS_ROWS];
        g.falling.clear();
        for c in &mut g.cranes {
            c.active = false;
        }
        let floor = BS_ROWS - 1;
        g.worker_col = 4;
        g.worker_row = floor as i32;

        // X toggles hint without jumping; Y does not jump
        let act_xy = g.update(1.0 / 60.0, btn::X | btn::Y, 0, &[], silent_snap(), false);
        assert!(matches!(act_xy, PluginAction::Continue));
        assert_eq!(g.worker_action, BsWorkerAction::Idle);

        // B jumps vertically, and holding B for > 1.5s NEVER returns PluginAction::Exit!
        let act_b = g.update(1.0 / 60.0, btn::B, btn::B, &[], silent_snap(), false);
        assert!(matches!(act_b, PluginAction::Continue));
        assert!(matches!(g.worker_action, BsWorkerAction::Jumping { .. }));
        for _ in 0..100 {
            let act_hold = g.update(1.0 / 60.0, 0, btn::B, &[], silent_snap(), false);
            assert!(matches!(act_hold, PluginAction::Continue));
        }

        // 3. Verify Strong-Beat 2x2 detonation
        for c in &mut g.cranes {
            c.active = false;
        }
        g.grid[floor][2] = Some(BsGroundedCrate::new(BsCrateKind::Heavy));
        g.grid[floor][3] = Some(BsGroundedCrate::new(BsCrateKind::Heavy));
        g.grid[floor - 1][2] = Some(BsGroundedCrate::new(BsCrateKind::Heavy));
        g.grid[floor - 1][3] = Some(BsGroundedCrate::new(BsCrateKind::Heavy));
        g.recompute_armed();
        for r in (floor - 1)..=floor {
            for c in 2..=3 {
                if let Some(ref mut cell) = g.grid[r][c] {
                    cell.armed_sec = 0.4;
                    cell.armed_beats = 1.0;
                }
            }
        }
        assert_eq!(g.step_strong_beat_detonation(false), 4);
        assert_eq!(g.cleared_crates, 4);

        // 4. Verify cranes drop 20+ crates continuously without freezing after drop #2
        let mut cgame = BeatStackPlugin::new(777);
        for _ in 0..(45 * 60) {
            for r in 2..BS_ROWS {
                for c in 0..BS_COLS {
                    cgame.grid[r][c] = None;
                }
            }
            cgame.falling.retain(|f| f.row < 2);
            cgame.worker_col = 0;
            cgame.worker_row = floor as i32;
            let _ = cgame.update(1.0 / 60.0, 0, 0, &[], silent_snap(), false);
        }
        assert!(cgame.drops_done >= 20, "drops_done={}", cgame.drops_done);
    }
}
