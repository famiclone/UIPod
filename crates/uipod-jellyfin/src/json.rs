//! Complete RFC 8259 UTF-8 JSON parser and Jellyfin REST API response decoders.

use std::collections::BTreeMap;

#[derive(Debug, Clone, PartialEq)]
pub enum JsonVal {
    Null,
    Bool(bool),
    Number(f64),
    Int(i64),
    Str(String),
    Array(Vec<JsonVal>),
    Object(BTreeMap<String, JsonVal>),
}

impl JsonVal {
    pub fn as_obj(&self) -> Option<&BTreeMap<String, JsonVal>> {
        match self {
            JsonVal::Object(m) => Some(m),
            _ => None,
        }
    }

    pub fn as_array(&self) -> Option<&[JsonVal]> {
        match self {
            JsonVal::Array(a) => Some(a),
            _ => None,
        }
    }

    pub fn as_str(&self) -> Option<&str> {
        match self {
            JsonVal::Str(s) => Some(s),
            _ => None,
        }
    }

    pub fn as_bool(&self) -> Option<bool> {
        match self {
            JsonVal::Bool(b) => Some(*b),
            _ => None,
        }
    }

    pub fn as_i64(&self) -> Option<i64> {
        match self {
            JsonVal::Int(n) => Some(*n),
            JsonVal::Number(f) => Some(*f as i64),
            JsonVal::Str(s) => s.parse::<i64>().ok(),
            _ => None,
        }
    }

    pub fn as_u64(&self) -> Option<u64> {
        self.as_i64().map(|n| if n < 0 { 0 } else { n as u64 })
    }
}

struct Parser<'a> {
    bytes: &'a [u8],
    pos: usize,
}

impl<'a> Parser<'a> {
    fn new(input: &'a [u8]) -> Self {
        let mut pos = 0;
        if input.len() >= 3 && input[0..3] == [0xef, 0xbb, 0xbf] {
            pos = 3;
        }
        Self { bytes: input, pos }
    }

    fn skip_ws(&mut self) {
        while self.pos < self.bytes.len() {
            match self.bytes[self.pos] {
                b' ' | b'\t' | b'\r' | b'\n' => self.pos += 1,
                _ => break,
            }
        }
    }

    fn peek(&self) -> Option<u8> {
        self.bytes.get(self.pos).copied()
    }

    fn parse_value(&mut self) -> Result<JsonVal, String> {
        self.skip_ws();
        let Some(b) = self.peek() else {
            return Err("Unexpected EOF in JSON".to_string());
        };
        match b {
            b'{' => self.parse_object(),
            b'[' => self.parse_array(),
            b'"' => self.parse_string().map(JsonVal::Str),
            b't' => self.expect_ident(b"true", JsonVal::Bool(true)),
            b'f' => self.expect_ident(b"false", JsonVal::Bool(false)),
            b'n' => self.expect_ident(b"null", JsonVal::Null),
            b'-' | b'0'..=b'9' => self.parse_number(),
            _ => Err(format!("Unexpected byte 0x{b:02x} at pos {}", self.pos)),
        }
    }

    fn expect_ident(&mut self, kw: &[u8], val: JsonVal) -> Result<JsonVal, String> {
        if self.bytes.get(self.pos..self.pos + kw.len()) == Some(kw) {
            self.pos += kw.len();
            Ok(val)
        } else {
            Err(format!("Invalid JSON keyword at pos {}", self.pos))
        }
    }

    fn parse_object(&mut self) -> Result<JsonVal, String> {
        self.pos += 1;
        let mut map = BTreeMap::new();
        self.skip_ws();
        if self.peek() == Some(b'}') {
            self.pos += 1;
            return Ok(JsonVal::Object(map));
        }
        loop {
            self.skip_ws();
            if self.peek() != Some(b'"') {
                return Err(format!("Expected string key at pos {}", self.pos));
            }
            let key = self.parse_string()?;
            self.skip_ws();
            if self.peek() != Some(b':') {
                return Err(format!("Expected ':' at pos {}", self.pos));
            }
            self.pos += 1;
            let val = self.parse_value()?;
            map.insert(key, val);
            self.skip_ws();
            match self.peek() {
                Some(b',') => self.pos += 1,
                Some(b'}') => {
                    self.pos += 1;
                    break;
                }
                _ => return Err(format!("Expected ',' or '}}' at pos {}", self.pos)),
            }
        }
        Ok(JsonVal::Object(map))
    }

