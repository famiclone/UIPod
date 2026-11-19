use uipod_ui::layout::LIST_VISIBLE_ROWS;
use uipod_ui::{
    FileIconKind, HeaderSourceIcon, HeaderState, KeyboardKey, KeyboardState, ListRow,
    ListScreenState, NowPlayingState, PlayMode, RightAccessory, ThemeColors, UiEngine, VizEngine,
    VIZ_PRESETS,
};

use crate::library::{built_in_demo_tracks, decode_cover_256, Track};
use crate::plugins::{btn, ActivePlugin, PluginAction, PluginId};
use crate::themes::{derive_4shade, parse_hex_rgb, THEME_PRESETS};

pub const AUTOEQ_PRESETS: &[&str] = &[
    "Off",
    "Sony WH-1000XM4",
    "Moondrop Aria",
    "Sennheiser HD 650",
    "Koss Porta Pro",
    "AirPods Pro 2",
    "Studio Warm",
];

pub const TRANSITION_NAMES_EN: &[&str] = &["Off", "Slide", "Cover", "Fade", "Pixels", "Blinds"];
pub const TRANSITION_NAMES_UK: &[&str] = &[
    "Вимк.",
    "Зсув",
    "Каскад",
    "Затухання",
    "Пікселі",
    "Жалюзі",
];

const REPEAT_MODES_EN: &[&str] = &["Off", "All", "One", "Shuffle"];
const REPEAT_MODES_UK: &[&str] = &["Вимк.", "Всі", "Один", "Випадково"];
const SLEEP_TIMERS: &[&str] = &["Off", "15m", "30m", "60m"];
const VIZ_DELAYS: &[&str] = &["Off", "10s", "30s", "1m", "2m"];
const VIZ_TRANSITIONS: &[&str] = &["Off", "30s", "60s", "2m"];
const CPU_FREQS: &[&str] = &[
    "Dynamic", "408 MHz", "600 MHz", "816 MHz", "1008 MHz", "1200 MHz", "1416 MHz", "1608 MHz",
    "1800 MHz", "2000 MHz",
];
const DISPLAY_OFF_TIMES: &[&str] = &["Off", "15s", "30s", "1m", "2m", "5m"];
const IDLE_OFF_TIMES: &[&str] = &["Off", "5m", "10m", "15m", "30m"];
const CHANNEL_MODES: &[&str] = &["Stereo", "Mono", "Custom", "Mono Left", "Mono Right"];
const CROSSFEED_MODES: &[&str] = &["Off", "Meier", "Custom"];
const RG_TYPES: &[&str] = &["Off", "Track", "Album", "Track (Shuffle)"];

/// Exact 13 toggleable Main Menu rows from `apps/uipod_mainmenu.c` (`uipod_mainmenu_id`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MainMenuId {
    Browse = 0,
    Files = 1,
    Extras = 2,
    Artists = 3,
    Albums = 4,
    Genres = 5,
    Videos = 6,
    Songs = 7,
    Playlists = 8,
    Podcasts = 9,
    Audiobooks = 10,
    Television = 11,
    Shuffle = 12,
}

