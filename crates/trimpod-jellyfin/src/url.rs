//! Jellyfin server URL parser, MediaBrowser auth header builder, REST endpoint builders,
//! and hardware-aware video Direct Stream vs H.264/AAC transcode stream planner.

use crate::config::{AudioQuality, VideoQuality};

pub const JELLYFIN_CLIENT_NAME: &str = "UIPod";
pub const JELLYFIN_DEVICE_NAME: &str = "TrimUI Brick";
pub const JELLYFIN_DEVICE_ID: &str = "uipod-tg5040";
pub const JELLYFIN_CLIENT_VERSION: &str = "1.0.0";

pub fn percent_encode_query(s: &str) -> String {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    let mut out = String::with_capacity(s.len());
    for &b in s.as_bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') {
            out.push(b as char);
        } else {
            out.push('%');
            out.push(HEX[(b >> 4) as usize] as char);
            out.push(HEX[(b & 0x0f) as usize] as char);
        }
    }
    out
}

pub fn json_escape_str(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 4);
    for ch in s.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => {
                let code = c as u32;
                out.push_str(&format!("\\u{code:04x}"));
            }
            c => out.push(c),
        }
    }
    out
}

fn strip_prefix_ci<'a>(s: &'a str, prefix: &str) -> Option<&'a str> {
    if s.len() >= prefix.len() && s[..prefix.len()].eq_ignore_ascii_case(prefix) {
        Some(&s[prefix.len()..])
    } else {
        None
    }
}

