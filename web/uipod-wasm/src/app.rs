use uipod_codecs::{
    get_nsf_active_chip_flags, get_nsf_apu_telemetry, get_nsf_channel_mute_mask,
    get_nsf_endless_loop, set_nsf_channel_mute_mask, set_nsf_endless_loop,
};
use uipod_nsf::{ChipFlags, SubtrackUri};
use uipod_ui::layout::LIST_VISIBLE_ROWS;
use uipod_ui::{
    FileIconKind, GraphicEqState, HeaderSourceIcon, HeaderState, KeyboardKey, KeyboardState,
    ListRow, ListScreenState, NowPlayingState, NsfApuPill, NsfChannelRow, NsfChannelsState,
    PlayMode, RightAccessory, ThemeColors, UiEngine, VizEngine, VIZ_PRESETS,
};

use crate::library::{built_in_demo_tracks, decode_cover_256, Track};
use crate::plugins::{btn, ActivePlugin, AudioSnapshot, PluginAction, PluginId};
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

pub const AUTOEQ_CURVES_TENTHS: [([i16; 10], i16); 7] = [
    ([0, 0, 0, 0, 0, 0, 0, 0, 0, 0], 0),
    ([-45, -52, -40, -18, 4, 15, 22, 34, 18, 10], -35),
    ([12, 8, -5, -10, 2, 8, 15, -12, 6, 15], -15),
    ([48, 32, 5, -14, -6, 0, 12, 18, 22, 16], -50),
    ([25, 5, -35, -30, -8, 6, 20, 38, 42, 30], -45),
    ([10, 6, -8, -4, 0, 4, 10, -6, 14, 20], -20),
    ([28, 22, 12, 4, -4, 0, 6, 12, -8, -14], -30),
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

/// Exact 12 toggleable Main Menu rows from `apps/uipod_mainmenu.c` (`uipod_mainmenu_id`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MainMenuId {
    Browse = 0,
    Extras = 1,
    Artists = 2,
    Albums = 3,
    Genres = 4,
    Videos = 5,
    Songs = 6,
    Playlists = 7,
    Podcasts = 8,
    Audiobooks = 9,
    Television = 10,
    Shuffle = 11,
}

