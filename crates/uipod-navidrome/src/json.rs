//! Complete RFC 8259 UTF-8 JSON parser and Subsonic / Navidrome response decoders.

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

    /// Coerces either JSON string or integer to `String` (Subsonic IDs can be string or int).
    pub fn to_id_string(&self) -> Option<String> {
        match self {
            JsonVal::Str(s) if !s.is_empty() => Some(s.clone()),
            JsonVal::Int(n) => Some(n.to_string()),
            JsonVal::Number(f) => Some((*f as i64).to_string()),
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
        self.pos += 1; // '{'
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
        self.pos += 1; // '['
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
        self.pos += 1; // '"'
        let mut out = Vec::new();
        while self.pos < self.bytes.len() {
            let b = self.bytes[self.pos];
            self.pos += 1;
            match b {
                b'"' => {
                    return Ok(String::from_utf8_lossy(&out).into_owned());
                }
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

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SubsonicSong {
    pub id: String,
    pub title: String,
    pub artist: String,
    pub album_artist: String,
    pub album: String,
    pub genre: String,
    pub cover_art: String,
    pub server_path: String,
    pub suffix: String,
    pub year: i32,
    pub track: i32,
    pub disc_number: i32,
    pub duration_secs: u32,
    pub size: u64,
    pub bit_rate: u32,
    pub bit_depth: u32,
    pub sampling_rate: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubsonicPlaylistSummary {
    pub id: String,
    pub name: String,
    pub song_count: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubsonicPlaylistDetail {
    pub id: String,
    pub name: String,
    pub songs: Vec<SubsonicSong>,
}

fn unwrap_subsonic_root(root: &JsonVal) -> Result<&BTreeMap<String, JsonVal>, String> {
    let top = root
        .as_obj()
        .ok_or_else(|| "Invalid JSON response root".to_string())?;
    let sr = top
        .get("subsonic-response")
        .and_then(JsonVal::as_obj)
        .ok_or_else(|| "Not a Subsonic API server".to_string())?;

    let status = sr.get("status").and_then(JsonVal::as_str).unwrap_or("");
    if !status.eq_ignore_ascii_case("ok") {
        if let Some(err_obj) = sr.get("error").and_then(JsonVal::as_obj) {
            let msg = err_obj
                .get("message")
                .and_then(JsonVal::as_str)
                .unwrap_or("Subsonic error");
            let code = err_obj.get("code").and_then(JsonVal::as_i64).unwrap_or(0);
            return Err(if code == 40 {
                "Wrong username or password".to_string()
            } else {
                msg.to_string()
            });
        }
        return Err("Server returned failed status".to_string());
    }
    Ok(sr)
}

pub fn decode_ping(bytes: &[u8]) -> Result<(), String> {
    let root = parse_json(bytes)?;
    let _ = unwrap_subsonic_root(&root)?;
    Ok(())
}

fn parse_song_obj(obj: &BTreeMap<String, JsonVal>) -> Option<SubsonicSong> {
    // Skip video entries if server returns mixed media
    if obj.get("isVideo") == Some(&JsonVal::Bool(true)) {
        return None;
    }
    let id = obj.get("id").and_then(JsonVal::to_id_string)?;
    let title = obj
        .get("title")
        .and_then(JsonVal::as_str)
        .unwrap_or("")
        .to_string();
    let artist = obj
        .get("displayArtist")
        .or_else(|| obj.get("artist"))
        .and_then(JsonVal::as_str)
        .unwrap_or("")
        .to_string();
    let album_artist = obj
        .get("displayAlbumArtist")
        .or_else(|| obj.get("albumArtist"))
        .and_then(JsonVal::as_str)
        .unwrap_or("")
        .to_string();
    let album = obj
        .get("album")
        .and_then(JsonVal::as_str)
        .unwrap_or("")
        .to_string();

    let genre = if let Some(g) = obj.get("genre").and_then(JsonVal::as_str) {
        g.to_string()
    } else if let Some(arr) = obj.get("genres").and_then(JsonVal::as_array) {
        arr.first()
            .and_then(JsonVal::as_obj)
            .and_then(|go| go.get("name"))
            .and_then(JsonVal::as_str)
            .unwrap_or("")
            .to_string()
    } else {
        String::new()
    };

    let cover_art = obj
        .get("coverArt")
        .and_then(JsonVal::to_id_string)
        .unwrap_or_default();
    let server_path = obj
        .get("path")
        .and_then(JsonVal::as_str)
        .unwrap_or("")
        .to_string();
    let suffix = obj
        .get("suffix")
        .and_then(JsonVal::as_str)
        .unwrap_or("flac")
        .to_ascii_lowercase();

    let year = obj.get("year").and_then(JsonVal::as_i64).unwrap_or(0) as i32;
    let track = obj.get("track").and_then(JsonVal::as_i64).unwrap_or(0) as i32;
    let disc_number = obj
        .get("discNumber")
        .and_then(JsonVal::as_i64)
        .unwrap_or(0) as i32;
    let duration_secs = obj
        .get("duration")
        .and_then(JsonVal::as_i64)
        .unwrap_or(0)
        .max(0) as u32;
    let size = obj
        .get("size")
        .and_then(JsonVal::as_i64)
        .unwrap_or(0)
        .max(0) as u64;
    let bit_rate = obj
        .get("bitRate")
        .and_then(JsonVal::as_i64)
        .unwrap_or(0)
        .max(0) as u32;
    let bit_depth = obj
        .get("bitDepth")
        .and_then(JsonVal::as_i64)
        .unwrap_or(0)
        .max(0) as u32;
    let sampling_rate = obj
        .get("samplingRate")
        .and_then(JsonVal::as_i64)
        .unwrap_or(0)
        .max(0) as u32;

    Some(SubsonicSong {
        id,
        title,
        artist,
        album_artist,
        album,
        genre,
        cover_art,
        server_path,
        suffix,
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

pub fn decode_search3_songs(bytes: &[u8]) -> Result<Vec<SubsonicSong>, String> {
    let root = parse_json(bytes)?;
    let sr = unwrap_subsonic_root(&root)?;
    let Some(res) = sr.get("searchResult3").and_then(JsonVal::as_obj) else {
        return Ok(Vec::new());
    };
    let Some(arr) = res.get("song").and_then(JsonVal::as_array) else {
        return Ok(Vec::new());
    };
    let mut songs = Vec::with_capacity(arr.len());
    for item in arr {
        if let Some(obj) = item.as_obj() {
            if let Some(song) = parse_song_obj(obj) {
                songs.push(song);
            }
        }
    }
    Ok(songs)
}

pub fn decode_playlists(bytes: &[u8]) -> Result<Vec<SubsonicPlaylistSummary>, String> {
    let root = parse_json(bytes)?;
    let sr = unwrap_subsonic_root(&root)?;
    let Some(pls) = sr.get("playlists").and_then(JsonVal::as_obj) else {
        return Ok(Vec::new());
    };
    let Some(arr) = pls.get("playlist").and_then(JsonVal::as_array) else {
        return Ok(Vec::new());
    };
    let mut out = Vec::with_capacity(arr.len());
    for item in arr {
        if let Some(obj) = item.as_obj() {
            if let Some(id) = obj.get("id").and_then(JsonVal::to_id_string) {
                let name = obj
                    .get("name")
                    .and_then(JsonVal::as_str)
                    .unwrap_or("Playlist")
                    .to_string();
                let song_count = obj
                    .get("songCount")
                    .and_then(JsonVal::as_i64)
                    .unwrap_or(0)
                    .max(0) as u32;
                out.push(SubsonicPlaylistSummary {
                    id,
                    name,
                    song_count,
                });
            }
        }
    }
    Ok(out)
}

pub fn decode_playlist_detail(bytes: &[u8]) -> Result<SubsonicPlaylistDetail, String> {
    let root = parse_json(bytes)?;
    let sr = unwrap_subsonic_root(&root)?;
    let pl = sr
        .get("playlist")
        .and_then(JsonVal::as_obj)
        .ok_or_else(|| "Missing playlist object".to_string())?;
    let id = pl
        .get("id")
        .and_then(JsonVal::to_id_string)
        .unwrap_or_default();
    let name = pl
        .get("name")
        .and_then(JsonVal::as_str)
        .unwrap_or("Playlist")
        .to_string();
    let mut songs = Vec::new();
    if let Some(entries) = pl.get("entry").and_then(JsonVal::as_array) {
        for item in entries {
            if let Some(obj) = item.as_obj() {
                if let Some(song) = parse_song_obj(obj) {
                    songs.push(song);
                }
            }
        }
    }
    Ok(SubsonicPlaylistDetail { id, name, songs })
}

pub fn decode_starred_songs(bytes: &[u8]) -> Result<Vec<SubsonicSong>, String> {
    let root = parse_json(bytes)?;
    let sr = unwrap_subsonic_root(&root)?;
    let Some(st) = sr
        .get("starred2")
        .or_else(|| sr.get("starred"))
        .and_then(JsonVal::as_obj)
    else {
        return Ok(Vec::new());
    };
    let Some(arr) = st.get("song").and_then(JsonVal::as_array) else {
        return Ok(Vec::new());
    };
    let mut songs = Vec::with_capacity(arr.len());
    for item in arr {
        if let Some(obj) = item.as_obj() {
            if let Some(song) = parse_song_obj(obj) {
                songs.push(song);
            }
        }
    }
    Ok(songs)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_decode_navidrome_search3_and_unicode() {
        let payload = br#"{
          "subsonic-response": {
            "status": "ok",
            "version": "1.16.1",
            "type": "navidrome",
            "serverVersion": "0.53.3",
            "openSubsonic": true,
            "searchResult3": {
              "song": [
                {
                  "id": "9f8e7d6c",
                  "title": "\u041e\u043a\u0435\u0430\u043d \u0415\u043b\u044c\u0437\u0438 - \u0411\u0435\u0437 \u0431\u043e\u044e",
                  "album": "GLORIA",
                  "artist": "Okean Elzy",
                  "displayAlbumArtist": "Okean Elzy",
                  "track": 4,
                  "discNumber": 1,
                  "year": 2005,
                  "genre": "Rock",
                  "coverArt": "al-9f8e",
                  "size": 28412900,
                  "suffix": "FLAC",
                  "duration": 259,
                  "bitRate": 877,
                  "bitDepth": 16,
                  "samplingRate": 44100,
                  "path": "Okean Elzy/GLORIA/04 - Bez boiu.flac"
                }
              ]
            }
          }
        }"#;
        let songs = decode_search3_songs(payload).unwrap();
        assert_eq!(songs.len(), 1);
        assert_eq!(songs[0].id, "9f8e7d6c");
        assert_eq!(songs[0].title, "Океан Ельзи - Без бою");
        assert_eq!(songs[0].suffix, "flac");
        assert_eq!(songs[0].bit_depth, 16);
        assert_eq!(songs[0].sampling_rate, 44100);
        assert_eq!(songs[0].server_path, "Okean Elzy/GLORIA/04 - Bez boiu.flac");
    }

    #[test]
    fn test_decode_subsonic_auth_error() {
        let payload = br#"{"subsonic-response":{"status":"failed","error":{"code":40,"message":"Wrong username or password"}}}"#;
        let err = decode_ping(payload).unwrap_err();
        assert_eq!(err, "Wrong username or password");
    }

    #[test]
    fn test_decode_starred2_songs() {
        let payload = br#"{
          "subsonic-response": {
            "status": "ok",
            "version": "1.16.1",
            "starred2": {
              "song": [
                {
                  "id": "fav-1",
                  "title": "Feel Good Inc.",
                  "artist": "Gorillaz",
                  "album": "Demon Days",
                  "track": 6,
                  "suffix": "flac"
                }
              ]
            }
          }
        }"#;
        let songs = decode_starred_songs(payload).unwrap();
        assert_eq!(songs.len(), 1);
        assert_eq!(songs[0].id, "fav-1");
        assert_eq!(songs[0].title, "Feel Good Inc.");
    }
}