fn strip_suffix_ci<'a>(s: &'a str, suffix: &str) -> Option<&'a str> {
    if s.len() >= suffix.len() && s[s.len() - suffix.len()..].eq_ignore_ascii_case(suffix) {
        Some(&s[..s.len() - suffix.len()])
    } else {
        None
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedJellyfinUrl {
    pub is_https: bool,
    pub host: String,
    pub port: u16,
    /// Leading slash if non-empty, no trailing slash (e.g. `""` or `"/jellyfin"`).
    pub base_path: String,
}

impl ParsedJellyfinUrl {
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

        // Strip query/fragment and web UI suffixes (`/web/index.html`, `/web`)
        let mut clean_path = path_part
            .split(['?', '#'])
            .next()
            .unwrap_or("")
            .trim_end_matches('/');
        if let Some(p) = strip_suffix_ci(clean_path, "/web/index.html") {
            clean_path = p.trim_end_matches('/');
        } else if let Some(p) = strip_suffix_ci(clean_path, "/web") {
            clean_path = p.trim_end_matches('/');
        }

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
            None => explicit_port == Some(443) || explicit_port == Some(8920),
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

    pub fn auth_headers(token: Option<&str>, json_body: bool) -> String {
        let mut h = String::with_capacity(256);
        h.push_str("Authorization: MediaBrowser ");
        h.push_str(&format!(
            "Client=\"{JELLYFIN_CLIENT_NAME}\", Device=\"{JELLYFIN_DEVICE_NAME}\", DeviceId=\"{JELLYFIN_DEVICE_ID}\", Version=\"{JELLYFIN_CLIENT_VERSION}\""
        ));
        if let Some(tok) = token {
            if !tok.is_empty() {
                let clean_tok: String = tok.chars().filter(|&c| c != '"' && c != '\r' && c != '\n').collect();
                h.push_str(&format!(", Token=\"{clean_tok}\""));
            }
        }
        h.push_str("\r\n");
        if let Some(tok) = token {
            if !tok.is_empty() {
                let clean_tok: String = tok.chars().filter(|&c| c != '\r' && c != '\n').collect();
                h.push_str(&format!("X-Emby-Token: {clean_tok}\r\n"));
            }
        }
        h.push_str("Accept: application/json\r\n");
        if json_body {
            h.push_str("Content-Type: application/json\r\n");
        }
        h
    }

    pub fn stream_headers(token: Option<&str>) -> String {
        let mut h = String::with_capacity(200);
        h.push_str("Authorization: MediaBrowser ");
        h.push_str(&format!(
            "Client=\"{JELLYFIN_CLIENT_NAME}\", Device=\"{JELLYFIN_DEVICE_NAME}\", DeviceId=\"{JELLYFIN_DEVICE_ID}\", Version=\"{JELLYFIN_CLIENT_VERSION}\""
        ));
        if let Some(tok) = token {
            if !tok.is_empty() {
                let clean_tok: String = tok.chars().filter(|&c| c != '"' && c != '\r' && c != '\n').collect();
                h.push_str(&format!(", Token=\"{clean_tok}\""));
            }
        }
        h.push_str("\r\n");
        if let Some(tok) = token {
            if !tok.is_empty() {
                let clean_tok: String = tok.chars().filter(|&c| c != '\r' && c != '\n').collect();
                h.push_str(&format!("X-Emby-Token: {clean_tok}\r\n"));
            }
        }
        h
    }

    pub fn auth_by_name_path(&self) -> String {
        format!("{}/Users/AuthenticateByName", self.base_path)
    }

    pub fn users_list_path(&self) -> String {
        format!("{}/Users", self.base_path)
    }

    pub fn user_views_path(&self, user_id: &str) -> String {
        format!(
            "{}/Users/{}/Views",
            self.base_path,
            percent_encode_query(user_id)
        )
    }

    pub fn video_items_path(
        &self,
        user_id: &str,
        parent_id: &str,
        start_index: usize,
        limit: usize,
    ) -> String {
        format!(
            "{}/Users/{}/Items?ParentId={}&Recursive=true&IncludeItemTypes=Movie,Series,Season,Episode,Video,MusicVideo&Fields=MediaSources,Chapters,Path&SortBy=SortName&SortOrder=Ascending&StartIndex={start_index}&Limit={limit}",
            self.base_path,
            percent_encode_query(user_id),
            percent_encode_query(parent_id),
        )
    }

    pub fn audio_items_path(
        &self,
        user_id: &str,
        start_index: usize,
        limit: usize,
    ) -> String {
        format!(
            "{}/Users/{}/Items?Recursive=true&IncludeItemTypes=Audio&Fields=MediaSources,Path&SortBy=SortName&SortOrder=Ascending&StartIndex={start_index}&Limit={limit}",
            self.base_path,
            percent_encode_query(user_id),
        )
    }

    pub fn playlists_path(&self, user_id: &str) -> String {
        format!(
            "{}/Users/{}/Items?Recursive=true&IncludeItemTypes=Playlist&StartIndex=0&Limit=200",
            self.base_path,
            percent_encode_query(user_id),
        )
    }

    pub fn playlist_items_path(&self, playlist_id: &str, user_id: &str) -> String {
        format!(
            "{}/Playlists/{}/Items?UserId={}&Fields=MediaSources,Path&Limit=1000",
            self.base_path,
            percent_encode_query(playlist_id),
            percent_encode_query(user_id),
        )
    }

    pub fn item_image_path(&self, item_id: &str, max_dim: u32, token: &str) -> String {
        let id = percent_encode_query(item_id);
        let tok = percent_encode_query(token);
        format!(
            "{}/Items/{id}/Images/Primary?maxWidth={max_dim}&maxHeight={max_dim}&quality=82&api_key={tok}",
            self.base_path
        )
    }

    pub fn audio_stream_path(
        &self,
        song_id: &str,
        original_suffix: &str,
        quality: AudioQuality,
        token: &str,
    ) -> String {
        let id = percent_encode_query(song_id);
        let tok = percent_encode_query(token);
        match quality {
            AudioQuality::Original => {
                let ext = quality.file_ext(original_suffix);
                format!(
                    "{}/Audio/{id}/stream.{ext}?Static=true&api_key={tok}",
                    self.base_path
                )
            }
            AudioQuality::Mp3_320 => format!(
                "{}/Audio/{id}/stream.mp3?AudioCodec=mp3&AudioBitRate=320000&MaxAudioChannels=2&api_key={tok}",
                self.base_path
            ),
            AudioQuality::Mp3_192 => format!(
                "{}/Audio/{id}/stream.mp3?AudioCodec=mp3&AudioBitRate=192000&MaxAudioChannels=2&api_key={tok}",
                self.base_path
            ),
        }
    }

    pub fn active_encodings_delete_path(&self, play_session_id: &str) -> String {
        let dev = percent_encode_query(JELLYFIN_DEVICE_ID);
        if play_session_id.is_empty() {
            format!("{}/Videos/ActiveEncodings?DeviceId={dev}", self.base_path)
        } else {
            let sid = percent_encode_query(play_session_id);
            format!(
                "{}/Videos/ActiveEncodings?DeviceId={dev}&PlaySessionId={sid}",
                self.base_path
            )
        }
    }

    pub fn session_playing_path(&self) -> String {
        format!("{}/Sessions/Playing", self.base_path)
    }

    pub fn session_progress_path(&self) -> String {
        format!("{}/Sessions/Playing/Progress", self.base_path)
    }

    pub fn session_stopped_path(&self) -> String {
        format!("{}/Sessions/Playing/Stopped", self.base_path)
    }
}

#[derive(Debug, Clone)]
pub struct VideoPlanInput<'a> {
    pub item_id: &'a str,
    pub media_source_id: &'a str,
    pub container: &'a str,
    pub video_codec: &'a str,
    pub audio_codec: &'a str,
    pub width: u32,
    pub height: u32,
    pub bit_depth: u32,
    pub bitrate: u32,
    pub quality: VideoQuality,
    pub start_ms: u32,
    pub play_session_id: &'a str,
    pub token: &'a str,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlannedVideoStream {
    pub path_and_query: String,
    pub is_transcoded: bool,
    pub download_ext: String,
}

