//! Parser and serializer for `lastfm.cfg` (editable on SD card or in-app).

pub const DEFAULT_API_KEY: &str = match option_env!("UIPOD_LASTFM_API_KEY") {
    Some(v) => v,
    None => "",
};

pub const DEFAULT_API_SECRET: &str = match option_env!("UIPOD_LASTFM_API_SECRET") {
    Some(v) => v,
    None => "",
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LastFmConfig {
    pub enabled: bool,
    pub scrobble_streaming: bool,
    pub api_key: String,
    pub api_secret: String,
    pub username: String,
    pub password: String,
    pub session_key: String,
}

impl Default for LastFmConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            scrobble_streaming: true,
            api_key: DEFAULT_API_KEY.trim().to_string(),
            api_secret: DEFAULT_API_SECRET.trim().to_string(),
            username: String::new(),
            password: String::new(),
            session_key: String::new(),
        }
    }
}

fn parse_bool(val: &str, default_val: bool) -> bool {
    match val.trim().to_ascii_lowercase().as_str() {
        "1" | "true" | "yes" | "on" => true,
        "0" | "false" | "no" | "off" => false,
        _ => default_val,
    }
}

impl LastFmConfig {
    pub fn effective_api_key(&self) -> &str {
        let k = self.api_key.trim();
        if !k.is_empty() {
            k
        } else {
            DEFAULT_API_KEY.trim()
        }
    }

    pub fn effective_api_secret(&self) -> &str {
        let s = self.api_secret.trim();
        if !s.is_empty() {
            s
        } else {
            DEFAULT_API_SECRET.trim()
        }
    }

    pub fn has_api_credentials(&self) -> bool {
        !self.effective_api_key().is_empty() && !self.effective_api_secret().is_empty()
    }

    pub fn is_authenticated(&self) -> bool {
        !self.session_key.trim().is_empty() && self.has_api_credentials()
    }

    pub fn has_password_login_pending(&self) -> bool {
        self.session_key.trim().is_empty()
            && !self.username.trim().is_empty()
            && !self.password.is_empty()
            && self.has_api_credentials()
    }

    pub fn parse_text(text: &str) -> Self {
        let mut cfg = Self::default();
        for raw_line in text.lines() {
            let line = raw_line.trim();
            if line.is_empty() || line.starts_with('#') || line.starts_with(';') || line.starts_with('[') {
                continue;
            }
            let Some((k_raw, v_raw)) = line.split_once('=') else {
                continue;
            };
            let k = k_raw.trim().to_ascii_lowercase();
            let v = v_raw.trim().trim_matches('"');
            match k.as_str() {
                "enabled" => cfg.enabled = parse_bool(v, true),
                "scrobble_streaming" | "streaming" => cfg.scrobble_streaming = parse_bool(v, true),
                "api_key" | "apikey" => {
                    if !v.is_empty() {
                        cfg.api_key = v.to_string();
                    }
                }
                "api_secret" | "apisecret" | "shared_secret" | "secret" => {
                    if !v.is_empty() {
                        cfg.api_secret = v.to_string();
                    }
                }
                "username" | "user" => cfg.username = v.to_string(),
                "password" | "pass" => cfg.password = v.to_string(),
                "session_key" | "sk" => cfg.session_key = v.to_string(),
                _ => {}
            }
        }
        cfg
    }

    pub fn format_text(&self) -> String {
        let mut out = String::with_capacity(512);
        out.push_str("# UIPod Last.fm configuration\n");
        out.push_str("# Create a free API key at https://www.last.fm/api/account/create\n");
        out.push_str(&format!(
            "enabled = {}\n",
            if self.enabled { "true" } else { "false" }
        ));
        out.push_str(&format!(
            "scrobble_streaming = {}\n",
            if self.scrobble_streaming { "true" } else { "false" }
        ));
        out.push_str(&format!("api_key = {}\n", self.api_key.trim()));
        out.push_str(&format!("api_secret = {}\n", self.api_secret.trim()));
        out.push_str(&format!("username = {}\n", self.username.trim()));
        if !self.password.is_empty() && self.session_key.trim().is_empty() {
            out.push_str(&format!("password = {}\n", self.password));
        }
        out.push_str(&format!("session_key = {}\n", self.session_key.trim()));
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_config_roundtrip() {
        let mut c = LastFmConfig::default();
        c.enabled = true;
        c.scrobble_streaming = false;
        c.api_key = "11223344".to_string();
        c.api_secret = "aabbccdd".to_string();
        c.username = "famiclone".to_string();
        c.session_key = "sess_xyz".to_string();

        let txt = c.format_text();
        let parsed = LastFmConfig::parse_text(&txt);
        assert_eq!(parsed, c);
        assert!(parsed.is_authenticated());
    }
}
