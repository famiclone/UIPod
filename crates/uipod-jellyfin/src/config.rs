//! Configuration model and INI serializer/parser for `jellyfin.cfg`.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum VideoQuality {
    /// Direct Stream compatible 8-bit H.264/MPEG4 up to 1080p (<= 15 Mbps),
    /// otherwise transcode to 720p H.264 (4.5 Mbps) + stereo AAC.
    Auto720p = 0,
    /// Direct Stream compatible 8-bit H.264/MPEG4 up to 720p (<= 4.5 Mbps),
    /// otherwise transcode to 720p H.264 (4.0 Mbps) + stereo AAC.
    Transcode720p = 1,
    /// Direct Stream compatible 8-bit H.264/MPEG4 up to 480p (<= 2.2 Mbps),
    /// otherwise transcode to 480p H.264 (2.0 Mbps) + stereo AAC.
    Transcode480p = 2,
    /// Direct Stream compatible 8-bit H.264/MPEG4 up to 1080p at any bitrate,
    /// otherwise transcode HEVC/AV1/VP9/10-bit/4K to 1080p H.264 (10 Mbps) + stereo AAC.
    Original1080p = 3,
}

impl VideoQuality {
    pub fn from_u8(v: u8) -> Self {
        match v {
            1 => Self::Transcode720p,
            2 => Self::Transcode480p,
            3 => Self::Original1080p,
            _ => Self::Auto720p,
        }
    }

    pub fn max_direct_height(self) -> u32 {
        match self {
            Self::Auto720p | Self::Original1080p => 1080,
            Self::Transcode720p => 720,
            Self::Transcode480p => 480,
        }
    }

    pub fn max_direct_bitrate(self) -> u32 {
        match self {
            Self::Auto720p => 15_000_000,
            Self::Transcode720p => 4_500_000,
            Self::Transcode480p => 2_200_000,
            Self::Original1080p => 80_000_000,
        }
    }