pub fn is_hw_video_codec(codec: &str, bit_depth: u32) -> bool {
    if bit_depth > 8 {
        return false;
    }
    let c = codec.trim().to_ascii_lowercase();
    matches!(
        c.as_str(),
        "h264"
            | "avc"
            | "avc1"
            | "mpeg4"
            | "xvid"
            | "divx"
            | "msmpeg4"
            | "msmpeg4v2"
            | "msmpeg4v3"
            | "mpeg2video"
            | "mpeg1video"
            | "mjpeg"
    )
}

pub fn is_hw_direct_container(container: &str) -> bool {
    let c = container.trim().to_ascii_lowercase();
    matches!(
        c.as_str(),
        "mp4" | "m4v" | "mkv" | "avi" | "mov" | "mpg" | "mpeg" | "ts"
    )
}

pub fn is_hw_audio_codec(codec: &str) -> bool {
    let c = codec.trim().to_ascii_lowercase();
    c.is_empty()
        || matches!(
            c.as_str(),
            "aac"
                | "mp3"
                | "mp2"
                | "ac3"
                | "flac"
                | "vorbis"
                | "pcm_s16le"
                | "pcm_s24le"
                | "pcm_u8"
                | "wmav2"
        )
}

pub fn plan_video_stream(
    server: &ParsedJellyfinUrl,
    inp: &VideoPlanInput<'_>,
) -> PlannedVideoStream {
    let id = percent_encode_query(inp.item_id);
    let msid_raw = if inp.media_source_id.trim().is_empty() {
        inp.item_id
    } else {
        inp.media_source_id
    };
    let msid = percent_encode_query(msid_raw);
    let tok = percent_encode_query(inp.token);

    let vcodec_ok = is_hw_video_codec(inp.video_codec, inp.bit_depth);
    let container_ok = is_hw_direct_container(inp.container);
    let acodec_ok = is_hw_audio_codec(inp.audio_codec);
    let res_ok = inp.height == 0 || inp.height <= inp.quality.max_direct_height();
    let br_ok = inp.bitrate == 0 || inp.bitrate <= inp.quality.max_direct_bitrate();

    if vcodec_ok && container_ok && acodec_ok && res_ok && br_ok {
        let ext = inp.container.trim().trim_start_matches('.').to_ascii_lowercase();
        let clean_ext = if ext.is_empty() { "mp4".to_string() } else { ext };
        let pq = format!(
            "{}/Videos/{id}/stream.{clean_ext}?Static=true&mediaSourceId={msid}&api_key={tok}",
            server.base_path
        );
        return PlannedVideoStream {
            path_and_query: pq,
            is_transcoded: false,
            download_ext: clean_ext,
        };
    }

    let (max_w, max_h, vbr, abr) = inp.quality.transcode_params();
    // Allow zero-CPU H.264 stream copy when only container or audio codec needed remux/transcode!
    let allow_vcopy = vcodec_ok && res_ok && br_ok && inp.start_ms == 0;
    let sid = percent_encode_query(if inp.play_session_id.is_empty() {
        "uipod-session"
    } else {
        inp.play_session_id
    });
    let dev = percent_encode_query(JELLYFIN_DEVICE_ID);
    let ticks: u64 = (inp.start_ms as u64).saturating_mul(10_000);

    let pq = format!(
        "{}/Videos/{id}/stream.ts?VideoCodec=h264&AudioCodec=aac&MaxWidth={max_w}&MaxHeight={max_h}&VideoBitRate={vbr}&AudioBitRate={abr}&MaxAudioChannels=2&TranscodingMaxAudioChannels=2&Profile=high&Level=41&RequireAvc=true&AllowVideoStreamCopy={allow_vcopy}&MediaSourceId={msid}&PlaySessionId={sid}&DeviceId={dev}&StartTimeTicks={ticks}&api_key={tok}",
        server.base_path
    );
    PlannedVideoStream {
        path_and_query: pq,
        is_transcoded: true,
        download_ext: "ts".to_string(),
    }
}
