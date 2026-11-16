//! Pure-Rust GPU UI engine for Trimpod (`taffy` + `femtovg` on OpenGL ES 3.0).
//!
//! Zero Rockbox header or symbol dependencies — usable both via C FFI during the
//! incremental Strangler-Fig migration and directly from Rust in the final binary.

pub mod ffi;
pub mod icons;
pub mod layout;
pub mod renderer;

pub use renderer::UiEngine;

/// Default `ui.figcraft` background color (`#D6D8D5`).
pub const DEFAULT_BG_RGB: u32 = 0xD6D8D5;
/// Default `ui.figcraft` foreground color (`#3C3637`).
pub const DEFAULT_FG_RGB: u32 = 0x3C3637;

/// Active theme colors (`0xRRGGBB`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ThemeColors {
    pub bg_rgb: u32,
    pub fg_rgb: u32,
}

impl Default for ThemeColors {
    fn default() -> Self {
        Self {
            bg_rgb: DEFAULT_BG_RGB,
            fg_rgb: DEFAULT_FG_RGB,
        }
    }
}

/// Playback transport state shown in the top-left `Header` slot.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PlayMode {
    #[default]
    Stop,
    Play,
    Pause,
    FastForward,
    Rewind,
}

/// File-type badge shown on the left of a `MenuItem` in file/track browsers.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum FileIconKind {
    #[default]
    None,
    Folder,
    AddFolder,
    Audio(String),
    Video(String),
    Text(String),
}

impl FileIconKind {
    /// Parses a Rockbox/Trimpod icon tag or extension into a typed `ui.figcraft` `icon_filetype`.
    pub fn from_tag(tag: &str) -> Self {
        let t = tag.trim().trim_start_matches('.');
        if t.is_empty() {
            return Self::None;
        }
        let upper = t.to_ascii_uppercase();
        match upper.as_str() {
            "DIR" | "FOLDER" => Self::Folder,
            "ADD" | "ADD_DIR" | "ADD_FOLDER" | "PLUS" => Self::AddFolder,
            "MP3" | "FLAC" | "WAV" | "AAC" | "M4A" | "M4B" | "OGG" | "OGA" | "OPUS" | "ALAC"
            | "AIFF" | "AIF" | "WMA" | "APE" | "WV" | "MPC" | "DSF" | "DFF" | "SPX" | "M3U"
            | "M3U8" | "PLS" => {
                let short = match upper.as_str() {
                    "M3U8" => "M3U",
                    "AIFF" => "AIF",
                    other => other,
                };
                Self::Audio(short.to_string())
            }
            "MP4" | "M4V" | "MKV" | "AVI" | "MOV" | "WEBM" | "MPG" | "MPEG" | "TS" => {
                let short = match upper.as_str() {
                    "MPEG" => "MPG",
                    "WEBM" => "WEB",
                    other => other,
                };
                Self::Video(short.to_string())
            }
            "TXT" | "LRC" | "CFG" | "LOG" | "MD" | "JSON" | "INI" | "CUE" => {
                let short = match upper.as_str() {
                    "JSON" => "JSN",
                    other => other,
                };
                Self::Text(short.to_string())
            }
            other => {
                let clipped: String = other.chars().take(4).collect();
                Self::Audio(clipped)
            }
        }
    }
}

/// Right-hand widget inside a `MenuItem`.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum RightAccessory<'a> {
    #[default]
    None,
    /// Submenu triangle chevron (`icon_play_s`, `32x32`).
    Chevron,
    /// Boolean pill switch (`toggle`, `64x32`).
    Toggle(bool),
    /// Right-aligned setting value string (e.g. `"-6 dB"`, `"All"`, `"80%"`).
    Value(&'a str),
}

/// State of the shared `Header` component (`960x80` + `4px` bottom border).
#[derive(Debug, Clone, Copy)]
pub struct HeaderState<'a> {
    pub title: &'a str,
    /// Optional left-aligned button legend (`%Tl`) overriding `play_mode` + `title` when non-empty.
    pub legend: &'a str,
    pub play_mode: PlayMode,
    pub battery_percent: u8,
    pub is_charging: bool,
    pub is_hold: bool,
}

/// Single visible row inside a `Main` / `Source` / `Settings` list screen.
#[derive(Debug, Clone)]
pub struct ListRow<'a> {
    pub label: &'a str,
    pub selected: bool,
    pub file_icon: FileIconKind,
    pub right: RightAccessory<'a>,
}

/// Complete state of a list/menu screen (`Main`, `Source`, file browser, settings).
#[derive(Debug, Clone)]
pub struct ListScreenState<'a> {
    pub theme: ThemeColors,
    pub header: HeaderState<'a>,
    pub rows: Vec<ListRow<'a>>,
    pub total_items: usize,
    pub start_item: usize,
    pub visible_items: usize,
}

/// Complete state of the `Now Playing` (`WPS`) screen (`[233]`).
#[derive(Debug, Clone, Copy)]
pub struct NowPlayingState<'a> {
    pub theme: ThemeColors,
    pub header: HeaderState<'a>,
    /// e.g. `"21 of 500"`
    pub track_pos_label: &'a str,
    pub eq_enabled: bool,
    pub title: &'a str,
    pub artist: &'a str,
    pub album: &'a str,
    /// e.g. `"FLAC"`, `"MP3"`, `"AAC"`, `"OPUS"`
    pub codec_label: &'a str,
    /// e.g. `"24bit"`, `"16bit"` (empty for lossy codecs if N/A)
    pub bitdepth_label: &'a str,
    /// e.g. `"192kHz"`, `"44.1kHz"`
    pub samplerate_label: &'a str,
    /// `0.0..=1.0` playback progress (or volume fraction when `center_status_label` is active)
    pub progress: f32,
    /// e.g. `"1:08"`
    pub elapsed_label: &'a str,
    /// e.g. `"-2:07"`
    pub remaining_label: &'a str,
    /// Optional centered transient status (e.g. `"Volume  -12 dB"`) replacing time labels briefly.
    pub center_status_label: &'a str,
    /// Optional `256 * 256 * 3` RGB888 album art pixels.
    pub art_rgb24: Option<&'a [u8]>,
    /// When `true` (default per user requirement), renders album art with monochrome + contrast filter.
    pub mono_album_art: bool,
}
