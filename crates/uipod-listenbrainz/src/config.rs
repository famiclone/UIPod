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

fn trim_surrounding_quotes(s: &str) -> &str {
    let bytes = s.as_bytes();
    if bytes.len() >= 2
        && ((bytes[0] == b'"' && bytes[bytes.len() - 1] == b'"')
            || (bytes[0] == b'\'' && bytes[bytes.len() - 1] == b'\''))
    {
        &s[1..s.len() - 1]
    } else {
        s
    }
}

fn strip_auth_scheme_prefix(s: &str) -> Option<&str> {
    for scheme in ["token", "bearer"] {
        if let Some(rest) = strip_prefix_ci(s, scheme) {
            if rest.is_empty() {
                return Some("");
            }
            if rest.starts_with(char::is_whitespace) {
                return Some(rest.trim_start());
            }
        }
    }
    None
}

/// Sanitize a raw ListenBrainz user token from `listenbrainz.cfg` or UI input:
/// strips `\r`/`\n`, surrounding whitespace, matching `"`/`'` quotes, and any
/// accidental case-insensitive `"Token "` or `"Bearer "` prefix.
pub fn sanitize_token(raw: &str) -> String {
    let filtered: String = raw.chars().filter(|&c| c != '\r' && c != '\n').collect();
    let mut s = filtered.as_str().trim();
    loop {
        let next_unquoted = trim_surrounding_quotes(s).trim();
        if let Some(after_scheme) = strip_auth_scheme_prefix(next_unquoted) {
            s = after_scheme.trim();
            continue;
        }
        if next_unquoted.len() != s.len() {
            s = next_unquoted;
            continue;
        }
        break;
    }
    s.to_string()
}

/// Format the centralized `Authorization` header value (`"Token <clean_token>"`).
/// Returns `None` if the sanitized token is empty.
pub fn format_auth_header_value(raw_token: &str) -> Option<String> {
    let clean = sanitize_token(raw_token);
    if clean.is_empty() {
        None
    } else {
        Some(format!("Token {clean}"))
    }
}

fn is_official_listenbrainz_host(host_lower: &str) -> bool {
    matches!(
        host_lower,
        "listenbrainz.org" | "www.listenbrainz.org" | "api.listenbrainz.org"
    )
}

fn collapse_slashes(path: &str) -> String {
    let mut out = String::with_capacity(path.len());
    let mut prev_slash = false;
    for ch in path.chars() {
        if ch == '/' {
            if !prev_slash {
                out.push('/');
                prev_slash = true;
            }
        } else {
            out.push(ch);
            prev_slash = false;
        }
    }
    out
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
        let filtered: String = raw.chars().filter(|&c| c != '\r' && c != '\n').collect();
        let unquoted = trim_surrounding_quotes(filtered.trim()).trim();
        let effective = if unquoted.is_empty() {
            DEFAULT_API_URL
        } else {
            unquoted
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

        let (authority, path_part) = match rest.find(['/', '?', '#']) {
            Some(idx) if rest.as_bytes()[idx] == b'/' => (&rest[..idx], &rest[idx..]),
            Some(idx) => (&rest[..idx], ""),
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

        let host_lower = host.to_ascii_lowercase();

        // Canonicalize official ListenBrainz domains (`listenbrainz.org`, `www.listenbrainz.org`,
        // `http://api.listenbrainz.org`, etc.) directly to `https://api.listenbrainz.org`
        // so cross-host or HTTP->HTTPS redirects never strip the `Authorization` header.
        if is_official_listenbrainz_host(&host_lower) {
            let port = match explicit_port {
                Some(p) if p != 80 && p != 443 => p,
                _ => 443,
            };
            return Ok(Self {
                is_https: true,
                host: "api.listenbrainz.org".to_string(),
                port,
                base_path: String::new(),
            });
        }

        let is_https = match scheme_opt {
            Some(v) => v,
            None => explicit_port != Some(80),
        };
        let port = explicit_port.unwrap_or(if is_https { 443 } else { 80 });

        let raw_path = path_part.split(['?', '#']).next().unwrap_or("");
        let collapsed = collapse_slashes(raw_path);
        let mut clean_path = collapsed.trim_end_matches('/');
        while clean_path == "/1" || clean_path.ends_with("/1") {
            if clean_path == "/1" {
                clean_path = "";
                break;
            } else if let Some(stripped) = clean_path.strip_suffix("/1") {
                clean_path = stripped.trim_end_matches('/');
            } else {
                break;
            }
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
            host: host_lower,
            port,
            base_path,
        })
    }

    pub fn canonical_base_url(&self) -> String {
        let scheme = if self.is_https { "https" } else { "http" };
        let default_port = (self.is_https && self.port == 443) || (!self.is_https && self.port == 80);
        if default_port {
            format!("{scheme}://{}{}", self.host, self.base_path)
        } else {
            format!("{scheme}://{}:{}{}", self.host, self.port, self.base_path)
        }
    }

    pub fn endpoint_path(&self, suffix: &str) -> String {
        let rel = suffix.trim().trim_start_matches('/');
        let rel = rel.strip_prefix("1/").unwrap_or(rel).trim_start_matches('/');
        if self.base_path.is_empty() {
            format!("/1/{rel}")
        } else {
            format!("{}/1/{rel}", self.base_path)
        }
    }
}