    fn parse_array(&mut self) -> Result<JsonVal, String> {
        self.pos += 1;
        let mut arr = Vec::new();
        self.skip_ws();
        if self.peek() == Some(b']') {
            self.pos += 1;
            return Ok(JsonVal::Array(arr));
        }
        loop {
            let val = self.parse_value()?;
            arr.push(val);
            self.skip_ws();
            match self.peek() {
                Some(b',') => self.pos += 1,
                Some(b']') => {
                    self.pos += 1;
                    break;
                }
                _ => return Err(format!("Expected ',' or ']' at pos {}", self.pos)),
            }
        }
        Ok(JsonVal::Array(arr))
    }

    fn parse_hex4(&mut self) -> Result<u16, String> {
        if self.pos + 4 > self.bytes.len() {
            return Err("Truncated \\u escape".to_string());
        }
        let s = std::str::from_utf8(&self.bytes[self.pos..self.pos + 4])
            .map_err(|_| "Invalid hex in \\u escape".to_string())?;
        self.pos += 4;
        u16::from_str_radix(s, 16).map_err(|_| "Invalid hex digits in \\u escape".to_string())
    }

    fn parse_string(&mut self) -> Result<String, String> {
        self.pos += 1;
        let mut out = Vec::new();
        while self.pos < self.bytes.len() {
            let b = self.bytes[self.pos];
            self.pos += 1;
            match b {
                b'"' => return Ok(String::from_utf8_lossy(&out).into_owned()),
                b'\\' => {
                    let Some(esc) = self.peek() else {
                        return Err("Truncated string escape".to_string());
                    };
                    self.pos += 1;
                    match esc {
                        b'"' => out.push(b'"'),
                        b'\\' => out.push(b'\\'),
                        b'/' => out.push(b'/'),
                        b'b' => out.push(0x08),
                        b'f' => out.push(0x0c),
                        b'n' => out.push(b'\n'),
                        b'r' => out.push(b'\r'),
                        b't' => out.push(b'\t'),
                        b'u' => {
                            let u1 = self.parse_hex4()?;
                            let codepoint = if (0xD800..=0xDBFF).contains(&u1) {
                                if self.bytes.get(self.pos..self.pos + 2) == Some(b"\\u") {
                                    self.pos += 2;
                                    let u2 = self.parse_hex4()?;
                                    if (0xDC00..=0xDFFF).contains(&u2) {
                                        0x10000
                                            + (((u1 as u32 - 0xD800) << 10)
                                                | (u2 as u32 - 0xDC00))
                                    } else {
                                        0xFFFD
                                    }
                                } else {
                                    0xFFFD
                                }
                            } else {
                                u1 as u32
                            };
                            let ch = char::from_u32(codepoint).unwrap_or('\u{FFFD}');
                            let mut utf8 = [0u8; 4];
                            out.extend_from_slice(ch.encode_utf8(&mut utf8).as_bytes());
                        }
                        _ => out.push(esc),
                    }
                }
                _ => out.push(b),
            }
        }
        Err("Unterminated JSON string".to_string())
    }

    fn parse_number(&mut self) -> Result<JsonVal, String> {
        let start = self.pos;
        let mut is_float = false;
        if self.peek() == Some(b'-') {
            self.pos += 1;
        }
        while let Some(b) = self.peek() {
            match b {
                b'0'..=b'9' => self.pos += 1,
                b'.' | b'e' | b'E' | b'+' | b'-' => {
                    is_float = true;
                    self.pos += 1;
                }
                _ => break,
            }
        }
        let s = std::str::from_utf8(&self.bytes[start..self.pos])
            .map_err(|_| "Invalid number".to_string())?;
        if !is_float {
            if let Ok(i) = s.parse::<i64>() {
                return Ok(JsonVal::Int(i));
            }
        }
        let f = s
            .parse::<f64>()
            .map_err(|_| format!("Invalid JSON number: {s}"))?;
        Ok(JsonVal::Number(f))
    }
}

pub fn parse_json(bytes: &[u8]) -> Result<JsonVal, String> {
    let mut p = Parser::new(bytes);
    p.parse_value()
}