    pub fn transcode_params(self) -> (u32, u32, u32, u32) {
        // (max_width, max_height, video_bitrate, audio_bitrate)
        match self {
            Self::Auto720p => (1280, 720, 4_500_000, 192_000),
            Self::Transcode720p => (1280, 720, 4_000_000, 192_000),
            Self::Transcode480p => (854, 480, 2_000_000, 160_000),
            Self::Original1080p => (1920, 1080, 10_000_000, 192_000),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum AudioQuality {
    Original = 0,
    Mp3_320 = 1,
    Mp3_192 = 2,
}

impl AudioQuality {
    pub fn from_u8(v: u8) -> Self {
        match v {
            1 => Self::Mp3_320,
            2 => Self::Mp3_192,
            _ => Self::Original,
        }
    }

    pub fn file_ext(self, original_suffix: &str) -> String {
        match self {
            Self::Original => {
                let clean = original_suffix.trim().trim_start_matches('.').to_ascii_lowercase();
                if clean.is_empty() {
                    "mp3".to_string()
                } else {
                    clean
                }
            }
            Self::Mp3_320 | Self::Mp3_192 => "mp3".to_string(),
        }
    }
}

#[cfg(target_os = "android")]
pub const DEFAULT_CACHE_LOCATION: &str = "/storage/emulated/0/Media";
#[cfg(not(target_os = "android"))]
pub const DEFAULT_CACHE_LOCATION: &str = "/mnt/SDCARD/Media";

pub fn normalize_cache_location(raw: &str) -> String {
    let mut s = raw.trim().trim_end_matches('/').to_string();
    if s.is_empty() {
        return DEFAULT_CACHE_LOCATION.to_string();
    }
    if let Some(stripped) = s.strip_suffix("/jellyfin") {
        if stripped.starts_with('/') && !stripped.is_empty() {
            s = stripped.to_string();
        }
    }
    if s.is_empty() {
        DEFAULT_CACHE_LOCATION.to_string()
    } else {
        s
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JellyfinConfig {
    pub url: String,
    pub username: String,
    pub password: String,
    pub access_token: String,
    pub user_id: String,
    pub video_quality: VideoQuality,
    pub audio_quality: AudioQuality,
    /// `0` means unlimited; default is `2048` MB (2 GB).
    pub cache_limit_mb: u32,
    pub allow_self_signed: bool,
    /// Base media directory (e.g. `"/mnt/SDCARD/Media"`); Jellyfin caches into `<cache_location>/jellyfin`.
    pub cache_location: String,
}

impl Default for JellyfinConfig {
    fn default() -> Self {
        Self {
            url: String::new(),
            username: String::new(),
            password: String::new(),
            access_token: String::new(),
            user_id: String::new(),
            video_quality: VideoQuality::Auto720p,
            audio_quality: AudioQuality::Original,
            cache_limit_mb: 2048,
            allow_self_signed: false,
            cache_location: String::new(),
        }
    }
}

impl JellyfinConfig {
    pub fn effective_roots(&self) -> Option<(std::path::PathBuf, std::path::PathBuf)> {
        if self.cache_location.trim().is_empty() {
            None
        } else {
            let jf_root = std::path::PathBuf::from(normalize_cache_location(&self.cache_location))
                .join("jellyfin");
            let audio_root = jf_root.join("Music");
            Some((jf_root, audio_root))
        }
    }
    pub fn is_configured(&self) -> bool {
        !self.url.trim().is_empty()
            && (!self.username.trim().is_empty()
                || !self.password.is_empty()
                || !self.access_token.trim().is_empty())
    }

    pub fn parse_ini(text: &str) -> Self {
        let mut cfg = Self::default();
        for raw_line in text.lines() {
            let line = raw_line.trim();
            if line.is_empty() || line.starts_with('#') || line.starts_with(';') || line.starts_with('[') {
                continue;
            }
            let Some((k, v)) = line.split_once('=') else {
                continue;
            };
            let key = k.trim();
            let val = v.trim();
            match key {
                "url" => cfg.url = val.to_string(),
                "username" => cfg.username = val.to_string(),
                "password" => cfg.password = val.to_string(),
                "access_token" => cfg.access_token = val.to_string(),
                "user_id" => cfg.user_id = val.to_string(),
                "video_quality" => {
                    cfg.video_quality = VideoQuality::from_u8(val.parse::<u8>().unwrap_or(0));
                }
                "audio_quality" => {
                    cfg.audio_quality = AudioQuality::from_u8(val.parse::<u8>().unwrap_or(0));
                }
                "cache_limit_mb" => {
                    if let Ok(mb) = val.parse::<u32>() {
                        cfg.cache_limit_mb = mb;
                    }
                }
                "allow_self_signed" => {
                    cfg.allow_self_signed = matches!(val, "1" | "true" | "yes" | "on");
                }
                "cache_location" | "cache_dir" | "cache_root" => {
                    cfg.cache_location = normalize_cache_location(val);
                }
                _ => {}
            }
        }
        cfg
    }

    pub fn to_ini(&self) -> String {
        fn clean_line(s: &str) -> String {
            s.chars().filter(|&c| c != '\r' && c != '\n').collect()
        }
        let loc_line = if self.cache_location.trim().is_empty() {
            String::new()
        } else {
            format!(
                "cache_location={}\n",
                clean_line(&normalize_cache_location(&self.cache_location))
            )
        };
        format!(
            "# UIPod Jellyfin Configuration\n\
             url={}\n\
             username={}\n\
             password={}\n\
             access_token={}\n\
             user_id={}\n\
             video_quality={}\n\
             audio_quality={}\n\
             cache_limit_mb={}\n\
             allow_self_signed={}\n\
             {}",
            clean_line(&self.url),
            clean_line(&self.username),
            clean_line(&self.password),
            clean_line(&self.access_token),
            clean_line(&self.user_id),
            self.video_quality as u8,
            self.audio_quality as u8,
            self.cache_limit_mb,
            if self.allow_self_signed { 1 } else { 0 },
            loc_line,
        )
    }
}