const ALL_MAIN_TOGGLES: [MainMenuId; 12] = [
    MainMenuId::Browse,
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
    VideosList { sel: usize },
    VideoPlayer { playing: bool, pos_ms: u32, dur_ms: u32 },
    VideoContextMenu { sel: usize },
    TrackContextMenu { track_idx: usize, sel: usize },
    EmptySection {
        title_en: &'static str,
        title_uk: &'static str,
        hint_en: &'static str,
        hint_uk: &'static str,
    },
    NowPlaying,
    NowPlayingContext { sel: usize },
    NsfChannels { sel: usize },
    NsfContainer { file_name: String, sel: usize },
    NsfSfx { file_name: String, sel: usize },
    SettingsMenu { sel: usize },
    DisplayMenu { sel: usize },
    ThemeList { sel: usize },
    CustomizeThemeMenu { sel: usize },
    MainMenuSettings { sel: usize },
    Keyboard {
        target: KbdTarget,
        text: String,
        row: usize,
        col: usize,
        caps: bool,
        sym_mode: bool,
        allow_symbols: bool,
    },
    SoundMenu { sel: usize },
    DjCrossfadeMenu { sel: usize },
    EqualizerMenu { sel: usize },
    GraphicEq { sel: usize },
    AutoEqList { sel: usize },
    EffectsMenu { sel: usize },
    TapeModeMenu { sel: usize },
    ReverbMenu { sel: usize },
    DelayMenu { sel: usize },
    ChorusMenu { sel: usize },
    FlangerMenu { sel: usize },
    ReplayGainMenu { sel: usize },
    SourcesMenu { sel: usize },
    SourceServerInfo { is_jellyfin: bool, sel: usize },
    AccountsMenu { sel: usize },
    LastFmMenu { sel: usize },
    LastFmQrAuth,
    ListenBrainzMenu { sel: usize },
    LibraryMenu { sel: usize },
    LibraryFolderList { kind: u8, sel: usize },
    VisualizersMenu { sel: usize },
    VisualizerPresets { sel: usize },
    VisualizerActive,
    PowerMenu { sel: usize },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KbdTarget {
    HexBackground,
    HexForeground,
    HexShade1,
    HexShade2,
    ServerUrl(bool),
    ServerUser(bool),
    LastFmApiKey,
    LastFmApiSecret,
    ListenBrainzToken,
    ListenBrainzUrl,
}

#[derive(Debug, Clone, Copy)]
pub struct DspConfig {
    pub fx_enabled: bool,
    pub eq_enabled: bool,
    pub eq_bypass: bool,
    pub autoeq_idx: usize,
    pub eq_preamp_tenths_db: i16,
    pub eq_bands_tenths_db: [i16; 10],
    pub tape_enabled: bool,
    pub tape_wow: u8,
    pub tape_hiss: u8,
    pub tape_crinkle: u8,
    pub tape_sat: u8,
    pub tape_hf_rolloff: u8,
    pub tape_stop_start: bool,
    pub tape_ff_sound: bool,
    pub reverb_enabled: bool,
    pub reverb_size: u8,
    pub reverb_damp: u8,
    pub reverb_predelay_ms: u8,
    pub reverb_width: u8,
    pub reverb_mix: u8,
    pub delay_enabled: bool,
    pub delay_time_ms: u16,
    pub delay_feedback: u8,
    pub delay_tone: u8,
    pub delay_pingpong: bool,
    pub delay_mix: u8,
    pub chorus_enabled: bool,
    pub chorus_rate: u8,
    pub chorus_depth: u8,
    pub chorus_mix: u8,
    pub flanger_enabled: bool,
    pub flanger_rate: u8,
    pub flanger_depth: u8,
    pub flanger_feedback: i8,
    pub flanger_mix: u8,
    pub speed_pct: u16,
    pub menu_click: bool,
    pub rg_type: usize,
    pub rg_noclip: bool,
    pub rg_preamp_db: i8,
    pub balance_pct: i8,
    pub channels_idx: usize,
    pub crossfeed_idx: usize,
    pub dj_mode: u8,
    pub dj_length: u8,
    pub dj_max_stretch: u8,
    pub dj_keep_gapless: bool,
    pub dj_quick_cut: bool,
}

impl Default for DspConfig {
    fn default() -> Self {
        Self {
            fx_enabled: true,
            eq_enabled: true,
            eq_bypass: false,
            autoeq_idx: 0,
            eq_preamp_tenths_db: 0,
            eq_bands_tenths_db: [0; 10],
            tape_enabled: false,
            tape_wow: 35,
            tape_hiss: 22,
            tape_crinkle: 15,
            tape_sat: 30,
            tape_hf_rolloff: 40,
            tape_stop_start: true,
            tape_ff_sound: true,
            reverb_enabled: false,
            reverb_size: 65,
            reverb_damp: 45,
            reverb_predelay_ms: 20,
            reverb_width: 100,
            reverb_mix: 30,
            delay_enabled: false,
            delay_time_ms: 360,
            delay_feedback: 45,
            delay_tone: 60,
            delay_pingpong: false,
            delay_mix: 35,
            chorus_enabled: false,
            chorus_rate: 35,
            chorus_depth: 50,
            chorus_mix: 40,
            flanger_enabled: false,
            flanger_rate: 25,
            flanger_depth: 60,
            flanger_feedback: 60,
            flanger_mix: 45,
            speed_pct: 100,
            menu_click: true,
            rg_type: 1,
            rg_noclip: true,
            rg_preamp_db: 0,
            balance_pct: 0,
            channels_idx: 0,
            crossfeed_idx: 0,
            dj_mode: 0,
            dj_length: 1,
            dj_max_stretch: 1,
            dj_keep_gapless: true,
            dj_quick_cut: true,
        }
    }
}

pub struct UIPodApp {
    pub tracks: Vec<Track>,
    pub current_track: usize,
    pub play_mode: PlayMode,
    pub audio_playing: bool,
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
    pub main_menu_enabled: [bool; 12],

    // Now Playing / Visualizers / Video / Power / Accounts settings
    pub repeat_idx: usize,
    pub sleep_idx: usize,
    pub viz_delay_idx: usize,
    pub viz_trans_idx: usize,
    pub brightness_pct: u8,
    pub disp_off_idx: usize,
    pub idle_off_idx: usize,
    pub cpu_idx: usize,
    pub charge_limit_pct: u8,
    pub video_aspect_idx: usize,
    pub video_audio_sync_ms: i32,
    pub navidrome_url: String,
    pub navidrome_user: String,
    pub jellyfin_url: String,
    pub jellyfin_user: String,
    pub lfm_enabled: bool,
    pub lfm_streaming: bool,
    pub lfm_api_key: String,
    pub lfm_api_secret: String,
    pub lb_enabled: bool,
    pub lb_now_playing: bool,
    pub lb_streaming: bool,
    pub lb_user_token: String,
    pub lb_api_url: String,

    pub volume_step: i32,
    pub clock_ms: u64,
    pub hold_locked: bool,

    pub dsp: DspConfig,
    pub dj_telemetry: uipod_dj::DjTelemetry,
    last_np_skip_ms: u64,

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

    // Firmware `tp_hold` (`650 ms`) & smooth Now Playing seek physics (`apps/action.c`)
    b_deferred: bool,
    b_hold_sec: f32,
    b_hold_fired: bool,
    a_deferred: bool,
    a_hold_sec: f32,
    a_hold_fired: bool,
    seek_dir: i8,
    seek_hold_sec: f32,
    seek_active: bool,
    seek_pos_f: f32,
    seek_settle_sec: f32,
    pending_np_skip: i8,

    pub outbound_cmds: Vec<String>,
    pub user_playlist_uris: Vec<String>,
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
            audio_playing: false,
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
            // Browse, Extras, Artists, Albums, Genres, Videos ON; rest OFF
            main_menu_enabled: [
                true, true, true, true, true, true, false, false, false, false, false, false,
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
            video_aspect_idx: 0,
            video_audio_sync_ms: 0,
            navidrome_url: String::new(),
            navidrome_user: String::new(),
            jellyfin_url: String::new(),
            jellyfin_user: String::new(),
            lfm_enabled: true,
            lfm_streaming: true,
            lfm_api_key: String::new(),
            lfm_api_secret: String::new(),
            lb_enabled: true,
            lb_now_playing: true,
            lb_streaming: true,
            lb_user_token: String::new(),
            lb_api_url: "https://api.listenbrainz.org".to_string(),
            volume_step: 32,
            clock_ms: 1000,
            hold_locked: false,
            dsp: DspConfig::default(),
            dj_telemetry: uipod_dj::DjTelemetry::default(),
            last_np_skip_ms: 0,
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
            b_deferred: false,
            b_hold_sec: 0.0,
            b_hold_fired: false,
            a_deferred: false,
            a_hold_sec: 0.0,
            a_hold_fired: false,
            seek_dir: 0,
            seek_hold_sec: 0.0,
            seek_active: false,
            seek_pos_f: 0.0,
            seek_settle_sec: 0.0,
            pending_np_skip: 0,
            outbound_cmds: Vec::new(),
            user_playlist_uris: Vec::new(),
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

    pub fn is_fx_active(&self) -> bool {
        let d = &self.dsp;
        d.fx_enabled
            && (d.speed_pct != 100
                || d.tape_enabled
                || d.reverb_enabled
                || d.delay_enabled
                || d.chorus_enabled
                || d.flanger_enabled)
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

    pub fn apply_autoeq_preset(&mut self, idx: usize) {
        let clamped = idx % AUTOEQ_CURVES_TENTHS.len();
        self.dsp.autoeq_idx = clamped;
        let (bands, preamp) = AUTOEQ_CURVES_TENTHS[clamped];
        self.dsp.eq_bands_tenths_db = bands;
        self.dsp.eq_preamp_tenths_db = preamp;
        if clamped > 0 {
            self.dsp.eq_enabled = true;
        }
    }

    fn emit_dsp(&mut self) {
        let d = self.dsp;
        let bands_json = format!(
            "[{},{},{},{},{},{},{},{},{},{}]",
            d.eq_bands_tenths_db[0],
            d.eq_bands_tenths_db[1],
            d.eq_bands_tenths_db[2],
            d.eq_bands_tenths_db[3],
            d.eq_bands_tenths_db[4],
            d.eq_bands_tenths_db[5],
            d.eq_bands_tenths_db[6],
            d.eq_bands_tenths_db[7],
            d.eq_bands_tenths_db[8],
            d.eq_bands_tenths_db[9],
        );
        self.outbound_cmds.push(format!(
            concat!(
                r#"{{"type":"dsp","fxEnabled":{},"eq":{},"eqBypass":{},"autoeq":{},"eqPreamp":{},"eqBands":{},"#,
                r#""tape":{},"wow":{},"hiss":{},"sat":{},"hf":{},"stopStart":{},"speed":{},"#,
                r#""reverb":{},"reverbSize":{},"reverbDamp":{},"reverbPredelay":{},"reverbMix":{},"#,
                r#""delay":{},"delayTime":{},"delayFeedback":{},"delayTone":{},"delayMix":{},"#,
                r#""chorus":{},"chorusRate":{},"chorusDepth":{},"chorusMix":{},"#,
                r#""flanger":{},"flangerRate":{},"flangerDepth":{},"flangerFeedback":{},"flangerMix":{},"#,
                r#""djMode":{},"djLength":{},"djMaxStretch":{},"djKeepGapless":{},"djQuickCut":{}}}"#
            ),
            d.fx_enabled,
            d.eq_enabled,
            d.eq_bypass,
            d.autoeq_idx,
            d.eq_preamp_tenths_db,
            bands_json,
            d.tape_enabled,
            d.tape_wow,
            d.tape_hiss,
            d.tape_sat,
            d.tape_hf_rolloff,
            d.tape_stop_start,
            d.speed_pct,
            d.reverb_enabled,
            d.reverb_size,
            d.reverb_damp,
            d.reverb_predelay_ms,
            d.reverb_mix,
            d.delay_enabled,
            d.delay_time_ms,
            d.delay_feedback,
            d.delay_tone,
            d.delay_mix,
            d.chorus_enabled,
            d.chorus_rate,
            d.chorus_depth,
            d.chorus_mix,
            d.flanger_enabled,
            d.flanger_rate,
            d.flanger_depth,
            d.flanger_feedback,
            d.flanger_mix,
            d.dj_mode,
            d.dj_length,
            d.dj_max_stretch,
            d.dj_keep_gapless,
            d.dj_quick_cut
        ));
        self.state_dirty = true;
    }

    fn track_quiz_pool(&self) -> Vec<(String, u32)> {
        self.tracks
            .iter()
            .filter(|t| !t.is_sfx)
            .map(|t| (format!("{} - {}", t.artist, t.title), t.duration_ms))
            .collect()
    }

    fn emit_play_track_at(&mut self, idx: usize, pos_ms: u32) {
        if self.tracks.is_empty() {
            return;
        }
        self.dj_telemetry = uipod_dj::DjTelemetry::default();
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
        self.dj_telemetry = uipod_dj::DjTelemetry::default();
        let idx = idx % self.tracks.len();
        self.current_track = idx;
        self.pos_ms = 0;
        self.dur_ms = self.tracks[idx].duration_ms;
        self.play_mode = PlayMode::Play;
        self.outbound_cmds
            .push(format!(r#"{{"type":"playTrack","index":{idx}}}"#));
        self.state_dirty = true;
    }

    fn emit_manual_next_skip(&mut self) {
        if self.tracks.is_empty() {
            return;
        }
        let next = self.step_playable_track(self.current_track, 1);
        let rapid_double = self.clock_ms.saturating_sub(self.last_np_skip_ms) <= 500;
        self.last_np_skip_ms = self.clock_ms;
        if !rapid_double
            && self.dsp.dj_mode > 0
            && self.dsp.dj_quick_cut
            && self.audio_playing
            && next != self.current_track
        {
            self.outbound_cmds
                .push(format!(r#"{{"type":"quickCutNext","index":{next}}}"#));
        } else {
            self.emit_play_track(next);
        }
    }

    fn emit_set_playing(&mut self, should_play: bool) {
        if !should_play {
            self.dj_telemetry = uipod_dj::DjTelemetry::default();
        }
        self.audio_playing = should_play;
        if !self.seek_active {
            self.play_mode = if should_play {
                PlayMode::Play
            } else {
                PlayMode::Pause
            };
        }
        self.outbound_cmds.push(format!(
            r#"{{"type":"setPlaying","playing":{},"trackIndex":{}}}"#,
            should_play, self.current_track
        ));
        self.state_dirty = true;
    }

    fn emit_toggle_play_pause(&mut self) {
        let next = !self.audio_playing && self.play_mode != PlayMode::Play;
        self.emit_set_playing(next);
    }

    fn emit_seek_abs(&mut self, pos_ms: u32) {
        self.dj_telemetry = uipod_dj::DjTelemetry::default();
        let clamped = pos_ms.min(self.dur_ms);
        self.pos_ms = clamped;
        self.seek_settle_sec = 0.25;
        self.outbound_cmds
            .push(format!(r#"{{"type":"seek","posMs":{clamped}}}"#));
        self.state_dirty = true;
    }

    fn emit_seek_delta(&mut self, delta_ms: i32) {
        let next = (self.pos_ms as i64 + delta_ms as i64).clamp(0, self.dur_ms as i64) as u32;
        self.emit_seek_abs(next);
    }

    fn emit_seek_motor(&mut self, dir: i8) {
        let enabled =
            dir != 0 && self.dsp.fx_enabled && self.dsp.tape_enabled && self.dsp.tape_ff_sound;
        self.outbound_cmds.push(format!(
            r#"{{"type":"seekMotor","dir":{dir},"enabled":{enabled}}}"#
        ));
    }

    fn cancel_seek_if_active(&mut self) {
        if self.seek_active {
            self.seek_active = false;
            self.emit_seek_motor(0);
            self.play_mode = if self.audio_playing {
                PlayMode::Play
            } else {
                PlayMode::Pause
            };
        }
        self.seek_dir = 0;
        self.seek_hold_sec = 0.0;
    }

    pub fn volume_db(&self) -> i32 {
        ((self.volume_step - 40) as f32 * 1.5).round() as i32
    }

    pub fn adjust_volume(&mut self, delta: i32, ui: &mut UiEngine) {
        self.volume_step = (self.volume_step + delta).clamp(0, 40);
        let norm = self.volume_step as f32 / 40.0;
        ui.trigger_volume_popup(norm, self.clock_ms);
        self.outbound_cmds.push(format!(
            r#"{{"type":"volume","norm":{norm:.3},"db":{}}}"#,
            self.volume_db()
        ));
        self.state_dirty = true;
    }

    pub fn set_hold(&mut self, held: bool) {
        self.hold_locked = held;
        if held {
            self.cancel_seek_if_active();
            self.held_buttons = 0;
            self.b_deferred = false;
            self.a_deferred = false;
        }
        self.state_dirty = true;
    }

    fn is_context_aware_screen(&self) -> bool {
        matches!(
            self.stack.last(),
            Some(
                Screen::NowPlaying
                    | Screen::NsfChannels { .. }
                    | Screen::NsfContainer { .. }
                    | Screen::NsfSfx { .. }
                    | Screen::VideoPlayer { .. }
                    | Screen::MainMenu { .. }
                    | Screen::Browse { .. }
                    | Screen::Artists { .. }
                    | Screen::ArtistSongs { .. }
                    | Screen::Albums { .. }
                    | Screen::AlbumSongs { .. }
                    | Screen::Genres { .. }
                    | Screen::GenreSongs { .. }
                    | Screen::AllSongs { .. }
                    | Screen::Playlists { .. }
            )
        )
    }

    pub(crate) fn nsf_container_name_for_track(t: &Track) -> Option<String> {
        if !t.codec.eq_ignore_ascii_case("NSF") {
            return None;
        }
        let uri = t.uri.as_deref()?;
        let parsed = SubtrackUri::parse(uri);
        if parsed.file_path.is_empty() {
            None
        } else {
            Some(parsed.file_path.to_string())
        }
    }

    pub(crate) fn browse_music_items(&self) -> Vec<BrowseMusicItem> {
        let mut out = Vec::new();
        let mut seen_nsf: Vec<String> = Vec::new();
        for (i, t) in self.tracks.iter().enumerate() {
            if let Some(fname) = Self::nsf_container_name_for_track(t) {
                if !seen_nsf.iter().any(|s| s.eq_ignore_ascii_case(&fname)) {
                    let mut music_count = 0usize;
                    let mut first_music_idx = None;
                    let mut fallback_idx = None;
                    for (j, cand) in self.tracks.iter().enumerate() {
                        if Self::nsf_container_name_for_track(cand)
                            .is_some_and(|cf| cf.eq_ignore_ascii_case(&fname))
                        {
                            if fallback_idx.is_none() {
                                fallback_idx = Some(j);
                            }
                            if !cand.is_sfx {
                                music_count += 1;
                                if first_music_idx.is_none() {
                                    first_music_idx = Some(j);
                                }
                            }
                        }
                    }
                    seen_nsf.push(fname.clone());
                    out.push(BrowseMusicItem::NsfContainer {
                        file_name: fname,
                        music_count,
                        play_idx: first_music_idx.or(fallback_idx),
                    });
                }
            } else if !t.is_sfx {
                out.push(BrowseMusicItem::Track(i));
            }
        }
        out.push(BrowseMusicItem::AboutTxt);
        out
    }

    pub(crate) fn nsf_subtracks_for_file(&self, file_name: &str) -> (Vec<usize>, Vec<usize>, String) {
        let mut music = Vec::new();
        let mut sfx = Vec::new();
        let mut game_title = String::new();
        for (i, t) in self.tracks.iter().enumerate() {
            if Self::nsf_container_name_for_track(t)
                .is_some_and(|cf| cf.eq_ignore_ascii_case(file_name))
            {
                if game_title.is_empty() && !t.album.is_empty() {
                    game_title = t.album.clone();
                }
                if t.is_sfx {
                    sfx.push(i);
                } else {
                    music.push(i);
                }
            }
        }
        if game_title.is_empty() {
            game_title = file_name
                .rsplit(['/', '\\'])
                .next()
                .unwrap_or(file_name)
                .rsplit_once('.')
                .map(|(s, _)| s)
                .unwrap_or(file_name)
                .to_string();
        }
        (music, sfx, game_title)
    }

    fn step_playable_track(&self, from: usize, dir: isize) -> usize {
        if self.tracks.is_empty() {
            return 0;
        }
        if self.tracks.get(from).is_some_and(|t| t.is_sfx) {
            let n = self.tracks.len() as isize;
            return (((from as isize + dir) % n + n) % n) as usize;
        }
        let music: Vec<usize> = self
            .tracks
            .iter()
            .enumerate()
            .filter(|(_, t)| !t.is_sfx)
            .map(|(i, _)| i)
            .collect();
        if music.is_empty() {
            return 0;
        }
        let pos = music.iter().position(|&i| i == from).unwrap_or(0) as isize;
        let n = music.len() as isize;
        music[(((pos + dir) % n + n) % n) as usize]
    }

    pub fn active_nsf_chip_flags_for_ui(&self) -> Option<ChipFlags> {
        if let Some(flags) = get_nsf_active_chip_flags() {
            return Some(flags);
        }
        let t = self.tracks.get(self.current_track)?;
        if t.codec.eq_ignore_ascii_case("NSF") {
            Some(ChipFlags(t.chip_flags))
        } else {
            None
        }
    }

    pub fn solo_or_restore_nsf_channel(chip_flags: ChipFlags, row: usize) {
        let descs = chip_flags.channel_descriptors();
        let Some(target) = descs.get(row) else {
            return;
        };
        let mut valid_mask = 0u16;
        for d in descs {
            valid_mask |= 1u16 << d.bit_index;
        }
        let bit = 1u16 << target.bit_index;
        let solo_mask = valid_mask & !bit;
        let cur = get_nsf_channel_mute_mask() & valid_mask;
        if cur == solo_mask {
            set_nsf_channel_mute_mask(0);
        } else {
            set_nsf_channel_mute_mask(solo_mask);
        }
    }

    pub fn toggle_nsf_channel_row(chip_flags: ChipFlags, row: usize) {
        let descs = chip_flags.channel_descriptors();
        if row == descs.len() {
            set_nsf_channel_mute_mask(0);
        } else if let Some(d) = descs.get(row) {
            let bit = 1u16 << d.bit_index;
            set_nsf_channel_mute_mask(get_nsf_channel_mute_mask() ^ bit);
        }
    }

    fn resolved_selected_track_for_top(&self) -> Option<usize> {
        match self.stack.last()? {
            Screen::AllSongs { sel } => self
                .tracks
                .iter()
                .enumerate()
                .filter(|(_, t)| !t.is_sfx)
                .map(|(i, _)| i)
                .nth(*sel),
            Screen::Browse { path, sel } if path == "/mnt/SDCARD/Media/Music" => {
                match self.browse_music_items().get(*sel)? {
                    BrowseMusicItem::Track(idx) => Some(*idx),
                    BrowseMusicItem::NsfContainer { play_idx, .. } => *play_idx,
                    BrowseMusicItem::AboutTxt => None,
                }
            }
            Screen::NsfContainer { file_name, sel } => {
                let (music, _, _) = self.nsf_subtracks_for_file(file_name);
                if !music.is_empty() && *sel >= 1 && *sel <= music.len() {
                    music.get(*sel - 1).copied()
                } else {
                    None
                }
            }
            Screen::NsfSfx { file_name, sel } => {
                let (_, sfx, _) = self.nsf_subtracks_for_file(file_name);
                sfx.get(*sel).copied()
            }
            Screen::ArtistSongs { artist, sel } => self
                .tracks
                .iter()
                .enumerate()
                .filter(|(_, t)| !t.is_sfx && &t.artist == artist)
                .map(|(i, _)| i)
                .nth(*sel),
            Screen::AlbumSongs { album, sel } => self
                .tracks
                .iter()
                .enumerate()
                .filter(|(_, t)| !t.is_sfx && &t.album == album)
                .map(|(i, _)| i)
                .nth(*sel),
            Screen::GenreSongs { genre, sel } => self
                .tracks
                .iter()
                .enumerate()
                .filter(|(_, t)| &t.genre == genre)
                .map(|(i, _)| i)
                .nth(*sel),
            Screen::Playlists { sel } => {
                (!self.tracks.is_empty()).then_some(*sel % self.tracks.len())
            }
            _ => (!self.tracks.is_empty()).then_some(self.current_track),
        }
    }

    fn open_context_menu_for_top(&mut self, ui: &mut UiEngine) -> bool {
        match self.stack.last() {
            Some(Screen::NowPlaying) => {
                self.emit_click();
                self.push_screen(Screen::NowPlayingContext { sel: 0 }, ui);
                true
            }
            Some(Screen::NsfChannels { sel }) => {
                let row = *sel;
                if let Some(cflags) = self.active_nsf_chip_flags_for_ui() {
                    if row < cflags.channel_descriptors().len() {
                        Self::solo_or_restore_nsf_channel(cflags, row);
                        self.emit_click();
                        self.state_dirty = true;
                        return true;
                    }
                }
                false
            }
            Some(Screen::VideoPlayer { .. }) => {
                self.emit_click();
                self.push_screen(Screen::VideoContextMenu { sel: 0 }, ui);
                true
            }
            Some(Screen::MainMenu { .. }) => {
                self.emit_click();
                self.push_screen(Screen::SettingsMenu { sel: 0 }, ui);
                true
            }
            Some(
                Screen::Browse { .. }
                | Screen::NsfContainer { .. }
                | Screen::NsfSfx { .. }
                | Screen::Artists { .. }
                | Screen::ArtistSongs { .. }
                | Screen::Albums { .. }
                | Screen::AlbumSongs { .. }
                | Screen::Genres { .. }
                | Screen::GenreSongs { .. }
                | Screen::AllSongs { .. }
                | Screen::Playlists { .. },
            ) => {
                if let Some(track_idx) = self.resolved_selected_track_for_top() {
                    self.emit_click();
                    self.push_screen(
                        Screen::TrackContextMenu {
                            track_idx,
                            sel: 0,
                        },
                        ui,
                    );
                    true
                } else {
                    false
                }
            }
            _ => false,
        }
    }

    fn go_home(&mut self, ui: &mut UiEngine) {
        self.cancel_seek_if_active();
        self.emit_click();
        self.active_plugin = None;
        if matches!(self.stack.last(), Some(Screen::MainMenu { .. })) && self.stack.len() == 1 {
            return;
        }
        ui.snapshot_fbo_for_transition();
        self.stack.clear();
        self.stack.push(Screen::MainMenu { sel: 0 });
        self.start_transition(-1);
        self.state_dirty = true;
    }

    pub fn button_down(&mut self, mask: u32) {
        if self.hold_locked {
            return;
        }
        self.held_buttons |= mask;

        let on_keyboard = matches!(self.stack.last(), Some(Screen::Keyboard { .. }));
        let on_now_playing = matches!(self.stack.last(), Some(Screen::NowPlaying));
        let ctx_aware = self.is_context_aware_screen();

        let mut immediate = mask;

        // `B` uses `650 ms` `tp_hold` (`Hold B` -> Main Menu, tap on release) everywhere except Keyboard
        if (mask & btn::B) != 0 && !on_keyboard {
            immediate &= !btn::B;
            self.b_deferred = true;
            self.b_hold_sec = 0.0;
            self.b_hold_fired = false;
        }

        // `A` uses `650 ms` `tp_hold` (`Hold A` -> Context Menu, tap on release) on context-aware screens
        if (mask & btn::A) != 0 && ctx_aware {
            immediate &= !btn::A;
            self.a_deferred = true;
            self.a_hold_sec = 0.0;
            self.a_hold_fired = false;
        }

        // `LEFT` / `RIGHT` on Now Playing: short tap skips track on release; hold engages smooth seek (`<<`/`>>`)
        if on_now_playing && (mask & (btn::LEFT | btn::RIGHT)) != 0 {
            immediate &= !(btn::LEFT | btn::RIGHT);
            self.seek_dir = if (mask & btn::RIGHT) != 0 { 1 } else { -1 };
            self.seek_hold_sec = 0.0;
            self.seek_active = false;
        }

        self.pending_pressed |= immediate;
        if immediate != 0 {
            self.repeat_btn = immediate;
            self.repeat_timer = 0.0;
        }
    }

    pub fn button_up(&mut self, mask: u32) {
        self.held_buttons &= !mask;
        if (self.repeat_btn & mask) != 0 {
            self.repeat_btn = 0;
            self.repeat_timer = 0.0;
        }

        if (mask & btn::B) != 0 && self.b_deferred {
            self.b_deferred = false;
            if !self.b_hold_fired && !self.hold_locked {
                self.pending_pressed |= btn::B;
            }
            self.b_hold_fired = false;
            self.b_hold_sec = 0.0;
        }

        if (mask & btn::A) != 0 && self.a_deferred {
            self.a_deferred = false;
            if !self.a_hold_fired && !self.hold_locked {
                self.pending_pressed |= btn::A;
            }
            self.a_hold_fired = false;
            self.a_hold_sec = 0.0;
        }

        if (mask & (btn::LEFT | btn::RIGHT)) != 0 && self.seek_dir != 0 {
            if self.seek_active {
                let target_ms = self.pos_ms;
                self.cancel_seek_if_active();
                self.emit_seek_abs(target_ms);
            } else if !self.hold_locked {
                self.pending_np_skip = self.seek_dir;
                self.seek_dir = 0;
                self.seek_hold_sec = 0.0;
            } else {
                self.seek_dir = 0;
                self.seek_hold_sec = 0.0;
            }
        }
    }

    pub fn button_long_press(&mut self, mask: u32, ui: &mut UiEngine) {
        if self.hold_locked {
            return;
        }
        if (mask & btn::B) != 0 {
            self.b_deferred = false;
            self.b_hold_fired = true;
            self.go_home(ui);
        } else if (mask & btn::A) != 0 {
            self.a_deferred = false;
            self.a_hold_fired = true;
            self.open_context_menu_for_top(ui);
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
        self.cancel_seek_if_active();
        ui.snapshot_fbo_for_transition();
        self.stack.push(screen);
        self.start_transition(1);
        self.state_dirty = true;
    }

    fn pop_screen(&mut self, ui: &mut UiEngine) {
        self.cancel_seek_if_active();
        if self.stack.len() > 1 {
            ui.snapshot_fbo_for_transition();
            self.stack.pop();
            self.start_transition(-1);
            self.state_dirty = true;
        }
    }

    pub fn jump_to_screen(&mut self, screen_id: &str, ui: &mut UiEngine, viz: &mut VizEngine) {
        self.cancel_seek_if_active();
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
                    Screen::SettingsMenu { sel: 6 },
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
                    Screen::Keyboard {
                        target: KbdTarget::HexBackground,
                        text: hex,
                        row: 0,
                        col: 0,
                        caps: true,
                        sym_mode: false,
                        allow_symbols: false,
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
            "dj_crossfade" => {
                self.stack = vec![
                    Screen::MainMenu { sel: 0 },
                    Screen::SettingsMenu { sel: 1 },
                    Screen::SoundMenu { sel: 2 },
                    Screen::DjCrossfadeMenu { sel: 0 },
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
                self.active_plugin = Some(ActivePlugin::launch(pid, &pool, self.pos_ms));
                self.stack = vec![
                    Screen::MainMenu { sel: 1 },
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
            uri: None,
            subtrack_index: None,
            is_sfx: false,
            chip_flags: 0,
            loop_point_ms: None,
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

    #[allow(clippy::too_many_arguments)]
    pub fn add_nsf_subtrack(
        &mut self,
        uri: String,
        subtrack_index: u16,
        is_sfx: bool,
        chip_flags: u8,
        loop_point_ms: Option<u32>,
        title: String,
        artist: String,
        album: String,
        duration_ms: u32,
        art_bytes: Option<&[u8]>,
    ) -> usize {
        let id = self.tracks.len();
        let art_rgb24 = art_bytes.and_then(decode_cover_256);
        self.tracks.push(Track {
            id,
            uri: Some(uri),
            subtrack_index: Some(subtrack_index),
            is_sfx,
            chip_flags,
            loop_point_ms,
            title,
            artist,
            album,
            genre: "Chiptune".to_string(),
            codec: "NSF".to_string(),
            bitdepth: "16bit".to_string(),
            samplerate: "44.1kHz".to_string(),
            duration_ms: duration_ms.max(400),
            is_user_file: true,
            art_rgb24,
        });
        self.state_dirty = true;
        id
    }

    pub fn sync_audio_state(&mut self, playing: bool, pos_ms: u32, dur_ms: u32) {
        self.audio_playing = playing;
        if dur_ms > 0 {
            self.dur_ms = dur_ms;
        }
        // Do NOT overwrite `PlayMode::FastForward` / `PlayMode::Rewind` (`>>` / `<<`) or live scrub position!
        if !self.seek_active && self.seek_settle_sec <= 0.0 {
            self.play_mode = if playing {
                PlayMode::Play
            } else {
                PlayMode::Pause
            };
            self.pos_ms = pos_ms;
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
            is_fx: self.is_fx_active(),
            is_downloading: false,
            download_done: 0,
            download_total: 0,
            source_icon: self.source_icon,
        }
    }

    fn artists_list(&self) -> Vec<String> {
        let mut out = Vec::new();
        for t in self.tracks.iter().filter(|t| !t.is_sfx) {
            if !out.contains(&t.artist) {
                out.push(t.artist.clone());
            }
        }
        out
    }

    fn albums_list(&self) -> Vec<String> {
        let mut out = Vec::new();
        for t in self.tracks.iter().filter(|t| !t.is_sfx) {
            if !out.contains(&t.album) {
                out.push(t.album.clone());
            }
        }
        out
    }

    fn genres_list(&self) -> Vec<String> {
        let mut out = Vec::new();
        for t in self.tracks.iter().filter(|t| !t.is_sfx) {
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
        if self.seek_settle_sec > 0.0 {
            self.seek_settle_sec = (self.seek_settle_sec - dt).max(0.0);
        }

        // Advance demo Video Player timeline when active & playing
        if let Some(Screen::VideoPlayer {
            playing: true,
            pos_ms,
            dur_ms,
        }) = self.stack.last_mut()
        {
            let step = ((dt * 1000.0).round() as u32).max(1);
            *pos_ms = (*pos_ms + step) % (*dur_ms).max(1000);
        }

        // 1. Firmware `650 ms` `Hold B` -> Home (`apps/action.c` `global_home_action`)
        if self.b_deferred && (self.held_buttons & btn::B) != 0 && !self.b_hold_fired {
            self.b_hold_sec += dt;
            if self.b_hold_sec >= 0.65 {
                self.b_hold_fired = true;
                self.go_home(ui);
                return;
            }
        }

        // 2. Firmware `650 ms` `Hold A` -> Context Menu (`apps/action.c` `global_context_action`)
        if self.a_deferred && (self.held_buttons & btn::A) != 0 && !self.a_hold_fired {
            self.a_hold_sec += dt;
            if self.a_hold_sec >= 0.65 {
                self.a_hold_fired = true;
                if self.open_context_menu_for_top(ui) {
                    return;
                }
            }
        }

        // 3. Smooth accelerated `Now Playing` rewind / fast-forward (`<<` / `>>` in Header)
        let on_now_playing = matches!(self.stack.last(), Some(Screen::NowPlaying));
        if on_now_playing && self.seek_dir != 0 && (self.held_buttons & (btn::LEFT | btn::RIGHT)) != 0 {
            self.seek_hold_sec += dt;
            if self.seek_hold_sec >= 0.32 {
                if !self.seek_active {
                    self.seek_active = true;
                    self.seek_pos_f = self.pos_ms as f32;
                    self.emit_seek_motor(self.seek_dir);
                }
                self.play_mode = if self.seek_dir > 0 {
                    PlayMode::FastForward
                } else {
                    PlayMode::Rewind
                };
                let hold_over = self.seek_hold_sec - 0.32;
                let rate_x = if hold_over < 1.2 {
                    5.0 + hold_over * 5.0
                } else if hold_over < 3.0 {
                    11.0 + (hold_over - 1.2) * 8.0
                } else {
                    26.0
                };
                self.seek_pos_f = (self.seek_pos_f
                    + (self.seek_dir as f32) * rate_x * 1000.0 * dt)
                    .clamp(0.0, self.dur_ms as f32);
                self.pos_ms = self.seek_pos_f.round() as u32;
            }
        } else if self.seek_active && !on_now_playing {
            self.cancel_seek_if_active();
        }

        let is_plugin = matches!(self.stack.last(), Some(Screen::PluginActive));
        let is_viz = matches!(self.stack.last(), Some(Screen::VisualizerActive));

        let on_nsf_channels = matches!(self.stack.last(), Some(Screen::NsfChannels { .. }));

        // Key auto-repeat for list scrolling / spinner cycling (NEVER auto-repeat Left/Right on NowPlaying or NsfChannels!)
        let repeat_mask = if on_now_playing || on_nsf_channels {
            btn::UP | btn::DOWN
        } else {
            btn::UP | btn::DOWN | btn::LEFT | btn::RIGHT
        };
        if !is_plugin && !is_viz && (self.held_buttons & repeat_mask) != 0 {
            self.repeat_timer += dt;
            if self.repeat_timer > 0.30 {
                self.repeat_timer -= 0.085;
                self.pending_pressed |= self.held_buttons & repeat_mask;
            }
        }

        let pressed = self.pending_pressed;
        self.pending_pressed = 0;

        let np_skip = self.pending_np_skip;
        self.pending_np_skip = 0;
        if on_now_playing && np_skip != 0 {
            self.emit_click();
            if np_skip < 0 {
                if self.pos_ms > 3_500 {
                    self.emit_seek_abs(0);
                } else {
                    let prev = self.step_playable_track(self.current_track, -1);
                    self.emit_play_track(prev);
                }
            } else {
                self.emit_manual_next_skip();
            }
        }

        let on_video = matches!(self.stack.last(), Some(Screen::VideoPlayer { .. }));
        let on_browser_play = matches!(
            self.stack.last(),
            Some(
                Screen::Browse { .. }
                    | Screen::NsfContainer { .. }
                    | Screen::NsfSfx { .. }
            )
        );

        // Global START Play/Pause outside games, video player, and File Browser direct-play
        if !is_plugin && !on_video && !on_browser_play && (pressed & btn::START) != 0 {
            self.emit_click();
            self.emit_toggle_play_pause();
        }

        if is_plugin {
            let cur_title = self
                .tracks
                .get(self.current_track)
                .map(|t| format!("{} — {}", t.artist, t.title))
                .unwrap_or_default();
            let audio_snap = AudioSnapshot {
                playing: self.audio_playing || self.play_mode == PlayMode::Play,
                track_title: &cur_title,
                pos_ms: self.pos_ms,
                dur_ms: self.dur_ms,
            };
            let act = if let Some(ref mut pl) = self.active_plugin {
                pl.update(dt, pressed, self.held_buttons, pcm_interleaved, audio_snap, self.uk)
            } else {
                PluginAction::Continue
            };
            match act {
                PluginAction::Exit => {
                    self.emit_click();
                    self.active_plugin = None;
                    self.pop_screen(ui);
                }
                PluginAction::Click => {
                    self.emit_click();
                }
                PluginAction::PlayTrackAt(idx, pos_ms) => {
                    self.audio_playing = true;
                    self.emit_play_track_at(idx, pos_ms);
                }
                PluginAction::SeekMs(pos_ms) => {
                    self.emit_set_playing(true);
                    self.emit_seek_abs(pos_ms);
                }
                PluginAction::EnsurePlaying => {
                    if !self.audio_playing {
                        self.emit_set_playing(true);
                    }
                }
                PluginAction::TogglePlayPause => {
                    self.emit_click();
                    self.emit_toggle_play_pause();
                }
                PluginAction::SetPlaying(play) => {
                    self.emit_set_playing(play);
                }
                PluginAction::NextTrack => {
                    self.audio_playing = true;
                    let next = (self.current_track + 1) % self.tracks.len().max(1);
                    self.emit_play_track(next);
                }
                PluginAction::PrevTrack => {
                    self.audio_playing = true;
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

        // Momentary X-hold bypass while on Graphic Equalizer
        let on_graphic_eq = matches!(self.stack.last(), Some(Screen::GraphicEq { .. }));
        let want_eq_bypass = on_graphic_eq && (self.held_buttons & btn::X) != 0;
        if want_eq_bypass != self.dsp.eq_bypass {
            self.dsp.eq_bypass = want_eq_bypass;
            self.emit_dsp();
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
                            MainMenuId::Extras => {
                                self.push_screen(Screen::ExtrasMenu { sel: 0 }, ui)
                            }
                            MainMenuId::Artists => {
                                self.push_screen(Screen::Artists { sel: 0 }, ui)
                            }
                            MainMenuId::Albums => self.push_screen(Screen::Albums { sel: 0 }, ui),
                            MainMenuId::Genres => self.push_screen(Screen::Genres { sel: 0 }, ui),
                            MainMenuId::Videos => {
                                self.push_screen(Screen::VideosList { sel: 0 }, ui)
                            }
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
                                let music_indices: Vec<usize> = self
                                    .tracks
                                    .iter()
                                    .enumerate()
                                    .filter(|(_, t)| !t.is_sfx)
                                    .map(|(i, _)| i)
                                    .collect();
                                if let Some(&idx) = if music_indices.len() > 1 {
                                    let seed = (self.pos_ms as usize)
                                        .wrapping_mul(1103515245)
                                        .wrapping_add(self.current_track * 7 + 12345);
                                    let mut pick = music_indices[seed % music_indices.len()];
                                    if pick == self.current_track {
                                        pick = music_indices[(seed + 1) % music_indices.len()];
                                    }
                                    Some(&music_indices[music_indices
                                        .iter()
                                        .position(|&x| x == pick)
                                        .unwrap_or(0)])
                                } else {
                                    music_indices.first()
                                } {
                                    self.emit_play_track(idx);
                                    self.push_screen(Screen::NowPlaying, ui);
                                }
                            }
                        },
                    }
                }
            }

            Screen::Browse { path, mut sel } => {
                let music_items = self.browse_music_items();
                let count = match path.as_str() {
                    "/mnt/SDCARD" => 3,
                    "/mnt/SDCARD/Media" => 2,
                    _ => music_items.len(),
                };
                if self.nav_list(pressed, &mut sel, count.max(1), ui) {
                    self.set_top_sel(sel);
                } else if (pressed & btn::MENU) != 0 {
                    self.open_context_menu_for_top(ui);
                } else if (pressed & btn::START) != 0 {
                    if path != "/mnt/SDCARD" && path != "/mnt/SDCARD/Media" {
                        if let Some(item) = music_items.get(sel) {
                            let play_target = match item {
                                BrowseMusicItem::NsfContainer { play_idx, .. } => *play_idx,
                                BrowseMusicItem::Track(idx) => Some(*idx),
                                BrowseMusicItem::AboutTxt => None,
                            };
                            if let Some(idx) = play_target {
                                self.emit_click();
                                self.emit_play_track(idx);
                                self.push_screen(Screen::NowPlaying, ui);
                            }
                        }
                    } else {
                        self.emit_click();
                        self.emit_toggle_play_pause();
                    }
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
                                self.active_plugin = Some(ActivePlugin::launch(
                                    PluginId::TextViewer,
                                    &[],
                                    self.pos_ms,
                                ));
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
                                self.active_plugin = Some(ActivePlugin::launch(
                                    PluginId::TextViewer,
                                    &[],
                                    self.pos_ms,
                                ));
                                self.push_screen(Screen::PluginActive, ui);
                            }
                        },
                        _ => match music_items.get(sel) {
                            Some(BrowseMusicItem::NsfContainer { file_name, .. }) => {
                                self.push_screen(
                                    Screen::NsfContainer {
                                        file_name: file_name.clone(),
                                        sel: 0,
                                    },
                                    ui,
                                );
                            }
                            Some(BrowseMusicItem::Track(idx)) => {
                                self.emit_play_track(*idx);
                                self.push_screen(Screen::NowPlaying, ui);
                            }
                            _ => {
                                self.active_plugin = Some(ActivePlugin::launch(
                                    PluginId::TextViewer,
                                    &[],
                                    self.pos_ms,
                                ));
                                self.push_screen(Screen::PluginActive, ui);
                            }
                        },
                    }
                }
            }

            Screen::NsfContainer {
                file_name,
                mut sel,
            } => {
                let (music, sfx, _) = self.nsf_subtracks_for_file(&file_name);
                let row_count = (if music.is_empty() { 0 } else { 1 + music.len() })
                    + usize::from(!sfx.is_empty());
                if self.nav_list(pressed, &mut sel, row_count.max(1), ui) {
                    self.set_top_sel(sel);
                } else if (pressed & btn::MENU) != 0 {
                    self.open_context_menu_for_top(ui);
                } else if (pressed & (btn::A | btn::START)) != 0 {
                    let is_start = (pressed & btn::START) != 0;
                    let mut cur = sel;
                    if !music.is_empty() {
                        if cur == 0 {
                            self.emit_click();
                            let seed = (self.pos_ms as usize)
                                .wrapping_mul(1103515245)
                                .wrapping_add(self.current_track + 7);
                            let pick = music[seed % music.len()];
                            self.emit_play_track(pick);
                            self.push_screen(Screen::NowPlaying, ui);
                            return;
                        }
                        cur -= 1;
                        if cur < music.len() {
                            self.emit_click();
                            self.emit_play_track(music[cur]);
                            self.push_screen(Screen::NowPlaying, ui);
                            return;
                        }
                        cur -= music.len();
                    }
                    if !is_start && !sfx.is_empty() && cur == 0 {
                        self.emit_click();
                        self.push_screen(
                            Screen::NsfSfx {
                                file_name,
                                sel: 0,
                            },
                            ui,
                        );
                    }
                }
            }

            Screen::NsfSfx {
                file_name,
                mut sel,
            } => {
                let (_, sfx, _) = self.nsf_subtracks_for_file(&file_name);
                if self.nav_list(pressed, &mut sel, sfx.len().max(1), ui) {
                    self.set_top_sel(sel);
                } else if (pressed & btn::MENU) != 0 {
                    self.open_context_menu_for_top(ui);
                } else if (pressed & (btn::A | btn::START)) != 0 {
                    if let Some(&idx) = sfx.get(sel) {
                        self.emit_click();
                        self.emit_play_track(idx);
                        self.push_screen(Screen::NowPlaying, ui);
                    }
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
                            self.active_plugin =
                                Some(ActivePlugin::launch(pid, &pool, self.pos_ms));
                            self.push_screen(Screen::PluginActive, ui);
                        }
                        1 => {
                            self.active_plugin = Some(ActivePlugin::launch(
                                PluginId::Stopwatch,
                                &pool,
                                self.pos_ms,
                            ));
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
                            self.active_plugin = Some(ActivePlugin::launch(
                                PluginId::TextViewer,
                                &pool,
                                self.pos_ms,
                            ));
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

            Screen::VideosList { mut sel } => {
                if self.nav_list(pressed, &mut sel, 1, ui) {
                    self.set_top_sel(sel);
                } else if (pressed & btn::A) != 0 {
                    self.emit_click();
                    self.push_screen(
                        Screen::VideoPlayer {
                            playing: true,
                            pos_ms: 0,
                            dur_ms: 94_000,
                        },
                        ui,
                    );
                }
            }

            Screen::VideoPlayer {
                mut playing,
                mut pos_ms,
                dur_ms,
            } => {
                if (pressed & btn::B) != 0 {
                    self.emit_click();
                    self.pop_screen(ui);
                    return;
                }
                if (pressed & (btn::A | btn::START)) != 0 {
                    self.emit_click();
                    playing = !playing;
                } else if (pressed & btn::MENU) != 0 {
                    self.emit_click();
                    self.push_screen(Screen::VideoContextMenu { sel: 0 }, ui);
                    return;
                } else if (pressed & (btn::LEFT | btn::L2)) != 0 {
                    pos_ms = pos_ms.saturating_sub(10_000);
                } else if (pressed & (btn::RIGHT | btn::R2)) != 0 {
                    pos_ms = (pos_ms + 10_000).min(dur_ms);
                } else if (pressed & btn::UP) != 0 {
                    self.adjust_volume(2, ui);
                } else if (pressed & btn::DOWN) != 0 {
                    self.adjust_volume(-2, ui);
                }
                if let Some(Screen::VideoPlayer {
                    playing: p,
                    pos_ms: pm,
                    ..
                }) = self.stack.last_mut()
                {
                    *p = playing;
                    *pm = pos_ms;
                }
            }

            Screen::VideoContextMenu { mut sel } => {
                // Exact 3 rows from `apps/uipod_video.c` (`#11301` — `Video Settings`):
                // Aspect Ratio, Brightness, Audio Sync
                if self.nav_list(pressed, &mut sel, 3, ui) {
                    self.set_top_sel(sel);
                } else if (pressed & (btn::A | btn::LEFT | btn::RIGHT)) != 0 {
                    let left = (pressed & btn::LEFT) != 0;
                    self.emit_click();
                    match sel {
                        0 => {
                            self.video_aspect_idx = if left {
                                (self.video_aspect_idx + 2) % 3
                            } else {
                                (self.video_aspect_idx + 1) % 3
                            };
                        }
                        1 => {
                            let d = if left { -10i32 } else { 10i32 };
                            self.brightness_pct =
                                (self.brightness_pct as i32 + d).clamp(10, 100) as u8;
                        }
                        _ => {
                            let d = if left { -50i32 } else { 50i32 };
                            self.video_audio_sync_ms =
                                (self.video_audio_sync_ms + d).clamp(-500, 500);
                        }
                    }
                    self.state_dirty = true;
                }
            }

            Screen::TrackContextMenu { track_idx, mut sel } => {
                if self.nav_list(pressed, &mut sel, 5, ui) {
                    self.set_top_sel(sel);
                } else if (pressed & btn::A) != 0 {
                    self.emit_click();
                    match sel {
                        0 => {
                            self.pop_screen(ui);
                            self.emit_play_track(track_idx);
                            self.push_screen(Screen::NowPlaying, ui);
                        }
                        1 => {
                            self.pop_screen(ui);
                        }
                        2 => {
                            if let Some(t) = self.tracks.get(track_idx) {
                                let entry = t
                                    .uri
                                    .clone()
                                    .unwrap_or_else(|| format!("{}.{}", t.title, t.codec.to_lowercase()));
                                if !self.user_playlist_uris.contains(&entry) {
                                    self.user_playlist_uris.push(entry);
                                }
                            }
                            self.state_dirty = true;
                            self.pop_screen(ui);
                        }
                        3 => {
                            let idx = (track_idx + 1) % self.tracks.len().max(1);
                            self.pop_screen(ui);
                            self.emit_play_track(idx);
                            self.push_screen(Screen::NowPlaying, ui);
                        }
                        _ => {
                            self.pop_screen(ui);
                            self.push_screen(Screen::NowPlaying, ui);
                        }
                    }
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
                } else if (pressed & btn::MENU) != 0 {
                    self.open_context_menu_for_top(ui);
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
                    .filter(|(_, t)| !t.is_sfx && t.artist == artist)
                    .map(|(i, _)| i)
                    .collect();
                if self.nav_list(pressed, &mut sel, indices.len().max(1), ui) {
                    self.set_top_sel(sel);
                } else if (pressed & btn::MENU) != 0 {
                    self.open_context_menu_for_top(ui);
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
                } else if (pressed & btn::MENU) != 0 {
                    self.open_context_menu_for_top(ui);
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
                    .filter(|(_, t)| !t.is_sfx && t.album == album)
                    .map(|(i, _)| i)
                    .collect();
                if self.nav_list(pressed, &mut sel, indices.len().max(1), ui) {
                    self.set_top_sel(sel);
                } else if (pressed & btn::MENU) != 0 {
                    self.open_context_menu_for_top(ui);
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
                } else if (pressed & btn::MENU) != 0 {
                    self.open_context_menu_for_top(ui);
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
                    .filter(|(_, t)| !t.is_sfx && t.genre == genre)
                    .map(|(i, _)| i)
                    .collect();
                if self.nav_list(pressed, &mut sel, indices.len().max(1), ui) {
                    self.set_top_sel(sel);
                } else if (pressed & btn::MENU) != 0 {
                    self.open_context_menu_for_top(ui);
                } else if (pressed & btn::A) != 0 {
                    if let Some(&idx) = indices.get(sel) {
                        self.emit_click();
                        self.emit_play_track(idx);
                        self.push_screen(Screen::NowPlaying, ui);
                    }
                }
            }

            Screen::AllSongs { mut sel } => {
                let indices: Vec<usize> = self
                    .tracks
                    .iter()
                    .enumerate()
                    .filter(|(_, t)| !t.is_sfx)
                    .map(|(i, _)| i)
                    .collect();
                let count = indices.len().max(1);
                if self.nav_list(pressed, &mut sel, count, ui) {
                    self.set_top_sel(sel);
                } else if (pressed & btn::MENU) != 0 {
                    self.open_context_menu_for_top(ui);
                } else if (pressed & btn::A) != 0 {
                    if let Some(&idx) = indices.get(sel) {
                        self.emit_click();
                        self.emit_play_track(idx);
                        self.push_screen(Screen::NowPlaying, ui);
                    }
                }
            }

            Screen::Playlists { mut sel } => {
                if self.nav_list(pressed, &mut sel, 3, ui) {
                    self.set_top_sel(sel);
                } else if (pressed & btn::MENU) != 0 {
                    self.open_context_menu_for_top(ui);
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
                } else if (pressed & btn::R1) != 0 {
                    self.emit_click();
                    self.emit_manual_next_skip();
                } else if (pressed & btn::L1) != 0 {
                    self.emit_click();
                    let prev = self.step_playable_track(self.current_track, -1);
                    self.emit_play_track(prev);
                } else if (pressed & btn::R2) != 0 {
                    self.emit_seek_delta(15_000);
                } else if (pressed & btn::L2) != 0 {
                    self.emit_seek_delta(-15_000);
                } else if (pressed & btn::UP) != 0 {
                    self.adjust_volume(2, ui);
                } else if (pressed & btn::DOWN) != 0 {
                    self.adjust_volume(-2, ui);
                }
            }

            Screen::NowPlayingContext { mut sel } => {
                // Exact rows from `apps/gui/wps.c`:
                // Pause/Resume, [Channels, Endless Loop when NSF], Effects, [SoundFont when MIDI],
                // Shuffle, Repeat, Sleep Timer, Large Album Art, Add to Playlist, Start Visualizer
                let actions = self.now_playing_context_actions();
                let row_count = actions.len().max(1);
                if self.nav_list(pressed, &mut sel, row_count, ui) {
                    self.set_top_sel(sel);
                } else if (pressed & (btn::A | btn::LEFT | btn::RIGHT)) != 0 {
                    let left = (pressed & btn::LEFT) != 0;
                    let is_dir = (pressed & (btn::LEFT | btn::RIGHT)) != 0;
                    self.emit_click();
                    if let Some(&act) = actions.get(sel) {
                        match act {
                            NpCtxAction::PlayPause => self.emit_toggle_play_pause(),
                            NpCtxAction::NsfChannels => {
                                if !is_dir {
                                    self.push_screen(Screen::NsfChannels { sel: 0 }, ui);
                                }
                            }
                            NpCtxAction::NsfEndlessLoop => {
                                set_nsf_endless_loop(!get_nsf_endless_loop());
                                self.state_dirty = true;
                            }
                            NpCtxAction::Effects => {
                                self.dsp.fx_enabled = !self.dsp.fx_enabled;
                                self.emit_dsp();
                            }
                            NpCtxAction::SoundFont => {}
                            NpCtxAction::Shuffle => {
                                let next = self.step_playable_track(self.current_track, 2);
                                self.emit_play_track(next);
                                self.pop_screen(ui);
                            }
                            NpCtxAction::Repeat => {
                                let n = REPEAT_MODES_EN.len();
                                self.repeat_idx = if left {
                                    (self.repeat_idx + n - 1) % n
                                } else {
                                    (self.repeat_idx + 1) % n
                                };
                            }
                            NpCtxAction::SleepTimer => {
                                let n = SLEEP_TIMERS.len();
                                self.sleep_idx = if left {
                                    (self.sleep_idx + n - 1) % n
                                } else {
                                    (self.sleep_idx + 1) % n
                                };
                            }
                            NpCtxAction::LargeArt => {
                                self.alt_now_playing = !self.alt_now_playing;
                                self.state_dirty = true;
                            }
                            NpCtxAction::AddToPlaylist => {
                                if let Some(t) = self.tracks.get(self.current_track) {
                                    let entry = t
                                        .uri
                                        .clone()
                                        .unwrap_or_else(|| format!("{}.{}", t.title, t.codec.to_lowercase()));
                                    if !self.user_playlist_uris.contains(&entry) {
                                        self.user_playlist_uris.push(entry);
                                    }
                                }
                                self.state_dirty = true;
                                self.pop_screen(ui);
                            }
                            NpCtxAction::StartViz => {
                                viz.set_preset(self.viz_preset);
                                self.viz_fade = 0.0;
                                self.pop_screen(ui);
                                self.push_screen(Screen::VisualizerActive, ui);
                            }
                        }
                    }
                }
            }

            Screen::NsfChannels { mut sel } => {
                let Some(cflags) = self.active_nsf_chip_flags_for_ui() else {
                    self.pop_screen(ui);
                    return;
                };
                let ch_count = cflags.channel_descriptors().len();
                let total_rows = ch_count + 1;
                if (pressed & btn::B) != 0 {
                    self.emit_click();
                    self.pop_screen(ui);
                } else if (pressed & btn::X) != 0 {
                    set_nsf_channel_mute_mask(0);
                    self.emit_click();
                    self.state_dirty = true;
                } else if (pressed & btn::MENU) != 0 {
                    if sel < ch_count {
                        Self::solo_or_restore_nsf_channel(cflags, sel);
                        self.emit_click();
                        self.state_dirty = true;
                    }
                } else if (pressed & btn::UP) != 0 {
                    sel = (sel + total_rows - 1) % total_rows;
                    self.emit_click();
                    self.set_top_sel(sel);
                } else if (pressed & btn::DOWN) != 0 {
                    sel = (sel + 1) % total_rows;
                    self.emit_click();
                    self.set_top_sel(sel);
                } else if (pressed & (btn::A | btn::LEFT | btn::RIGHT)) != 0 {
                    let is_dir = (pressed & (btn::LEFT | btn::RIGHT)) != 0;
                    if !is_dir || sel < ch_count {
                        Self::toggle_nsf_channel_row(cflags, sel);
                        self.emit_click();
                        self.state_dirty = true;
                    }
                }
            }

            Screen::SettingsMenu { mut sel } => {
                // Exact 10 rows from `apps/menus/main_menu.c`:
                // Display, Sound, Offline Mode, Sources, Accounts, Library, Visualizers, Power, About, Exit
                if self.nav_list(pressed, &mut sel, 10, ui) {
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
                        4 if !is_dir => self.push_screen(Screen::AccountsMenu { sel: 0 }, ui),
                        5 if !is_dir => self.push_screen(Screen::LibraryMenu { sel: 0 }, ui),
                        6 if !is_dir => self.push_screen(Screen::VisualizersMenu { sel: 0 }, ui),
                        7 if !is_dir => self.push_screen(Screen::PowerMenu { sel: 0 }, ui),
                        8 if !is_dir => {
                            self.active_plugin = Some(ActivePlugin::launch(
                                PluginId::TextViewer,
                                &[],
                                self.pos_ms,
                            ));
                            self.push_screen(Screen::PluginActive, ui);
                        }
                        9 if !is_dir => {
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
                        0 => (KbdTarget::HexBackground, self.theme.bg_rgb),
                        1 => (KbdTarget::HexForeground, self.theme.fg_rgb),
                        2 => (KbdTarget::HexShade1, self.theme.fg1_rgb),
                        _ => (KbdTarget::HexShade2, self.theme.fg2_rgb),
                    };
                    self.push_screen(
                        Screen::Keyboard {
                            target,
                            text: format!("{rgb:06X}"),
                            row: 0,
                            col: 0,
                            caps: true,
                            sym_mode: false,
                            allow_symbols: false,
                        },
                        ui,
                    );
                }
            }

            Screen::MainMenuSettings { mut sel } => {
                if self.nav_list(pressed, &mut sel, 12, ui) {
                    self.set_top_sel(sel);
                } else if (pressed & (btn::A | btn::LEFT | btn::RIGHT)) != 0 {
                    self.emit_click();
                    self.main_menu_enabled[sel] = !self.main_menu_enabled[sel];
                }
            }

            Screen::Keyboard {
                target,
                mut text,
                mut row,
                mut col,
                mut caps,
                mut sym_mode,
                allow_symbols,
            } => {
                if (pressed & btn::B) != 0 {
                    self.emit_click();
                    self.pop_screen(ui);
                    return;
                }
                if (pressed & btn::Y) != 0 {
                    text.pop();
                    self.emit_click();
                } else if (pressed & btn::UP) != 0 {
                    let next_row = (row + 4) % 5;
                    col = Self::kbd_remap_col(allow_symbols, sym_mode, row, col, next_row);
                    row = next_row;
                    self.emit_click();
                } else if (pressed & btn::DOWN) != 0 {
                    let next_row = (row + 1) % 5;
                    col = Self::kbd_remap_col(allow_symbols, sym_mode, row, col, next_row);
                    row = next_row;
                    self.emit_click();
                } else if (pressed & btn::LEFT) != 0 {
                    let n = Self::kbd_row_len(allow_symbols, sym_mode, row);
                    col = (col + n - 1) % n;
                    self.emit_click();
                } else if (pressed & btn::RIGHT) != 0 {
                    let n = Self::kbd_row_len(allow_symbols, sym_mode, row);
                    col = (col + 1) % n;
                    self.emit_click();
                } else if (pressed & btn::A) != 0 {
                    self.emit_click();
                    let k = Self::kbd_key_label(allow_symbols, caps, sym_mode, row, col);
                    match k.as_str() {
                        "caps" | "CAPS" => caps = !caps,
                        "sym" | "abc" => sym_mode = !sym_mode,
                        "space" => {
                            let max_len = if allow_symbols { 128 } else { 6 };
                            if allow_symbols && text.len() < max_len {
                                text.push(' ');
                            }
                        }
                        "del" => {
                            text.pop();
                        }
                        "enter" => {
                            match target {
                                KbdTarget::HexBackground
                                | KbdTarget::HexForeground
                                | KbdTarget::HexShade1
                                | KbdTarget::HexShade2 => {
                                    if let Some(rgb) = parse_hex_rgb(&text) {
                                        match target {
                                            KbdTarget::HexBackground => {
                                                self.theme = derive_4shade(rgb, self.theme.fg_rgb);
                                            }
                                            KbdTarget::HexForeground => {
                                                self.theme = derive_4shade(self.theme.bg_rgb, rgb);
                                            }
                                            KbdTarget::HexShade1 => self.theme.fg1_rgb = rgb,
                                            KbdTarget::HexShade2 => self.theme.fg2_rgb = rgb,
                                            _ => {}
                                        }
                                        self.theme_idx = None;
                                    }
                                }
                                KbdTarget::ServerUrl(is_jf) => {
                                    if is_jf {
                                        self.jellyfin_url = text;
                                    } else {
                                        self.navidrome_url = text;
                                    }
                                }
                                KbdTarget::ServerUser(is_jf) => {
                                    if is_jf {
                                        self.jellyfin_user = text;
                                    } else {
                                        self.navidrome_user = text;
                                    }
                                }
                                KbdTarget::LastFmApiKey => self.lfm_api_key = text,
                                KbdTarget::LastFmApiSecret => self.lfm_api_secret = text,
                                KbdTarget::ListenBrainzToken => self.lb_user_token = text,
                                KbdTarget::ListenBrainzUrl => {
                                    self.lb_api_url = if text.trim().is_empty() {
                                        "https://api.listenbrainz.org".to_string()
                                    } else {
                                        text
                                    };
                                }
                            }
                            self.state_dirty = true;
                            self.pop_screen(ui);
                            return;
                        }
                        ch => {
                            let max_len = if allow_symbols { 128 } else { 6 };
                            if text.len() < max_len {
                                text.push_str(ch);
                            }
                        }
                    }
                }
                if let Some(last) = self.stack.last_mut() {
                    *last = Screen::Keyboard {
                        target,
                        text,
                        row,
                        col,
                        caps,
                        sym_mode,
                        allow_symbols,
                    };
                }
            }

            Screen::SoundMenu { mut sel } => {
                // Exact 8 rows from `apps/menus/sound_menu.c`:
                // Equalizer, Effects, Crossfade & Auto-DJ, Replaygain, Balance, Channels, Crossfeed, Menu Click
                if self.nav_list(pressed, &mut sel, 8, ui) {
                    self.set_top_sel(sel);
                } else if (pressed & (btn::A | btn::LEFT | btn::RIGHT)) != 0 {
                    let left = (pressed & btn::LEFT) != 0;
                    let is_dir = (pressed & (btn::LEFT | btn::RIGHT)) != 0;
                    self.emit_click();
                    match sel {
                        0 if !is_dir => self.push_screen(Screen::EqualizerMenu { sel: 0 }, ui),
                        1 if !is_dir => self.push_screen(Screen::EffectsMenu { sel: 0 }, ui),
                        2 if !is_dir => self.push_screen(Screen::DjCrossfadeMenu { sel: 0 }, ui),
                        3 if !is_dir => self.push_screen(Screen::ReplayGainMenu { sel: 0 }, ui),
                        4 => {
                            let d = if left { -5i8 } else { 5i8 };
                            self.dsp.balance_pct = (self.dsp.balance_pct + d).clamp(-100, 100);
                        }
                        5 => {
                            let n = CHANNEL_MODES.len();
                            self.dsp.channels_idx = if left {
                                (self.dsp.channels_idx + n - 1) % n
                            } else {
                                (self.dsp.channels_idx + 1) % n
                            };
                        }
                        6 => {
                            let n = CROSSFEED_MODES.len();
                            self.dsp.crossfeed_idx = if left {
                                (self.dsp.crossfeed_idx + n - 1) % n
                            } else {
                                (self.dsp.crossfeed_idx + 1) % n
                            };
                        }
                        7 => {
                            self.dsp.menu_click = !self.dsp.menu_click;
                            self.state_dirty = true;
                        }
                        _ => {}
                    }
                }
            }

            Screen::DjCrossfadeMenu { mut sel } => {
                // Exact 5 rows from `apps/menus/sound_menu.c` `dj_crossfade_menu`:
                // Crossfade Mode, Transition Length, Max Tempo Stretch, Keep Album Gapless, Quick Cut on Next
                if self.nav_list(pressed, &mut sel, 5, ui) {
                    self.set_top_sel(sel);
                } else if (pressed & (btn::A | btn::LEFT | btn::RIGHT)) != 0 {
                    let left = (pressed & btn::LEFT) != 0;
                    self.emit_click();
                    match sel {
                        0 => {
                            self.dsp.dj_mode = if left {
                                (self.dsp.dj_mode + 3) % 4
                            } else {
                                (self.dsp.dj_mode + 1) % 4
                            };
                            self.emit_dsp();
                        }
                        1 => {
                            self.dsp.dj_length = if left {
                                (self.dsp.dj_length + 2) % 3
                            } else {
                                (self.dsp.dj_length + 1) % 3
                            };
                            self.emit_dsp();
                        }
                        2 => {
                            self.dsp.dj_max_stretch = if left {
                                (self.dsp.dj_max_stretch + 2) % 3
                            } else {
                                (self.dsp.dj_max_stretch + 1) % 3
                            };
                            self.emit_dsp();
                        }
                        3 => {
                            self.dsp.dj_keep_gapless = !self.dsp.dj_keep_gapless;
                            self.emit_dsp();
                        }
                        4 => {
                            self.dsp.dj_quick_cut = !self.dsp.dj_quick_cut;
                            self.emit_dsp();
                        }
                        _ => {}
                    }
                }
            }

            Screen::EqualizerMenu { mut sel } => {
                // Exact 6 rows from `apps/menus/eq_menu.c`:
                // Enable EQ, Graphic Equalizer, AutoEQ, Browse EQ Presets, Save, Reset
                if self.nav_list(pressed, &mut sel, 6, ui) {
                    self.set_top_sel(sel);
                } else if (pressed & (btn::A | btn::LEFT | btn::RIGHT)) != 0 {
                    let is_dir = (pressed & (btn::LEFT | btn::RIGHT)) != 0;
                    self.emit_click();
                    match sel {
                        0 => {
                            self.dsp.eq_enabled = !self.dsp.eq_enabled;
                            self.emit_dsp();
                        }
                        1 if !is_dir => {
                            self.push_screen(Screen::GraphicEq { sel: 1 }, ui);
                        }
                        2 if !is_dir => {
                            self.push_screen(
                                Screen::AutoEqList {
                                    sel: self.dsp.autoeq_idx,
                                },
                                ui,
                            );
                        }
                        3 if !is_dir => {
                            self.push_screen(
                                Screen::AutoEqList {
                                    sel: self.dsp.autoeq_idx,
                                },
                                ui,
                            );
                        }
                        5 if !is_dir => {
                            self.apply_autoeq_preset(0);
                            self.emit_dsp();
                        }
                        _ => {}
                    }
                }
            }

            Screen::GraphicEq { mut sel } => {
                let a_held = (self.held_buttons & btn::A) != 0;
                if (pressed & (btn::B | btn::MENU)) != 0 {
                    self.dsp.eq_bypass = false;
                    self.emit_dsp();
                    self.emit_click();
                    self.pop_screen(ui);
                } else if (pressed & btn::LEFT) != 0 {
                    sel = (sel + 10) % 11;
                    self.emit_click();
                    self.set_top_sel(sel);
                } else if (pressed & btn::RIGHT) != 0 {
                    sel = (sel + 1) % 11;
                    self.emit_click();
                    self.set_top_sel(sel);
                } else if a_held && (pressed & (btn::UP | btn::DOWN)) != 0 {
                    let dir = if (pressed & btn::UP) != 0 { 5i16 } else { -5i16 };
                    if sel == 0 {
                        let cur = self.dsp.eq_preamp_tenths_db;
                        let snapped = ((cur as f32 / 5.0).round() as i16) * 5;
                        self.dsp.eq_preamp_tenths_db = (snapped + dir).clamp(-120, 120);
                    } else {
                        let b = sel - 1;
                        let cur = self.dsp.eq_bands_tenths_db[b];
                        let snapped = ((cur as f32 / 5.0).round() as i16) * 5;
                        self.dsp.eq_bands_tenths_db[b] = (snapped + dir).clamp(-120, 120);
                    }
                    self.dsp.eq_enabled = true;
                    self.emit_click();
                    self.emit_dsp();
                }
            }

            Screen::AutoEqList { mut sel } => {
                let count = AUTOEQ_PRESETS.len();
                if self.nav_list(pressed, &mut sel, count, ui) {
                    self.set_top_sel(sel);
                    self.apply_autoeq_preset(sel);
                    self.emit_dsp();
                } else if (pressed & btn::A) != 0 {
                    self.emit_click();
                    self.apply_autoeq_preset(sel);
                    self.emit_dsp();
                    self.pop_screen(ui);
                }
            }

            Screen::EffectsMenu { mut sel } => {
                // Exact 7 rows from `apps/menus/sound_menu.c`:
                // Enable Effects, Speed, Tape Mode, Reverb, Delay, Chorus, Flanger
                if self.nav_list(pressed, &mut sel, 7, ui) {
                    self.set_top_sel(sel);
                } else if (pressed & (btn::A | btn::LEFT | btn::RIGHT)) != 0 {
                    let left = (pressed & btn::LEFT) != 0;
                    let is_dir = (pressed & (btn::LEFT | btn::RIGHT)) != 0;
                    self.emit_click();
                    match sel {
                        0 => {
                            self.dsp.fx_enabled = !self.dsp.fx_enabled;
                            self.emit_dsp();
                        }
                        1 => {
                            let d = if left { -10i32 } else { 10i32 };
                            self.dsp.speed_pct =
                                ((self.dsp.speed_pct as i32 + d).clamp(50, 150)) as u16;
                            if self.dsp.speed_pct != 100 {
                                self.dsp.fx_enabled = true;
                            }
                            self.emit_dsp();
                        }
                        2 if !is_dir => self.push_screen(Screen::TapeModeMenu { sel: 0 }, ui),
                        3 if !is_dir => self.push_screen(Screen::ReverbMenu { sel: 0 }, ui),
                        4 if !is_dir => self.push_screen(Screen::DelayMenu { sel: 0 }, ui),
                        5 if !is_dir => self.push_screen(Screen::ChorusMenu { sel: 0 }, ui),
                        6 if !is_dir => self.push_screen(Screen::FlangerMenu { sel: 0 }, ui),
                        _ => {}
                    }
                }
            }

            Screen::TapeModeMenu { mut sel } => {
                // Exact 8 rows from `apps/menus/sound_menu.c`:
                // Enable, Wow & Flutter, Tape Hiss, Tape Crinkles, Saturation, HF Roll-Off, Stop/Start FX, Seek Motor Sound
                if self.nav_list(pressed, &mut sel, 8, ui) {
                    self.set_top_sel(sel);
                } else if (pressed & (btn::A | btn::LEFT | btn::RIGHT)) != 0 {
                    let delta = if (pressed & btn::LEFT) != 0 { -10i32 } else { 10i32 };
                    self.emit_click();
                    match sel {
                        0 => {
                            self.dsp.tape_enabled = !self.dsp.tape_enabled;
                            if self.dsp.tape_enabled {
                                self.dsp.fx_enabled = true;
                            }
                        }
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

            Screen::ReverbMenu { mut sel } => {
                if self.nav_list(pressed, &mut sel, 6, ui) {
                    self.set_top_sel(sel);
                } else if (pressed & (btn::A | btn::LEFT | btn::RIGHT)) != 0 {
                    let left = (pressed & btn::LEFT) != 0;
                    let d5 = if left { -5i32 } else { 5i32 };
                    let d10 = if left { -10i32 } else { 10i32 };
                    self.emit_click();
                    match sel {
                        0 => {
                            self.dsp.reverb_enabled = !self.dsp.reverb_enabled;
                            if self.dsp.reverb_enabled {
                                self.dsp.fx_enabled = true;
                            }
                        }
                        1 => {
                            self.dsp.reverb_size =
                                (self.dsp.reverb_size as i32 + d5).clamp(0, 100) as u8;
                        }
                        2 => {
                            self.dsp.reverb_damp =
                                (self.dsp.reverb_damp as i32 + d5).clamp(0, 100) as u8;
                        }
                        3 => {
                            self.dsp.reverb_predelay_ms =
                                (self.dsp.reverb_predelay_ms as i32 + d5).clamp(0, 120) as u8;
                        }
                        4 => {
                            self.dsp.reverb_width =
                                (self.dsp.reverb_width as i32 + d10).clamp(0, 100) as u8;
                        }
                        _ => {
                            self.dsp.reverb_mix =
                                (self.dsp.reverb_mix as i32 + d5).clamp(0, 100) as u8;
                        }
                    }
                    self.emit_dsp();
                }
            }

            Screen::DelayMenu { mut sel } => {
                if self.nav_list(pressed, &mut sel, 6, ui) {
                    self.set_top_sel(sel);
                } else if (pressed & (btn::A | btn::LEFT | btn::RIGHT)) != 0 {
                    let left = (pressed & btn::LEFT) != 0;
                    let d5 = if left { -5i32 } else { 5i32 };
                    let d20 = if left { -20i32 } else { 20i32 };
                    self.emit_click();
                    match sel {
                        0 => {
                            self.dsp.delay_enabled = !self.dsp.delay_enabled;
                            if self.dsp.delay_enabled {
                                self.dsp.fx_enabled = true;
                            }
                        }
                        1 => {
                            self.dsp.delay_time_ms =
                                (self.dsp.delay_time_ms as i32 + d20).clamp(40, 1000) as u16;
                        }
                        2 => {
                            self.dsp.delay_feedback =
                                (self.dsp.delay_feedback as i32 + d5).clamp(0, 90) as u8;
                        }
                        3 => {
                            self.dsp.delay_tone =
                                (self.dsp.delay_tone as i32 + d5).clamp(0, 100) as u8;
                        }
                        4 => self.dsp.delay_pingpong = !self.dsp.delay_pingpong,
                        _ => {
                            self.dsp.delay_mix =
                                (self.dsp.delay_mix as i32 + d5).clamp(0, 100) as u8;
                        }
                    }
                    self.emit_dsp();
                }
            }

            Screen::ChorusMenu { mut sel } => {
                if self.nav_list(pressed, &mut sel, 4, ui) {
                    self.set_top_sel(sel);
                } else if (pressed & (btn::A | btn::LEFT | btn::RIGHT)) != 0 {
                    let d5 = if (pressed & btn::LEFT) != 0 { -5i32 } else { 5i32 };
                    self.emit_click();
                    match sel {
                        0 => {
                            self.dsp.chorus_enabled = !self.dsp.chorus_enabled;
                            if self.dsp.chorus_enabled {
                                self.dsp.fx_enabled = true;
                            }
                        }
                        1 => {
                            self.dsp.chorus_rate =
                                (self.dsp.chorus_rate as i32 + d5).clamp(0, 100) as u8;
                        }
                        2 => {
                            self.dsp.chorus_depth =
                                (self.dsp.chorus_depth as i32 + d5).clamp(0, 100) as u8;
                        }
                        _ => {
                            self.dsp.chorus_mix =
                                (self.dsp.chorus_mix as i32 + d5).clamp(0, 100) as u8;
                        }
                    }
                    self.emit_dsp();
                }
            }

            Screen::FlangerMenu { mut sel } => {
                if self.nav_list(pressed, &mut sel, 5, ui) {
                    self.set_top_sel(sel);
                } else if (pressed & (btn::A | btn::LEFT | btn::RIGHT)) != 0 {
                    let d5 = if (pressed & btn::LEFT) != 0 { -5i32 } else { 5i32 };
                    self.emit_click();
                    match sel {
                        0 => {
                            self.dsp.flanger_enabled = !self.dsp.flanger_enabled;
                            if self.dsp.flanger_enabled {
                                self.dsp.fx_enabled = true;
                            }
                        }
                        1 => {
                            self.dsp.flanger_rate =
                                (self.dsp.flanger_rate as i32 + d5).clamp(0, 100) as u8;
                        }
                        2 => {
                            self.dsp.flanger_depth =
                                (self.dsp.flanger_depth as i32 + d5).clamp(0, 100) as u8;
                        }
                        3 => {
                            self.dsp.flanger_feedback =
                                (self.dsp.flanger_feedback as i32 + d5).clamp(-90, 90) as i8;
                        }
                        _ => {
                            self.dsp.flanger_mix =
                                (self.dsp.flanger_mix as i32 + d5).clamp(0, 100) as u8;
                        }
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

            Screen::SourceServerInfo {
                is_jellyfin,
                mut sel,
            } => {
                if self.nav_list(pressed, &mut sel, 4, ui) {
                    self.set_top_sel(sel);
                } else if (pressed & btn::A) != 0 {
                    self.emit_click();
                    match sel {
                        0 => {
                            let cur = if is_jellyfin {
                                self.jellyfin_url.clone()
                            } else {
                                self.navidrome_url.clone()
                            };
                            self.push_screen(
                                Screen::Keyboard {
                                    target: KbdTarget::ServerUrl(is_jellyfin),
                                    text: if cur.is_empty() {
                                        "https://".to_string()
                                    } else {
                                        cur
                                    },
                                    row: 1,
                                    col: 0,
                                    caps: false,
                                    sym_mode: false,
                                    allow_symbols: true,
                                },
                                ui,
                            );
                        }
                        1 => {
                            let cur = if is_jellyfin {
                                self.jellyfin_user.clone()
                            } else {
                                self.navidrome_user.clone()
                            };
                            self.push_screen(
                                Screen::Keyboard {
                                    target: KbdTarget::ServerUser(is_jellyfin),
                                    text: cur,
                                    row: 1,
                                    col: 0,
                                    caps: false,
                                    sym_mode: false,
                                    allow_symbols: true,
                                },
                                ui,
                            );
                        }
                        _ => {}
                    }
                }
            }

            Screen::AccountsMenu { mut sel } => {
                if self.nav_list(pressed, &mut sel, 2, ui) {
                    self.set_top_sel(sel);
                } else if (pressed & btn::A) != 0 {
                    self.emit_click();
                    if sel == 0 {
                        self.push_screen(Screen::LastFmMenu { sel: 0 }, ui);
                    } else {
                        self.push_screen(Screen::ListenBrainzMenu { sel: 0 }, ui);
                    }
                }
            }

            Screen::LastFmMenu { mut sel } => {
                if self.nav_list(pressed, &mut sel, 9, ui) {
                    self.set_top_sel(sel);
                } else if (pressed & (btn::A | btn::LEFT | btn::RIGHT)) != 0 {
                    let is_dir = (pressed & (btn::LEFT | btn::RIGHT)) != 0;
                    self.emit_click();
                    match sel {
                        0 => {
                            self.lfm_enabled = !self.lfm_enabled;
                            self.state_dirty = true;
                        }
                        2 if !is_dir => {
                            self.push_screen(Screen::LastFmQrAuth, ui);
                        }
                        4 => {
                            self.lfm_streaming = !self.lfm_streaming;
                            self.state_dirty = true;
                        }
                        7 if !is_dir => {
                            let cur = self.lfm_api_key.clone();
                            self.push_screen(
                                Screen::Keyboard {
                                    target: KbdTarget::LastFmApiKey,
                                    text: cur,
                                    row: 0,
                                    col: 0,
                                    caps: false,
                                    sym_mode: false,
                                    allow_symbols: true,
                                },
                                ui,
                            );
                        }
                        8 if !is_dir => {
                            let cur = self.lfm_api_secret.clone();
                            self.push_screen(
                                Screen::Keyboard {
                                    target: KbdTarget::LastFmApiSecret,
                                    text: cur,
                                    row: 0,
                                    col: 0,
                                    caps: false,
                                    sym_mode: false,
                                    allow_symbols: true,
                                },
                                ui,
                            );
                        }
                        _ => {}
                    }
                }
            }

            Screen::LastFmQrAuth => {
                if (pressed & (btn::B | btn::A)) != 0 {
                    self.emit_click();
                    self.pop_screen(ui);
                }
            }

            Screen::ListenBrainzMenu { mut sel } => {
                if self.nav_list(pressed, &mut sel, 9, ui) {
                    self.set_top_sel(sel);
                } else if (pressed & (btn::A | btn::LEFT | btn::RIGHT)) != 0 {
                    let is_dir = (pressed & (btn::LEFT | btn::RIGHT)) != 0;
                    self.emit_click();
                    match sel {
                        0 => {
                            self.lb_enabled = !self.lb_enabled;
                            self.state_dirty = true;
                        }
                        1 | 2 | 7 if !is_dir => {
                            let cur = self.lb_user_token.clone();
                            self.push_screen(
                                Screen::Keyboard {
                                    target: KbdTarget::ListenBrainzToken,
                                    text: cur,
                                    row: 0,
                                    col: 0,
                                    caps: false,
                                    sym_mode: false,
                                    allow_symbols: true,
                                },
                                ui,
                            );
                        }
                        3 => {
                            self.lb_now_playing = !self.lb_now_playing;
                            self.state_dirty = true;
                        }
                        4 => {
                            self.lb_streaming = !self.lb_streaming;
                            self.state_dirty = true;
                        }
                        8 if !is_dir => {
                            let cur = self.lb_api_url.clone();
                            self.push_screen(
                                Screen::Keyboard {
                                    target: KbdTarget::ListenBrainzUrl,
                                    text: cur,
                                    row: 0,
                                    col: 0,
                                    caps: false,
                                    sym_mode: false,
                                    allow_symbols: true,
                                },
                                ui,
                            );
                        }
                        _ => {}
                    }
                }
            }

            Screen::LibraryMenu { mut sel } => {
                // Exact 7 rows from `apps/menus/audio_folders_menu.c`:
                // Music, Podcast, Audiobook, Movie, Television, SoundFont Folders, Rescan Library
                if self.nav_list(pressed, &mut sel, 7, ui) {
                    self.set_top_sel(sel);
                } else if (pressed & btn::A) != 0 {
                    self.emit_click();
                    if sel < 6 {
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
                | Screen::ExtrasMenu { sel }
                | Screen::PluginsHub { sel }
                | Screen::PluginCategory { sel, .. }
                | Screen::Downloads { sel }
                | Screen::VideosList { sel }
                | Screen::VideoContextMenu { sel }
                | Screen::TrackContextMenu { sel, .. }
                | Screen::Artists { sel }
                | Screen::ArtistSongs { sel, .. }
                | Screen::Albums { sel }
                | Screen::AlbumSongs { sel, .. }
                | Screen::Genres { sel }
                | Screen::GenreSongs { sel, .. }
                | Screen::AllSongs { sel }
                | Screen::Playlists { sel }
                | Screen::NowPlayingContext { sel }
                | Screen::NsfChannels { sel }
                | Screen::NsfContainer { sel, .. }
                | Screen::NsfSfx { sel, .. }
                | Screen::SettingsMenu { sel }
                | Screen::DisplayMenu { sel }
                | Screen::ThemeList { sel }
                | Screen::CustomizeThemeMenu { sel }
                | Screen::MainMenuSettings { sel }
                | Screen::SoundMenu { sel }
                | Screen::DjCrossfadeMenu { sel }
                | Screen::EqualizerMenu { sel }
                | Screen::GraphicEq { sel }
                | Screen::AutoEqList { sel }
                | Screen::EffectsMenu { sel }
                | Screen::TapeModeMenu { sel }
                | Screen::ReverbMenu { sel }
                | Screen::DelayMenu { sel }
                | Screen::ChorusMenu { sel }
                | Screen::FlangerMenu { sel }
                | Screen::ReplayGainMenu { sel }
                | Screen::SourcesMenu { sel }
                | Screen::SourceServerInfo { sel, .. }
                | Screen::AccountsMenu { sel }
                | Screen::LastFmMenu { sel }
                | Screen::ListenBrainzMenu { sel }
                | Screen::LibraryMenu { sel }
                | Screen::LibraryFolderList { sel, .. }
                | Screen::VisualizersMenu { sel }
                | Screen::VisualizerPresets { sel }
                | Screen::PowerMenu { sel } => *sel = new_sel,
                _ => {}
            }
        }
    }

    /* ---- Exact 45-Key UIPod Keyboard (`apps/uipod_keyboard.c` + Figcraft `#4212`) ---- */

    fn kbd_alpha_row(allow_symbols: bool, sym_mode: bool, row: usize) -> &'static str {
        const PLAIN: [&str; 4] = ["1234567890", "qwertyuiop", "asdfghjkl", "zxcvbnm-"];
        const URL: [&str; 4] = ["1234567890", "qwertyuiop", "asdfghjkl-", "zxcvbnm.:/"];
        const SYM: [&str; 4] = ["1234567890", ":/.-_@?=&+", "#!$%*~,;|\\", "()[]{}<>'\""];
        if allow_symbols && sym_mode {
            SYM[row]
        } else if allow_symbols {
            URL[row]
        } else {
            PLAIN[row]
        }
    }

    fn kbd_row_len(allow_symbols: bool, sym_mode: bool, row: usize) -> usize {
        if row == 4 {
            if allow_symbols { 5 } else { 4 }
        } else {
            Self::kbd_alpha_row(allow_symbols, sym_mode, row).len()
        }
    }

    fn kbd_key_label(
        allow_symbols: bool,
        caps: bool,
        sym_mode: bool,
        row: usize,
        col: usize,
    ) -> String {
        if row == 4 {
            let lbl = if allow_symbols {
                match col {
                    0 => if caps { "CAPS" } else { "caps" },
                    1 => if sym_mode { "abc" } else { "sym" },
                    2 => "space",
                    3 => "del",
                    _ => "enter",
                }
            } else {
                match col {
                    0 => if caps { "CAPS" } else { "caps" },
                    1 => "space",
                    2 => "del",
                    _ => "enter",
                }
            };
            return lbl.to_string();
        }
        let bytes = Self::kbd_alpha_row(allow_symbols, sym_mode, row).as_bytes();
        let ch = bytes.get(col).copied().unwrap_or(b'a') as char;
        if !sym_mode && caps {
            ch.to_ascii_uppercase().to_string()
        } else {
            ch.to_string()
        }
    }

    fn kbd_key_center_x(allow_symbols: bool, sym_mode: bool, row: usize, col: usize) -> i32 {
        let w10 = 1024 * 10;
        let inner10 = 960 * 10;
        let gap10 = 12 * 10;
        let n = Self::kbd_row_len(allow_symbols, sym_mode, row) as i32;
        if n <= 0 {
            return w10 / 2;
        }
        let kw10 = if row == 4 {
            (inner10 - (n - 1) * gap10) / n
        } else {
            (inner10 - 9 * gap10) / 10
        };
        let row_w10 = n * kw10 + (n - 1) * gap10;
        let start_x10 = (w10 - row_w10) / 2;
        start_x10 + (col as i32) * (kw10 + gap10) + kw10 / 2
    }

    fn kbd_remap_col(
        allow_symbols: bool,
        sym_mode: bool,
        from_row: usize,
        col: usize,
        to_row: usize,
    ) -> usize {
        let to_n = Self::kbd_row_len(allow_symbols, sym_mode, to_row);
        if to_n <= 1 {
            return 0;
        }
        let target_cx = Self::kbd_key_center_x(allow_symbols, sym_mode, from_row, col);
        let mut best_c = 0usize;
        let mut best_d = i32::MAX;
        for c in 0..to_n {
            let d = (Self::kbd_key_center_x(allow_symbols, sym_mode, to_row, c) - target_cx).abs();
            if d < best_d {
                best_d = d;
                best_c = c;
            }
        }
        best_c
    }

    pub(crate) fn now_playing_context_actions(&self) -> Vec<NpCtxAction> {
        let is_nsf = self.active_nsf_chip_flags_for_ui().is_some();
        let is_midi = self
            .tracks
            .get(self.current_track)
            .is_some_and(|t| t.codec.eq_ignore_ascii_case("MIDI"));
        let mut out = Vec::with_capacity(11);
        out.push(NpCtxAction::PlayPause);
        if is_nsf {
            out.push(NpCtxAction::NsfChannels);
            out.push(NpCtxAction::NsfEndlessLoop);
        }
        out.push(NpCtxAction::Effects);
        if is_midi {
            out.push(NpCtxAction::SoundFont);
        }
        out.extend([
            NpCtxAction::Shuffle,
            NpCtxAction::Repeat,
            NpCtxAction::SleepTimer,
            NpCtxAction::LargeArt,
            NpCtxAction::AddToPlaylist,
            NpCtxAction::StartViz,
        ]);
        out
    }

    pub(crate) fn now_playing_context_items(
        &self,
    ) -> Vec<(String, FileIconKind, OwnedRight)> {
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
        let mut items = Vec::with_capacity(11);
        for act in self.now_playing_context_actions() {
            match act {
                NpCtxAction::PlayPause => {
                    items.push((pause_lbl.to_string(), FileIconKind::None, OwnedRight::None));
                }
                NpCtxAction::NsfChannels => {
                    let cflags = self.active_nsf_chip_flags_for_ui().unwrap_or(ChipFlags(0));
                    let descs = cflags.channel_descriptors();
                    let mask = get_nsf_channel_mute_mask();
                    let enabled = descs
                        .iter()
                        .filter(|d| (mask & (1u16 << d.bit_index)) == 0)
                        .count();
                    let summary = if enabled == descs.len() {
                        self.tr("All ON", "Усі УВІМК").to_string()
                    } else {
                        format!("{}/{} {}", enabled, descs.len(), self.tr("ON", "УВІМК"))
                    };
                    items.push((
                        self.tr("Channels", "Канали").to_string(),
                        FileIconKind::None,
                        OwnedRight::ValueChevron(summary),
                    ));
                }
                NpCtxAction::NsfEndlessLoop => {
                    items.push((
                        self.tr("Endless Loop", "Нескінченний цикл").to_string(),
                        FileIconKind::None,
                        OwnedRight::Toggle(get_nsf_endless_loop()),
                    ));
                }
                NpCtxAction::Effects => {
                    items.push((
                        self.tr("Effects", "Ефекти").to_string(),
                        FileIconKind::None,
                        OwnedRight::Toggle(self.dsp.fx_enabled),
                    ));
                }
                NpCtxAction::SoundFont => {
                    items.push((
                        self.tr("SoundFont", "Саундфонт").to_string(),
                        FileIconKind::None,
                        OwnedRight::Chevron,
                    ));
                }
                NpCtxAction::Shuffle => {
                    items.push((
                        self.tr("Shuffle", "Перемішати").to_string(),
                        FileIconKind::None,
                        OwnedRight::None,
                    ));
                }
                NpCtxAction::Repeat => {
                    items.push((
                        self.tr("Repeat", "Повтор").to_string(),
                        FileIconKind::None,
                        OwnedRight::CycleValue(rep.to_string()),
                    ));
                }
                NpCtxAction::SleepTimer => {
                    items.push((
                        self.tr("Sleep Timer", "Таймер сну").to_string(),
                        FileIconKind::None,
                        OwnedRight::CycleValue(SLEEP_TIMERS[self.sleep_idx].to_string()),
                    ));
                }
                NpCtxAction::LargeArt => {
                    items.push((
                        self.tr("Large Album Art", "Великий кавер").to_string(),
                        FileIconKind::None,
                        OwnedRight::Toggle(self.alt_now_playing),
                    ));
                }
                NpCtxAction::AddToPlaylist => {
                    items.push((
                        self.tr("Add to Playlist", "Додати у плейлист").to_string(),
                        FileIconKind::None,
                        OwnedRight::None,
                    ));
                }
                NpCtxAction::StartViz => {
                    items.push((
                        self.tr("Start Visualizer", "Запустити візуалізатор").to_string(),
                        FileIconKind::None,
                        OwnedRight::None,
                    ));
                }
            }
        }
        items
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
        self.clock_ms = self
            .clock_ms
            .saturating_add(((dt * 1000.0).round() as u64).max(1));
        ui.set_volume_popup_clock(self.clock_ms);
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
                if self.dsp.fx_enabled {
                    self.dsp.speed_pct as f32 / 100.0
                } else {
                    1.0
                }
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
            ui.render_volume_popup_over_screen(win_w, win_h, self.theme, self.clock_ms);
            return;
        }

        if matches!(top, Screen::PluginActive) {
            let hdr = self
                .active_plugin
                .as_ref()
                .map(|pl| self.header_state(pl.title(self.uk)));
            if let (Some(ref mut pl), Some(hdr)) = (&mut self.active_plugin, hdr) {
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
                    _ => self
                        .browse_music_items()
                        .into_iter()
                        .map(|entry| match entry {
                            BrowseMusicItem::NsfContainer {
                                file_name,
                                music_count,
                                ..
                            } => (
                                file_name,
                                FileIconKind::from_tag("NSF"),
                                OwnedRight::Value(format!("[{music_count} ♪]")),
                            ),
                            BrowseMusicItem::Track(idx) => {
                                let t = &self.tracks[idx];
                                (
                                    format!("{}.{}", t.title, t.codec.to_lowercase()),
                                    FileIconKind::from_tag(&t.codec),
                                    OwnedRight::Value(fmt_ms(t.duration_ms)),
                                )
                            }
                            BrowseMusicItem::AboutTxt => (
                                "about.txt".to_string(),
                                FileIconKind::Text("TXT".into()),
                                OwnedRight::Value("6 KB".into()),
                            ),
                        })
                        .collect(),
                };
                self.render_list(ui, &path, &items, sel);
            }

            Screen::NsfContainer { file_name, sel } => {
                let (music, sfx, game_title) = self.nsf_subtracks_for_file(&file_name);
                let mut items = Vec::with_capacity(music.len() + 2);
                if !music.is_empty() {
                    items.push((
                        self.tr("Shuffle Game Music", "Перемішати музику гри").to_string(),
                        FileIconKind::None,
                        OwnedRight::None,
                    ));
                    for (pos, &idx) in music.iter().enumerate() {
                        let t = &self.tracks[idx];
                        let label = if has_leading_track_num(&t.title) {
                            t.title.clone()
                        } else {
                            format!("{:02}. {}", pos + 1, t.title)
                        };
                        items.push((
                            label,
                            FileIconKind::from_tag("NSF"),
                            OwnedRight::Value(fmt_ms(t.duration_ms)),
                        ));
                    }
                }
                if !sfx.is_empty() {
                    let sfx_lbl = if self.uk {
                        format!("Звукові ефекти ({})", sfx.len())
                    } else {
                        format!("Sound Effects ({})", sfx.len())
                    };
                    items.push((sfx_lbl, FileIconKind::Folder, OwnedRight::Chevron));
                }
                self.render_list(ui, &game_title, &items, sel);
            }

            Screen::NsfSfx { file_name, sel } => {
                let (_, sfx, _) = self.nsf_subtracks_for_file(&file_name);
                let title = if self.uk {
                    format!("Звукові ефекти ({})", sfx.len())
                } else {
                    format!("Sound Effects ({})", sfx.len())
                };
                let items: Vec<_> = sfx
                    .iter()
                    .enumerate()
                    .map(|(pos, &idx)| {
                        let t = &self.tracks[idx];
                        let def_name =
                            format!("Track {:02}", t.subtrack_index.unwrap_or(pos as u16) + 1);
                        let lbl = if t.title.eq_ignore_ascii_case(&def_name) {
                            format!("SFX {:02}", pos + 1)
                        } else {
                            format!("SFX {:02} - {}", pos + 1, t.title)
                        };
                        (
                            lbl,
                            FileIconKind::from_tag("NSF"),
                            OwnedRight::Value(fmt_ms(t.duration_ms)),
                        )
                    })
                    .collect();
                self.render_list(ui, &title, &items, sel);
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

            Screen::VideosList { sel } => {
                let items = vec![(
                    "Cyberia_2000_Reel_4x3.mp4".to_string(),
                    FileIconKind::Video("MP4".into()),
                    OwnedRight::Value("1:34".into()),
                )];
                self.render_list(ui, self.tr("Videos", "Відео"), &items, sel);
            }

            Screen::VideoPlayer {
                playing,
                pos_ms,
                dur_ms,
            } => {
                let mut hdr = self.header_state("Cyberia_2000_Reel_4x3.mp4");
                hdr.play_mode = if playing {
                    PlayMode::Play
                } else {
                    PlayMode::Pause
                };
                ui.plugin_begin(self.theme, self.theme.bg_rgb, Some(&hdr));

                // 4-shade video frame viewport (`#10724` Video Player)
                let aspect_w = match self.video_aspect_idx {
                    1 => 960.0f32,
                    2 => 720.0f32,
                    _ => 840.0f32,
                };
                let vx = (1024.0 - aspect_w) * 0.5;
                let vy = 148.0f32;
                let vh = 460.0f32;
                ui.plugin_stroke_rect(vx, vy, aspect_w, vh, 12.0, 4.0, self.theme.fg1_rgb);
                let phase = (pos_ms as f32 / 1000.0) * 1.4;
                for i in 0..5 {
                    let r = 34.0 + ((phase + i as f32 * 0.6).fract()) * 160.0;
                    ui.plugin_stroke_rect(
                        512.0 - r * 1.3,
                        vy + vh * 0.5 - r * 0.72,
                        r * 2.6,
                        r * 1.44,
                        16.0,
                        3.0,
                        if i % 2 == 0 {
                            self.theme.fg2_rgb
                        } else {
                            self.theme.fg1_rgb
                        },
                    );
                }
                ui.plugin_draw_text(
                    512.0,
                    vy + vh * 0.5,
                    "CYBERIA // 4:3 VIDEO",
                    34.0,
                    3,
                    1,
                    self.theme.fg_rgb,
                );

                // Bottom OSD pill
                let prog = (pos_ms as f32 / dur_ms.max(1) as f32).clamp(0.0, 1.0);
                ui.plugin_draw_progress_bar(self.theme, 48.0, 646.0, 928.0, 22.0, prog);
                let elapsed = fmt_ms(pos_ms);
                let rem = format!("-{}", fmt_ms(dur_ms.saturating_sub(pos_ms)));
                ui.plugin_draw_text(48.0, 698.0, &elapsed, 28.0, 2, 0, self.theme.fg_rgb);
                ui.plugin_draw_text(
                    512.0,
                    698.0,
                    self.tr("Hold A / Menu: Video Settings", "Утрим. A / Menu: Параметри відео"),
                    24.0,
                    1,
                    1,
                    self.theme.fg2_rgb,
                );
                ui.plugin_draw_text(976.0, 698.0, &rem, 28.0, 2, 2, self.theme.fg_rgb);
            }

            Screen::VideoContextMenu { sel } => {
                // Exact 3 rows from `apps/uipod_video.c` (`#11301` — `Video Settings`)
                let aspect_lbl = match self.video_aspect_idx {
                    1 => self.tr("Fill (4:3)", "На весь екран (4:3)"),
                    2 => self.tr("Original", "Оригінал"),
                    _ => self.tr("Fit", "Вписати"),
                };
                let sync_lbl = if self.video_audio_sync_ms > 0 {
                    format!("+{} ms", self.video_audio_sync_ms)
                } else {
                    format!("{} ms", self.video_audio_sync_ms)
                };
                let items = vec![
                    (
                        self.tr("Aspect Ratio", "Пропорції").to_string(),
                        FileIconKind::None,
                        OwnedRight::CycleValue(aspect_lbl.to_string()),
                    ),
                    (
                        self.tr("Brightness", "Яскравість").to_string(),
                        FileIconKind::None,
                        OwnedRight::CycleValue(format!("{}%", self.brightness_pct)),
                    ),
                    (
                        self.tr("Audio Sync", "Синхронізація звуку").to_string(),
                        FileIconKind::None,
                        OwnedRight::CycleValue(sync_lbl),
                    ),
                ];
                self.render_list(
                    ui,
                    self.tr("Video Settings", "Параметри відео"),
                    &items,
                    sel,
                );
            }

            Screen::TrackContextMenu { track_idx, sel } => {
                let title = self
                    .tracks
                    .get(track_idx)
                    .map(|t| t.title.as_str())
                    .unwrap_or("Context Menu");
                let items = vec![
                    (
                        self.tr("Play Now", "Відтворити зараз").to_string(),
                        FileIconKind::None,
                        OwnedRight::None,
                    ),
                    (
                        self.tr("Play Next", "Відтворити наступним").to_string(),
                        FileIconKind::None,
                        OwnedRight::None,
                    ),
                    (
                        self.tr("Add to Playlist", "Додати у плейлист").to_string(),
                        FileIconKind::None,
                        OwnedRight::None,
                    ),
                    (
                        self.tr("Shuffle All", "Перемішати все").to_string(),
                        FileIconKind::None,
                        OwnedRight::None,
                    ),
                    (
                        self.tr("Now Playing", "Програється").to_string(),
                        FileIconKind::None,
                        OwnedRight::Chevron,
                    ),
                ];
                self.render_list(ui, title, &items, sel);
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
                    .filter(|t| !t.is_sfx && t.artist == artist)
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
                    .filter(|t| !t.is_sfx && t.album == album)
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
                    .filter(|t| !t.is_sfx && t.genre == genre)
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
                    .filter(|t| !t.is_sfx)
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
                let music_total = self.tracks.iter().filter(|t| !t.is_sfx).count();
                let fav_count = music_total + self.user_playlist_uris.len();
                let items = vec![
                    ("Y2K System Mix".to_string(), FileIconKind::Audio("M3U".into()), OwnedRight::Value(format!("{music_total}"))),
                    ("Liquid Millennium".to_string(), FileIconKind::Audio("M3U".into()), OwnedRight::Value("2".into())),
                    (self.tr("Favourites", "Обране").to_string(), FileIconKind::Audio("M3U".into()), OwnedRight::Value(format!("{fav_count}"))),
                ];
                self.render_list(ui, self.tr("Playlists", "Плейлисти"), &items, sel);
            }

            Screen::NowPlaying => {
                let trk = self.tracks.get(self.current_track).or_else(|| self.tracks.first());
                if let Some(t) = trk {
                    let music_tracks: Vec<usize> = self
                        .tracks
                        .iter()
                        .enumerate()
                        .filter(|(_, tr)| !tr.is_sfx)
                        .map(|(i, _)| i)
                        .collect();
                    let (disp_pos, disp_total) = if t.is_sfx || music_tracks.is_empty() {
                        (self.current_track + 1, self.tracks.len().max(1))
                    } else {
                        let p = music_tracks
                            .iter()
                            .position(|&i| i == self.current_track)
                            .unwrap_or(0)
                            + 1;
                        (p, music_tracks.len())
                    };
                    let track_pos = format!(
                        "{} {} {}",
                        disp_pos,
                        self.tr("of", "з"),
                        disp_total
                    );
                    let elapsed = fmt_ms(self.pos_ms);
                    let rem_ms = self.dur_ms.saturating_sub(self.pos_ms);
                    let remaining = format!("-{}", fmt_ms(rem_ms));
                    let progress = if self.dur_ms > 0 {
                        (self.pos_ms as f32 / self.dur_ms as f32).clamp(0.0, 1.0)
                    } else {
                        0.0
                    };

                    let is_nsf = t.codec.eq_ignore_ascii_case("NSF")
                        || get_nsf_active_chip_flags().is_some();
                    let cflags = self.active_nsf_chip_flags_for_ui().unwrap_or(ChipFlags(t.chip_flags));
                    let nsf_badge = if is_nsf {
                        cflags.primary_badge_label()
                    } else {
                        ""
                    };
                    let nsf_loop_progress = if is_nsf {
                        t.loop_point_ms
                            .filter(|&lp| lp > 0 && self.dur_ms > lp)
                            .map(|lp| (lp as f32 / self.dur_ms as f32).clamp(0.02, 0.98))
                    } else {
                        None
                    };
                    let descs: &[uipod_nsf::ChannelDescriptor] = if is_nsf {
                        cflags.channel_descriptors()
                    } else {
                        &[]
                    };
                    let mute_mask = get_nsf_channel_mute_mask();
                    let apu_tel = get_nsf_apu_telemetry();
                    let playing_now = self.play_mode == PlayMode::Play;
                    let pills: Vec<NsfApuPill<'_>> = descs
                        .iter()
                        .enumerate()
                        .map(|(i, d)| {
                            let muted = (mute_mask & (1u16 << d.bit_index)) != 0;
                            let volume = if !muted && playing_now && i < apu_tel.channel_count as usize {
                                apu_tel.channels[i].volume.clamp(0.0, 1.0)
                            } else {
                                0.0
                            };
                            let active = !muted
                                && playing_now
                                && i < apu_tel.channel_count as usize
                                && apu_tel.channels[i].active != 0
                                && volume > 0.02;
                            NsfApuPill {
                                short_code: d.short_code,
                                active,
                                muted,
                                volume,
                            }
                        })
                        .collect();

                    let dj_label = if self.dj_telemetry.active != 0 {
                        if self.dj_telemetry.mix_kind == 2
                            && self.dj_telemetry.bpm_a > 1.0
                            && self.dj_telemetry.bpm_b > 1.0
                        {
                            format!(
                                "DJ SYNC · {:.0}→{:.0} BPM",
                                self.dj_telemetry.bpm_a, self.dj_telemetry.bpm_b
                            )
                        } else if self.dj_telemetry.mix_kind == 3 {
                            "DJ WASH › NEXT".to_string()
                        } else {
                            "FADE › NEXT".to_string()
                        }
                    } else {
                        String::new()
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
                        center_status_label: "",
                        art_rgb24: t.art_rgb24.as_deref(),
                        mono_album_art: self.mono_album_art,
                        alt_layout: self.alt_now_playing,
                        is_nsf,
                        nsf_chip_badge: nsf_badge,
                        nsf_loop_progress,
                        nsf_endless_loop: is_nsf && get_nsf_endless_loop(),
                        nsf_channels: &pills,
                        dj_status_label: &dj_label,
                        dj_progress: self.dj_telemetry.progress,
                        dj_bass_swap_flash: self.dj_telemetry.bass_swap_flash != 0,
                    };
                    ui.render_now_playing_to_fbo(&state);
                }
            }

            Screen::NowPlayingContext { sel } => {
                let items = self.now_playing_context_items();
                self.render_list(ui, self.tr("Now Playing", "Програється"), &items, sel);
            }

            Screen::NsfChannels { sel } => {
                let cflags = self.active_nsf_chip_flags_for_ui().unwrap_or(ChipFlags(0));
                let descs = cflags.channel_descriptors();
                let mute_mask = get_nsf_channel_mute_mask();
                let mut valid_mask = 0u16;
                for d in descs {
                    valid_mask |= 1u16 << d.bit_index;
                }
                let apu_tel = get_nsf_apu_telemetry();
                let playing_now = self.play_mode == PlayMode::Play;
                let total_items = descs.len() + 1;
                let vis = LIST_VISIBLE_ROWS;
                let sel = sel.min(total_items.saturating_sub(1));
                let start = if total_items <= vis {
                    0
                } else if sel >= vis {
                    (sel + 1 - vis).min(total_items - vis)
                } else {
                    0
                };
                let enable_all_lbl = self.tr("Enable All Channels", "Увімкнути всі канали");
                let footer_hint = self.tr(
                    "A: Toggle  ·  Hold A: Solo  ·  X: Enable All",
                    "A: Перемкнути  ·  Утрим. A: Соло  ·  X: Усі УВІМК",
                );

                let rows: Vec<NsfChannelRow<'_>> = descs
                    .iter()
                    .enumerate()
                    .map(|(idx, d)| {
                        let bit = 1u16 << d.bit_index;
                        let enabled = (mute_mask & bit) == 0;
                        let soloed = descs.len() > 1 && (mute_mask & valid_mask) == (valid_mask & !bit);
                        let volume = if enabled && playing_now && idx < apu_tel.channel_count as usize {
                            apu_tel.channels[idx].volume.clamp(0.0, 1.0)
                        } else {
                            0.0
                        };
                        let active = enabled && volume > 0.02;
                        NsfChannelRow {
                            short_code: d.short_code,
                            display_name: d.display_name,
                            enabled,
                            soloed,
                            active,
                            volume,
                            selected: idx == sel,
                        }
                    })
                    .collect();

                let state = NsfChannelsState {
                    theme: self.theme,
                    header: self.header_state(self.tr("Channels", "Канали")),
                    channels: &rows,
                    enable_all_label: enable_all_lbl,
                    enable_all_selected: sel == descs.len(),
                    footer_hint,
                    start_item: start,
                    visible_items: vis,
                };
                ui.render_nsf_channels_to_fbo(&state);
            }

            Screen::SettingsMenu { sel } => {
                let items = vec![
                    (self.tr("Display", "Вигляд").to_string(), FileIconKind::None, OwnedRight::Chevron),
                    (self.tr("Sound", "Звук").to_string(), FileIconKind::None, OwnedRight::Chevron),
                    (self.tr("Offline Mode", "Офлайн-режим").to_string(), FileIconKind::None, OwnedRight::Toggle(self.offline_mode)),
                    (self.tr("Sources", "Джерела").to_string(), FileIconKind::None, OwnedRight::Chevron),
                    (self.tr("Accounts", "Акаунти").to_string(), FileIconKind::None, OwnedRight::Chevron),
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

            Screen::Keyboard {
                target,
                text,
                row,
                col,
                caps,
                sym_mode,
                allow_symbols,
            } => {
                let mut key_labels: [Vec<String>; 5] = Default::default();
                for r_idx in 0..5 {
                    let n = Self::kbd_row_len(allow_symbols, sym_mode, r_idx);
                    key_labels[r_idx] = (0..n)
                        .map(|c_idx| {
                            Self::kbd_key_label(allow_symbols, caps, sym_mode, r_idx, c_idx)
                        })
                        .collect();
                }
                let mut rows: [Vec<KeyboardKey<'_>>; 5] = Default::default();
                for r_idx in 0..5 {
                    rows[r_idx] = key_labels[r_idx]
                        .iter()
                        .enumerate()
                        .map(|(c_idx, lbl)| KeyboardKey {
                            label: lbl.as_str(),
                            selected: r_idx == row && c_idx == col,
                            is_modifier: r_idx == 4,
                            is_active_toggle: r_idx == 4
                                && ((c_idx == 0 && caps)
                                    || (allow_symbols && c_idx == 1 && sym_mode)),
                        })
                        .collect();
                }
                let is_hex = matches!(
                    target,
                    KbdTarget::HexBackground
                        | KbdTarget::HexForeground
                        | KbdTarget::HexShade1
                        | KbdTarget::HexShade2
                );
                let display = if is_hex { format!("#{text}") } else { text };
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
                    (self.tr("Crossfade & Auto-DJ", "Авто-DJ і кросфейд").to_string(), FileIconKind::None, OwnedRight::Chevron),
                    ("Replaygain".to_string(), FileIconKind::None, OwnedRight::Chevron),
                    (self.tr("Balance", "Баланс").to_string(), FileIconKind::None, OwnedRight::CycleValue(format!("{}%", self.dsp.balance_pct))),
                    (self.tr("Channels", "Канали").to_string(), FileIconKind::None, OwnedRight::CycleValue(CHANNEL_MODES[self.dsp.channels_idx].to_string())),
                    (self.tr("Crossfeed", "Кросфід").to_string(), FileIconKind::None, OwnedRight::CycleValue(CROSSFEED_MODES[self.dsp.crossfeed_idx].to_string())),
                    (self.tr("Menu Click", "Звук кліку").to_string(), FileIconKind::None, OwnedRight::Toggle(self.dsp.menu_click)),
                ];
                self.render_list(ui, self.tr("Sound", "Звук"), &items, sel);
            }

            Screen::DjCrossfadeMenu { sel } => {
                let mode_lbl = match self.dsp.dj_mode {
                    1 => self.tr("Classic", "Класичний"),
                    2 => self.tr("DJ Vinyl", "DJ Вініл"),
                    3 => self.tr("DJ Master Tempo", "DJ Майстер-темп"),
                    _ => self.tr("Off", "Вимк."),
                };
                let len_lbl = match self.dsp.dj_length {
                    0 => self.tr("4 Bars (Short)", "4 такти (Короткий)"),
                    2 => self.tr("12s (Long)", "12 с (Довгий)"),
                    _ => self.tr("8 Bars (Club)", "8 тактів (Клубний)"),
                };
                let stretch_lbl = match self.dsp.dj_max_stretch {
                    0 => "±6%",
                    2 => "±12%",
                    _ => "±10%",
                };
                let items = vec![
                    (self.tr("Crossfade Mode", "Режим кросфейду").to_string(), FileIconKind::None, OwnedRight::CycleValue(mode_lbl.to_string())),
                    (self.tr("Transition Length", "Тривалість переходу").to_string(), FileIconKind::None, OwnedRight::CycleValue(len_lbl.to_string())),
                    (self.tr("Max Tempo Stretch", "Макс. зсув темпу").to_string(), FileIconKind::None, OwnedRight::CycleValue(stretch_lbl.to_string())),
                    (self.tr("Keep Album Gapless", "Безшовний альбом").to_string(), FileIconKind::None, OwnedRight::Toggle(self.dsp.dj_keep_gapless)),
                    (self.tr("Quick Cut on Next", "Плавний скіп (R1)").to_string(), FileIconKind::None, OwnedRight::Toggle(self.dsp.dj_quick_cut)),
                ];
                self.render_list(ui, self.tr("Crossfade & Auto-DJ", "Авто-DJ і кросфейд"), &items, sel);
            }

            Screen::EqualizerMenu { sel } => {
                let items = vec![
                    (self.tr("Enable EQ", "Увімкнути EQ").to_string(), FileIconKind::None, OwnedRight::Toggle(self.dsp.eq_enabled)),
                    (self.tr("Graphic Equalizer", "Графічний еквалайзер").to_string(), FileIconKind::None, OwnedRight::Chevron),
                    ("AutoEQ".to_string(), FileIconKind::None, OwnedRight::Chevron),
                    (self.tr("Browse EQ Presets", "Пресети EQ").to_string(), FileIconKind::None, OwnedRight::Chevron),
                    (self.tr("Save EQ Preset", "Зберегти пресет EQ").to_string(), FileIconKind::None, OwnedRight::None),
                    (self.tr("Reset EQ", "Скинути EQ").to_string(), FileIconKind::None, OwnedRight::None),
                ];
                self.render_list(ui, self.tr("Equalizer", "Еквалайзер"), &items, sel);
            }

            Screen::GraphicEq { sel } => {
                let a_held = (self.held_buttons & btn::A) != 0;
                let x_held = (self.held_buttons & btn::X) != 0;
                let title = if x_held {
                    self.tr("Bypass", "Байпас")
                } else {
                    self.tr("Graphic Equalizer", "Графічний еквалайзер")
                };
                let state = GraphicEqState {
                    theme: self.theme,
                    header: self.header_state(title),
                    preamp_tenths_db: self.dsp.eq_preamp_tenths_db,
                    band_gains_tenths_db: self.dsp.eq_bands_tenths_db,
                    selected_col: sel,
                    adjusting: a_held,
                    bypassed: x_held,
                };
                ui.render_graphic_eq_to_fbo(&state);
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
                    (self.tr("Enable Effects", "Увімкнути ефекти").to_string(), FileIconKind::None, OwnedRight::Toggle(self.dsp.fx_enabled)),
                    (self.tr("Speed", "Швидкість").to_string(), FileIconKind::None, OwnedRight::CycleValue(format!("{}%", self.dsp.speed_pct))),
                    (self.tr("Tape Mode", "Режим касети").to_string(), FileIconKind::None, OwnedRight::Chevron),
                    (self.tr("Reverb", "Реверберація").to_string(), FileIconKind::None, OwnedRight::Chevron),
                    (self.tr("Delay", "Ділей").to_string(), FileIconKind::None, OwnedRight::Chevron),
                    (self.tr("Chorus", "Хорус").to_string(), FileIconKind::None, OwnedRight::Chevron),
                    (self.tr("Flanger", "Фленжер").to_string(), FileIconKind::None, OwnedRight::Chevron),
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

            Screen::ReverbMenu { sel } => {
                let items = vec![
                    (self.tr("Reverb", "Реверберація").to_string(), FileIconKind::None, OwnedRight::Toggle(self.dsp.reverb_enabled)),
                    (self.tr("Room Size", "Розмір кімнати").to_string(), FileIconKind::None, OwnedRight::CycleValue(format!("{}%", self.dsp.reverb_size))),
                    (self.tr("Damping", "Демпфування").to_string(), FileIconKind::None, OwnedRight::CycleValue(format!("{}%", self.dsp.reverb_damp))),
                    (self.tr("Pre-Delay", "Передзатримка").to_string(), FileIconKind::None, OwnedRight::CycleValue(format!("{} ms", self.dsp.reverb_predelay_ms))),
                    (self.tr("Stereo Width", "Ширина стерео").to_string(), FileIconKind::None, OwnedRight::CycleValue(format!("{}%", self.dsp.reverb_width))),
                    (self.tr("Mix", "Мікс").to_string(), FileIconKind::None, OwnedRight::CycleValue(format!("{}%", self.dsp.reverb_mix))),
                ];
                self.render_list(ui, self.tr("Reverb", "Реверберація"), &items, sel);
            }

            Screen::DelayMenu { sel } => {
                let items = vec![
                    (self.tr("Delay", "Ділей").to_string(), FileIconKind::None, OwnedRight::Toggle(self.dsp.delay_enabled)),
                    (self.tr("Delay Time", "Час затримки").to_string(), FileIconKind::None, OwnedRight::CycleValue(format!("{} ms", self.dsp.delay_time_ms))),
                    (self.tr("Feedback", "Зворотний зв'язок").to_string(), FileIconKind::None, OwnedRight::CycleValue(format!("{}%", self.dsp.delay_feedback))),
                    (self.tr("Tone", "Тон").to_string(), FileIconKind::None, OwnedRight::CycleValue(format!("{}%", self.dsp.delay_tone))),
                    (self.tr("Ping-Pong", "Пінг-понг").to_string(), FileIconKind::None, OwnedRight::Toggle(self.dsp.delay_pingpong)),
                    (self.tr("Mix", "Мікс").to_string(), FileIconKind::None, OwnedRight::CycleValue(format!("{}%", self.dsp.delay_mix))),
                ];
                self.render_list(ui, self.tr("Delay", "Ділей"), &items, sel);
            }

            Screen::ChorusMenu { sel } => {
                let items = vec![
                    (self.tr("Chorus", "Хорус").to_string(), FileIconKind::None, OwnedRight::Toggle(self.dsp.chorus_enabled)),
                    (self.tr("Rate", "Частота").to_string(), FileIconKind::None, OwnedRight::CycleValue(format!("{}%", self.dsp.chorus_rate))),
                    (self.tr("Depth", "Глибина").to_string(), FileIconKind::None, OwnedRight::CycleValue(format!("{}%", self.dsp.chorus_depth))),
                    (self.tr("Mix", "Мікс").to_string(), FileIconKind::None, OwnedRight::CycleValue(format!("{}%", self.dsp.chorus_mix))),
                ];
                self.render_list(ui, self.tr("Chorus", "Хорус"), &items, sel);
            }

            Screen::FlangerMenu { sel } => {
                let items = vec![
                    (self.tr("Flanger", "Фленжер").to_string(), FileIconKind::None, OwnedRight::Toggle(self.dsp.flanger_enabled)),
                    (self.tr("Rate", "Частота").to_string(), FileIconKind::None, OwnedRight::CycleValue(format!("{}%", self.dsp.flanger_rate))),
                    (self.tr("Depth", "Глибина").to_string(), FileIconKind::None, OwnedRight::CycleValue(format!("{}%", self.dsp.flanger_depth))),
                    (self.tr("Feedback", "Зворотний зв'язок").to_string(), FileIconKind::None, OwnedRight::CycleValue(format!("{}%", self.dsp.flanger_feedback))),
                    (self.tr("Mix", "Мікс").to_string(), FileIconKind::None, OwnedRight::CycleValue(format!("{}%", self.dsp.flanger_mix))),
                ];
                self.render_list(ui, self.tr("Flanger", "Фленжер"), &items, sel);
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
                let not_set = self.tr("Not set", "Не задано").to_string();
                let url_val = if is_jellyfin {
                    &self.jellyfin_url
                } else {
                    &self.navidrome_url
                };
                let usr_val = if is_jellyfin {
                    &self.jellyfin_user
                } else {
                    &self.navidrome_user
                };
                let items = vec![
                    (
                        "Server URL".to_string(),
                        FileIconKind::None,
                        OwnedRight::Value(if url_val.is_empty() {
                            not_set.clone()
                        } else {
                            url_val.clone()
                        }),
                    ),
                    (
                        "Username".to_string(),
                        FileIconKind::None,
                        OwnedRight::Value(if usr_val.is_empty() {
                            not_set
                        } else {
                            usr_val.clone()
                        }),
                    ),
                    (
                        "Audio Quality".to_string(),
                        FileIconKind::None,
                        OwnedRight::Value("Original".into()),
                    ),
                    (
                        "Connect & Sync".to_string(),
                        FileIconKind::None,
                        OwnedRight::None,
                    ),
                ];
                self.render_list(ui, title, &items, sel);
            }

            Screen::AccountsMenu { sel } => {
                let not_signed = self.tr("Not signed in", "Не здійснено вхід").to_string();
                let off_lbl = self.tr("Off", "Вимк.").to_string();
                let lfm_st = if !self.lfm_enabled {
                    off_lbl.clone()
                } else {
                    not_signed.clone()
                };
                let lb_st = if !self.lb_enabled {
                    off_lbl
                } else if !self.lb_user_token.trim().is_empty() {
                    self.tr("Signed in", "Вхід виконано").to_string()
                } else {
                    not_signed
                };
                let items = vec![
                    (
                        "Last.fm".to_string(),
                        FileIconKind::None,
                        OwnedRight::ValueChevron(lfm_st),
                    ),
                    (
                        "ListenBrainz".to_string(),
                        FileIconKind::None,
                        OwnedRight::ValueChevron(lb_st),
                    ),
                ];
                self.render_list(ui, self.tr("Accounts", "Акаунти"), &items, sel);
            }

            Screen::LastFmMenu { sel } => {
                let not_set = self.tr("Not set", "Не задано").to_string();
                let key_lbl = if self.lfm_api_key.is_empty() {
                    not_set.clone()
                } else {
                    self.lfm_api_key.clone()
                };
                let sec_lbl = if self.lfm_api_secret.is_empty() {
                    not_set
                } else {
                    "••••••••".to_string()
                };
                let items = vec![
                    (
                        self.tr("Scrobbling", "Скроблінг").to_string(),
                        FileIconKind::None,
                        OwnedRight::Toggle(self.lfm_enabled),
                    ),
                    (
                        self.tr("Account", "Акаунт").to_string(),
                        FileIconKind::None,
                        OwnedRight::Value(self.tr("Not signed in", "Не здійснено вхід").into()),
                    ),
                    (
                        self.tr("Sign In via QR / Browser", "Вхід за QR / браузером").to_string(),
                        FileIconKind::None,
                        OwnedRight::Chevron,
                    ),
                    (
                        self.tr("Sign In with Password", "Вхід за логіном і паролем").to_string(),
                        FileIconKind::None,
                        OwnedRight::Chevron,
                    ),
                    (
                        self.tr("Scrobble Streaming", "Скроблити стрімінг").to_string(),
                        FileIconKind::None,
                        OwnedRight::Toggle(self.lfm_streaming),
                    ),
                    (
                        self.tr("Sync Cached Scrobbles", "Відправити кеш скроблів").to_string(),
                        FileIconKind::None,
                        OwnedRight::Value("0".into()),
                    ),
                    (
                        self.tr("Clear Cached Scrobbles", "Очистити кеш скроблів").to_string(),
                        FileIconKind::None,
                        OwnedRight::Value("0".into()),
                    ),
                    ("API Key".to_string(), FileIconKind::None, OwnedRight::Value(key_lbl)),
                    ("API Secret".to_string(), FileIconKind::None, OwnedRight::Value(sec_lbl)),
                ];
                self.render_list(ui, "Last.fm", &items, sel);
            }

            Screen::ListenBrainzMenu { sel } => {
                let not_set = self.tr("Not set", "Не задано").to_string();
                let tok = self.lb_user_token.trim();
                let tok_lbl = if tok.is_empty() {
                    not_set
                } else if tok.len() <= 8 {
                    "••••••••".to_string()
                } else {
                    format!("{}…{}", &tok[..4], &tok[tok.len() - 4..])
                };
                let acc_lbl = if tok.is_empty() {
                    self.tr("Not signed in", "Не здійснено вхід").to_string()
                } else {
                    self.tr("Signed in", "Вхід виконано").to_string()
                };
                let url_disp = self
                    .lb_api_url
                    .trim()
                    .strip_prefix("https://")
                    .or_else(|| self.lb_api_url.trim().strip_prefix("http://"))
                    .unwrap_or(self.lb_api_url.trim())
                    .to_string();
                let items = vec![
                    (
                        self.tr("Scrobbling", "Скроблінг").to_string(),
                        FileIconKind::None,
                        OwnedRight::Toggle(self.lb_enabled),
                    ),
                    (
                        self.tr("Account", "Акаунт").to_string(),
                        FileIconKind::None,
                        OwnedRight::Value(acc_lbl),
                    ),
                    (
                        self.tr("Sign In with Token", "Вхід за токеном").to_string(),
                        FileIconKind::None,
                        OwnedRight::Chevron,
                    ),
                    (
                        self.tr("Now Playing", "Статус відтворення").to_string(),
                        FileIconKind::None,
                        OwnedRight::Toggle(self.lb_now_playing),
                    ),
                    (
                        self.tr("Scrobble Streaming", "Скроблити стрімінг").to_string(),
                        FileIconKind::None,
                        OwnedRight::Toggle(self.lb_streaming),
                    ),
                    (
                        self.tr("Sync Cached Scrobbles", "Відправити кеш скроблів").to_string(),
                        FileIconKind::None,
                        OwnedRight::Value("0".into()),
                    ),
                    (
                        self.tr("Clear Cached Scrobbles", "Очистити кеш скроблів").to_string(),
                        FileIconKind::None,
                        OwnedRight::Value("0".into()),
                    ),
                    (
                        self.tr("User Token", "Токен користувача").to_string(),
                        FileIconKind::None,
                        OwnedRight::Value(tok_lbl),
                    ),
                    (
                        self.tr("Server URL", "Адреса сервера").to_string(),
                        FileIconKind::None,
                        OwnedRight::Value(url_disp),
                    ),
                ];
                self.render_list(ui, "ListenBrainz", &items, sel);
            }

            Screen::LastFmQrAuth => {
                // Exact 4-shade `Last.fm Sign In` QR Auth screen (`#3725`)
                let hdr = self.header_state(self.tr("Last.fm Sign In", "Вхід у Last.fm"));
                ui.plugin_begin(self.theme, self.theme.bg_rgb, Some(&hdr));

                let qx = 96.0f32;
                let qy = 196.0f32;
                let qs = 360.0f32;
                ui.plugin_stroke_rect(qx, qy, qs, qs, 18.0, 4.0, self.theme.fg1_rgb);
                let n = 21usize;
                let pad = 24.0f32;
                let m_sz = (qs - pad * 2.0) / (n as f32);
                for r in 0..n {
                    for c in 0..n {
                        let finder = (r < 7 && c < 7)
                            || (r < 7 && c + 7 >= n)
                            || (r + 7 >= n && c < 7);
                        let on = if finder {
                            let lr = if r + 7 >= n { r - (n - 7) } else { r };
                            let lc = if c + 7 >= n { c - (n - 7) } else { c };
                            lr == 0
                                || lr == 6
                                || lc == 0
                                || lc == 6
                                || ((2..=4).contains(&lr) && (2..=4).contains(&lc))
                        } else {
                            ((r * 17 + c * 31 + (r ^ c) * 7) % 5) < 2
                        };
                        if on {
                            ui.plugin_fill_rect(
                                qx + pad + (c as f32) * m_sz,
                                qy + pad + (r as f32) * m_sz,
                                m_sz - 1.0,
                                m_sz - 1.0,
                                1.5,
                                self.theme.fg_rgb,
                            );
                        }
                    }
                }

                let tx = 500.0f32;
                ui.plugin_draw_text(
                    tx,
                    244.0,
                    self.tr("Scan with your phone", "Зіскануйте телефоном"),
                    34.0,
                    3,
                    0,
                    self.theme.fg_rgb,
                );
                ui.plugin_draw_text(
                    tx,
                    308.0,
                    "last.fm/api/auth",
                    28.0,
                    2,
                    0,
                    self.theme.fg2_rgb,
                );
                ui.plugin_draw_text(
                    tx,
                    382.0,
                    self.tr("Approve UIPod in browser,", "Підтвердіть UIPod у браузері,"),
                    24.0,
                    1,
                    0,
                    self.theme.fg2_rgb,
                );
                ui.plugin_draw_text(
                    tx,
                    420.0,
                    self.tr("then press A to finish.", "потім натисніть A."),
                    24.0,
                    1,
                    0,
                    self.theme.fg2_rgb,
                );
                ui.plugin_fill_rect(tx, 488.0, 428.0, 68.0, 14.0, self.theme.fg_rgb);
                ui.plugin_draw_text(
                    tx + 214.0,
                    522.0,
                    self.tr("A: Complete   •   B: Cancel", "A: Завершити   •   B: Скасувати"),
                    24.0,
                    2,
                    1,
                    self.theme.bg_rgb,
                );
            }

            Screen::LibraryMenu { sel } => {
                let items = vec![
                    (self.tr("Music Folders", "Теки музики").to_string(), FileIconKind::None, OwnedRight::Chevron),
                    (self.tr("Podcast Folders", "Теки подкастів").to_string(), FileIconKind::None, OwnedRight::Chevron),
                    (self.tr("Audiobook Folders", "Теки аудіокниг").to_string(), FileIconKind::None, OwnedRight::Chevron),
                    (self.tr("Movie Folders", "Теки відео").to_string(), FileIconKind::None, OwnedRight::Chevron),
                    (self.tr("Television Folders", "Теки серіалів").to_string(), FileIconKind::None, OwnedRight::Chevron),
                    (self.tr("SoundFont Folders", "Теки саундфонтів").to_string(), FileIconKind::None, OwnedRight::Chevron),
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
                    4 => (self.tr("Television Folders", "Теки серіалів"), "/mnt/SDCARD/Media/TV"),
                    _ => (self.tr("SoundFont Folders", "Теки саундфонтів"), "/mnt/SDCARD/SoundFonts"),
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
            Some(Screen::Keyboard { .. }) => "keyboard",
            Some(Screen::PluginActive) => {
                if matches!(self.active_plugin, Some(ActivePlugin::MusicQuiz(_))) {
                    "music_quiz"
                } else {
                    "plugin"
                }
            }
            Some(
                Screen::SoundMenu { .. }
                | Screen::DjCrossfadeMenu { .. }
                | Screen::TapeModeMenu { .. },
            ) => "sound",
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
                r##""eq":{},"autoeq":{},"tape":{},"wow":{},"hiss":{},"sat":{},"hf":{},"stopStart":{},"speed":{},"menuClick":{},"##,
                r##""djMode":{},"djLength":{},"djMaxStretch":{},"djKeepGapless":{},"djQuickCut":{}}}"##
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
            self.dsp.dj_mode,
            self.dsp.dj_length,
            self.dsp.dj_max_stretch,
            self.dsp.dj_keep_gapless,
            self.dsp.dj_quick_cut,
        )
    }
}

#[derive(Debug, Clone)]
pub(crate) enum BrowseMusicItem {
    NsfContainer {
        file_name: String,
        music_count: usize,
        play_idx: Option<usize>,
    },
    Track(usize),
    AboutTxt,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum NpCtxAction {
    PlayPause,
    NsfChannels,
    NsfEndlessLoop,
    Effects,
    SoundFont,
    Shuffle,
    Repeat,
    SleepTimer,
    LargeArt,
    AddToPlaylist,
    StartViz,
}

pub(crate) enum OwnedRight {
    None,
    Chevron,
    Toggle(bool),
    CycleValue(String),
    Value(String),
    ValueChevron(String),
}

impl OwnedRight {
    fn as_ref(&self) -> RightAccessory<'_> {
        match self {
            Self::None => RightAccessory::None,
            Self::Chevron => RightAccessory::Chevron,
            Self::Toggle(v) => RightAccessory::Toggle(*v),
            Self::CycleValue(s) => RightAccessory::CycleValue(s.as_str()),
            Self::Value(s) => RightAccessory::Value(s.as_str()),
            Self::ValueChevron(s) => RightAccessory::ValueChevron(s.as_str()),
        }
    }
}

fn has_leading_track_num(s: &str) -> bool {
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() && bytes[i].is_ascii_digit() {
        i += 1;
    }
    i > 0
        && i + 1 < bytes.len()
        && matches!(bytes[i], b'.' | b')' | b'-')
        && bytes[i + 1] == b' '
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

#[cfg(test)]
mod phase3_tests {
    use super::*;
    use uipod_codecs::reset_nsf_runtime_session;

    #[test]
    fn channels_menu_visibility_and_2a03_vrc6_mmc5_toggles_solo_enable_all() {
        reset_nsf_runtime_session();
        let mut app = UIPodApp::new();

        // 1. Standard MP3/FLAC tracks MUST hide `Channels` and `Endless Loop`
        app.current_track = 0; // built-in MP3 demo track
        assert!(app.active_nsf_chip_flags_for_ui().is_none());
        let mp3_actions = app.now_playing_context_actions();
        assert!(!mp3_actions.contains(&NpCtxAction::NsfChannels));
        assert!(!mp3_actions.contains(&NpCtxAction::NsfEndlessLoop));

        let flac_idx = app.add_user_track(
            "Lossless Suite".into(),
            "Studio".into(),
            "Reference".into(),
            "FLAC".into(),
            "24bit".into(),
            "96kHz".into(),
            180_000,
            None,
        );
        app.current_track = flac_idx;
        assert!(!app
            .now_playing_context_actions()
            .contains(&NpCtxAction::NsfChannels));

        // 2. 2A03 `.nsf` track: 5 channels, live bit toggles, Solo, Enable All
        let nsf_2a03 = app.add_nsf_subtrack(
            "/Music/Mega Man 2.nsf#0".into(),
            0,
            false,
            0,
            Some(42_000),
            "Title Theme".into(),
            "Takashi Tateishi".into(),
            "Mega Man 2".into(),
            102_000,
            None,
        );
        app.current_track = nsf_2a03;
        let cflags_2a03 = app.active_nsf_chip_flags_for_ui().expect("2A03 active");
        let descs_2a03 = cflags_2a03.channel_descriptors();
        assert_eq!(descs_2a03.len(), 5);
        assert_eq!(descs_2a03[0].display_name, "Pulse 1");
        assert_eq!(descs_2a03[4].display_name, "DPCM");

        let nsf_items = app.now_playing_context_items();
        assert!(nsf_items.iter().any(|(lbl, _, r)| {
            lbl == "Channels" && matches!(r, OwnedRight::ValueChevron(s) if s == "All ON")
        }));
        assert!(nsf_items.iter().any(|(lbl, _, _)| lbl == "Endless Loop"));

        // Toggle Pulse 1 (bit 0) and Noise (bit 3) -> 3/5 ON
        UIPodApp::toggle_nsf_channel_row(cflags_2a03, 0);
        UIPodApp::toggle_nsf_channel_row(cflags_2a03, 3);
        assert_eq!(get_nsf_channel_mute_mask(), (1 << 0) | (1 << 3));
        let nsf_items_muted = app.now_playing_context_items();
        assert!(nsf_items_muted.iter().any(|(lbl, _, r)| {
            lbl == "Channels" && matches!(r, OwnedRight::ValueChevron(s) if s == "3/5 ON")
        }));

        // Solo Triangle (row 2, bit 2) -> all other 4 channels muted (`1/5 ON`), second Solo restores `0`
        UIPodApp::solo_or_restore_nsf_channel(cflags_2a03, 2);
        assert_eq!(get_nsf_channel_mute_mask(), 0b1_1011);
        UIPodApp::solo_or_restore_nsf_channel(cflags_2a03, 2);
        assert_eq!(get_nsf_channel_mute_mask(), 0);

        // Mute DPCM and trigger `Enable All Channels` row (`row == 5`)
        UIPodApp::toggle_nsf_channel_row(cflags_2a03, 4);
        assert_eq!(get_nsf_channel_mute_mask(), 1 << 4);
        UIPodApp::toggle_nsf_channel_row(cflags_2a03, 5);
        assert_eq!(get_nsf_channel_mute_mask(), 0);

        // 3. VRC6 & MMC5 `.nsf` tracks: 8 channels (`2A03` 5 + 3 expansion channels)
        let nsf_vrc6 = app.add_nsf_subtrack(
            "/Music/Castlevania III.nsf#0".into(),
            0,
            false,
            ChipFlags::VRC6,
            None,
            "Beginning".into(),
            "Konami Kukeiha Club".into(),
            "Castlevania III".into(),
            135_000,
            None,
        );
        app.current_track = nsf_vrc6;
        let cflags_vrc6 = app.active_nsf_chip_flags_for_ui().unwrap();
        let descs_vrc6 = cflags_vrc6.channel_descriptors();
        assert_eq!(descs_vrc6.len(), 8);
        assert_eq!(descs_vrc6[7].display_name, "VRC6 Sawtooth");
        UIPodApp::toggle_nsf_channel_row(cflags_vrc6, 7);
        assert_eq!(get_nsf_channel_mute_mask(), 1 << 7);
        assert!(app.now_playing_context_items().iter().any(|(lbl, _, r)| {
            lbl == "Channels" && matches!(r, OwnedRight::ValueChevron(s) if s == "7/8 ON")
        }));

        let cflags_mmc5 = ChipFlags(ChipFlags::MMC5);
        assert_eq!(cflags_mmc5.channel_descriptors().len(), 8);
        assert_eq!(cflags_mmc5.channel_descriptors()[7].display_name, "MMC5 PCM");

        reset_nsf_runtime_session();
    }

    #[test]
    fn file_browser_nsf_drilldown_separates_music_sfx_and_persists_playlist_uri() {
        let mut app = UIPodApp::new();
        let m0 = app.add_nsf_subtrack(
            "/Music/Mega Man 2.nsf#0".into(),
            0,
            false,
            0,
            Some(36_000),
            "Title Theme".into(),
            "Capcom".into(),
            "Mega Man 2".into(),
            102_000,
            None,
        );
        let m1 = app.add_nsf_subtrack(
            "/Music/Mega Man 2.nsf#1".into(),
            1,
            false,
            0,
            Some(48_000),
            "Dr. Wily Stage 1".into(),
            "Capcom".into(),
            "Mega Man 2".into(),
            145_000,
            None,
        );
        let sfx0 = app.add_nsf_subtrack(
            "/Music/Mega Man 2.nsf#2".into(),
            2,
            true,
            0,
            None,
            "Track 03".into(),
            "Capcom".into(),
            "Mega Man 2".into(),
            1_800,
            None,
        );

        // 1. File Browser groups the container with `music_count == 2` (`[2 ♪]`)
        let browse = app.browse_music_items();
        let container_entries: Vec<_> = browse
            .iter()
            .filter_map(|it| match it {
                BrowseMusicItem::NsfContainer {
                    file_name,
                    music_count,
                    play_idx,
                } => Some((file_name.as_str(), *music_count, *play_idx)),
                _ => None,
            })
            .collect();
        assert_eq!(
            container_entries,
            vec![("/Music/Mega Man 2.nsf", 2, Some(m0))]
        );

        // 2. Container drill-down separates Music (2) and Sound Effects (1)
        let (music, sfx, game_title) = app.nsf_subtracks_for_file("/Music/Mega Man 2.nsf");
        assert_eq!(game_title, "Mega Man 2");
        assert_eq!(music, vec![m0, m1]);
        assert_eq!(sfx, vec![sfx0]);

        // 3. Library Songs / Next-Prev navigation exclude SFX subtracks
        assert_eq!(app.step_playable_track(m1, 1), 0); // wraps back to track 0, skipping `sfx0`!

        // 4. Add to Playlist on subtrack `#1` stores exact `/Music/Mega Man 2.nsf#1` URI
        let chosen_uri = app.tracks[m1].uri.clone().unwrap();
        app.user_playlist_uris.push(chosen_uri);
        assert_eq!(app.user_playlist_uris, vec!["/Music/Mega Man 2.nsf#1"]);
    }
}
