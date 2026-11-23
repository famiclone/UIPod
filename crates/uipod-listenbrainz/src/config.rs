//! Parser and serializer for `listenbrainz.cfg` and server URL resolver.

pub const DEFAULT_API_URL: &str = "https://api.listenbrainz.org";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ListenBrainzConfig {
    pub enabled: bool,
    pub now_playing: bool,
    pub scrobble_streaming: bool,
    pub allow_self_signed: bool,
    pub api_url: String,
    pub user_token: String,
    pub username: String,
}

impl Default for ListenBrainzConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            now_playing: true,
            scrobble_streaming: true,
            allow_self_signed: false,
            api_url: DEFAULT_API_URL.to_string(),
            user_token: String::new(),
            username: String::new(),
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

fn strip_prefix_ci<'a>(s: &'a str, prefix: &str) -> Option<&'a str> {
    if s.len() >= prefix.len() && s[..prefix.len()].eq_ignore_ascii_case(prefix) {
        Some(&s[prefix.len()..])
    } else {
        None
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedLbUrl {
    pub is_https: bool,
    pub host: String,
    pub port: u16,
    /// Leading slash if non-empty, no trailing slash, and without trailing `/1`
    /// (e.g. `""` for `https://api.listenbrainz.org` or `"/apis/listenbrainz"` for Maloja).
    pub base_path: String,
}

impl ParsedLbUrl {
    pub fn parse(raw: &str) -> Result<Self, String> {
        let trimmed = raw.trim();
        let effective = if trimmed.is_empty() {
            DEFAULT_API_URL
        } else {
            trimmed
        };

        let (scheme_opt, rest) = if let Some(r) = strip_prefix_ci(effective, "https://") {
            (Some(true), r)
        } else if let Some(r) = strip_prefix_ci(effective, "http://") {
            (Some(false), r)
        } else if effective.contains("://") {
            return Err("Unsupported URL scheme".to_string());
        } else {
            (None, effective)
        };

        let (authority, path_part) = match rest.find('/') {
            Some(idx) => (&rest[..idx], &rest[idx..]),
            None => (rest, ""),
        };

        if authority.is_empty() {
            return Err("Missing server host".to_string());
        }

        let host_port = match authority.rfind('@') {
            Some(at) => &authority[at + 1..],
            None => authority,
        };

        let (host, explicit_port) = if let Some(colon) = host_port.rfind(':') {
            let h = &host_port[..colon];
            let p_str = &host_port[colon + 1..];
            if p_str.is_empty() {
                (h, None)
            } else {
                let p: u16 = p_str
                    .parse()
                    .map_err(|_| format!("Invalid port: {p_str}"))?;
                if p == 0 {
                    return Err("Port cannot be 0".to_string());
                }
                (h, Some(p))
            }
        } else {
            (host_port, None)
        };

        let host = host.trim();
        if host.is_empty() || host.contains(char::is_whitespace) {
            return Err("Invalid server hostname".to_string());
        }

        let is_https = match scheme_opt {
            Some(v) => v,
            None => explicit_port != Some(80),
        };
        let port = explicit_port.unwrap_or(if is_https { 443 } else { 80 });

        let mut clean_path = path_part
            .split(['?', '#'])
            .next()
            .unwrap_or("")
            .trim_end_matches('/');
        if clean_path == "/1" {
            clean_path = "";
        } else if let Some(stripped) = clean_path.strip_suffix("/1") {
            clean_path = stripped.trim_end_matches('/');
        }

        let base_path = if clean_path.is_empty() || clean_path == "/" {
            String::new()
        } else if clean_path.starts_with('/') {
            clean_path.to_string()
        } else {
            format!("/{clean_path}")
        };

        Ok(Self {
            is_https,
            host: host.to_ascii_lowercase(),
            port,
            base_path,
        })
    }

    pub fn endpoint_path(&self, suffix: &str) -> String {
        if self.base_path.is_empty() {
            suffix.to_string()
        } else {
            format!("{}{}", self.base_path, suffix)
        }
    }
}

impl ListenBrainzConfig {
    pub fn effective_api_url(&self) -> &str {
        let u = self.api_url.trim();
        if u.is_empty() {
            DEFAULT_API_URL
        } else {
            u
        }
    }

    pub fn has_token(&self) -> bool {
        let clean: String = self
            .user_token
            .trim()
            .chars()
            .filter(|&c| c != '\r' && c != '\n')
            .collect();
        !clean.is_empty()
    }

    pub fn clean_token(&self) -> String {
        let raw = self.user_token.trim();
        let stripped = strip_prefix_ci(raw, "token ").unwrap_or(raw).trim();
        stripped
            .chars()
            .filter(|&c| c != '\r' && c != '\n')
            .collect()
    }

    pub fn is_authenticated(&self) -> bool {
        self.has_token() && ParsedLbUrl::parse(self.effective_api_url()).is_ok()
    }

    pub fn parse_text(text: &str) -> Self {
        let mut cfg = Self::default();
        for raw_line in text.lines() {
            let line = raw_line.trim();
            if line.is_empty()
                || line.starts_with('#')
                || line.starts_with(';')
                || line.starts_with('[')
            {
                continue;
            }
            let Some((k_raw, v_raw)) = line.split_once('=') else {
                continue;
            };
            let k = k_raw.trim().to_ascii_lowercase();
            let v = v_raw.trim().trim_matches('"');
            match k.as_str() {
                "enabled" => cfg.enabled = parse_bool(v, true),
                "now_playing" | "nowplaying" => cfg.now_playing = parse_bool(v, true),
                "scrobble_streaming" | "streaming" => cfg.scrobble_streaming = parse_bool(v, true),
                "allow_self_signed" | "insecure_tls" => {
                    cfg.allow_self_signed = parse_bool(v, false);
                }
                "api_url" | "url" | "server_url" => {
                    cfg.api_url = if v.trim().is_empty() {
                        DEFAULT_API_URL.to_string()
                    } else {
                        v.trim().to_string()
                    };
                }
                "user_token" | "token" => {
                    let stripped = strip_prefix_ci(v, "token ").unwrap_or(v).trim();
                    cfg.user_token = stripped.to_string();
                }
                "username" | "user" | "user_name" => cfg.username = v.trim().to_string(),
                _ => {}
            }
        }
        cfg
    }

    pub fn format_text(&self) -> String {
        let mut out = String::with_capacity(512);
        out.push_str("# UIPod ListenBrainz configuration\n");
        out.push_str("# Find your User Token at https://listenbrainz.org/settings/\n");
        out.push_str(&format!(
            "enabled = {}\n",
            if self.enabled { "true" } else { "false" }
        ));
        out.push_str(&format!(
            "now_playing = {}\n",
            if self.now_playing { "true" } else { "false" }
        ));
        out.push_str(&format!(
            "scrobble_streaming = {}\n",
            if self.scrobble_streaming { "true" } else { "false" }
        ));
        if self.allow_self_signed {
            out.push_str("allow_self_signed = true\n");
        }
        out.push_str(&format!("api_url = {}\n", self.effective_api_url()));
        out.push_str(&format!("user_token = {}\n", self.clean_token()));
        out.push_str(&format!("username = {}\n", self.username.trim()));
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_config_roundtrip() {
        let mut c = ListenBrainzConfig::default();
        c.enabled = true;
        c.now_playing = false;
        c.scrobble_streaming = false;
        c.api_url = "https://maloja.local:8443/apis/listenbrainz".to_string();
        c.user_token = "11111111-2222-3333-4444-555555555555".to_string();
        c.username = "famiclone".to_string();

        let txt = c.format_text();
        let parsed = ListenBrainzConfig::parse_text(&txt);
        assert_eq!(parsed, c);
        assert!(parsed.is_authenticated());
    }

    #[test]
    fn test_url_parsing_variants() {
        let def = ParsedLbUrl::parse("https://api.listenbrainz.org").unwrap();
        assert!(def.is_https);
        assert_eq!(def.host, "api.listenbrainz.org");
        assert_eq!(def.port, 443);
        assert_eq!(def.base_path, "");
        assert_eq!(def.endpoint_path("/1/validate-token"), "/1/validate-token");

        // Accidental `/1/` suffix stripped cleanly
        let with_v1 = ParsedLbUrl::parse("https://api.listenbrainz.org/1/").unwrap();
        assert_eq!(with_v1.base_path, "");
        assert_eq!(
            with_v1.endpoint_path("/1/submit-listens"),
            "/1/submit-listens"
        );

        // Self-hosted Maloja over HTTP with base path
        let maloja = ParsedLbUrl::parse("http://192.168.1.42:42010/apis/listenbrainz/1").unwrap();
        assert!(!maloja.is_https);
        assert_eq!(maloja.host, "192.168.1.42");
        assert_eq!(maloja.port, 42010);
        assert_eq!(
            maloja.endpoint_path("/1/submit-listens"),
            "/apis/listenbrainz/1/submit-listens"
        );
    }
}