const ALL_MAIN_TOGGLES: [MainMenuId; 13] = [
    MainMenuId::Browse,
    MainMenuId::Files,
    MainMenuId::Extras,
    MainMenuId::Artists,
    MainMenuId::Albums,
    MainMenuId::Genres,
    MainMenuId::Videos,
    MainMenuId::Songs,
    MainMenuId::Playlists,
    MainMenuId::Podcasts,
    MainMenuId::Audiobooks,
    MainMenuId::Television,
    MainMenuId::Shuffle,
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MainRowAction {
    Toggleable(MainMenuId),
    Settings,
    NowPlaying,
}

#[derive(Debug, Clone)]
pub enum Screen {
    MainMenu { sel: usize },
    Browse { path: String, sel: usize },
    Files { sel: usize },
    ExtrasMenu { sel: usize },
    PluginsHub { sel: usize },
    PluginCategory { cat: u8, sel: usize },
    PluginActive,
    Downloads { sel: usize },
    Artists { sel: usize },
    ArtistSongs { artist: String, sel: usize },
    Albums { sel: usize },
    AlbumSongs { album: String, sel: usize },
    Genres { sel: usize },
    GenreSongs { genre: String, sel: usize },
    AllSongs { sel: usize },
    Playlists { sel: usize },
    EmptySection { title_en: &'static str, title_uk: &'static str, hint_en: &'static str, hint_uk: &'static str },
    NowPlaying,
    NowPlayingContext { sel: usize },
    SettingsMenu { sel: usize },
    DisplayMenu { sel: usize },
    ThemeList { sel: usize },
    CustomizeThemeMenu { sel: usize },
    MainMenuSettings { sel: usize },
    KeyboardHex {
        target: HexTarget,
        text: String,
        row: usize,
        col: usize,
    },
    SoundMenu { sel: usize },
    EqualizerMenu { sel: usize },
    AutoEqList { sel: usize },
    EffectsMenu { sel: usize },
    TapeModeMenu { sel: usize },
    ReplayGainMenu { sel: usize },
    SourcesMenu { sel: usize },
    SourceServerInfo { is_jellyfin: bool, sel: usize },
    LibraryMenu { sel: usize },
    LibraryFolderList { kind: u8, sel: usize },
    VisualizersMenu { sel: usize },
    VisualizerPresets { sel: usize },
    VisualizerActive,
    PowerMenu { sel: usize },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HexTarget {
    Background,
    Foreground,
    Shade1,
    Shade2,
}

#[derive(Debug, Clone, Copy)]
pub struct DspConfig {
    pub eq_enabled: bool,
    pub autoeq_idx: usize,
    pub eq_precut_tenth_db: i16,
    pub tape_enabled: bool,
    pub tape_wow: u8,
    pub tape_hiss: u8,
    pub tape_crinkle: u8,
    pub tape_sat: u8,
    pub tape_hf_rolloff: u8,
    pub tape_stop_start: bool,
    pub tape_ff_sound: bool,
    pub speed_pct: u16,
    pub menu_click: bool,
    pub rg_type: usize,
    pub rg_noclip: bool,
    pub rg_preamp_db: i8,
    pub balance_pct: i8,
    pub channels_idx: usize,
    pub crossfeed_idx: usize,
}

impl Default for DspConfig {
    fn default() -> Self {
        Self {
            eq_enabled: true,
            autoeq_idx: 0,
            eq_precut_tenth_db: 0,
            tape_enabled: false,
            tape_wow: 35,
            tape_hiss: 22,
            tape_crinkle: 15,
            tape_sat: 30,
            tape_hf_rolloff: 40,
            tape_stop_start: true,
            tape_ff_sound: true,
            speed_pct: 100,
            menu_click: true,
            rg_type: 1,
            rg_noclip: true,
            rg_preamp_db: 0,
            balance_pct: 0,
            channels_idx: 0,
            crossfeed_idx: 0,
        }
    }
}

pub struct UIPodApp {
    pub tracks: Vec<Track>,
    pub current_track: usize,
    pub play_mode: PlayMode,
    pub pos_ms: u32,
    pub dur_ms: u32,

    pub theme: ThemeColors,
    pub theme_idx: Option<usize>,
    pub mono_album_art: bool,
    pub alt_now_playing: bool,
    pub transition_kind: u8,
    pub uk: bool,
    pub source_icon: HeaderSourceIcon,
    pub offline_mode: bool,

    // Exact shipped Main Menu visibility flags (`apps/uipod_mainmenu.c` mm_default)
    pub main_menu_enabled: [bool; 13],

    // Now Playing / Visualizers / Power settings
    pub repeat_idx: usize,
    pub sleep_idx: usize,
    pub viz_delay_idx: usize,
    pub viz_trans_idx: usize,
    pub brightness_pct: u8,
    pub disp_off_idx: usize,
    pub idle_off_idx: usize,
    pub cpu_idx: usize,
    pub charge_limit_pct: u8,

    pub volume_step: i32,
    pub volume_toast_t: f32,
    pub hold_locked: bool,

    pub dsp: DspConfig,

    pub stack: Vec<Screen>,
    pub active_plugin: Option<ActivePlugin>,

    pub trans_t: f32,
    pub trans_dir: i8,

    pub viz_preset: usize,
    pub viz_fade: f32,
    pub viz_custom: bool,

    pending_pressed: u32,
    held_buttons: u32,
    repeat_timer: f32,
    repeat_btn: u32,

    pub outbound_cmds: Vec<String>,
    pub state_dirty: bool,
}

impl UIPodApp {
    pub fn new() -> Self {
        let tracks = built_in_demo_tracks();
        let dur_ms = tracks.first().map(|t| t.duration_ms).unwrap_or(180_000);
        Self {
            tracks,
            current_track: 0,
            play_mode: PlayMode::Pause,
            pos_ms: 0,
            dur_ms,
            theme: THEME_PRESETS[0].colors,
            theme_idx: Some(0),
            mono_album_art: true,
            alt_now_playing: false,
            transition_kind: 1,
            uk: false,
            source_icon: HeaderSourceIcon::MicroSd,
            offline_mode: false,
            // Exact `mm_default` from `apps/uipod_mainmenu.c`:
            // Browse, Files, Extras, Artists, Albums, Genres, Videos ON; rest OFF
            main_menu_enabled: [
                true, true, true, true, true, true, true, false, false, false, false, false, false,
            ],
            repeat_idx: 0,
            sleep_idx: 0,
            viz_delay_idx: 0,
            viz_trans_idx: 2,
            brightness_pct: 80,
            disp_off_idx: 2,
            idle_off_idx: 2,
            cpu_idx: 0,
            charge_limit_pct: 80,
            volume_step: 32,
            volume_toast_t: 0.0,
            hold_locked: false,
            dsp: DspConfig::default(),
            stack: vec![Screen::MainMenu { sel: 0 }],
            active_plugin: None,
            trans_t: 1.0,
            trans_dir: 1,
            viz_preset: 9, // Cassette Deck
            viz_fade: 0.0,
            viz_custom: false,
            pending_pressed: 0,
            held_buttons: 0,
            repeat_timer: 0.0,
            repeat_btn: 0,
            outbound_cmds: Vec::new(),
            state_dirty: true,
        }
    }

    #[inline]
    fn tr<'a>(&self, en: &'a str, uk: &'a str) -> &'a str {
        if self.uk {
            uk
        } else {
            en
        }
    }

    fn main_menu_label(&self, id: MainMenuId) -> &'static str {
        match id {
            MainMenuId::Browse => self.tr("Browse", "Огляд"),
            MainMenuId::Files => self.tr("Files", "Файли"),
            MainMenuId::Extras => self.tr("Extras", "Екстра"),
            MainMenuId::Artists => self.tr("Artists", "Артисти"),
            MainMenuId::Albums => self.tr("Albums", "Альбоми"),
            MainMenuId::Genres => self.tr("Genres", "Жанри"),
            MainMenuId::Videos => self.tr("Videos", "Відео"),
            MainMenuId::Songs => self.tr("Songs", "Пісні"),
            MainMenuId::Playlists => self.tr("Playlists", "Плейлисти"),
            MainMenuId::Podcasts => self.tr("Podcasts", "Подкасти"),
            MainMenuId::Audiobooks => self.tr("Audiobooks", "Аудіокниги"),
            MainMenuId::Television => self.tr("Television", "Серіали"),
            MainMenuId::Shuffle => self.tr("Shuffle All", "Перемішати все"),
        }
    }

    fn visible_main_rows(&self) -> Vec<MainRowAction> {
        let mut out = Vec::with_capacity(15);
        for id in ALL_MAIN_TOGGLES {
            if self.main_menu_enabled[id as usize] {
                out.push(MainRowAction::Toggleable(id));
            }
        }
        out.push(MainRowAction::Settings);
        out.push(MainRowAction::NowPlaying);
        out
    }

    fn emit_click(&mut self) {
        if self.dsp.menu_click {
            self.outbound_cmds.push(r#"{"type":"click"}"#.to_string());
        }
    }

    fn emit_dsp(&mut self) {
        let d = self.dsp;
        self.outbound_cmds.push(format!(
            r#"{{"type":"dsp","eq":{},"autoeq":{},"tape":{},"wow":{},"hiss":{},"sat":{},"hf":{},"stopStart":{},"speed":{}}}"#,
            d.eq_enabled,
            d.autoeq_idx,
            d.tape_enabled,
            d.tape_wow,
            d.tape_hiss,
            d.tape_sat,
            d.tape_hf_rolloff,
            d.tape_stop_start,
            d.speed_pct
        ));
        self.state_dirty = true;
    }

    fn track_quiz_pool(&self) -> Vec<(String, u32)> {
        self.tracks
            .iter()
            .map(|t| (format!("{} - {}", t.artist, t.title), t.duration_ms))
            .collect()
    }

    fn emit_play_track_at(&mut self, idx: usize, pos_ms: u32) {
        if self.tracks.is_empty() {
            return;
        }
        let idx = idx % self.tracks.len();
        self.current_track = idx;
        self.dur_ms = self.tracks[idx].duration_ms;
        self.pos_ms = pos_ms.min(self.dur_ms);
        self.play_mode = PlayMode::Play;
        self.outbound_cmds.push(format!(
            r#"{{"type":"playTrackAt","index":{idx},"posMs":{pos_ms}}}"#
        ));
        self.state_dirty = true;
    }

    fn emit_play_track(&mut self, idx: usize) {
        if self.tracks.is_empty() {
            return;
        }
        let idx = idx % self.tracks.len();
        self.current_track = idx;
        self.pos_ms = 0;
        self.dur_ms = self.tracks[idx].duration_ms;
        self.play_mode = PlayMode::Play;
        self.outbound_cmds
            .push(format!(r#"{{"type":"playTrack","index":{idx}}}"#));
        self.state_dirty = true;
    }

    fn emit_toggle_play_pause(&mut self) {
        if self.play_mode == PlayMode::Play {
            self.play_mode = PlayMode::Pause;
        } else {
            self.play_mode = PlayMode::Play;
        }
        self.outbound_cmds.push(format!(
            r#"{{"type":"setPlaying","playing":{},"trackIndex":{}}}"#,
            self.play_mode == PlayMode::Play,
            self.current_track
        ));
        self.state_dirty = true;
    }

    fn emit_seek_delta(&mut self, delta_ms: i32) {
        let next = (self.pos_ms as i64 + delta_ms as i64).clamp(0, self.dur_ms as i64) as u32;
        self.pos_ms = next;
        self.outbound_cmds
            .push(format!(r#"{{"type":"seek","posMs":{next}}}"#));
    }

    pub fn volume_db(&self) -> i32 {
        ((self.volume_step - 40) as f32 * 1.5).round() as i32
    }

    pub fn adjust_volume(&mut self, delta: i32) {
        self.volume_step = (self.volume_step + delta).clamp(0, 40);
        self.volume_toast_t = 1.6;
        let norm = self.volume_step as f32 / 40.0;
        self.outbound_cmds
            .push(format!(r#"{{"type":"volume","norm":{norm:.3},"db":{}}}"#, self.volume_db()));
        self.state_dirty = true;
    }

    pub fn set_hold(&mut self, held: bool) {
        self.hold_locked = held;
        self.state_dirty = true;
    }

    pub fn button_down(&mut self, mask: u32) {
        if self.hold_locked {
            return;
        }
        self.pending_pressed |= mask;
        self.held_buttons |= mask;
        self.repeat_btn = mask;
        self.repeat_timer = 0.0;
    }

    pub fn button_up(&mut self, mask: u32) {
        self.held_buttons &= !mask;
        if (self.repeat_btn & mask) != 0 {
            self.repeat_btn = 0;
            self.repeat_timer = 0.0;
        }
    }

    pub fn button_long_press(&mut self, mask: u32, ui: &mut UiEngine) {
        if self.hold_locked {
            return;
        }
        if (mask & btn::B) != 0 {
            self.emit_click();
            self.active_plugin = None;
            ui.snapshot_fbo_for_transition();
            self.stack.clear();
            self.stack.push(Screen::MainMenu { sel: 0 });
            self.start_transition(-1);
            self.state_dirty = true;
        } else if (mask & btn::A) != 0 && matches!(self.stack.last(), Some(Screen::NowPlaying)) {
            self.emit_click();
            self.push_screen(Screen::NowPlayingContext { sel: 0 }, ui);
        }
    }

    fn start_transition(&mut self, dir: i8) {
        if self.transition_kind == 0 {
            self.trans_t = 1.0;
        } else {
            self.trans_t = 0.0;
            self.trans_dir = dir;
        }
    }

    fn push_screen(&mut self, screen: Screen, ui: &mut UiEngine) {
        ui.snapshot_fbo_for_transition();
        self.stack.push(screen);
        self.start_transition(1);
        self.state_dirty = true;
    }

    fn pop_screen(&mut self, ui: &mut UiEngine) {
        if self.stack.len() > 1 {
            ui.snapshot_fbo_for_transition();
            self.stack.pop();
            self.start_transition(-1);
            self.state_dirty = true;
        }
    }

    pub fn jump_to_screen(&mut self, screen_id: &str, ui: &mut UiEngine, viz: &mut VizEngine) {
        self.active_plugin = None;
        ui.snapshot_fbo_for_transition();
        match screen_id {
            "main" => {
                self.stack = vec![Screen::MainMenu { sel: 0 }];
            }
            "now_playing" => {
                self.stack = vec![Screen::MainMenu { sel: 0 }, Screen::NowPlaying];
            }
            "visualizer" => {
                viz.set_preset(self.viz_preset);
                self.viz_fade = 0.0;
                self.stack = vec![
                    Screen::MainMenu { sel: 0 },
                    Screen::SettingsMenu { sel: 5 },
                    Screen::VisualizersMenu { sel: 0 },
                    Screen::VisualizerPresets {
                        sel: self.viz_preset,
                    },
                    Screen::VisualizerActive,
                ];
            }
            "themes" => {
                self.stack = vec![
                    Screen::MainMenu { sel: 0 },
                    Screen::SettingsMenu { sel: 0 },
                    Screen::DisplayMenu { sel: 0 },
                    Screen::ThemeList {
                        sel: self.theme_idx.unwrap_or(0),
                    },
                ];
            }
            "keyboard" => {
                let hex = format!("{:06X}", self.theme.bg_rgb);
                self.stack = vec![
                    Screen::MainMenu { sel: 0 },
                    Screen::SettingsMenu { sel: 0 },
                    Screen::DisplayMenu { sel: 1 },
                    Screen::CustomizeThemeMenu { sel: 0 },
                    Screen::KeyboardHex {
                        target: HexTarget::Background,
                        text: hex,
                        row: 0,
                        col: 0,
                    },
                ];
            }
            "sound" => {
                self.stack = vec![
                    Screen::MainMenu { sel: 0 },
                    Screen::SettingsMenu { sel: 1 },
                    Screen::SoundMenu { sel: 0 },
                ];
            }
            other if other.starts_with("plugin:") => {
                let pid = match &other[7..] {
                    "music_quiz" => PluginId::MusicQuiz,
                    "sound_racer" => PluginId::SoundRacer,
                    "tetris" => PluginId::Tetris,
                    "snake" => PluginId::Snake,
                    "2048" => PluginId::Game2048,
                    "brick_breaker" => PluginId::BrickBreaker,
                    "stopwatch" => PluginId::Stopwatch,
                    _ => PluginId::TextViewer,
                };
                let pool = self.track_quiz_pool();
                self.active_plugin = Some(ActivePlugin::launch(pid, &pool));
                self.stack = vec![
                    Screen::MainMenu { sel: 2 },
                    Screen::ExtrasMenu { sel: 0 },
                    Screen::PluginsHub { sel: 0 },
                    Screen::PluginActive,
                ];
            }
            _ => {}
        }
        self.start_transition(1);
        self.state_dirty = true;
    }

    pub fn add_user_track(
        &mut self,
        title: String,
        artist: String,
        album: String,
        codec: String,
        bitdepth: String,
        samplerate: String,
        duration_ms: u32,
        art_bytes: Option<&[u8]>,
    ) -> usize {
        let id = self.tracks.len();
        let art_rgb24 = art_bytes.and_then(decode_cover_256);
        self.tracks.push(Track {
            id,
            title,
            artist,
            album,
            genre: "Local Upload".to_string(),
            codec,
            bitdepth,
            samplerate,
            duration_ms: duration_ms.max(1000),
            is_user_file: true,
            art_rgb24,
        });
        self.state_dirty = true;
        id
    }

    pub fn sync_audio_state(&mut self, playing: bool, pos_ms: u32, dur_ms: u32) {
        self.play_mode = if playing {
            PlayMode::Play
        } else {
            PlayMode::Pause
        };
        self.pos_ms = pos_ms;
        if dur_ms > 0 {
            self.dur_ms = dur_ms;
        }
    }

    fn header_state<'a>(&self, title: &'a str) -> HeaderState<'a> {
        HeaderState {
            title,
            legend: "",
            play_mode: self.play_mode,
            battery_percent: 87,
            is_charging: false,
            is_hold: self.hold_locked,
            is_downloading: false,
            download_done: 0,
            download_total: 0,
            source_icon: self.source_icon,
        }
    }

    fn artists_list(&self) -> Vec<String> {
        let mut out = Vec::new();
        for t in &self.tracks {
            if !out.contains(&t.artist) {
                out.push(t.artist.clone());
            }
        }
        out
    }

    fn albums_list(&self) -> Vec<String> {
        let mut out = Vec::new();
        for t in &self.tracks {
            if !out.contains(&t.album) {
                out.push(t.album.clone());
            }
        }
        out
    }

    fn genres_list(&self) -> Vec<String> {
        let mut out = Vec::new();
        for t in &self.tracks {
            if !out.contains(&t.genre) {
                out.push(t.genre.clone());
            }
        }
        out
    }

    fn handle_input(
        &mut self,
        dt: f32,
        pcm_interleaved: &[i16],
        ui: &mut UiEngine,
        viz: &mut VizEngine,
    ) {
        let is_plugin = matches!(self.stack.last(), Some(Screen::PluginActive));
        let is_viz = matches!(self.stack.last(), Some(Screen::VisualizerActive));

        if !is_plugin
            && !is_viz
            && (self.held_buttons & (btn::UP | btn::DOWN | btn::LEFT | btn::RIGHT)) != 0
        {
            self.repeat_timer += dt;
            if self.repeat_timer > 0.30 {
                self.repeat_timer -= 0.085;
                self.pending_pressed |=
                    self.held_buttons & (btn::UP | btn::DOWN | btn::LEFT | btn::RIGHT);
            }
        }

        let pressed = self.pending_pressed;
        self.pending_pressed = 0;

        // Global START Play/Pause outside games
        if !is_plugin && (pressed & btn::START) != 0 {
            self.emit_click();
            self.emit_toggle_play_pause();
        }

        if is_plugin {
            let cur_title = self
                .tracks
                .get(self.current_track)
                .map(|t| format!("{} — {}", t.artist, t.title))
                .unwrap_or_default();
            let act = if let Some(ref mut pl) = self.active_plugin {
                pl.update(dt, pressed, self.held_buttons, pcm_interleaved, &cur_title)
            } else {
                PluginAction::Continue
            };
            match act {
                PluginAction::Exit => {
                    self.emit_click();
                    self.active_plugin = None;
                    self.pop_screen(ui);
                }
                PluginAction::PlayTrackAt(idx, pos_ms) => {
                    self.emit_play_track_at(idx, pos_ms);
                }
                PluginAction::EnsurePlaying => {
                    if self.play_mode != PlayMode::Play {
                        self.emit_toggle_play_pause();
                    }
                }
                PluginAction::NextTrack => {
                    let next = (self.current_track + 1) % self.tracks.len().max(1);
                    self.emit_play_track(next);
                }
                PluginAction::PrevTrack => {
                    let prev =
                        (self.current_track + self.tracks.len() - 1) % self.tracks.len().max(1);
                    self.emit_play_track(prev);
                }
                PluginAction::Continue => {}
            }
            return;
        }

        if is_viz {
            if (pressed & btn::B) != 0 {
                self.emit_click();
                self.pop_screen(ui);
                return;
            }
            if (pressed & (btn::L2 | btn::LEFT)) != 0 {
                self.emit_click();
                self.viz_preset = (self.viz_preset + VIZ_PRESETS.len() - 1) % VIZ_PRESETS.len();
                self.viz_custom = false;
                viz.set_preset(self.viz_preset);
                self.state_dirty = true;
            } else if (pressed & (btn::R2 | btn::RIGHT)) != 0 {
                self.emit_click();
                self.viz_preset = (self.viz_preset + 1) % VIZ_PRESETS.len();
                self.viz_custom = false;
                viz.set_preset(self.viz_preset);
                self.state_dirty = true;
            } else if (pressed & btn::L1) != 0 {
                let prev = (self.current_track + self.tracks.len() - 1) % self.tracks.len().max(1);
                self.emit_play_track(prev);
            } else if (pressed & btn::R1) != 0 {
                let next = (self.current_track + 1) % self.tracks.len().max(1);
                self.emit_play_track(next);
            } else if (pressed & btn::X) != 0 {
                self.emit_click();
                viz.toggle_grain();
                self.state_dirty = true;
            }
            return;
        }

        if pressed == 0 {
            return;
        }

        let current = match self.stack.last().cloned() {
            Some(s) => s,
            None => return,
        };

        match current {
            Screen::MainMenu { mut sel } => {
                let rows = self.visible_main_rows();
                let count = rows.len().max(1);
                sel = sel.min(count - 1);
                if (pressed & btn::UP) != 0 {
                    sel = (sel + count - 1) % count;
                    self.emit_click();
                    self.set_top_sel(sel);
                } else if (pressed & btn::DOWN) != 0 {
                    sel = (sel + 1) % count;
                    self.emit_click();
                    self.set_top_sel(sel);
                } else if (pressed & btn::MENU) != 0 {
                    self.emit_click();
                    self.push_screen(Screen::SettingsMenu { sel: 0 }, ui);
                } else if (pressed & btn::A) != 0 {
                    self.emit_click();
                    match rows[sel] {
                        MainRowAction::Settings => {
                            self.push_screen(Screen::SettingsMenu { sel: 0 }, ui);
                        }
                        MainRowAction::NowPlaying => {
                            self.push_screen(Screen::NowPlaying, ui);
                        }
                        MainRowAction::Toggleable(id) => match id {
                            MainMenuId::Browse => self.push_screen(
                                Screen::Browse {
                                    path: "/mnt/SDCARD".to_string(),
                                    sel: 0,
                                },
                                ui,
                            ),
                            MainMenuId::Files => self.push_screen(Screen::Files { sel: 0 }, ui),
                            MainMenuId::Extras => {
                                self.push_screen(Screen::ExtrasMenu { sel: 0 }, ui)
                            }
                            MainMenuId::Artists => {
                                self.push_screen(Screen::Artists { sel: 0 }, ui)
                            }
                            MainMenuId::Albums => self.push_screen(Screen::Albums { sel: 0 }, ui),
                            MainMenuId::Genres => self.push_screen(Screen::Genres { sel: 0 }, ui),
                            MainMenuId::Videos => self.push_screen(
                                Screen::EmptySection {
                                    title_en: "Videos",
                                    title_uk: "Відео",
                                    hint_en: "No videos in /mnt/SDCARD/Media/Videos",
                                    hint_uk: "Немає відео у /mnt/SDCARD/Media/Videos",
                                },
                                ui,
                            ),
                            MainMenuId::Songs => self.push_screen(Screen::AllSongs { sel: 0 }, ui),
                            MainMenuId::Playlists => {
                                self.push_screen(Screen::Playlists { sel: 0 }, ui)
                            }
                            MainMenuId::Podcasts => self.push_screen(
                                Screen::EmptySection {
                                    title_en: "Podcasts",
                                    title_uk: "Подкасти",
                                    hint_en: "No podcasts found",
                                    hint_uk: "Подкасти не знайдено",
                                },
                                ui,
                            ),
                            MainMenuId::Audiobooks => self.push_screen(
                                Screen::EmptySection {
                                    title_en: "Audiobooks",
                                    title_uk: "Аудіокниги",
                                    hint_en: "No audiobooks (.m4b) found",
                                    hint_uk: "Аудіокниги (.m4b) не знайдено",
                                },
                                ui,
                            ),
                            MainMenuId::Television => self.push_screen(
                                Screen::EmptySection {
                                    title_en: "Television",
                                    title_uk: "Серіали",
                                    hint_en: "Connect Jellyfin or add TV folders",
                                    hint_uk: "Підключіть Jellyfin або додайте теки серіалів",
                                },
                                ui,
                            ),
                            MainMenuId::Shuffle => {
                                let idx = (self.current_track + 2) % self.tracks.len().max(1);
                                self.emit_play_track(idx);
                                self.push_screen(Screen::NowPlaying, ui);
                            }
                        },
                    }
                }
            }

            Screen::Browse { path, mut sel } => {
                let count = match path.as_str() {
                    "/mnt/SDCARD" => 3,
                    "/mnt/SDCARD/Media" => 2,
                    _ => self.tracks.len() + 1,
                };
                if self.nav_list(pressed, &mut sel, count.max(1), ui) {
                    self.set_top_sel(sel);
                } else if (pressed & btn::A) != 0 {
                    self.emit_click();
                    match path.as_str() {
                        "/mnt/SDCARD" => match sel {
                            0 => self.push_screen(
                                Screen::Browse {
                                    path: "/mnt/SDCARD/Media".to_string(),
                                    sel: 0,
                                },
                                ui,
                            ),
                            1 => self.push_screen(Screen::PluginsHub { sel: 0 }, ui),
                            _ => {
                                self.active_plugin = Some(ActivePlugin::launch(PluginId::TextViewer, &[]));
                                self.push_screen(Screen::PluginActive, ui);
                            }
                        },
                        "/mnt/SDCARD/Media" => match sel {
                            0 => self.push_screen(
                                Screen::Browse {
                                    path: "/mnt/SDCARD/Media/Music".to_string(),
                                    sel: 0,
                                },
                                ui,
                            ),
                            _ => {
                                self.active_plugin = Some(ActivePlugin::launch(PluginId::TextViewer, &[]));
                                self.push_screen(Screen::PluginActive, ui);
                            }
                        },
                        _ => {
                            if sel < self.tracks.len() {
                                self.emit_play_track(sel);
                                self.push_screen(Screen::NowPlaying, ui);
                            } else {
                                self.active_plugin = Some(ActivePlugin::launch(PluginId::TextViewer, &[]));
                                self.push_screen(Screen::PluginActive, ui);
                            }
                        }
                    }
                }
            }

            Screen::Files { mut sel } => {
                let count = self.tracks.len().max(1);
                if self.nav_list(pressed, &mut sel, count, ui) {
                    self.set_top_sel(sel);
                } else if (pressed & btn::A) != 0 && sel < self.tracks.len() {
                    self.emit_click();
                    self.emit_play_track(sel);
                    self.push_screen(Screen::NowPlaying, ui);
                }
            }

            Screen::ExtrasMenu { mut sel } => {
                if self.nav_list(pressed, &mut sel, 2, ui) {
                    self.set_top_sel(sel);
                } else if (pressed & btn::A) != 0 {
                    self.emit_click();
                    if sel == 0 {
                        self.push_screen(Screen::PluginsHub { sel: 0 }, ui);
                    } else {
                        self.push_screen(Screen::Downloads { sel: 0 }, ui);
                    }
                }
            }

            Screen::PluginsHub { mut sel } => {
                if self.nav_list(pressed, &mut sel, 5, ui) {
                    self.set_top_sel(sel);
                } else if (pressed & btn::A) != 0 {
                    self.emit_click();
                    if sel < 4 {
                        self.push_screen(
                            Screen::PluginCategory {
                                cat: sel as u8,
                                sel: 0,
                            },
                            ui,
                        );
                    }
                }
            }

            Screen::PluginCategory { cat, mut sel } => {
                let count = match cat {
                    0 => 6, // Games: Music Quiz, Sound Racer, Tetris, Snake, 2048, Brick Breaker
                    1 => 1, // Apps: Stopwatch
                    2 => 1, // Demos: LCD Plasma
                    _ => 1, // Viewers: Text Viewer
                };
                if self.nav_list(pressed, &mut sel, count, ui) {
                    self.set_top_sel(sel);
                } else if (pressed & btn::A) != 0 {
                    self.emit_click();
                    let pool = self.track_quiz_pool();
                    match cat {
                        0 => {
                            let pid = match sel {
                                0 => PluginId::MusicQuiz,
                                1 => PluginId::SoundRacer,
                                2 => PluginId::Tetris,
                                3 => PluginId::Snake,
                                4 => PluginId::Game2048,
                                _ => PluginId::BrickBreaker,
                            };
                            self.active_plugin = Some(ActivePlugin::launch(pid, &pool));
                            self.push_screen(Screen::PluginActive, ui);
                        }
                        1 => {
                            self.active_plugin =
                                Some(ActivePlugin::launch(PluginId::Stopwatch, &pool));
                            self.push_screen(Screen::PluginActive, ui);
                        }
                        2 => {
                            self.viz_preset = 0; // LCD Plasma
                            self.viz_custom = false;
                            viz.set_preset(0);
                            self.viz_fade = 0.0;
                            self.push_screen(Screen::VisualizerActive, ui);
                        }
                        _ => {
                            self.active_plugin =
                                Some(ActivePlugin::launch(PluginId::TextViewer, &pool));
                            self.push_screen(Screen::PluginActive, ui);
                        }
                    }
                }
            }

            Screen::Downloads { mut sel } => {
                if self.nav_list(pressed, &mut sel, 1, ui) {
                    self.set_top_sel(sel);
                }
            }

            Screen::EmptySection { .. } => {
                if (pressed & btn::B) != 0 {
                    self.emit_click();
                    self.pop_screen(ui);
                }
            }

            Screen::Artists { mut sel } => {
                let artists = self.artists_list();
                if self.nav_list(pressed, &mut sel, artists.len().max(1), ui) {
                    self.set_top_sel(sel);
                } else if (pressed & btn::A) != 0 {
                    if let Some(artist) = artists.get(sel).cloned() {
                        self.emit_click();
                        self.push_screen(Screen::ArtistSongs { artist, sel: 0 }, ui);
                    }
                }
            }

            Screen::ArtistSongs { artist, mut sel } => {
                let indices: Vec<usize> = self
                    .tracks
                    .iter()
                    .enumerate()
                    .filter(|(_, t)| t.artist == artist)
                    .map(|(i, _)| i)
                    .collect();
                if self.nav_list(pressed, &mut sel, indices.len().max(1), ui) {
                    self.set_top_sel(sel);
                } else if (pressed & btn::A) != 0 {
                    if let Some(&idx) = indices.get(sel) {
                        self.emit_click();
                        self.emit_play_track(idx);
                        self.push_screen(Screen::NowPlaying, ui);
                    }
                }
            }

            Screen::Albums { mut sel } => {
                let albums = self.albums_list();
                if self.nav_list(pressed, &mut sel, albums.len().max(1), ui) {
                    self.set_top_sel(sel);
                } else if (pressed & btn::A) != 0 {
                    if let Some(album) = albums.get(sel).cloned() {
                        self.emit_click();
                        self.push_screen(Screen::AlbumSongs { album, sel: 0 }, ui);
                    }
                }
            }

            Screen::AlbumSongs { album, mut sel } => {
                let indices: Vec<usize> = self
                    .tracks
                    .iter()
                    .enumerate()
                    .filter(|(_, t)| t.album == album)
                    .map(|(i, _)| i)
                    .collect();
                if self.nav_list(pressed, &mut sel, indices.len().max(1), ui) {
                    self.set_top_sel(sel);
                } else if (pressed & btn::A) != 0 {
                    if let Some(&idx) = indices.get(sel) {
                        self.emit_click();
                        self.emit_play_track(idx);
                        self.push_screen(Screen::NowPlaying, ui);
                    }
                }
            }

            Screen::Genres { mut sel } => {
                let genres = self.genres_list();
                if self.nav_list(pressed, &mut sel, genres.len().max(1), ui) {
                    self.set_top_sel(sel);
                } else if (pressed & btn::A) != 0 {
                    if let Some(genre) = genres.get(sel).cloned() {
                        self.emit_click();
                        self.push_screen(Screen::GenreSongs { genre, sel: 0 }, ui);
                    }
                }
            }

            Screen::GenreSongs { genre, mut sel } => {
                let indices: Vec<usize> = self
                    .tracks
                    .iter()
                    .enumerate()
                    .filter(|(_, t)| t.genre == genre)
                    .map(|(i, _)| i)
                    .collect();
                if self.nav_list(pressed, &mut sel, indices.len().max(1), ui) {
                    self.set_top_sel(sel);
                } else if (pressed & btn::A) != 0 {
                    if let Some(&idx) = indices.get(sel) {
                        self.emit_click();
                        self.emit_play_track(idx);
                        self.push_screen(Screen::NowPlaying, ui);
                    }
                }
            }

            Screen::AllSongs { mut sel } => {
                let count = self.tracks.len().max(1);
                if self.nav_list(pressed, &mut sel, count, ui) {
                    self.set_top_sel(sel);
                } else if (pressed & btn::A) != 0 && sel < self.tracks.len() {
                    self.emit_click();
                    self.emit_play_track(sel);
                    self.push_screen(Screen::NowPlaying, ui);
                }
            }

            Screen::Playlists { mut sel } => {
                if self.nav_list(pressed, &mut sel, 3, ui) {
                    self.set_top_sel(sel);
                } else if (pressed & btn::A) != 0 {
                    self.emit_click();
                    let idx = sel % self.tracks.len().max(1);
                    self.emit_play_track(idx);
                    self.push_screen(Screen::NowPlaying, ui);
                }
            }

            Screen::NowPlaying => {
                if (pressed & btn::B) != 0 {
                    self.emit_click();
                    self.pop_screen(ui);
                } else if (pressed & btn::A) != 0 {
                    self.emit_click();
                    self.emit_toggle_play_pause();
                } else if (pressed & btn::X) != 0 {
                    self.emit_click();
                    self.alt_now_playing = !self.alt_now_playing;
                    self.state_dirty = true;
                } else if (pressed & btn::MENU) != 0 {
                    self.emit_click();
                    self.push_screen(Screen::NowPlayingContext { sel: 0 }, ui);
                } else if (pressed & (btn::RIGHT | btn::R1)) != 0 {
                    self.emit_click();
                    let next = (self.current_track + 1) % self.tracks.len().max(1);
                    self.emit_play_track(next);
                } else if (pressed & (btn::LEFT | btn::L1)) != 0 {
                    self.emit_click();
                    let prev =
                        (self.current_track + self.tracks.len() - 1) % self.tracks.len().max(1);
                    self.emit_play_track(prev);
                } else if (pressed & btn::R2) != 0 {
                    self.emit_seek_delta(15_000);
                } else if (pressed & btn::L2) != 0 {
                    self.emit_seek_delta(-15_000);
                } else if (pressed & btn::UP) != 0 {
                    self.adjust_volume(2);
                } else if (pressed & btn::DOWN) != 0 {
                    self.adjust_volume(-2);
                }
            }

            Screen::NowPlayingContext { mut sel } => {
                // Exact 8 rows from `apps/gui/wps.c` line 614:
                // Pause/Resume, Shuffle, Repeat, Sleep Timer, Monochrome album art, Large Album Art, Add to Playlist, Start Visualizer
                if self.nav_list(pressed, &mut sel, 8, ui) {
                    self.set_top_sel(sel);
                } else if (pressed & (btn::A | btn::LEFT | btn::RIGHT)) != 0 {
                    let left = (pressed & btn::LEFT) != 0;
                    self.emit_click();
                    match sel {
                        0 => self.emit_toggle_play_pause(),
                        1 => {
                            let next = (self.current_track + 2) % self.tracks.len().max(1);
                            self.emit_play_track(next);
                            self.pop_screen(ui);
                        }
                        2 => {
                            let n = REPEAT_MODES_EN.len();
                            self.repeat_idx = if left {
                                (self.repeat_idx + n - 1) % n
                            } else {
                                (self.repeat_idx + 1) % n
                            };
                        }
                        3 => {
                            let n = SLEEP_TIMERS.len();
                            self.sleep_idx = if left {
                                (self.sleep_idx + n - 1) % n
                            } else {
                                (self.sleep_idx + 1) % n
                            };
                        }
                        4 => {
                            self.mono_album_art = !self.mono_album_art;
                            self.state_dirty = true;
                        }
                        5 => {
                            self.alt_now_playing = !self.alt_now_playing;
                            self.state_dirty = true;
                        }
                        6 => {
                            self.pop_screen(ui);
                        }
                        _ => {
                            viz.set_preset(self.viz_preset);
                            self.viz_fade = 0.0;
                            self.pop_screen(ui);
                            self.push_screen(Screen::VisualizerActive, ui);
                        }
                    }
                }
            }

            Screen::SettingsMenu { mut sel } => {
                // Exact 9 rows from `apps/menus/main_menu.c` lines 189-193:
                // Display, Sound, Offline Mode, Sources, Library, Visualizers, Power, About, Exit
                if self.nav_list(pressed, &mut sel, 9, ui) {
                    self.set_top_sel(sel);
                } else if (pressed & (btn::A | btn::LEFT | btn::RIGHT)) != 0 {
                    let is_dir = (pressed & (btn::LEFT | btn::RIGHT)) != 0;
                    self.emit_click();
                    match sel {
                        0 if !is_dir => self.push_screen(Screen::DisplayMenu { sel: 0 }, ui),
                        1 if !is_dir => self.push_screen(Screen::SoundMenu { sel: 0 }, ui),
                        2 => {
                            self.offline_mode = !self.offline_mode;
                            self.state_dirty = true;
                        }
                        3 if !is_dir => self.push_screen(Screen::SourcesMenu { sel: 0 }, ui),
                        4 if !is_dir => self.push_screen(Screen::LibraryMenu { sel: 0 }, ui),
                        5 if !is_dir => self.push_screen(Screen::VisualizersMenu { sel: 0 }, ui),
                        6 if !is_dir => self.push_screen(Screen::PowerMenu { sel: 0 }, ui),
                        7 if !is_dir => {
                            self.active_plugin = Some(ActivePlugin::launch(PluginId::TextViewer, &[]));
                            self.push_screen(Screen::PluginActive, ui);
                        }
                        8 if !is_dir => {
                            // Exit returns to Main Menu in browser simulator
                            self.pop_screen(ui);
                        }
                        _ => {}
                    }
                }
            }

            Screen::DisplayMenu { mut sel } => {
                // Exact 6 rows from `apps/menus/power_menu.c` lines 568-571:
                // Theme, Customize Theme, Screen Transition, Language, Monochrome album art, Main menu
                if self.nav_list(pressed, &mut sel, 6, ui) {
                    self.set_top_sel(sel);
                } else if (pressed & (btn::A | btn::LEFT | btn::RIGHT)) != 0 {
                    let left = (pressed & btn::LEFT) != 0;
                    let is_dir = (pressed & (btn::LEFT | btn::RIGHT)) != 0;
                    self.emit_click();
                    match sel {
                        0 => {
                            if is_dir {
                                let cur = self.theme_idx.unwrap_or(0);
                                let next = if left {
                                    cur.saturating_sub(1)
                                } else {
                                    (cur + 1).min(THEME_PRESETS.len() - 1)
                                };
                                self.theme_idx = Some(next);
                                self.theme = THEME_PRESETS[next].colors;
                                self.state_dirty = true;
                            } else {
                                self.push_screen(
                                    Screen::ThemeList {
                                        sel: self.theme_idx.unwrap_or(0),
                                    },
                                    ui,
                                );
                            }
                        }
                        1 if !is_dir => {
                            self.push_screen(Screen::CustomizeThemeMenu { sel: 0 }, ui)
                        }
                        2 => {
                            self.transition_kind = if left {
                                (self.transition_kind + 5) % 6
                            } else {
                                (self.transition_kind + 1) % 6
                            };
                            self.state_dirty = true;
                        }
                        3 => {
                            self.uk = !self.uk;
                            self.state_dirty = true;
                        }
                        4 => {
                            self.mono_album_art = !self.mono_album_art;
                            self.state_dirty = true;
                        }
                        5 if !is_dir => {
                            self.push_screen(Screen::MainMenuSettings { sel: 0 }, ui)
                        }
                        _ => {}
                    }
                }
            }

            Screen::ThemeList { mut sel } => {
                let count = THEME_PRESETS.len();
                if self.nav_list(pressed, &mut sel, count, ui) {
                    self.set_top_sel(sel);
                    self.theme = THEME_PRESETS[sel].colors;
                    self.theme_idx = Some(sel);
                    self.state_dirty = true;
                } else if (pressed & btn::A) != 0 {
                    self.emit_click();
                    self.theme = THEME_PRESETS[sel].colors;
                    self.theme_idx = Some(sel);
                    self.state_dirty = true;
                    self.pop_screen(ui);
                }
            }

            Screen::CustomizeThemeMenu { mut sel } => {
                // Exact 4 rows from `apps/menus/power_menu.c` (`CUST_ROW_COUNT = 4`):
                // Background, Foreground, Secondary color 1, Secondary color 2
                if self.nav_list(pressed, &mut sel, 4, ui) {
                    self.set_top_sel(sel);
                } else if (pressed & btn::A) != 0 {
                    self.emit_click();
                    let (target, rgb) = match sel {
                        0 => (HexTarget::Background, self.theme.bg_rgb),
                        1 => (HexTarget::Foreground, self.theme.fg_rgb),
                        2 => (HexTarget::Shade1, self.theme.fg1_rgb),
                        _ => (HexTarget::Shade2, self.theme.fg2_rgb),
                    };
                    self.push_screen(
                        Screen::KeyboardHex {
                            target,
                            text: format!("{rgb:06X}"),
                            row: 0,
                            col: 0,
                        },
                        ui,
                    );
                }
            }

            Screen::MainMenuSettings { mut sel } => {
                if self.nav_list(pressed, &mut sel, 13, ui) {
                    self.set_top_sel(sel);
                } else if (pressed & (btn::A | btn::LEFT | btn::RIGHT)) != 0 {
                    self.emit_click();
                    self.main_menu_enabled[sel] = !self.main_menu_enabled[sel];
                }
            }

            Screen::KeyboardHex {
                target,
                mut text,
                mut row,
                mut col,
            } => {
                let rows = Self::qwerty_rows();
                if (pressed & btn::B) != 0 {
                    self.emit_click();
                    self.pop_screen(ui);
                    return;
                }
                if (pressed & btn::UP) != 0 {
                    row = (row + 4) % 5;
                    col = col.min(rows[row].len() - 1);
                    self.emit_click();
                } else if (pressed & btn::DOWN) != 0 {
                    row = (row + 1) % 5;
                    col = col.min(rows[row].len() - 1);
                    self.emit_click();
                } else if (pressed & btn::LEFT) != 0 {
                    col = (col + rows[row].len() - 1) % rows[row].len();
                    self.emit_click();
                } else if (pressed & btn::RIGHT) != 0 {
                    col = (col + 1) % rows[row].len();
                    self.emit_click();
                } else if (pressed & btn::A) != 0 {
                    self.emit_click();
                    let k = rows[row][col];
                    match k {
                        "DEL" => {
                            text.pop();
                        }
                        "CLR" => {
                            text.clear();
                        }
                        "DONE" => {
                            if let Some(rgb) = parse_hex_rgb(&text) {
                                match target {
                                    HexTarget::Background => {
                                        self.theme = derive_4shade(rgb, self.theme.fg_rgb);
                                    }
                                    HexTarget::Foreground => {
                                        self.theme = derive_4shade(self.theme.bg_rgb, rgb);
                                    }
                                    HexTarget::Shade1 => self.theme.fg1_rgb = rgb,
                                    HexTarget::Shade2 => self.theme.fg2_rgb = rgb,
                                }
                                self.theme_idx = None;
                                self.state_dirty = true;
                            }
                            self.pop_screen(ui);
                            return;
                        }
                        "CANCEL" => {
                            self.pop_screen(ui);
                            return;
                        }
                        ch => {
                            if text.len() < 6 {
                                text.push_str(ch);
                            }
                        }
                    }
                }
                if let Some(last) = self.stack.last_mut() {
                    *last = Screen::KeyboardHex {
                        target,
                        text,
                        row,
                        col,
                    };
                }
            }

            Screen::SoundMenu { mut sel } => {
                // Exact 7 rows from `apps/menus/sound_menu.c` lines 94-101:
                // Equalizer, Effects, Replaygain, Balance, Channels, Crossfeed, Menu Click
                if self.nav_list(pressed, &mut sel, 7, ui) {
                    self.set_top_sel(sel);
                } else if (pressed & (btn::A | btn::LEFT | btn::RIGHT)) != 0 {
                    let left = (pressed & btn::LEFT) != 0;
                    let is_dir = (pressed & (btn::LEFT | btn::RIGHT)) != 0;
                    self.emit_click();
                    match sel {
                        0 if !is_dir => self.push_screen(Screen::EqualizerMenu { sel: 0 }, ui),
                        1 if !is_dir => self.push_screen(Screen::EffectsMenu { sel: 0 }, ui),
                        2 if !is_dir => self.push_screen(Screen::ReplayGainMenu { sel: 0 }, ui),
                        3 => {
                            let d = if left { -5i8 } else { 5i8 };
                            self.dsp.balance_pct = (self.dsp.balance_pct + d).clamp(-100, 100);
                        }
                        4 => {
                            let n = CHANNEL_MODES.len();
                            self.dsp.channels_idx = if left {
                                (self.dsp.channels_idx + n - 1) % n
                            } else {
                                (self.dsp.channels_idx + 1) % n
                            };
                        }
                        5 => {
                            let n = CROSSFEED_MODES.len();
                            self.dsp.crossfeed_idx = if left {
                                (self.dsp.crossfeed_idx + n - 1) % n
                            } else {
                                (self.dsp.crossfeed_idx + 1) % n
                            };
                        }
                        6 => {
                            self.dsp.menu_click = !self.dsp.menu_click;
                            self.state_dirty = true;
                        }
                        _ => {}
                    }
                }
            }

            Screen::EqualizerMenu { mut sel } => {
                // Exact 8 rows from `apps/menus/eq_menu.c` lines 378-380:
                // Enable EQ, AutoEQ, Browse EQ Presets, Precut, Simple EQ, Advanced EQ, Save, Reset
                if self.nav_list(pressed, &mut sel, 8, ui) {
                    self.set_top_sel(sel);
                } else if (pressed & (btn::A | btn::LEFT | btn::RIGHT)) != 0 {
                    let left = (pressed & btn::LEFT) != 0;
                    let is_dir = (pressed & (btn::LEFT | btn::RIGHT)) != 0;
                    self.emit_click();
                    match sel {
                        0 => {
                            self.dsp.eq_enabled = !self.dsp.eq_enabled;
                            self.emit_dsp();
                        }
                        1 if !is_dir => {
                            self.push_screen(
                                Screen::AutoEqList {
                                    sel: self.dsp.autoeq_idx,
                                },
                                ui,
                            );
                        }
                        2 if !is_dir => {
                            self.push_screen(
                                Screen::AutoEqList {
                                    sel: self.dsp.autoeq_idx,
                                },
                                ui,
                            );
                        }
                        3 => {
                            let d = if left { -5 } else { 5 };
                            self.dsp.eq_precut_tenth_db =
                                (self.dsp.eq_precut_tenth_db + d).clamp(0, 120);
                        }
                        7 if !is_dir => {
                            self.dsp.autoeq_idx = 0;
                            self.dsp.eq_precut_tenth_db = 0;
                            self.emit_dsp();
                        }
                        _ => {}
                    }
                }
            }

            Screen::AutoEqList { mut sel } => {
                let count = AUTOEQ_PRESETS.len();
                if self.nav_list(pressed, &mut sel, count, ui) {
                    self.set_top_sel(sel);
                    self.dsp.autoeq_idx = sel;
                    if sel > 0 {
                        self.dsp.eq_enabled = true;
                    }
                    self.emit_dsp();
                } else if (pressed & btn::A) != 0 {
                    self.emit_click();
                    self.dsp.autoeq_idx = sel;
                    if sel > 0 {
                        self.dsp.eq_enabled = true;
                    }
                    self.emit_dsp();
                    self.pop_screen(ui);
                }
            }

            Screen::EffectsMenu { mut sel } => {
                // Exact 2 rows from `apps/menus/sound_menu.c` lines 67-68:
                // Speed, Tape Mode
                if self.nav_list(pressed, &mut sel, 2, ui) {
                    self.set_top_sel(sel);
                } else if (pressed & (btn::A | btn::LEFT | btn::RIGHT)) != 0 {
                    let left = (pressed & btn::LEFT) != 0;
                    let is_dir = (pressed & (btn::LEFT | btn::RIGHT)) != 0;
                    self.emit_click();
                    if sel == 0 {
                        let d = if left { -10i32 } else { 10i32 };
                        self.dsp.speed_pct =
                            ((self.dsp.speed_pct as i32 + d).clamp(50, 150)) as u16;
                        self.emit_dsp();
                    } else if !is_dir {
                        self.push_screen(Screen::TapeModeMenu { sel: 0 }, ui);
                    }
                }
            }

            Screen::TapeModeMenu { mut sel } => {
                // Exact 8 rows from `apps/menus/sound_menu.c` lines 57-65:
                // Enable, Wow & Flutter, Tape Hiss, Tape Crinkles, Saturation, HF Roll-Off, Stop/Start FX, Seek Motor Sound
                if self.nav_list(pressed, &mut sel, 8, ui) {
                    self.set_top_sel(sel);
                } else if (pressed & (btn::A | btn::LEFT | btn::RIGHT)) != 0 {
                    let delta = if (pressed & btn::LEFT) != 0 { -10i32 } else { 10i32 };
                    self.emit_click();
                    match sel {
                        0 => self.dsp.tape_enabled = !self.dsp.tape_enabled,
                        1 => {
                            self.dsp.tape_wow =
                                (self.dsp.tape_wow as i32 + delta).clamp(0, 100) as u8;
                        }
                        2 => {
                            self.dsp.tape_hiss =
                                (self.dsp.tape_hiss as i32 + delta).clamp(0, 100) as u8;
                        }
                        3 => {
                            self.dsp.tape_crinkle =
                                (self.dsp.tape_crinkle as i32 + delta).clamp(0, 100) as u8;
                        }
                        4 => {
                            self.dsp.tape_sat =
                                (self.dsp.tape_sat as i32 + delta).clamp(0, 100) as u8;
                        }
                        5 => {
                            self.dsp.tape_hf_rolloff =
                                (self.dsp.tape_hf_rolloff as i32 + delta).clamp(0, 100) as u8;
                        }
                        6 => self.dsp.tape_stop_start = !self.dsp.tape_stop_start,
                        _ => self.dsp.tape_ff_sound = !self.dsp.tape_ff_sound,
                    }
                    self.emit_dsp();
                }
            }

            Screen::ReplayGainMenu { mut sel } => {
                if self.nav_list(pressed, &mut sel, 3, ui) {
                    self.set_top_sel(sel);
                } else if (pressed & (btn::A | btn::LEFT | btn::RIGHT)) != 0 {
                    let left = (pressed & btn::LEFT) != 0;
                    self.emit_click();
                    match sel {
                        0 => {
                            let n = RG_TYPES.len();
                            self.dsp.rg_type = if left {
                                (self.dsp.rg_type + n - 1) % n
                            } else {
                                (self.dsp.rg_type + 1) % n
                            };
                        }
                        1 => self.dsp.rg_noclip = !self.dsp.rg_noclip,
                        _ => {
                            let d = if left { -1 } else { 1 };
                            self.dsp.rg_preamp_db = (self.dsp.rg_preamp_db + d).clamp(-12, 12);
                        }
                    }
                }
            }

            Screen::SourcesMenu { mut sel } => {
                // Exact 3 rows from `apps/menus/main_menu.c` lines 144-146:
                // Offline Mode, Navidrome, Jellyfin
                if self.nav_list(pressed, &mut sel, 3, ui) {
                    self.set_top_sel(sel);
                } else if (pressed & (btn::A | btn::LEFT | btn::RIGHT)) != 0 {
                    let is_dir = (pressed & (btn::LEFT | btn::RIGHT)) != 0;
                    self.emit_click();
                    match sel {
                        0 => {
                            self.offline_mode = !self.offline_mode;
                            self.state_dirty = true;
                        }
                        1 if !is_dir => {
                            self.source_icon = HeaderSourceIcon::Navidrome;
                            self.push_screen(
                                Screen::SourceServerInfo {
                                    is_jellyfin: false,
                                    sel: 0,
                                },
                                ui,
                            );
                        }
                        2 if !is_dir => {
                            self.source_icon = HeaderSourceIcon::Jellyfin;
                            self.push_screen(
                                Screen::SourceServerInfo {
                                    is_jellyfin: true,
                                    sel: 0,
                                },
                                ui,
                            );
                        }
                        _ => {}
                    }
                }
            }

            Screen::SourceServerInfo { mut sel, .. } => {
                if self.nav_list(pressed, &mut sel, 4, ui) {
                    self.set_top_sel(sel);
                } else if (pressed & btn::B) != 0 {
                    self.pop_screen(ui);
                }
            }

            Screen::LibraryMenu { mut sel } => {
                // Exact 6 rows from `apps/menus/audio_folders_menu.c` lines 56-58:
                // Music Folders, Podcast Folders, Audiobook Folders, Movie Folders, Television Folders, Rescan Library
                if self.nav_list(pressed, &mut sel, 6, ui) {
                    self.set_top_sel(sel);
                } else if (pressed & btn::A) != 0 {
                    self.emit_click();
                    if sel < 5 {
                        self.push_screen(
                            Screen::LibraryFolderList {
                                kind: sel as u8,
                                sel: 0,
                            },
                            ui,
                        );
                    }
                }
            }

            Screen::LibraryFolderList { mut sel, .. } => {
                if self.nav_list(pressed, &mut sel, 2, ui) {
                    self.set_top_sel(sel);
                }
            }

            Screen::VisualizersMenu { mut sel } => {
                // Exact 3 rows from `apps/menus/visualizer_menu.c` lines 35-38:
                // Visualization Presets, Start Visualizer After, Visualization Transition
                if self.nav_list(pressed, &mut sel, 3, ui) {
                    self.set_top_sel(sel);
                } else if (pressed & (btn::A | btn::LEFT | btn::RIGHT)) != 0 {
                    let left = (pressed & btn::LEFT) != 0;
                    let is_dir = (pressed & (btn::LEFT | btn::RIGHT)) != 0;
                    self.emit_click();
                    match sel {
                        0 if !is_dir => self.push_screen(
                            Screen::VisualizerPresets {
                                sel: self.viz_preset,
                            },
                            ui,
                        ),
                        1 => {
                            let n = VIZ_DELAYS.len();
                            self.viz_delay_idx = if left {
                                (self.viz_delay_idx + n - 1) % n
                            } else {
                                (self.viz_delay_idx + 1) % n
                            };
                        }
                        2 => {
                            let n = VIZ_TRANSITIONS.len();
                            self.viz_trans_idx = if left {
                                (self.viz_trans_idx + n - 1) % n
                            } else {
                                (self.viz_trans_idx + 1) % n
                            };
                        }
                        _ => {}
                    }
                }
            }

            Screen::VisualizerPresets { mut sel } => {
                let count = VIZ_PRESETS.len();
                if self.nav_list(pressed, &mut sel, count, ui) {
                    self.set_top_sel(sel);
                } else if (pressed & btn::A) != 0 {
                    self.emit_click();
                    self.viz_preset = sel;
                    self.viz_custom = false;
                    viz.set_preset(sel);
                    self.viz_fade = 0.0;
                    self.push_screen(Screen::VisualizerActive, ui);
                }
            }

            Screen::PowerMenu { mut sel } => {
                // Exact 5 rows from `apps/menus/power_menu.c` lines 585-587:
                // Brightness, Display Poweroff, Idle Poweroff, CPU Frequency, Charge Limit
                if self.nav_list(pressed, &mut sel, 5, ui) {
                    self.set_top_sel(sel);
                } else if (pressed & (btn::A | btn::LEFT | btn::RIGHT)) != 0 {
                    let left = (pressed & btn::LEFT) != 0;
                    self.emit_click();
                    match sel {
                        0 => {
                            let d = if left { -10i32 } else { 10i32 };
                            self.brightness_pct =
                                (self.brightness_pct as i32 + d).clamp(10, 100) as u8;
                        }
                        1 => {
                            let n = DISPLAY_OFF_TIMES.len();
                            self.disp_off_idx = if left {
                                (self.disp_off_idx + n - 1) % n
                            } else {
                                (self.disp_off_idx + 1) % n
                            };
                        }
                        2 => {
                            let n = IDLE_OFF_TIMES.len();
                            self.idle_off_idx = if left {
                                (self.idle_off_idx + n - 1) % n
                            } else {
                                (self.idle_off_idx + 1) % n
                            };
                        }
                        3 => {
                            let n = CPU_FREQS.len();
                            self.cpu_idx = if left {
                                (self.cpu_idx + n - 1) % n
                            } else {
                                (self.cpu_idx + 1) % n
                            };
                        }
                        _ => {
                            let d = if left { -5i32 } else { 5i32 };
                            self.charge_limit_pct =
                                (self.charge_limit_pct as i32 + d).clamp(75, 100) as u8;
                        }
                    }
                }
            }

            Screen::VisualizerActive | Screen::PluginActive => {}
        }
    }

    fn nav_list(&mut self, pressed: u32, sel: &mut usize, count: usize, ui: &mut UiEngine) -> bool {
        if (pressed & btn::B) != 0 {
            self.emit_click();
            self.pop_screen(ui);
            return false;
        }
        if (pressed & btn::UP) != 0 {
            *sel = (*sel + count - 1) % count;
            self.emit_click();
            return true;
        }
        if (pressed & btn::DOWN) != 0 {
            *sel = (*sel + 1) % count;
            self.emit_click();
            return true;
        }
        if (pressed & btn::L1) != 0 {
            *sel = sel.saturating_sub(LIST_VISIBLE_ROWS);
            self.emit_click();
            return true;
        }
        if (pressed & btn::R1) != 0 {
            *sel = (*sel + LIST_VISIBLE_ROWS).min(count.saturating_sub(1));
            self.emit_click();
            return true;
        }
        false
    }

    fn set_top_sel(&mut self, new_sel: usize) {
        if let Some(top) = self.stack.last_mut() {
            match top {
                Screen::MainMenu { sel }
                | Screen::Browse { sel, .. }
                | Screen::Files { sel }
                | Screen::ExtrasMenu { sel }
                | Screen::PluginsHub { sel }
                | Screen::PluginCategory { sel, .. }
                | Screen::Downloads { sel }
                | Screen::Artists { sel }
                | Screen::ArtistSongs { sel, .. }
                | Screen::Albums { sel }
                | Screen::AlbumSongs { sel, .. }
                | Screen::Genres { sel }
                | Screen::GenreSongs { sel, .. }
                | Screen::AllSongs { sel }
                | Screen::Playlists { sel }
                | Screen::NowPlayingContext { sel }
                | Screen::SettingsMenu { sel }
                | Screen::DisplayMenu { sel }
                | Screen::ThemeList { sel }
                | Screen::CustomizeThemeMenu { sel }
                | Screen::MainMenuSettings { sel }
                | Screen::SoundMenu { sel }
                | Screen::EqualizerMenu { sel }
                | Screen::AutoEqList { sel }
                | Screen::EffectsMenu { sel }
                | Screen::TapeModeMenu { sel }
                | Screen::ReplayGainMenu { sel }
                | Screen::SourcesMenu { sel }
                | Screen::SourceServerInfo { sel, .. }
                | Screen::LibraryMenu { sel }
                | Screen::LibraryFolderList { sel, .. }
                | Screen::VisualizersMenu { sel }
                | Screen::VisualizerPresets { sel }
                | Screen::PowerMenu { sel } => *sel = new_sel,
                _ => {}
            }
        }
    }

    fn qwerty_rows() -> [&'static [&'static str]; 5] {
        [
            &["1", "2", "3", "4", "5", "6", "7", "8", "9", "0"],
            &["Q", "W", "E", "R", "T", "Y", "U", "I", "O", "P"],
            &["A", "S", "D", "F", "G", "H", "J", "K", "L"],
            &["Z", "X", "C", "V", "B", "N", "M", "DEL"],
            &["CANCEL", "CLR", "DONE"],
        ]
    }

    fn render_list(
        &self,
        ui: &mut UiEngine,
        title: &str,
        items: &[(String, FileIconKind, OwnedRight)],
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
            .map(|(i, (lbl, icon, right))| ListRow {
                label: lbl.as_str(),
                selected: start + i == sel,
                file_icon: icon.clone(),
                right: right.as_ref(),
            })
            .collect();

        let state = ListScreenState {
            theme: self.theme,
            header: self.header_state(title),
            rows,
            total_items: total,
            start_item: start,
            visible_items: vis,
        };
        ui.render_list_to_fbo(&state);
    }

    pub fn tick_and_render(
        &mut self,
        dt: f32,
        pcm_interleaved: &[i16],
        sample_rate: u32,
        win_w: u32,
        win_h: u32,
        ui: &mut UiEngine,
        viz: &mut VizEngine,
    ) {
        let dt = dt.clamp(1.0 / 240.0, 0.1);
        if self.volume_toast_t > 0.0 {
            self.volume_toast_t = (self.volume_toast_t - dt).max(0.0);
        }
        if self.trans_t < 1.0 {
            self.trans_t = (self.trans_t + dt * 4.6).min(1.0);
        }

        self.handle_input(dt, pcm_interleaved, ui, viz);

        let top = match self.stack.last().cloned() {
            Some(s) => s,
            None => return,
        };

        if matches!(top, Screen::VisualizerActive) {
            self.viz_fade = (self.viz_fade + dt * 2.8).min(1.0);
            let prog = if self.dur_ms > 0 {
                (self.pos_ms as f32 / self.dur_ms as f32).clamp(0.0, 1.0)
            } else {
                0.0
            };
            let transport_speed = if self.play_mode == PlayMode::Play {
                self.dsp.speed_pct as f32 / 100.0
            } else {
                0.0
            };
            unsafe {
                viz.render_frame(
                    pcm_interleaved,
                    sample_rate,
                    dt,
                    win_w as i32,
                    win_h as i32,
                    self.theme,
                    self.viz_fade,
                    prog,
                    transport_speed,
                );
            }
            return;
        }

        if matches!(top, Screen::PluginActive) {
            if let Some(ref pl) = self.active_plugin {
                let title = pl.title(self.uk);
                let hdr = self.header_state(title);
                pl.render(ui, self.theme, &hdr, self.uk);
                ui.present_fbo_transition(
                    win_w,
                    win_h,
                    self.transition_kind,
                    self.trans_t,
                    self.trans_dir,
                );
                return;
            }
        }

        match top {
            Screen::MainMenu { sel } => {
                let rows = self.visible_main_rows();
                let items: Vec<_> = rows
                    .iter()
                    .map(|r| match *r {
                        MainRowAction::Toggleable(id) => {
                            let right = if id == MainMenuId::Shuffle {
                                OwnedRight::None
                            } else {
                                OwnedRight::Chevron
                            };
                            (self.main_menu_label(id).to_string(), FileIconKind::None, right)
                        }
                        MainRowAction::Settings => (
                            self.tr("Settings", "Налаштування").to_string(),
                            FileIconKind::None,
                            OwnedRight::Chevron,
                        ),
                        MainRowAction::NowPlaying => (
                            self.tr("Now Playing", "Програється").to_string(),
                            FileIconKind::None,
                            OwnedRight::None,
                        ),
                    })
                    .collect();
                self.render_list(ui, "UIPod", &items, sel);
            }

            Screen::Browse { path, sel } => {
                let items = match path.as_str() {
                    "/mnt/SDCARD" => vec![
                        ("Media".to_string(), FileIconKind::Folder, OwnedRight::Chevron),
                        ("Plugins".to_string(), FileIconKind::Folder, OwnedRight::Chevron),
                        ("about.txt".to_string(), FileIconKind::Text("TXT".into()), OwnedRight::Value("6 KB".into())),
                    ],
                    "/mnt/SDCARD/Media" => vec![
                        ("Music".to_string(), FileIconKind::Folder, OwnedRight::Chevron),
                        ("about.txt".to_string(), FileIconKind::Text("TXT".into()), OwnedRight::Value("6 KB".into())),
                    ],
                    _ => {
                        let mut list: Vec<_> = self
                            .tracks
                            .iter()
                            .map(|t| {
                                (
                                    format!("{}.{}", t.title, t.codec.to_lowercase()),
                                    FileIconKind::from_tag(&t.codec),
                                    OwnedRight::Value(fmt_ms(t.duration_ms)),
                                )
                            })
                            .collect();
                        list.push((
                            "about.txt".to_string(),
                            FileIconKind::Text("TXT".into()),
                            OwnedRight::Value("6 KB".into()),
                        ));
                        list
                    }
                };
                self.render_list(ui, &path, &items, sel);
            }

            Screen::Files { sel } => {
                let items: Vec<_> = self
                    .tracks
                    .iter()
                    .map(|t| {
                        (
                            format!("{} - {}.{}", t.artist, t.title, t.codec.to_lowercase()),
                            FileIconKind::from_tag(&t.codec),
                            OwnedRight::Value(fmt_ms(t.duration_ms)),
                        )
                    })
                    .collect();
                self.render_list(ui, self.tr("Files", "Файли"), &items, sel);
            }

            Screen::ExtrasMenu { sel } => {
                let items = vec![
                    (self.tr("Plugins", "Плагіни").to_string(), FileIconKind::None, OwnedRight::Chevron),
                    (self.tr("Downloads", "Завантаження").to_string(), FileIconKind::None, OwnedRight::Chevron),
                ];
                self.render_list(ui, self.tr("Extras", "Екстра"), &items, sel);
            }

            Screen::PluginsHub { sel } => {
                let items = vec![
                    (self.tr("Games", "Ігри").to_string(), FileIconKind::None, OwnedRight::Value("6".into())),
                    (self.tr("Apps", "Програми").to_string(), FileIconKind::None, OwnedRight::Value("1".into())),
                    (self.tr("Demos", "Демо").to_string(), FileIconKind::None, OwnedRight::Value("1".into())),
                    (self.tr("Viewers", "Переглядачі").to_string(), FileIconKind::None, OwnedRight::Value("1".into())),
                    (self.tr("Rescan Plugins", "Оновити плагіни").to_string(), FileIconKind::None, OwnedRight::None),
                ];
                self.render_list(ui, self.tr("Plugins", "Плагіни"), &items, sel);
            }

            Screen::PluginCategory { cat, sel } => {
                let (title, items) = match cat {
                    0 => (
                        self.tr("Games", "Ігри"),
                        vec![
                            (self.tr("Music Quiz", "Вгадай мелодію").to_string(), FileIconKind::Text("PLG".into()), OwnedRight::None),
                            ("Sound Racer".to_string(), FileIconKind::Text("PLG".into()), OwnedRight::None),
                            (self.tr("Tetris", "Тетріс").to_string(), FileIconKind::Text("PLG".into()), OwnedRight::None),
                            (self.tr("Snake", "Змійка").to_string(), FileIconKind::Text("PLG".into()), OwnedRight::None),
                            ("2048".to_string(), FileIconKind::Text("PLG".into()), OwnedRight::None),
                            (self.tr("Brick Breaker", "Арканоїд").to_string(), FileIconKind::Text("PLG".into()), OwnedRight::None),
                        ],
                    ),
                    1 => (
                        self.tr("Apps", "Програми"),
                        vec![(
                            self.tr("Stopwatch", "Секундомір").to_string(),
                            FileIconKind::Text("PLG".into()),
                            OwnedRight::None,
                        )],
                    ),
                    2 => (
                        self.tr("Demos", "Демо"),
                        vec![(
                            self.tr("LCD Plasma", "LCD Плазма").to_string(),
                            FileIconKind::Text("PLG".into()),
                            OwnedRight::None,
                        )],
                    ),
                    _ => (
                        self.tr("Viewers", "Переглядачі"),
                        vec![(
                            self.tr("Text Viewer", "Переглядач тексту").to_string(),
                            FileIconKind::Text("PLG".into()),
                            OwnedRight::None,
                        )],
                    ),
                };
                self.render_list(ui, title, &items, sel);
            }

            Screen::Downloads { sel } => {
                let items = vec![(
                    self.tr("(No downloads)", "(Немає завантажень)").to_string(),
                    FileIconKind::None,
                    OwnedRight::None,
                )];
                self.render_list(ui, self.tr("Downloads", "Завантаження"), &items, sel);
            }

            Screen::EmptySection {
                title_en,
                title_uk,
                hint_en,
                hint_uk,
            } => {
                let title = self.tr(title_en, title_uk);
                let hint = self.tr(hint_en, hint_uk);
                let hdr = self.header_state(title);
                ui.plugin_begin(self.theme, self.theme.bg_rgb, Some(&hdr));
                ui.plugin_draw_text(512.0, 420.0, hint, 34.0, 1, 1, self.theme.fg2_rgb);
            }

            Screen::Artists { sel } => {
                let items: Vec<_> = self
                    .artists_list()
                    .into_iter()
                    .map(|a| (a, FileIconKind::None, OwnedRight::Chevron))
                    .collect();
                self.render_list(ui, self.tr("Artists", "Артисти"), &items, sel);
            }

            Screen::ArtistSongs { artist, sel } => {
                let items: Vec<_> = self
                    .tracks
                    .iter()
                    .filter(|t| t.artist == artist)
                    .map(|t| {
                        (
                            t.title.clone(),
                            FileIconKind::from_tag(&t.codec),
                            OwnedRight::Value(fmt_ms(t.duration_ms)),
                        )
                    })
                    .collect();
                self.render_list(ui, &artist, &items, sel);
            }

            Screen::Albums { sel } => {
                let items: Vec<_> = self
                    .albums_list()
                    .into_iter()
                    .map(|a| (a, FileIconKind::None, OwnedRight::Chevron))
                    .collect();
                self.render_list(ui, self.tr("Albums", "Альбоми"), &items, sel);
            }

            Screen::AlbumSongs { album, sel } => {
                let items: Vec<_> = self
                    .tracks
                    .iter()
                    .filter(|t| t.album == album)
                    .map(|t| {
                        (
                            t.title.clone(),
                            FileIconKind::from_tag(&t.codec),
                            OwnedRight::Value(fmt_ms(t.duration_ms)),
                        )
                    })
                    .collect();
                self.render_list(ui, &album, &items, sel);
            }

            Screen::Genres { sel } => {
                let items: Vec<_> = self
                    .genres_list()
                    .into_iter()
                    .map(|g| (g, FileIconKind::None, OwnedRight::Chevron))
                    .collect();
                self.render_list(ui, self.tr("Genres", "Жанри"), &items, sel);
            }

            Screen::GenreSongs { genre, sel } => {
                let items: Vec<_> = self
                    .tracks
                    .iter()
                    .filter(|t| t.genre == genre)
                    .map(|t| {
                        (
                            t.title.clone(),
                            FileIconKind::from_tag(&t.codec),
                            OwnedRight::Value(fmt_ms(t.duration_ms)),
                        )
                    })
                    .collect();
                self.render_list(ui, &genre, &items, sel);
            }

            Screen::AllSongs { sel } => {
                let items: Vec<_> = self
                    .tracks
                    .iter()
                    .map(|t| {
                        (
                            format!("{} — {}", t.title, t.artist),
                            FileIconKind::from_tag(&t.codec),
                            OwnedRight::Value(fmt_ms(t.duration_ms)),
                        )
                    })
                    .collect();
                self.render_list(ui, self.tr("Songs", "Пісні"), &items, sel);
            }

            Screen::Playlists { sel } => {
                let items = vec![
                    ("Y2K System Mix".to_string(), FileIconKind::Audio("M3U".into()), OwnedRight::Value(format!("{}", self.tracks.len()))),
                    ("Liquid Millennium".to_string(), FileIconKind::Audio("M3U".into()), OwnedRight::Value("2".into())),
                    (self.tr("Favourites", "Обране").to_string(), FileIconKind::Audio("M3U".into()), OwnedRight::Value(format!("{}", self.tracks.len()))),
                ];
                self.render_list(ui, self.tr("Playlists", "Плейлисти"), &items, sel);
            }

            Screen::NowPlaying => {
                let trk = self.tracks.get(self.current_track).or_else(|| self.tracks.first());
                if let Some(t) = trk {
                    let track_pos = format!(
                        "{} {} {}",
                        self.current_track + 1,
                        self.tr("of", "з"),
                        self.tracks.len()
                    );
                    let elapsed = fmt_ms(self.pos_ms);
                    let rem_ms = self.dur_ms.saturating_sub(self.pos_ms);
                    let remaining = format!("-{}", fmt_ms(rem_ms));
                    let (progress, center_status) = if self.volume_toast_t > 0.0 {
                        (
                            self.volume_step as f32 / 40.0,
                            format!(
                                "{}  {} dB",
                                self.tr("Volume", "Гучність"),
                                self.volume_db()
                            ),
                        )
                    } else {
                        let p = if self.dur_ms > 0 {
                            (self.pos_ms as f32 / self.dur_ms as f32).clamp(0.0, 1.0)
                        } else {
                            0.0
                        };
                        (p, String::new())
                    };
                    let state = NowPlayingState {
                        theme: self.theme,
                        header: self.header_state(self.tr("Now Playing", "Програється")),
                        track_pos_label: &track_pos,
                        eq_enabled: self.dsp.eq_enabled,
                        title: &t.title,
                        artist: &t.artist,
                        album: &t.album,
                        codec_label: &t.codec,
                        bitdepth_label: &t.bitdepth,
                        samplerate_label: &t.samplerate,
                        progress,
                        elapsed_label: &elapsed,
                        remaining_label: &remaining,
                        center_status_label: &center_status,
                        art_rgb24: t.art_rgb24.as_deref(),
                        mono_album_art: self.mono_album_art,
                        alt_layout: self.alt_now_playing,
                    };
                    ui.render_now_playing_to_fbo(&state);
                }
            }

            Screen::NowPlayingContext { sel } => {
                let pause_lbl = if self.play_mode == PlayMode::Play {
                    self.tr("Pause", "Пауза")
                } else {
                    self.tr("Resume", "Продовжити")
                };
                let rep = if self.uk {
                    REPEAT_MODES_UK[self.repeat_idx]
                } else {
                    REPEAT_MODES_EN[self.repeat_idx]
                };
                let items = vec![
                    (pause_lbl.to_string(), FileIconKind::None, OwnedRight::None),
                    (self.tr("Shuffle", "Перемішати").to_string(), FileIconKind::None, OwnedRight::None),
                    (self.tr("Repeat", "Повтор").to_string(), FileIconKind::None, OwnedRight::CycleValue(rep.to_string())),
                    (self.tr("Sleep Timer", "Таймер сну").to_string(), FileIconKind::None, OwnedRight::CycleValue(SLEEP_TIMERS[self.sleep_idx].to_string())),
                    (self.tr("Monochrome album art", "Монохромна обкладинка").to_string(), FileIconKind::None, OwnedRight::Toggle(self.mono_album_art)),
                    (self.tr("Large Album Art", "Великий кавер").to_string(), FileIconKind::None, OwnedRight::Toggle(self.alt_now_playing)),
                    (self.tr("Add to Playlist", "Додати у плейлист").to_string(), FileIconKind::None, OwnedRight::None),
                    (self.tr("Start Visualizer", "Запустити візуалізатор").to_string(), FileIconKind::None, OwnedRight::None),
                ];
                self.render_list(ui, self.tr("Now Playing", "Програється"), &items, sel);
            }

            Screen::SettingsMenu { sel } => {
                let items = vec![
                    (self.tr("Display", "Вигляд").to_string(), FileIconKind::None, OwnedRight::Chevron),
                    (self.tr("Sound", "Звук").to_string(), FileIconKind::None, OwnedRight::Chevron),
                    (self.tr("Offline Mode", "Офлайн-режим").to_string(), FileIconKind::None, OwnedRight::Toggle(self.offline_mode)),
                    (self.tr("Sources", "Джерела").to_string(), FileIconKind::None, OwnedRight::Chevron),
                    (self.tr("Library", "Бібліотека").to_string(), FileIconKind::None, OwnedRight::Chevron),
                    (self.tr("Visualizers", "Візуалізатори").to_string(), FileIconKind::None, OwnedRight::Chevron),
                    (self.tr("Power", "Живлення").to_string(), FileIconKind::None, OwnedRight::Chevron),
                    (self.tr("About", "Про програму").to_string(), FileIconKind::None, OwnedRight::Chevron),
                    (self.tr("Exit", "Вихід").to_string(), FileIconKind::None, OwnedRight::None),
                ];
                self.render_list(ui, self.tr("Settings", "Налаштування"), &items, sel);
            }

            Screen::DisplayMenu { sel } => {
                let theme_name = self
                    .theme_idx
                    .and_then(|i| THEME_PRESETS.get(i))
                    .map(|p| p.name.to_string())
                    .unwrap_or_else(|| "Custom".to_string());
                let tr_name = if self.uk {
                    TRANSITION_NAMES_UK[(self.transition_kind as usize) % 6]
                } else {
                    TRANSITION_NAMES_EN[(self.transition_kind as usize) % 6]
                };
                let items = vec![
                    (self.tr("Theme", "Тема").to_string(), FileIconKind::None, OwnedRight::CycleValue(theme_name)),
                    (self.tr("Customize Theme", "Кастомізація теми").to_string(), FileIconKind::None, OwnedRight::Chevron),
                    (self.tr("Screen Transition", "Анімація переходу").to_string(), FileIconKind::None, OwnedRight::CycleValue(tr_name.to_string())),
                    (self.tr("Language", "Мова").to_string(), FileIconKind::None, OwnedRight::CycleValue(if self.uk { "Українська" } else { "English" }.to_string())),
                    (self.tr("Monochrome album art", "Монохромна обкладинка").to_string(), FileIconKind::None, OwnedRight::Toggle(self.mono_album_art)),
                    (self.tr("Main menu", "Головне меню").to_string(), FileIconKind::None, OwnedRight::Chevron),
                ];
                self.render_list(ui, self.tr("Display", "Вигляд"), &items, sel);
            }

            Screen::ThemeList { sel } => {
                let items: Vec<_> = THEME_PRESETS
                    .iter()
                    .enumerate()
                    .map(|(i, t)| {
                        let right = if Some(i) == self.theme_idx {
                            OwnedRight::Value(format!("#{:06X}", t.colors.bg_rgb))
                        } else {
                            OwnedRight::None
                        };
                        (t.name.to_string(), FileIconKind::None, right)
                    })
                    .collect();
                self.render_list(ui, self.tr("Theme", "Тема"), &items, sel);
            }

            Screen::CustomizeThemeMenu { sel } => {
                let items = vec![
                    (self.tr("Background", "Колір фону").to_string(), FileIconKind::None, OwnedRight::Value(format!("#{:06X}", self.theme.bg_rgb))),
                    (self.tr("Foreground", "Основний колір").to_string(), FileIconKind::None, OwnedRight::Value(format!("#{:06X}", self.theme.fg_rgb))),
                    (self.tr("Secondary color 1", "Допоміжний колір 1").to_string(), FileIconKind::None, OwnedRight::Value(format!("#{:06X}", self.theme.fg1_rgb))),
                    (self.tr("Secondary color 2", "Допоміжний колір 2").to_string(), FileIconKind::None, OwnedRight::Value(format!("#{:06X}", self.theme.fg2_rgb))),
                ];
                self.render_list(ui, self.tr("Customize Theme", "Кастомізація теми"), &items, sel);
            }

            Screen::MainMenuSettings { sel } => {
                let items: Vec<_> = ALL_MAIN_TOGGLES
                    .iter()
                    .map(|&id| {
                        (
                            self.main_menu_label(id).to_string(),
                            FileIconKind::None,
                            OwnedRight::Toggle(self.main_menu_enabled[id as usize]),
                        )
                    })
                    .collect();
                self.render_list(ui, self.tr("Main menu", "Головне меню"), &items, sel);
            }

            Screen::KeyboardHex {
                text, row, col, ..
            } => {
                let raw_rows = Self::qwerty_rows();
                let mut rows: [Vec<KeyboardKey<'_>>; 5] = Default::default();
                for (r_idx, r) in raw_rows.iter().enumerate() {
                    rows[r_idx] = r
                        .iter()
                        .enumerate()
                        .map(|(c_idx, &lbl)| KeyboardKey {
                            label: lbl,
                            selected: r_idx == row && c_idx == col,
                            is_modifier: lbl.len() > 1,
                            is_active_toggle: lbl == "DONE",
                        })
                        .collect();
                }
                let display = format!("#{text}");
                let state = KeyboardState {
                    theme: self.theme,
                    text: &display,
                    rows,
                };
                ui.render_keyboard_to_fbo(&state);
            }

            Screen::SoundMenu { sel } => {
                let items = vec![
                    (self.tr("Equalizer", "Еквалайзер").to_string(), FileIconKind::None, OwnedRight::Chevron),
                    (self.tr("Effects", "Ефекти").to_string(), FileIconKind::None, OwnedRight::Chevron),
                    ("Replaygain".to_string(), FileIconKind::None, OwnedRight::Chevron),
                    (self.tr("Balance", "Баланс").to_string(), FileIconKind::None, OwnedRight::CycleValue(format!("{}%", self.dsp.balance_pct))),
                    (self.tr("Channels", "Канали").to_string(), FileIconKind::None, OwnedRight::CycleValue(CHANNEL_MODES[self.dsp.channels_idx].to_string())),
                    (self.tr("Crossfeed", "Кросфід").to_string(), FileIconKind::None, OwnedRight::CycleValue(CROSSFEED_MODES[self.dsp.crossfeed_idx].to_string())),
                    (self.tr("Menu Click", "Звук кліку").to_string(), FileIconKind::None, OwnedRight::Toggle(self.dsp.menu_click)),
                ];
                self.render_list(ui, self.tr("Sound", "Звук"), &items, sel);
            }

            Screen::EqualizerMenu { sel } => {
                let pc = self.dsp.eq_precut_tenth_db;
                let pc_str = if pc == 0 {
                    "0.0 dB".to_string()
                } else {
                    format!("-{}.{} dB", pc / 10, pc % 10)
                };
                let items = vec![
                    (self.tr("Enable EQ", "Увімкнути EQ").to_string(), FileIconKind::None, OwnedRight::Toggle(self.dsp.eq_enabled)),
                    ("AutoEQ".to_string(), FileIconKind::None, OwnedRight::Chevron),
                    (self.tr("Browse EQ Presets", "Пресети EQ").to_string(), FileIconKind::None, OwnedRight::Chevron),
                    ("Precut".to_string(), FileIconKind::None, OwnedRight::CycleValue(pc_str)),
                    (self.tr("Simple EQ Settings", "Простий EQ").to_string(), FileIconKind::None, OwnedRight::Chevron),
                    (self.tr("Advanced EQ Settings", "Розширений EQ").to_string(), FileIconKind::None, OwnedRight::Chevron),
                    (self.tr("Save EQ Preset", "Зберегти пресет EQ").to_string(), FileIconKind::None, OwnedRight::None),
                    (self.tr("Reset EQ", "Скинути EQ").to_string(), FileIconKind::None, OwnedRight::None),
                ];
                self.render_list(ui, self.tr("Equalizer", "Еквалайзер"), &items, sel);
            }

            Screen::AutoEqList { sel } => {
                let items: Vec<_> = AUTOEQ_PRESETS
                    .iter()
                    .enumerate()
                    .map(|(i, &name)| {
                        let right = if i == self.dsp.autoeq_idx {
                            OwnedRight::Value("Active".into())
                        } else {
                            OwnedRight::None
                        };
                        (name.to_string(), FileIconKind::None, right)
                    })
                    .collect();
                self.render_list(ui, "AutoEQ", &items, sel);
            }

            Screen::EffectsMenu { sel } => {
                let items = vec![
                    (self.tr("Speed", "Швидкість").to_string(), FileIconKind::None, OwnedRight::CycleValue(format!("{}%", self.dsp.speed_pct))),
                    (self.tr("Tape Mode", "Режим касети").to_string(), FileIconKind::None, OwnedRight::Chevron),
                ];
                self.render_list(ui, self.tr("Effects", "Ефекти"), &items, sel);
            }

            Screen::TapeModeMenu { sel } => {
                let items = vec![
                    (self.tr("Enable", "Увімкнути").to_string(), FileIconKind::None, OwnedRight::Toggle(self.dsp.tape_enabled)),
                    (self.tr("Wow & Flutter", "Плавання швидкості").to_string(), FileIconKind::None, OwnedRight::CycleValue(format!("{}%", self.dsp.tape_wow))),
                    (self.tr("Tape Hiss", "Шум стрічки").to_string(), FileIconKind::None, OwnedRight::CycleValue(format!("{}%", self.dsp.tape_hiss))),
                    (self.tr("Tape Crinkles", "Заломи стрічки").to_string(), FileIconKind::None, OwnedRight::CycleValue(format!("{}%", self.dsp.tape_crinkle))),
                    (self.tr("Saturation", "Сатурація").to_string(), FileIconKind::None, OwnedRight::CycleValue(format!("{}%", self.dsp.tape_sat))),
                    (self.tr("HF Roll-Off", "Зріз ВЧ").to_string(), FileIconKind::None, OwnedRight::CycleValue(format!("{}%", self.dsp.tape_hf_rolloff))),
                    (self.tr("Stop / Start FX", "Старт / Стоп мотора").to_string(), FileIconKind::None, OwnedRight::Toggle(self.dsp.tape_stop_start)),
                    (self.tr("Seek Motor Sound", "Звук перемотки").to_string(), FileIconKind::None, OwnedRight::Toggle(self.dsp.tape_ff_sound)),
                ];
                self.render_list(ui, self.tr("Tape Mode", "Режим касети"), &items, sel);
            }

            Screen::ReplayGainMenu { sel } => {
                let items = vec![
                    ("Replaygain Type".to_string(), FileIconKind::None, OwnedRight::CycleValue(RG_TYPES[self.dsp.rg_type].to_string())),
                    ("Prevent Clipping".to_string(), FileIconKind::None, OwnedRight::Toggle(self.dsp.rg_noclip)),
                    ("Pre-amp".to_string(), FileIconKind::None, OwnedRight::CycleValue(format!("{} dB", self.dsp.rg_preamp_db))),
                ];
                self.render_list(ui, "Replaygain", &items, sel);
            }

            Screen::SourcesMenu { sel } => {
                let items = vec![
                    (self.tr("Offline Mode", "Офлайн-режим").to_string(), FileIconKind::None, OwnedRight::Toggle(self.offline_mode)),
                    ("Navidrome".to_string(), FileIconKind::None, OwnedRight::Chevron),
                    ("Jellyfin".to_string(), FileIconKind::None, OwnedRight::Chevron),
                ];
                self.render_list(ui, self.tr("Sources", "Джерела"), &items, sel);
            }

            Screen::SourceServerInfo { is_jellyfin, sel } => {
                let title = if is_jellyfin { "Jellyfin" } else { "Navidrome" };
                let items = vec![
                    ("Server URL".to_string(), FileIconKind::None, OwnedRight::Value("Not set".into())),
                    ("Username".to_string(), FileIconKind::None, OwnedRight::Value("Not set".into())),
                    ("Audio Quality".to_string(), FileIconKind::None, OwnedRight::Value("Original".into())),
                    ("Connect & Sync".to_string(), FileIconKind::None, OwnedRight::None),
                ];
                self.render_list(ui, title, &items, sel);
            }

            Screen::LibraryMenu { sel } => {
                let items = vec![
                    (self.tr("Music Folders", "Теки музики").to_string(), FileIconKind::None, OwnedRight::Chevron),
                    (self.tr("Podcast Folders", "Теки подкастів").to_string(), FileIconKind::None, OwnedRight::Chevron),
                    (self.tr("Audiobook Folders", "Теки аудіокниг").to_string(), FileIconKind::None, OwnedRight::Chevron),
                    (self.tr("Movie Folders", "Теки відео").to_string(), FileIconKind::None, OwnedRight::Chevron),
                    (self.tr("Television Folders", "Теки серіалів").to_string(), FileIconKind::None, OwnedRight::Chevron),
                    (self.tr("Rescan Library", "Оновити бібліотеку").to_string(), FileIconKind::None, OwnedRight::None),
                ];
                self.render_list(ui, self.tr("Library", "Бібліотека"), &items, sel);
            }

            Screen::LibraryFolderList { kind, sel } => {
                let (title, path) = match kind {
                    0 => (self.tr("Music Folders", "Теки музики"), "/mnt/SDCARD/Media/Music"),
                    1 => (self.tr("Podcast Folders", "Теки подкастів"), "/mnt/SDCARD/Media/Podcasts"),
                    2 => (self.tr("Audiobook Folders", "Теки аудіокниг"), "/mnt/SDCARD/Media/Audiobooks"),
                    3 => (self.tr("Movie Folders", "Теки відео"), "/mnt/SDCARD/Media/Videos"),
                    _ => (self.tr("Television Folders", "Теки серіалів"), "/mnt/SDCARD/Media/TV"),
                };
                let items = vec![
                    (path.to_string(), FileIconKind::Folder, OwnedRight::None),
                    ("+ Add Folder".to_string(), FileIconKind::AddFolder, OwnedRight::None),
                ];
                self.render_list(ui, title, &items, sel);
            }

            Screen::VisualizersMenu { sel } => {
                let items = vec![
                    (self.tr("Visualization Presets", "Пресети візуалізації").to_string(), FileIconKind::None, OwnedRight::Chevron),
                    (self.tr("Start Visualizer After", "Запуск візуалізатора").to_string(), FileIconKind::None, OwnedRight::CycleValue(VIZ_DELAYS[self.viz_delay_idx].to_string())),
                    (self.tr("Visualization Transition", "Зміна пресету").to_string(), FileIconKind::None, OwnedRight::CycleValue(VIZ_TRANSITIONS[self.viz_trans_idx].to_string())),
                ];
                self.render_list(ui, self.tr("Visualizers", "Візуалізатори"), &items, sel);
            }

            Screen::VisualizerPresets { sel } => {
                let items: Vec<_> = VIZ_PRESETS
                    .iter()
                    .map(|p| {
                        (
                            if self.uk { p.name_uk } else { p.name_en }.to_string(),
                            FileIconKind::None,
                            OwnedRight::None,
                        )
                    })
                    .collect();
                self.render_list(ui, self.tr("Visualization Presets", "Пресети візуалізації"), &items, sel);
            }

            Screen::PowerMenu { sel } => {
                let items = vec![
                    (self.tr("Brightness", "Яскравість").to_string(), FileIconKind::None, OwnedRight::CycleValue(format!("{}%", self.brightness_pct))),
                    (self.tr("Display Poweroff", "Вимкнення екрана").to_string(), FileIconKind::None, OwnedRight::CycleValue(DISPLAY_OFF_TIMES[self.disp_off_idx].to_string())),
                    (self.tr("Idle Poweroff", "Автовимкнення").to_string(), FileIconKind::None, OwnedRight::CycleValue(IDLE_OFF_TIMES[self.idle_off_idx].to_string())),
                    (self.tr("CPU Frequency", "Частота CPU").to_string(), FileIconKind::None, OwnedRight::CycleValue(CPU_FREQS[self.cpu_idx].to_string())),
                    ("Charge Limit".to_string(), FileIconKind::None, OwnedRight::CycleValue(format!("{}%", self.charge_limit_pct))),
                ];
                self.render_list(ui, self.tr("Power", "Живлення"), &items, sel);
            }

            Screen::VisualizerActive | Screen::PluginActive => {}
        }

        ui.present_fbo_transition(
            win_w,
            win_h,
            self.transition_kind,
            self.trans_t,
            self.trans_dir,
        );
    }

    pub fn state_json(&self, viz: &VizEngine) -> String {
        let screen_tag = match self.stack.last() {
            Some(Screen::MainMenu { .. }) => "main",
            Some(Screen::NowPlaying) => "now_playing",
            Some(Screen::VisualizerActive) => "visualizer",
            Some(Screen::ThemeList { .. } | Screen::CustomizeThemeMenu { .. }) => "themes",
            Some(Screen::KeyboardHex { .. }) => "keyboard",
            Some(Screen::PluginActive) => {
                if matches!(self.active_plugin, Some(ActivePlugin::MusicQuiz(_))) {
                    "music_quiz"
                } else {
                    "plugin"
                }
            }
            Some(Screen::SoundMenu { .. } | Screen::TapeModeMenu { .. }) => "sound",
            _ => "menu",
        };
        let trk = self.tracks.get(self.current_track);
        let title = trk.map(|t| escape_json(&t.title)).unwrap_or_default();
        let artist = trk.map(|t| escape_json(&t.artist)).unwrap_or_default();
        let theme_name = self
            .theme_idx
            .and_then(|i| THEME_PRESETS.get(i))
            .map(|p| p.name)
            .unwrap_or("Custom");
        let viz_name = if self.viz_custom {
            "Custom Shader"
        } else {
            let p = &VIZ_PRESETS[self.viz_preset % VIZ_PRESETS.len()];
            if self.uk {
                p.name_uk
            } else {
                p.name_en
            }
        };
        format!(
            concat!(
                r##"{{"screen":"{}","playing":{},"trackIndex":{},"trackCount":{},"title":"{}","artist":"{}","##,
                r##""themeIndex":{},"themeName":"{}","bg":"#{:06X}","fg":"#{:06X}","fg1":"#{:06X}","fg2":"#{:06X}","##,
                r##""monoArt":{},"altLayout":{},"transition":{},"uk":{},"hold":{},"volumeStep":{},"volumeDb":{},"##,
                r##""vizPreset":{},"vizName":"{}","vizFine":{},"vizCustom":{},"##,
                r##""eq":{},"autoeq":{},"tape":{},"wow":{},"hiss":{},"sat":{},"hf":{},"stopStart":{},"speed":{},"menuClick":{}}}"##
            ),
            screen_tag,
            self.play_mode == PlayMode::Play,
            self.current_track,
            self.tracks.len(),
            title,
            artist,
            self.theme_idx.map(|i| i as i32).unwrap_or(-1),
            escape_json(theme_name),
            self.theme.bg_rgb,
            self.theme.fg_rgb,
            self.theme.fg1_rgb,
            self.theme.fg2_rgb,
            self.mono_album_art,
            self.alt_now_playing,
            self.transition_kind,
            self.uk,
            self.hold_locked,
            self.volume_step,
            self.volume_db(),
            self.viz_preset,
            escape_json(viz_name),
            viz.is_fine_grain(),
            self.viz_custom,
            self.dsp.eq_enabled,
            self.dsp.autoeq_idx,
            self.dsp.tape_enabled,
            self.dsp.tape_wow,
            self.dsp.tape_hiss,
            self.dsp.tape_sat,
            self.dsp.tape_hf_rolloff,
            self.dsp.tape_stop_start,
            self.dsp.speed_pct,
            self.dsp.menu_click,
        )
    }
}

enum OwnedRight {
    None,
    Chevron,
    Toggle(bool),
    CycleValue(String),
    Value(String),
}

impl OwnedRight {
    fn as_ref(&self) -> RightAccessory<'_> {
        match self {
            Self::None => RightAccessory::None,
            Self::Chevron => RightAccessory::Chevron,
            Self::Toggle(v) => RightAccessory::Toggle(*v),
            Self::CycleValue(s) => RightAccessory::CycleValue(s.as_str()),
            Self::Value(s) => RightAccessory::Value(s.as_str()),
        }
    }
}

fn fmt_ms(ms: u32) -> String {
    let total_s = ms / 1000;
    let m = total_s / 60;
    let s = total_s % 60;
    format!("{m}:{s:02}")
}

fn escape_json(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => {}
            c => out.push(c),
        }
    }
    out
}
