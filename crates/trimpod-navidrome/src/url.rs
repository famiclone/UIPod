//! Server URL parser and Subsonic / Navidrome REST API endpoint builder.

use crate::config::StreamQuality;
use crate::md5::{generate_salt, percent_encode_query, subsonic_token};

pub const SUBSONIC_API_VERSION: &str = "1.16.1";
pub const SUBSONIC_CLIENT_NAME: &str = "UIPod";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedServerUrl {
    pub is_https: bool,
    pub host: String,
    pub port: u16,
    /// Leading slash if non-empty, no trailing slash (e.g. `""` or `"/navidrome"`).
    pub base_path: String,
}

impl ParsedServerUrl {
    pub fn parse(raw: &str) -> Result<Self, String> {
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            return Err("Empty server URL".to_string());
        }

        let (scheme_opt, rest) = if let Some(r) = strip_prefix_ci(trimmed, "https://") {
            (Some(true), r)
        } else if let Some(r) = strip_prefix_ci(trimmed, "http://") {
            (Some(false), r)
        } else if trimmed.contains("://") {
            return Err("Unsupported URL scheme".to_string());
        } else {
            (None, trimmed)
        };

        let (authority, path_part) = match rest.find('/') {
            Some(idx) => (&rest[..idx], &rest[idx..]),
            None => (rest, ""),
        };

        // Strip optional query/fragment from base URL if user pasted a web UI link
        let clean_path = path_part
            .split(['?', '#'])
            .next()
            .unwrap_or("")
            .trim_end_matches('/');
        // Also strip trailing "/app" if user pasted Navidrome's web UI URL (`http://host:4533/app/#/login`)
        let clean_path = clean_path.strip_suffix("/app").unwrap_or(clean_path);

        if authority.is_empty() {
            return Err("Missing server host".to_string());
        }

        // Reject embedded userinfo (`user:pass@host`) cleanly or strip it
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
            None => explicit_port == Some(443),
        };
        let port = explicit_port.unwrap_or(if is_https { 443 } else { 80 });

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

    fn auth_query(&self, username: &str, password: &str, json: bool) -> String {
        let salt = generate_salt();
        self.auth_query_with_salt(username, password, &salt, json)
    }

    pub fn auth_query_with_salt(
        &self,
        username: &str,
        password: &str,
        salt: &str,
        json: bool,
    ) -> String {
        let token = subsonic_token(password, salt);
        let u = percent_encode_query(username);
        let s = percent_encode_query(salt);
        if json {
            format!("u={u}&t={token}&s={s}&v={SUBSONIC_API_VERSION}&c={SUBSONIC_CLIENT_NAME}&f=json")
        } else {
            format!("u={u}&t={token}&s={s}&v={SUBSONIC_API_VERSION}&c={SUBSONIC_CLIENT_NAME}")
        }
    }

    pub fn ping_path(&self, username: &str, password: &str) -> String {
        let auth = self.auth_query(username, password, true);
        format!("{}/rest/ping.view?{auth}", self.base_path)
    }

    pub fn search3_songs_path(
        &self,
        username: &str,
        password: &str,
        count: usize,
        offset: usize,
    ) -> String {
        let auth = self.auth_query(username, password, true);
        format!(
            "{}/rest/search3.view?query=&artistCount=0&albumCount=0&songCount={count}&songOffset={offset}&{auth}",
            self.base_path
        )
    }

    pub fn get_playlists_path(&self, username: &str, password: &str) -> String {
        let auth = self.auth_query(username, password, true);
        format!("{}/rest/getPlaylists.view?{auth}", self.base_path)
    }

    pub fn get_playlist_path(&self, username: &str, password: &str, playlist_id: &str) -> String {
        let auth = self.auth_query(username, password, true);
        let id = percent_encode_query(playlist_id);
        format!("{}/rest/getPlaylist.view?id={id}&{auth}", self.base_path)
    }

    pub fn stream_path(
        &self,
        username: &str,
        password: &str,
        song_id: &str,
        quality: StreamQuality,
    ) -> String {
        let auth = self.auth_query(username, password, false);
        let id = percent_encode_query(song_id);
        match quality {
            StreamQuality::Original => format!(
                "{}/rest/stream.view?id={id}&maxBitRate=0&format=raw&{auth}",
                self.base_path
            ),
            StreamQuality::Mp3_320 => format!(
                "{}/rest/stream.view?id={id}&maxBitRate=320&format=mp3&estimateContentLength=true&{auth}",
                self.base_path
            ),
            StreamQuality::Mp3_192 => format!(
                "{}/rest/stream.view?id={id}&maxBitRate=192&format=mp3&estimateContentLength=true&{auth}",
                self.base_path
            ),
        }
    }

    pub fn cover_art_path(
        &self,
        username: &str,
        password: &str,
        cover_id: &str,
        size: u32,
    ) -> String {
        let auth = self.auth_query(username, password, false);
        let id = percent_encode_query(cover_id);
        format!(
            "{}/rest/getCoverArt.view?id={id}&size={size}&{auth}",
            self.base_path
        )
    }

    pub fn scrobble_path(
        &self,
        username: &str,
        password: &str,
        song_id: &str,
        submission: bool,
    ) -> String {
        let auth = self.auth_query(username, password, true);
        let id = percent_encode_query(song_id);
        let sub = if submission { "true" } else { "false" };
        format!(
            "{}/rest/scrobble.view?id={id}&submission={sub}&{auth}",
            self.base_path
        )
    }
}

fn strip_prefix_ci<'a>(s: &'a str, prefix: &str) -> Option<&'a str> {
    if s.len() >= prefix.len() && s[..prefix.len()].eq_ignore_ascii_case(prefix) {
        Some(&s[prefix.len()..])
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_lan_tailscale_and_https_urls() {
        let u1 = ParsedServerUrl::parse("192.168.1.50:4533").unwrap();
        assert!(!u1.is_https);
        assert_eq!(u1.host, "192.168.1.50");
        assert_eq!(u1.port, 4533);
        assert_eq!(u1.base_path, "");

        let u2 = ParsedServerUrl::parse("https://Music.Example.COM/navidrome/app/#/login").unwrap();
        assert!(u2.is_https);
        assert_eq!(u2.host, "music.example.com");
        assert_eq!(u2.port, 443);
        assert_eq!(u2.base_path, "/navidrome");

        let u3 = ParsedServerUrl::parse("http://100.84.12.9:4533/").unwrap();
        assert!(!u3.is_https);
        assert_eq!(u3.host, "100.84.12.9");
        assert_eq!(u3.port, 4533);
        assert_eq!(u3.base_path, "");

        let stream = u2.stream_path("alice", "pw", "song-123", StreamQuality::Mp3_320);
        assert!(stream.starts_with(
            "/navidrome/rest/stream.view?id=song-123&maxBitRate=320&format=mp3&estimateContentLength=true&u=alice&"
        ));
    }
}