/// Normalize a raw `server_url` / `api_url` into canonical base URL form:
/// - Empty / whitespace -> `"https://api.listenbrainz.org"`
/// - `http(s)://(www.)listenbrainz.org[/...]` or `http://api.listenbrainz.org[/1/]`
///   -> `"https://api.listenbrainz.org"`
/// - Strips trailing slashes and optional trailing `/1` on custom self-hosted servers.
pub fn normalize_base_url(raw: &str) -> String {
    match ParsedLbUrl::parse(raw) {
        Ok(parsed) => parsed.canonical_base_url(),
        Err(_) => trim_surrounding_quotes(raw.trim()).trim().to_string(),
    }
}

impl ListenBrainzConfig {
    pub fn effective_api_url(&self) -> String {
        normalize_base_url(&self.api_url)
    }

    pub fn has_token(&self) -> bool {
        !self.clean_token().is_empty()
    }

    pub fn clean_token(&self) -> String {
        sanitize_token(&self.user_token)
    }

    pub fn is_authenticated(&self) -> bool {
        self.has_token() && ParsedLbUrl::parse(&self.effective_api_url()).is_ok()
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
            let v = trim_surrounding_quotes(v_raw.trim()).trim();
            match k.as_str() {
                "enabled" => cfg.enabled = parse_bool(v, true),
                "now_playing" | "nowplaying" => cfg.now_playing = parse_bool(v, true),
                "scrobble_streaming" | "streaming" => cfg.scrobble_streaming = parse_bool(v, true),
                "allow_self_signed" | "insecure_tls" => {
                    cfg.allow_self_signed = parse_bool(v, false);
                }
                "api_url" | "url" | "server_url" => {
                    cfg.api_url = normalize_base_url(v);
                }
                "user_token" | "token" => {
                    cfg.user_token = sanitize_token(v);
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

    #[test]
    fn test_token_sanitization_and_auth_header_helper() {
        let cases = [
            "  abc\r\n",
            "\"abc\"",
            "'abc'",
            "Token abc",
            "token   abc",
            "Bearer abc",
            "BEARER\tabc\r\n",
            "\"Token abc\"",
            "'Bearer abc'",
            "Token \"abc\"\r\n",
        ];
        for raw in cases {
            assert_eq!(sanitize_token(raw), "abc", "failed for input: {raw:?}");
            assert_eq!(
                format_auth_header_value(raw).as_deref(),
                Some("Token abc"),
                "header failed for input: {raw:?}"
            );
        }

        assert_eq!(sanitize_token(""), "");
        assert_eq!(sanitize_token("   \r\n "), "");
        assert_eq!(sanitize_token("\"\" "), "");
        assert_eq!(sanitize_token("Token "), "");
        assert_eq!(sanitize_token("Bearer"), "");
        assert_eq!(format_auth_header_value("   "), None);

        // Also verify `listenbrainz.cfg` parsing with single/double quotes and prefixes
        let cfg_single = ListenBrainzConfig::parse_text("user_token = 'Bearer my-uuid-99'\r\n");
        assert_eq!(cfg_single.clean_token(), "my-uuid-99");
        let cfg_double = ListenBrainzConfig::parse_text("token = \"Token my-uuid-99\"\n");
        assert_eq!(cfg_double.clean_token(), "my-uuid-99");
    }

    #[test]
    fn test_canonical_url_normalization() {
        // Default & official ListenBrainz variants rewrite to `https://api.listenbrainz.org`
        // so cross-host or HTTP->HTTPS redirects never strip `Authorization`.
        for raw in [
            "",
            "   ",
            "https://listenbrainz.org",
            "https://listenbrainz.org/",
            "http://listenbrainz.org",
            "http://listenbrainz.org/1/",
            "https://www.listenbrainz.org/",
            "https://listenbrainz.org/settings/",
            "http://api.listenbrainz.org",
            "https://api.listenbrainz.org/",
            "https://api.listenbrainz.org/1",
            "https://api.listenbrainz.org/1/",
            "'https://listenbrainz.org/'",
            "\"https://api.listenbrainz.org/1/\"",
        ] {
            assert_eq!(
                normalize_base_url(raw),
                "https://api.listenbrainz.org",
                "failed to canonicalize official URL: {raw:?}"
            );
            let parsed = ParsedLbUrl::parse(raw).unwrap();
            assert!(parsed.is_https);
            assert_eq!(parsed.host, "api.listenbrainz.org");
            assert_eq!(parsed.port, 443);
            assert_eq!(parsed.base_path, "");
            assert_eq!(parsed.endpoint_path("/1/validate-token"), "/1/validate-token");
            assert_eq!(parsed.endpoint_path("/1/submit-listens"), "/1/submit-listens");
        }

        // Custom self-hosted servers (Maloja / Koito) with trailing `/` or `/1/`
        for raw in [
            "https://my-maloja.local",
            "https://my-maloja.local/",
            "https://my-maloja.local/1",
            "https://my-maloja.local/1/",
        ] {
            assert_eq!(normalize_base_url(raw), "https://my-maloja.local");
            let parsed = ParsedLbUrl::parse(raw).unwrap();
            assert_eq!(parsed.endpoint_path("/1/validate-token"), "/1/validate-token");
            assert_eq!(parsed.endpoint_path("/1/submit-listens"), "/1/submit-listens");
        }

        for raw in [
            "https://my-maloja.local/apis/listenbrainz",
            "https://my-maloja.local/apis/listenbrainz/",
            "https://my-maloja.local/apis/listenbrainz/1",
            "https://my-maloja.local/apis/listenbrainz/1/",
        ] {
            assert_eq!(
                normalize_base_url(raw),
                "https://my-maloja.local/apis/listenbrainz"
            );
            let parsed = ParsedLbUrl::parse(raw).unwrap();
            assert_eq!(
                parsed.endpoint_path("/1/submit-listens"),
                "/apis/listenbrainz/1/submit-listens"
            );
        }
    }
}
