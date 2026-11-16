//! Parser and serializer for `navidrome.cfg` (editable on SD card or in-app).

#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StreamQuality {
    Original = 0,
    Mp3_320 = 1,
    Mp3_192 = 2,
}

impl StreamQuality {
    pub fn from_u8(v: u8) -> Self {
        match v {
            1 => Self::Mp3_320,
            2 => Self::Mp3_192,
            _ => Self::Original,
        }
    }

    pub fn from_cfg_str(s: &str) -> Self {
        match s.trim().to_ascii_lowercase().as_str() {
            "mp3_320" | "320" | "mp3-320" | "high" => Self::Mp3_320,
            "mp3_192" | "192" | "mp3-192" | "medium" | "low" => Self::Mp3_192,
            _ => Self::Original,
        }
    }

    pub fn as_cfg_str(self) -> &'static str {
        match self {
            Self::Original => "original",
            Self::Mp3_320 => "mp3_320",
            Self::Mp3_192 => "mp3_192",
        }
    }

    pub fn cache_subdir(self) -> &'static str {
        match self {
            Self::Original => "raw",
            Self::Mp3_320 => "mp3_320",
            Self::Mp3_192 => "mp3_192",
        }
    }

    pub fn file_ext<'a>(self, original_suffix: &'a str) -> &'a str {
        match self {
            Self::Original => {
                let s = original_suffix.trim().trim_start_matches('.');
                if s.is_empty() {
                    "flac"
                } else {
                    s
                }
            }
            Self::Mp3_320 | Self::Mp3_192 => "mp3",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NavidromeConfig {
    pub url: String,
    pub username: String,
    pub password: String,
    pub quality: StreamQuality,
    /// `0` means Unlimited; default `1024` MB (1 GB).
    pub cache_limit_mb: u32,
    pub allow_self_signed: bool,
}

impl Default for NavidromeConfig {
    fn default() -> Self {
        Self {
            url: String::new(),
            username: String::new(),
            password: String::new(),
            quality: StreamQuality::Original,
            cache_limit_mb: 1024,
            allow_self_signed: true,
        }
    }
}

impl NavidromeConfig {
    pub fn is_configured(&self) -> bool {
        !self.url.trim().is_empty()
            && !self.username.trim().is_empty()
            && crate::url::ParsedServerUrl::parse(&self.url).is_ok()
    }

    pub fn parse_ini(text: &str) -> Self {
        let mut cfg = Self::default();
        let text = text.strip_prefix('\u{feff}').unwrap_or(text);

        for raw_line in text.lines() {
            let line = raw_line.trim();
            if line.is_empty() || line.starts_with('#') || line.starts_with(';') || line.starts_with('[')
            {
                continue;
            }
            let Some((k, v)) = line.split_once('=') else {
                continue;
            };
            let key = k.trim().to_ascii_lowercase();
            let mut val = v.trim();
            if (val.starts_with('"') && val.ends_with('"') && val.len() >= 2)
                || (val.starts_with('\'') && val.ends_with('\'') && val.len() >= 2)
            {
                val = &val[1..val.len() - 1];
            }

            match key.as_str() {
                "url" | "server" | "host" | "server_url" => cfg.url = val.trim().to_string(),
                "username" | "user" => cfg.username = val.trim().to_string(),
                "password" | "pass" => cfg.password = val.to_string(),
                "quality" | "stream_quality" | "format" => {
                    cfg.quality = StreamQuality::from_cfg_str(val);
                }
                "cache_limit_mb" | "cache_mb" | "cache_limit" => {
                    if val.eq_ignore_ascii_case("unlimited") || val.eq_ignore_ascii_case("none") {
                        cfg.cache_limit_mb = 0;
                    } else if let Ok(mb) = val.parse::<u32>() {
                        cfg.cache_limit_mb = mb;
                    }
                }
                "allow_self_signed" | "insecure_tls" | "self_signed" => {
                    cfg.allow_self_signed = matches!(
                        val.to_ascii_lowercase().as_str(),
                        "1" | "true" | "yes" | "on"
                    );
                }
                _ => {}
            }
        }
        cfg
    }

    pub fn to_ini(&self) -> String {
        format!(
            "# Trimpod Navidrome Configuration\n\
             # Quality options: original, mp3_320, mp3_192\n\
             # Cache limit in MB (0 = unlimited)\n\
             url={}\n\
             username={}\n\
             password={}\n\
             quality={}\n\
             cache_limit_mb={}\n\
             allow_self_signed={}\n",
            self.url.trim(),
            self.username.trim(),
            self.password,
            self.quality.as_cfg_str(),
            self.cache_limit_mb,
            if self.allow_self_signed { 1 } else { 0 }
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_config_roundtrip_and_windows_crlf() {
        let raw = "\u{feff}# Comment\r\nurl = \"http://192.168.1.10:4533\"\r\nuser = demo\r\npassword = \"p@ss=word#1\"\r\nquality = 320\r\ncache_limit_mb = 2048\r\nallow_self_signed = false\r\n";
        let cfg = NavidromeConfig::parse_ini(raw);
        assert_eq!(cfg.url, "http://192.168.1.10:4533");
        assert_eq!(cfg.username, "demo");
        assert_eq!(cfg.password, "p@ss=word#1");
        assert_eq!(cfg.quality, StreamQuality::Mp3_320);
        assert_eq!(cfg.cache_limit_mb, 2048);
        assert!(!cfg.allow_self_signed);
        assert!(cfg.is_configured());

        let saved = cfg.to_ini();
        let reloaded = NavidromeConfig::parse_ini(&saved);
        assert_eq!(cfg, reloaded);
    }
}