fn get_prop<'a>(obj: &'a BTreeMap<String, JsonVal>, pascal: &str, camel: &str) -> Option<&'a JsonVal> {
    obj.get(pascal).or_else(|| obj.get(camel))
}

fn get_str(obj: &BTreeMap<String, JsonVal>, pascal: &str, camel: &str) -> String {
    get_prop(obj, pascal, camel)
        .and_then(JsonVal::as_str)
        .unwrap_or("")
        .to_string()
}

fn get_i32(obj: &BTreeMap<String, JsonVal>, pascal: &str, camel: &str) -> i32 {
    get_prop(obj, pascal, camel)
        .and_then(JsonVal::as_i64)
        .unwrap_or(0) as i32
}

fn get_u32(obj: &BTreeMap<String, JsonVal>, pascal: &str, camel: &str) -> u32 {
    get_prop(obj, pascal, camel)
        .and_then(JsonVal::as_u64)
        .unwrap_or(0)
        .min(u32::MAX as u64) as u32
}

fn get_u64(obj: &BTreeMap<String, JsonVal>, pascal: &str, camel: &str) -> u64 {
    get_prop(obj, pascal, camel)
        .and_then(JsonVal::as_u64)
        .unwrap_or(0)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthResult {
    pub access_token: String,
    pub user_id: String,
    pub user_name: String,
}

pub fn decode_auth_response(bytes: &[u8]) -> Result<AuthResult, String> {
    let root = parse_json(bytes)?;
    let obj = root
        .as_obj()
        .ok_or_else(|| "Invalid Jellyfin auth JSON".to_string())?;
    let token = get_str(obj, "AccessToken", "accessToken");
    let user_obj = get_prop(obj, "User", "user")
        .and_then(JsonVal::as_obj)
        .ok_or_else(|| "Missing User in auth response".to_string())?;
    let user_id = get_str(user_obj, "Id", "id");
    let user_name = get_str(user_obj, "Name", "name");
    if token.is_empty() || user_id.is_empty() {
        return Err("Missing AccessToken or User.Id".to_string());
    }
    Ok(AuthResult {
        access_token: token,
        user_id,
        user_name,
    })
}

pub fn decode_first_user_id(bytes: &[u8]) -> Result<String, String> {
    let root = parse_json(bytes)?;
    let arr = root
        .as_array()
        .ok_or_else(|| "Expected user list array".to_string())?;
    for item in arr {
        if let Some(obj) = item.as_obj() {
            let id = get_str(obj, "Id", "id");
            if !id.is_empty() {
                return Ok(id);
            }
        }
    }
    Err("No Jellyfin users found".to_string())
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JellyfinView {
    pub id: String,
    pub name: String,
    /// Lowercase collection type: `"movies"`, `"tvshows"`, `"music"`, `"homevideos"`, `"musicvideos"`, `""`, etc.
    pub collection_type: String,
}

impl JellyfinView {
    /// `1` = Video library, `2` = Music library, `0` = Mixed / Folder library.
    pub fn view_kind(&self) -> u8 {
        match self.collection_type.as_str() {
            "music" | "audiobooks" => 2,
            "movies" | "tvshows" | "homevideos" | "musicvideos" | "photos" => 1,
            _ => 0,
        }
    }

    pub fn should_skip_top_level(&self) -> bool {
        matches!(
            self.collection_type.as_str(),
            "playlists" | "boxsets" | "livetv" | "books" | "photos"
        )
    }
}

pub fn decode_views(bytes: &[u8]) -> Result<Vec<JellyfinView>, String> {
    let root = parse_json(bytes)?;
    let items = if let Some(obj) = root.as_obj() {
        get_prop(obj, "Items", "items")
            .and_then(JsonVal::as_array)
            .ok_or_else(|| "Missing Items in Views response".to_string())?
    } else if let Some(arr) = root.as_array() {
        arr
    } else {
        return Err("Invalid Views JSON".to_string());
    };

    let mut out = Vec::with_capacity(items.len());
    for item in items {
        let Some(obj) = item.as_obj() else {
            continue;
        };
        let id = get_str(obj, "Id", "id");
        let name = get_str(obj, "Name", "name");
        if id.is_empty() || name.is_empty() {
            continue;
        }
        let ctype = get_str(obj, "CollectionType", "collectionType").to_ascii_lowercase();
        let view = JellyfinView {
            id,
            name,
            collection_type: ctype,
        };
        if !view.should_skip_top_level() {
            out.push(view);
        }
    }
    Ok(out)
}

/// Item type constants shared with C (`TpJfVideoItemC.item_type` & `jf_videos.item_type`).
pub const JF_ITEM_MOVIE: u8 = 1;
pub const JF_ITEM_SERIES: u8 = 2;
pub const JF_ITEM_SEASON: u8 = 3;
pub const JF_ITEM_EPISODE: u8 = 4;
pub const JF_ITEM_VIDEO: u8 = 5;

#[derive(Debug, Clone)]
pub struct JellyfinVideoItem {
    pub id: String,
    pub item_type: u8,
    pub name: String,
    pub series_id: String,
    pub series_name: String,
    pub season_id: String,
    pub season_name: String,
    pub season_number: i32,
    pub episode_number: i32,
    pub year: i32,
    pub duration_ms: u32,
    pub resume_ms: u32,
    pub played: bool,
    pub media_source_id: String,
    pub container: String,
    pub video_codec: String,
    pub audio_codec: String,
    pub width: u32,
    pub height: u32,
    pub ratio_num: u32,
    pub ratio_den: u32,
    pub bit_depth: u32,
    pub bitrate: u32,
    pub chapters_csv: String,
}

impl JellyfinVideoItem {
    pub fn is_playable(&self) -> bool {
        matches!(
            self.item_type,
            JF_ITEM_MOVIE | JF_ITEM_EPISODE | JF_ITEM_VIDEO
        )
    }
}

pub fn normalize_video_container(raw_container: &str, server_path: &str) -> String {
    if let Some((_, ext)) = server_path.rsplit_once('.') {
        let e = ext.trim().to_ascii_lowercase();
        if matches!(
            e.as_str(),
            "mp4" | "m4v" | "mkv" | "avi" | "mov" | "webm" | "mpg" | "mpeg" | "ts" | "wmv"
        ) {
            return e;
        }
    }
    let c = raw_container.trim().to_ascii_lowercase();
    if c.is_empty() {
        return "mp4".to_string();
    }
    if c.contains("matroska") || c == "mkv" {
        return "mkv".to_string();
    }
    if c.contains("mp4") || c.contains("m4v") {
        return "mp4".to_string();
    }
    if c.contains("webm") {
        return "webm".to_string();
    }
    if c.contains("avi") {
        return "avi".to_string();
    }
    if c.contains("mpegts") || c == "ts" {
        return "ts".to_string();
    }
    if c.contains("mov") {
        return "mov".to_string();
    }
    c.split(',').next().unwrap_or("mp4").trim().to_string()
}

pub fn normalize_audio_suffix(raw_container: &str, server_path: &str) -> String {
    if let Some((_, ext)) = server_path.rsplit_once('.') {
        let e = ext.trim().to_ascii_lowercase();
        if !e.is_empty() && e.len() <= 6 && e.chars().all(|ch| ch.is_ascii_alphanumeric()) {
            return e;
        }
    }
    let c = raw_container.trim().to_ascii_lowercase();
    if c.is_empty() {
        return "mp3".to_string();
    }
    if c.contains("flac") {
        return "flac".to_string();
    }
    if c.contains("mp3") || c.contains("mpeg") {
        return "mp3".to_string();
    }
    if c.contains("m4a") || c.contains("mp4") || c.contains("aac") {
        return "m4a".to_string();
    }
    if c.contains("opus") {
        return "opus".to_string();
    }
    if c.contains("ogg") || c.contains("vorbis") {
        return "ogg".to_string();
    }
    if c.contains("wav") {
        return "wav".to_string();
    }
    c.split(',').next().unwrap_or("mp3").trim().to_string()
}

fn parse_aspect_ratio(ar: &str, width: u32, height: u32) -> (u32, u32) {
    let trimmed = ar.trim();
    if let Some((l, r)) = trimmed.split_once(':') {
        if let (Ok(lf), Ok(rf)) = (l.trim().parse::<f64>(), r.trim().parse::<f64>()) {
            if lf > 0.01 && rf > 0.01 {
                let n = (lf * 100.0).round() as u32;
                let d = (rf * 100.0).round() as u32;
                if n > 0 && d > 0 {
                    return (n, d);
                }
            }
        }
    }
    if width > 0 && height > 0 {
        (width, height)
    } else {
        (16, 9)
    }
}

pub fn decode_video_items_page(bytes: &[u8]) -> Result<(Vec<JellyfinVideoItem>, usize), String> {
    let root = parse_json(bytes)?;
    let obj = root
        .as_obj()
        .ok_or_else(|| "Expected object for Jellyfin Items response".to_string())?;
    let total = get_u64(obj, "TotalRecordCount", "totalRecordCount") as usize;
    let items = get_prop(obj, "Items", "items")
        .and_then(JsonVal::as_array)
        .ok_or_else(|| "Missing Items array".to_string())?;

    let mut out = Vec::with_capacity(items.len());
    for item in items {
        let Some(io) = item.as_obj() else {
            continue;
        };
        let id = get_str(io, "Id", "id");
        let name = get_str(io, "Name", "name");
        if id.is_empty() || name.is_empty() {
            continue;
        }
        let type_str = get_str(io, "Type", "type");
        let item_type = match type_str.as_str() {
            "Movie" => JF_ITEM_MOVIE,
            "Series" => JF_ITEM_SERIES,
            "Season" => JF_ITEM_SEASON,
            "Episode" => JF_ITEM_EPISODE,
            "Video" | "MusicVideo" => JF_ITEM_VIDEO,
            _ => continue,
        };

        let series_id = get_str(io, "SeriesId", "seriesId");
        let series_name = get_str(io, "SeriesName", "seriesName");
        let season_id = get_str(io, "SeasonId", "seasonId");
        let season_name = get_str(io, "SeasonName", "seasonName");

        let index_num = get_i32(io, "IndexNumber", "indexNumber");
        let parent_index = get_i32(io, "ParentIndexNumber", "parentIndexNumber");
        let (season_number, episode_number) = if item_type == JF_ITEM_SEASON {
            (index_num, 0)
        } else if item_type == JF_ITEM_EPISODE {
            (parent_index, index_num)
        } else {
            (0, 0)
        };

        let year = get_i32(io, "ProductionYear", "productionYear");
        let runtime_ticks = get_u64(io, "RunTimeTicks", "runTimeTicks");
        let duration_ms = (runtime_ticks / 10_000).min(u32::MAX as u64) as u32;

        let mut resume_ms = 0u32;
        let mut played = false;
        if let Some(ud) = get_prop(io, "UserData", "userData").and_then(JsonVal::as_obj) {
            let pos_ticks = get_u64(ud, "PlaybackPositionTicks", "playbackPositionTicks");
            resume_ms = (pos_ticks / 10_000).min(u32::MAX as u64) as u32;
            played = get_prop(ud, "Played", "played")
                .and_then(JsonVal::as_bool)
                .unwrap_or(false);
        }

        let server_path = get_str(io, "Path", "path");
        let mut media_source_id = String::new();
        let mut raw_container = get_str(io, "Container", "container");
        let mut video_codec = String::new();
        let mut audio_codec = String::new();
        let mut width = 0u32;
        let mut height = 0u32;
        let mut aspect_str = String::new();
        let mut bit_depth = 0u32;
        let mut bitrate = 0u32;

        if let Some(ms_arr) = get_prop(io, "MediaSources", "mediaSources").and_then(JsonVal::as_array) {
            if let Some(ms0) = ms_arr.first().and_then(JsonVal::as_obj) {
                media_source_id = get_str(ms0, "Id", "id");
                let c = get_str(ms0, "Container", "container");
                if !c.is_empty() {
                    raw_container = c;
                }
                bitrate = get_u32(ms0, "Bitrate", "bitrate");
                if let Some(streams) = get_prop(ms0, "MediaStreams", "mediaStreams").and_then(JsonVal::as_array) {
                    for st in streams {
                        let Some(so) = st.as_obj() else {
                            continue;
                        };
                        let st_type = get_str(so, "Type", "type");
                        if st_type.eq_ignore_ascii_case("Video") && video_codec.is_empty() {
                            video_codec = get_str(so, "Codec", "codec");
                            width = get_u32(so, "Width", "width");
                            height = get_u32(so, "Height", "height");
                            aspect_str = get_str(so, "AspectRatio", "aspectRatio");
                            bit_depth = get_u32(so, "BitDepth", "bitDepth");
                            if bit_depth == 0 {
                                let prof = get_str(so, "Profile", "profile").to_ascii_lowercase();
                                if prof.contains("10") {
                                    bit_depth = 10;
                                }
                            }
                            if bitrate == 0 {
                                bitrate = get_u32(so, "BitRate", "bitRate");
                            }
                        } else if st_type.eq_ignore_ascii_case("Audio") && audio_codec.is_empty() {
                            audio_codec = get_str(so, "Codec", "codec");
                        }
                    }
                }
            }
        }

        let container = if matches!(item_type, JF_ITEM_SERIES | JF_ITEM_SEASON) {
            String::new()
        } else {
            normalize_video_container(&raw_container, &server_path)
        };
        let (ratio_num, ratio_den) = parse_aspect_ratio(&aspect_str, width, height);

        let mut chapters_csv = String::new();
        if let Some(ch_arr) = get_prop(io, "Chapters", "chapters").and_then(JsonVal::as_array) {
            for (idx, ch) in ch_arr.iter().take(64).enumerate() {
                if let Some(co) = ch.as_obj() {
                    let ticks = get_u64(co, "StartPositionTicks", "startPositionTicks");
                    let ms = (ticks / 10_000).min(u32::MAX as u64) as u32;
                    if idx > 0 {
                        chapters_csv.push(',');
                    }
                    chapters_csv.push_str(&ms.to_string());
                }
            }
        }

        out.push(JellyfinVideoItem {
            id,
            item_type,
            name,
            series_id,
            series_name,
            season_id,
            season_name,
            season_number,
            episode_number,
            year,
            duration_ms,
            resume_ms,
            played,
            media_source_id,
            container,
            video_codec,
            audio_codec,
            width,
            height,
            ratio_num,
            ratio_den,
            bit_depth,
            bitrate,
            chapters_csv,
        });
    }

    Ok((out, total))
}

#[derive(Debug, Clone)]
pub struct JellyfinAudioSong {
    pub id: String,
    pub cover_id: String,
    pub server_path: String,
    pub suffix: String,
    pub title: String,
    pub artist: String,
    pub album_artist: String,
    pub album: String,
    pub genre: String,
    pub year: i32,
    pub track: i32,
    pub disc_number: i32,
    pub duration_secs: u32,
    pub size: u64,
    pub bit_rate: u32,
    pub bit_depth: u32,
    pub sampling_rate: u32,
}

fn decode_single_audio_obj(io: &BTreeMap<String, JsonVal>) -> Option<JellyfinAudioSong> {
    let id = get_str(io, "Id", "id");
    if id.is_empty() {
        return None;
    }
    let type_str = get_str(io, "Type", "type");
    if !type_str.is_empty() && !type_str.eq_ignore_ascii_case("Audio") {
        return None;
    }

    let title = {
        let n = get_str(io, "Name", "name");
        if n.is_empty() { id.clone() } else { n }
    };
    let album = get_str(io, "Album", "album");
    let album_artist = get_str(io, "AlbumArtist", "albumArtist");
    let mut artist = String::new();
    if let Some(artists) = get_prop(io, "Artists", "artists").and_then(JsonVal::as_array) {
        for a in artists {
            if let Some(s) = a.as_str() {
                if !s.is_empty() {
                    if !artist.is_empty() {
                        artist.push_str(", ");
                    }
                    artist.push_str(s);
                }
            }
        }
    }
    if artist.is_empty() {
        artist = album_artist.clone();
    }

    let mut genre = String::new();
    if let Some(genres) = get_prop(io, "Genres", "genres").and_then(JsonVal::as_array) {
        if let Some(first_g) = genres.first().and_then(JsonVal::as_str) {
            genre = first_g.to_string();
        }
    }

    let year = get_i32(io, "ProductionYear", "productionYear");
    let track = get_i32(io, "IndexNumber", "indexNumber");
    let disc_number = {
        let d = get_i32(io, "ParentIndexNumber", "parentIndexNumber");
        if d <= 0 { 1 } else { d }
    };

    let runtime_ticks = get_u64(io, "RunTimeTicks", "runTimeTicks");
    let duration_secs = (runtime_ticks / 10_000_000).min(u32::MAX as u64) as u32;

    let server_path = get_str(io, "Path", "path");
    let mut raw_container = get_str(io, "Container", "container");
    let mut size = get_u64(io, "Size", "size");
    let mut bitrate_bps = 0u32;
    let mut bit_depth = 0u32;
    let mut sampling_rate = 0u32;

    if let Some(ms_arr) = get_prop(io, "MediaSources", "mediaSources").and_then(JsonVal::as_array) {
        if let Some(ms0) = ms_arr.first().and_then(JsonVal::as_obj) {
            let c = get_str(ms0, "Container", "container");
            if !c.is_empty() {
                raw_container = c;
            }
            if size == 0 {
                size = get_u64(ms0, "Size", "size");
            }
            bitrate_bps = get_u32(ms0, "Bitrate", "bitrate");
            if let Some(streams) = get_prop(ms0, "MediaStreams", "mediaStreams").and_then(JsonVal::as_array) {
                for st in streams {
                    if let Some(so) = st.as_obj() {
                        if get_str(so, "Type", "type").eq_ignore_ascii_case("Audio") {
                            bit_depth = get_u32(so, "BitDepth", "bitDepth");
                            sampling_rate = get_u32(so, "SampleRate", "sampleRate");
                            if bitrate_bps == 0 {
                                bitrate_bps = get_u32(so, "BitRate", "bitRate");
                            }
                            break;
                        }
                    }
                }
            }
        }
    }

    let suffix = normalize_audio_suffix(&raw_container, &server_path);
    let bit_rate = bitrate_bps / 1000;

    let mut cover_id = String::new();
    let album_img_tag = get_str(io, "AlbumPrimaryImageTag", "albumPrimaryImageTag");
    let album_id = get_str(io, "AlbumId", "albumId");
    if !album_img_tag.is_empty() && !album_id.is_empty() {
        cover_id = album_id;
    } else if let Some(tags) = get_prop(io, "ImageTags", "imageTags").and_then(JsonVal::as_obj) {
        if !get_str(tags, "Primary", "primary").is_empty() {
            cover_id = id.clone();
        }
    }

    Some(JellyfinAudioSong {
        id,
        cover_id,
        server_path,
        suffix,
        title,
        artist,
        album_artist,
        album,
        genre,
        year,
        track,
        disc_number,
        duration_secs,
        size,
        bit_rate,
        bit_depth,
        sampling_rate,
    })
}

pub fn decode_audio_items_page(bytes: &[u8]) -> Result<(Vec<JellyfinAudioSong>, usize), String> {
    let root = parse_json(bytes)?;
    let obj = root
        .as_obj()
        .ok_or_else(|| "Expected object for Jellyfin audio Items".to_string())?;
    let total = get_u64(obj, "TotalRecordCount", "totalRecordCount") as usize;
    let items = get_prop(obj, "Items", "items")
        .and_then(JsonVal::as_array)
        .ok_or_else(|| "Missing Items array".to_string())?;

    let mut out = Vec::with_capacity(items.len());
    for item in items {
        if let Some(io) = item.as_obj() {
            if let Some(song) = decode_single_audio_obj(io) {
                out.push(song);
            }
        }
    }
    Ok((out, total))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JellyfinPlaylistSummary {
    pub id: String,
    pub name: String,
}

pub fn decode_playlists_list(bytes: &[u8]) -> Result<Vec<JellyfinPlaylistSummary>, String> {
    let root = parse_json(bytes)?;
    let obj = root
        .as_obj()
        .ok_or_else(|| "Expected object for Jellyfin Playlists".to_string())?;
    let items = get_prop(obj, "Items", "items")
        .and_then(JsonVal::as_array)
        .ok_or_else(|| "Missing Items in Playlists".to_string())?;

    let mut out = Vec::with_capacity(items.len());
    for item in items {
        if let Some(io) = item.as_obj() {
            let id = get_str(io, "Id", "id");
            let name = get_str(io, "Name", "name");
            if !id.is_empty() && !name.is_empty() {
                out.push(JellyfinPlaylistSummary { id, name });
            }
        }
    }
    Ok(out)
}
