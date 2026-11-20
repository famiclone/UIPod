//! Pure-Rust Last.fm Scrobbling 2.0 client, persistent offline scrobble cache,
//! ISO/IEC 18004 QR code generator, and C ABI for UIPod.
//!
//! Zero Rockbox headers or symbols — communicates strictly through `extern "C"` FFI.

pub mod config;
pub mod json;
pub mod md5;
pub mod qr;
pub mod queue;

use std::collections::BTreeMap;
use std::ffi::{c_char, CStr, CString};
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};

use config::LastFmConfig;
use json::{decode_auth_token, decode_scrobble_response, decode_session, LastFmApiError};
use md5::{build_signed_post_body, form_urlencode};
use qr::encode_qr_bytes;
use queue::{ActiveTrackSession, QueuedScrobble, ScrobbleQueue};

const LASTFM_API_HOST: &str = "ws.audioscrobbler.com";
const LASTFM_API_PORT: u16 = 443;
const LASTFM_API_PATH: &str = "/2.0/";
const LASTFM_HEADERS: &str =
    "Content-Type: application/x-www-form-urlencoded\r\nUser-Agent: UIPod/0.5\r\n";

#[repr(C)]
#[derive(Clone, Copy)]
pub struct TpLfmHttpVtable {
    pub http_request: unsafe extern "C" fn(
        is_https: u8,
        host: *const c_char,
        port: u16,
        method: *const c_char,
        path_and_query: *const c_char,
        extra_headers: *const c_char,
        req_body: *const u8,
        req_body_len: usize,
        timeout_ms: i32,
        out_status: *mut i32,
        out_data: *mut *mut u8,
        out_len: *mut usize,
        err_buf: *mut c_char,
        err_cap: usize,
    ) -> i32,
    pub http_free: unsafe extern "C" fn(ptr: *mut u8),
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct TpLfmConfigC {
    pub enabled: u8,
    pub scrobble_streaming: u8,
    pub _pad: [u8; 2],
    pub api_key: [c_char; 96],
    pub api_secret: [c_char; 96],
    pub username: [c_char; 96],
    pub password: [c_char; 96],
    pub session_key: [c_char; 96],
}

#[derive(Debug, Clone)]
struct PendingNowPlaying {
    artist: String,
    track: String,
    album: String,
    album_artist: String,
    track_number: u32,
    duration_secs: u32,
}

struct Transport {
    vt: TpLfmHttpVtable,
}

unsafe impl Send for Transport {}
unsafe impl Sync for Transport {}

#[derive(Debug)]
enum RequestFailure {
    Network(String),
    HttpStatus(i32, String),
    Api(LastFmApiError),
}

impl RequestFailure {
    fn is_transient(&self) -> bool {
        match self {
            Self::Network(_) => true,
            Self::HttpStatus(code, _) => *code >= 500 || *code == 429 || *code == 408,
            Self::Api(err) => matches!(err.code, 11 | 16 | 29),
        }
    }

    fn is_invalid_session(&self) -> bool {
        matches!(self, Self::Api(err) if err.code == 9)
    }

    fn message(&self) -> String {
        match self {
            Self::Network(m) => m.clone(),
            Self::HttpStatus(code, m) => {
                if m.is_empty() {
                    format!("HTTP {code}")
                } else {
                    m.clone()
                }
            }
            Self::Api(err) => err.message.clone(),
        }
    }
}

impl Transport {
    fn post_lastfm(
        &self,
        params: &BTreeMap<String, String>,
        api_secret: &str,
        timeout_ms: i32,
    ) -> Result<Vec<u8>, RequestFailure> {
        let body = build_signed_post_body(params, api_secret);
        let c_host = CString::new(LASTFM_API_HOST).unwrap();
        let c_method = CString::new("POST").unwrap();
        let c_pq = CString::new(LASTFM_API_PATH).unwrap();
        let c_hdr = CString::new(LASTFM_HEADERS).unwrap();

        let mut status: i32 = 0;
        let mut out_ptr: *mut u8 = std::ptr::null_mut();
        let mut out_len: usize = 0;
        let mut err_buf = [0u8; 160];

        let rc = unsafe {
            (self.vt.http_request)(
                1,
                c_host.as_ptr(),
                LASTFM_API_PORT,
                c_method.as_ptr(),
                c_pq.as_ptr(),
                c_hdr.as_ptr(),
                body.as_ptr(),
                body.len(),
                timeout_ms,
                &mut status,
                &mut out_ptr,
                &mut out_len,
                err_buf.as_mut_ptr() as *mut c_char,
                err_buf.len(),
            )
        };

        let resp_bytes = if !out_ptr.is_null() && out_len > 0 {
            let v = unsafe { std::slice::from_raw_parts(out_ptr, out_len) }.to_vec();
            unsafe { (self.vt.http_free)(out_ptr) };
            v
        } else {
            if !out_ptr.is_null() {
                unsafe { (self.vt.http_free)(out_ptr) };
            }
            Vec::new()
        };

        if rc != 0 {
            return Err(RequestFailure::Network(c_buf_to_string(
                &err_buf,
                "Connection failed",
            )));
        }

        if !resp_bytes.is_empty() {
            if let Err(api_err) = json::check_api_error(&resp_bytes) {
                return Err(RequestFailure::Api(api_err));
            }
        }

        if status < 200 || status >= 300 {
            return Err(RequestFailure::HttpStatus(
                status,
                format!("HTTP {status}"),
            ));
        }

        Ok(resp_bytes)
    }
}

struct LastFmState {
    cfg: LastFmConfig,
    queue: ScrobbleQueue,
    active: Option<ActiveTrackSession>,
    pending_np: Option<PendingNowPlaying>,
    auth_invalid: bool,
    last_error: String,
}

struct GlobalLastFm {
    transport: Transport,
    state: Mutex<LastFmState>,
}

static GLOBAL_LFM: OnceLock<GlobalLastFm> = OnceLock::new();

fn c_str_to_string(ptr: *const c_char) -> String {
    if ptr.is_null() {
        return String::new();
    }
    unsafe { CStr::from_ptr(ptr) }
        .to_string_lossy()
        .into_owned()
}

fn c_buf_to_string(buf: &[u8], fallback: &str) -> String {
    let end = buf.iter().position(|&b| b == 0).unwrap_or(buf.len());
    let s = String::from_utf8_lossy(&buf[..end]).trim().to_string();
    if s.is_empty() {
        fallback.to_string()
    } else {
        s
    }
}

fn write_c_str(dst: *mut c_char, cap: usize, s: &str) -> usize {
    if dst.is_null() || cap == 0 {
        return 0;
    }
    let bytes = s.as_bytes();
    let n = bytes.len().min(cap - 1);
    unsafe {
        std::ptr::copy_nonoverlapping(bytes.as_ptr(), dst as *mut u8, n);
        *dst.add(n) = 0;
    }
    n
}

fn write_fixed_c<const N: usize>(dst: &mut [c_char; N], s: &str) {
    dst.fill(0);
    write_c_str(dst.as_mut_ptr(), N, s);
}

fn read_fixed_c<const N: usize>(src: &[c_char; N]) -> String {
    c_str_to_string(src.as_ptr())
}

// ---- C ABI (`tp_lastfm_*`) --------------------------------------------------

#[no_mangle]
pub unsafe extern "C" fn tp_lastfm_init(
    queue_file_path: *const c_char,
    vt: *const TpLfmHttpVtable,
) -> bool {
    if vt.is_null() {
        return false;
    }
    let q_path = c_str_to_string(queue_file_path);
    let store_path = if q_path.trim().is_empty() {
        None
    } else {
        Some(PathBuf::from(q_path.trim()))
    };

    if let Some(g) = GLOBAL_LFM.get() {
        if let Some(p) = store_path {
            if let Ok(mut st) = g.state.lock() {
                st.queue.set_store_path(p);
            }
        }
        return true;
    }

    let _ = GLOBAL_LFM.set(GlobalLastFm {
        transport: Transport { vt: unsafe { *vt } },
        state: Mutex::new(LastFmState {
            cfg: LastFmConfig::default(),
            queue: ScrobbleQueue::new(store_path),
            active: None,
            pending_np: None,
            auth_invalid: false,
            last_error: String::new(),
        }),
    });
    true
}

#[no_mangle]
pub unsafe extern "C" fn tp_lastfm_load_config_text(ini_text: *const c_char) -> bool {
    let Some(g) = GLOBAL_LFM.get() else {
        return false;
    };
    let text = c_str_to_string(ini_text);
    let parsed = LastFmConfig::parse_text(&text);
    let Ok(mut st) = g.state.lock() else {
        return false;
    };
    st.cfg = parsed;
    st.auth_invalid = false;
    true
}

#[no_mangle]
pub unsafe extern "C" fn tp_lastfm_format_config_text(
    out_buf: *mut c_char,
    out_cap: usize,
) -> usize {
    let Some(g) = GLOBAL_LFM.get() else {
        return 0;
    };
    let Ok(st) = g.state.lock() else {
        return 0;
    };
    let formatted = st.cfg.format_text();
    write_c_str(out_buf, out_cap, &formatted)
}

#[no_mangle]
pub unsafe extern "C" fn tp_lastfm_get_config(out_cfg: *mut TpLfmConfigC) -> bool {
    if out_cfg.is_null() {
        return false;
    }
    let Some(g) = GLOBAL_LFM.get() else {
        return false;
    };
    let Ok(st) = g.state.lock() else {
        return false;
    };
    let mut c = TpLfmConfigC {
        enabled: if st.cfg.enabled { 1 } else { 0 },
        scrobble_streaming: if st.cfg.scrobble_streaming { 1 } else { 0 },
        _pad: [0; 2],
        api_key: [0; 96],
        api_secret: [0; 96],
        username: [0; 96],
        password: [0; 96],
        session_key: [0; 96],
    };
    write_fixed_c(&mut c.api_key, st.cfg.effective_api_key());
    write_fixed_c(&mut c.api_secret, st.cfg.effective_api_secret());
    write_fixed_c(&mut c.username, &st.cfg.username);
    write_fixed_c(&mut c.password, &st.cfg.password);
    write_fixed_c(&mut c.session_key, &st.cfg.session_key);
    unsafe { *out_cfg = c };
    true
}

#[no_mangle]
pub unsafe extern "C" fn tp_lastfm_set_config(in_cfg: *const TpLfmConfigC) -> bool {
    if in_cfg.is_null() {
        return false;
    }
    let Some(g) = GLOBAL_LFM.get() else {
        return false;
    };
    let c = unsafe { &*in_cfg };
    let Ok(mut st) = g.state.lock() else {
        return false;
    };
    st.cfg.enabled = c.enabled != 0;
    st.cfg.scrobble_streaming = c.scrobble_streaming != 0;
    st.cfg.api_key = read_fixed_c(&c.api_key).trim().to_string();
    st.cfg.api_secret = read_fixed_c(&c.api_secret).trim().to_string();
    st.cfg.username = read_fixed_c(&c.username).trim().to_string();
    st.cfg.password = read_fixed_c(&c.password);
    st.cfg.session_key = read_fixed_c(&c.session_key).trim().to_string();
    st.auth_invalid = false;
    true
}

#[no_mangle]
pub extern "C" fn tp_lastfm_is_authenticated() -> bool {
    GLOBAL_LFM
        .get()
        .and_then(|g| g.state.lock().ok().map(|s| s.cfg.is_authenticated() && !s.auth_invalid))
        .unwrap_or(false)
}

#[no_mangle]
pub extern "C" fn tp_lastfm_has_api_credentials() -> bool {
    GLOBAL_LFM
        .get()
        .and_then(|g| g.state.lock().ok().map(|s| s.cfg.has_api_credentials()))
        .unwrap_or(false)
}

#[no_mangle]
pub extern "C" fn tp_lastfm_cached_count() -> u32 {
    GLOBAL_LFM
        .get()
        .and_then(|g| g.state.lock().ok().map(|s| s.queue.len() as u32))
        .unwrap_or(0)
}

#[no_mangle]
pub extern "C" fn tp_lastfm_has_active_track() -> bool {
    GLOBAL_LFM
        .get()
        .and_then(|g| g.state.lock().ok().map(|s| s.active.is_some()))
        .unwrap_or(false)
}

#[no_mangle]
pub extern "C" fn tp_lastfm_clear_cache() {
    if let Some(g) = GLOBAL_LFM.get() {
        if let Ok(mut st) = g.state.lock() {
            st.queue.clear();
        }
    }
}

#[no_mangle]
pub extern "C" fn tp_lastfm_logout() {
    if let Some(g) = GLOBAL_LFM.get() {
        if let Ok(mut st) = g.state.lock() {
            st.cfg.session_key.clear();
            st.cfg.password.clear();
            st.auth_invalid = false;
            st.pending_np = None;
        }
    }
}

/// Request a web/QR authorization token (`auth.getToken`) and populate `out_token` + `out_url`.
/// Returns `0` on success, `-1` on error.
#[no_mangle]
pub unsafe extern "C" fn tp_lastfm_request_auth_token(
    out_token: *mut c_char,
    token_cap: usize,
    out_url: *mut c_char,
    url_cap: usize,
    err_buf: *mut c_char,
    err_cap: usize,
) -> i32 {
    let Some(g) = GLOBAL_LFM.get() else {
        write_c_str(err_buf, err_cap, "Last.fm not initialized");
        return -1;
    };

    let (api_key, api_secret) = {
        let Ok(st) = g.state.lock() else {
            return -1;
        };
        (
            st.cfg.effective_api_key().to_string(),
            st.cfg.effective_api_secret().to_string(),
        )
    };

    if api_key.is_empty() || api_secret.is_empty() {
        write_c_str(err_buf, err_cap, "Set API Key & Secret first");
        return -1;
    }

    let mut params = BTreeMap::new();
    params.insert("method".to_string(), "auth.getToken".to_string());
    params.insert("api_key".to_string(), api_key.clone());

    match g.transport.post_lastfm(&params, &api_secret, 10_000) {
        Ok(resp) => match decode_auth_token(&resp) {
            Ok(tok) => {
                let auth_url = format!(
                    "https://www.last.fm/api/auth/?api_key={}&token={}",
                    form_urlencode(&api_key),
                    form_urlencode(&tok)
                );
                write_c_str(out_token, token_cap, &tok);
                write_c_str(out_url, url_cap, &auth_url);
                0
            }
            Err(e) => {
                write_c_str(err_buf, err_cap, &e.message);
                -1
            }
        },
        Err(fail) => {
            write_c_str(err_buf, err_cap, &fail.message());
            -1
        }
    }
}

/// Poll `auth.getSession` for `token`.
/// Returns:
/// - `1`: Authorized! Session saved into in-memory config (caller should persist `lastfm.cfg`).
/// - `0`: Still waiting for user approval in browser (`error == 14`).
/// - `-1`: Failed / expired / network error (`err_buf` populated).
#[no_mangle]
pub unsafe extern "C" fn tp_lastfm_poll_auth_session(
    token: *const c_char,
    err_buf: *mut c_char,
    err_cap: usize,
) -> i32 {
    let Some(g) = GLOBAL_LFM.get() else {
        return -1;
    };
    let tok = c_str_to_string(token);
    if tok.trim().is_empty() {
        write_c_str(err_buf, err_cap, "Empty auth token");
        return -1;
    }

    let (api_key, api_secret) = {
        let Ok(st) = g.state.lock() else {
            return -1;
        };
        (
            st.cfg.effective_api_key().to_string(),
            st.cfg.effective_api_secret().to_string(),
        )
    };

    let mut params = BTreeMap::new();
    params.insert("method".to_string(), "auth.getSession".to_string());
    params.insert("api_key".to_string(), api_key);
    params.insert("token".to_string(), tok.trim().to_string());

    match g.transport.post_lastfm(&params, &api_secret, 6_000) {
        Ok(resp) => match decode_session(&resp) {
            Ok(sess) => {
                if let Ok(mut st) = g.state.lock() {
                    if !sess.username.is_empty() {
                        st.cfg.username = sess.username;
                    }
                    st.cfg.session_key = sess.session_key;
                    st.cfg.password.clear();
                    st.auth_invalid = false;
                }
                1
            }
            Err(e) if e.code == 14 => 0,
            Err(e) => {
                write_c_str(err_buf, err_cap, &e.message);
                -1
            }
        },
        Err(RequestFailure::Api(e)) if e.code == 14 => 0,
        Err(fail) if fail.is_transient() => {
            // Transient hiccup while polling: don't abort QR screen
            0
        }
        Err(fail) => {
            write_c_str(err_buf, err_cap, &fail.message());
            -1
        }
    }
}

/// Authenticate directly with username & password (`auth.getMobileSession` over HTTPS POST).
/// Returns `0` on success, `-1` on failure.
#[no_mangle]
pub unsafe extern "C" fn tp_lastfm_login_password(
    username: *const c_char,
    password: *const c_char,
    err_buf: *mut c_char,
    err_cap: usize,
) -> i32 {
    let Some(g) = GLOBAL_LFM.get() else {
        write_c_str(err_buf, err_cap, "Last.fm not initialized");
        return -1;
    };
    let user = c_str_to_string(username).trim().to_string();
    let pass = c_str_to_string(password);
    if user.is_empty() || pass.is_empty() {
        write_c_str(err_buf, err_cap, "Enter username and password");
        return -1;
    }

    let (api_key, api_secret) = {
        let Ok(st) = g.state.lock() else {
            return -1;
        };
        (
            st.cfg.effective_api_key().to_string(),
            st.cfg.effective_api_secret().to_string(),
        )
    };

    if api_key.is_empty() || api_secret.is_empty() {
        write_c_str(err_buf, err_cap, "Set API Key & Secret first");
        return -1;
    }

    let mut params = BTreeMap::new();
    params.insert("method".to_string(), "auth.getMobileSession".to_string());
    params.insert("api_key".to_string(), api_key);
    params.insert("username".to_string(), user.clone());
    params.insert("password".to_string(), pass);

    match g.transport.post_lastfm(&params, &api_secret, 10_000) {
        Ok(resp) => match decode_session(&resp) {
            Ok(sess) => {
                if let Ok(mut st) = g.state.lock() {
                    st.cfg.username = if sess.username.is_empty() {
                        user
                    } else {
                        sess.username
                    };
                    st.cfg.session_key = sess.session_key;
                    st.cfg.password.clear();
                    st.auth_invalid = false;
                }
                0
            }
            Err(e) => {
                write_c_str(err_buf, err_cap, &e.message);
                -1
            }
        },
        Err(fail) => {
            write_c_str(err_buf, err_cap, &fail.message());
            -1
        }
    }
}

/// Encode `text` into a QR matrix (`out_modules` receives `size * size` bytes of `0`/`1`).
/// Returns `size` (`21..=49`) on success, or `0` if `text` is empty / too long / `cap` too small.
#[no_mangle]
pub unsafe extern "C" fn tp_lastfm_encode_qr(
    text: *const c_char,
    out_modules: *mut u8,
    modules_cap: usize,
) -> usize {
    if text.is_null() || out_modules.is_null() {
        return 0;
    }
    let s = c_str_to_string(text);
    let Some(qr) = encode_qr_bytes(s.as_bytes()) else {
        return 0;
    };
    if qr.modules.len() > modules_cap {
        return 0;
    }
    unsafe {
        std::ptr::copy_nonoverlapping(qr.modules.as_ptr(), out_modules, qr.modules.len());
    }
    qr.size
}

/// Non-blocking playback hook: called when a track starts playing (`audio_playlist_track_change`).
/// Returns `true` if background worker should wake up.
#[no_mangle]
pub unsafe extern "C" fn tp_lastfm_on_track_start(
    path: *const c_char,
    artist: *const c_char,
    track: *const c_char,
    album: *const c_char,
    album_artist: *const c_char,
    track_number: u32,
    duration_ms: u64,
    is_streaming: bool,
    now_unix: u64,
    now_mono_ms: u64,
) -> bool {
    let Some(g) = GLOBAL_LFM.get() else {
        return false;
    };
    let artist_s = c_str_to_string(artist).trim().to_string();
    let track_s = c_str_to_string(track).trim().to_string();
    let album_s = c_str_to_string(album).trim().to_string();
    let aa_s = c_str_to_string(album_artist).trim().to_string();
    let path_s = c_str_to_string(path);

    let Ok(mut st) = g.state.lock() else {
        return false;
    };

    // Finalize any previous active session first
    if let Some(mut prev) = st.active.take() {
        prev.tick(now_mono_ms, false);
        if let Some(scrob) = prev.ready_to_scrobble() {
            st.queue.enqueue(scrob);
        }
    }

    if !st.cfg.enabled {
        return false;
    }
    if is_streaming && !st.cfg.scrobble_streaming {
        return false;
    }
    if artist_s.is_empty() || track_s.is_empty() {
        return false;
    }

    st.active = Some(ActiveTrackSession {
        path: path_s,
        artist: artist_s.clone(),
        track: track_s.clone(),
        album: album_s.clone(),
        album_artist: aa_s.clone(),
        track_number,
        duration_ms,
        is_streaming,
        started_unix: now_unix,
        accum_played_ms: 0,
        last_tick_mono_ms: now_mono_ms,
        is_playing: true,
        scrobbled: false,
    });

    if duration_ms > 30_000 {
        st.pending_np = Some(PendingNowPlaying {
            artist: artist_s,
            track: track_s,
            album: album_s,
            album_artist: aa_s,
            track_number,
            duration_secs: (duration_ms / 1000) as u32,
        });
    }

    true
}

/// Non-blocking periodic tick (~every 2s from background worker while a track is active).
/// Commits the scrobble to `lastfm_queue.tsv` immediately once `min(duration/2, 240s)` is reached!
/// Returns `true` if a new scrobble was just committed to the persistent queue.
#[no_mangle]
pub extern "C" fn tp_lastfm_on_playback_tick(now_mono_ms: u64, is_playing: bool) -> bool {
    let Some(g) = GLOBAL_LFM.get() else {
        return false;
    };
    let Ok(mut st) = g.state.lock() else {
        return false;
    };
    let mut committed = false;
    let mut ready = None;
    if let Some(act) = st.active.as_mut() {
        act.tick(now_mono_ms, is_playing);
        if let Some(scrob) = act.ready_to_scrobble() {
            act.scrobbled = true;
            ready = Some(scrob);
        }
    }
    if let Some(scrob) = ready {
        if st.cfg.enabled && (!st.active.as_ref().map(|a| a.is_streaming).unwrap_or(false) || st.cfg.scrobble_streaming) {
            committed = st.queue.enqueue(scrob);
        }
    }
    committed
}

/// Non-blocking playback hook: called when a track ends or is skipped (`audio_playlist_track_finish`).
/// Returns `true` if pending scrobbles exist in the queue.
#[no_mangle]
pub extern "C" fn tp_lastfm_on_track_finish(now_mono_ms: u64) -> bool {
    let Some(g) = GLOBAL_LFM.get() else {
        return false;
    };
    let Ok(mut st) = g.state.lock() else {
        return false;
    };
    if let Some(mut act) = st.active.take() {
        act.tick(now_mono_ms, false);
        if let Some(scrob) = act.ready_to_scrobble() {
            if st.cfg.enabled && (!act.is_streaming || st.cfg.scrobble_streaming) {
                st.queue.enqueue(scrob);
            }
        }
    }
    !st.queue.is_empty()
}

fn send_now_playing_http(transport: &Transport, cfg: &LastFmConfig, np: &PendingNowPlaying) {
    let mut params = BTreeMap::new();
    params.insert("method".to_string(), "track.updateNowPlaying".to_string());
    params.insert("api_key".to_string(), cfg.effective_api_key().to_string());
    params.insert("sk".to_string(), cfg.session_key.trim().to_string());
    params.insert("artist".to_string(), np.artist.clone());
    params.insert("track".to_string(), np.track.clone());
    if !np.album.is_empty() {
        params.insert("album".to_string(), np.album.clone());
    }
    if !np.album_artist.is_empty() && np.album_artist != np.artist {
        params.insert("albumArtist".to_string(), np.album_artist.clone());
    }
    if np.track_number > 0 {
        params.insert("trackNumber".to_string(), np.track_number.to_string());
    }
    if np.duration_secs > 0 {
        params.insert("duration".to_string(), np.duration_secs.to_string());
    }
    let _ = transport.post_lastfm(&params, cfg.effective_api_secret(), 6_000);
}

fn send_scrobble_batch_http(
    transport: &Transport,
    cfg: &LastFmConfig,
    batch: &[QueuedScrobble],
) -> Result<u32, RequestFailure> {
    let mut params = BTreeMap::new();
    params.insert("method".to_string(), "track.scrobble".to_string());
    params.insert("api_key".to_string(), cfg.effective_api_key().to_string());
    params.insert("sk".to_string(), cfg.session_key.trim().to_string());

    for (i, s) in batch.iter().enumerate() {
        params.insert(format!("artist[{i}]"), s.artist.clone());
        params.insert(format!("track[{i}]"), s.track.clone());
        params.insert(format!("timestamp[{i}]"), s.timestamp.to_string());
        if !s.album.is_empty() {
            params.insert(format!("album[{i}]"), s.album.clone());
        }
        if !s.album_artist.is_empty() && s.album_artist != s.artist {
            params.insert(format!("albumArtist[{i}]"), s.album_artist.clone());
        }
        if s.track_number > 0 {
            params.insert(format!("trackNumber[{i}]"), s.track_number.to_string());
        }
        if s.duration_secs > 0 {
            params.insert(format!("duration[{i}]"), s.duration_secs.to_string());
        }
    }

    let resp = transport.post_lastfm(&params, cfg.effective_api_secret(), 10_000)?;
    let res = decode_scrobble_response(&resp).map_err(RequestFailure::Api)?;
    Ok(res.accepted.max((batch.len() as u32).saturating_sub(res.ignored)))
}

/// Synchronously flush pending `Now Playing` and cached offline scrobbles (up to all batches of 50).
/// Designed to run on the background Last.fm worker thread (or foreground "Sync Now" action).
///
/// Returns:
/// - `>= 0`: Number of scrobbles flushed (0 if nothing queued).
/// - `-1`: Transient network/service error (caller should back off; cached scrobbles are preserved).
/// - `-2`: Auth/config error (not signed in or session revoked; cached scrobbles are preserved).
#[no_mangle]
pub unsafe extern "C" fn tp_lastfm_sync_now(
    now_unix: u64,
    out_cfg_updated: *mut bool,
    err_buf: *mut c_char,
    err_cap: usize,
) -> i32 {
    if !out_cfg_updated.is_null() {
        unsafe { *out_cfg_updated = false };
    }
    let Some(g) = GLOBAL_LFM.get() else {
        write_c_str(err_buf, err_cap, "Last.fm not initialized");
        return -2;
    };

    // 1. Auto-exchange password in `lastfm.cfg` into a persistent `session_key` if needed
    let pending_pw = {
        let Ok(st) = g.state.lock() else {
            return -2;
        };
        if st.cfg.has_password_login_pending() {
            Some((st.cfg.username.clone(), st.cfg.password.clone()))
        } else {
            None
        }
    };
    if let Some((u, p)) = pending_pw {
        let c_u = CString::new(u).unwrap_or_default();
        let c_p = CString::new(p).unwrap_or_default();
        if unsafe { tp_lastfm_login_password(c_u.as_ptr(), c_p.as_ptr(), err_buf, err_cap) } == 0 {
            if !out_cfg_updated.is_null() {
                unsafe { *out_cfg_updated = true };
            }
        }
    }

    let (cfg_snapshot, np_snapshot) = {
        let Ok(mut st) = g.state.lock() else {
            return -2;
        };
        if !st.cfg.enabled || !st.cfg.is_authenticated() || st.auth_invalid {
            write_c_str(err_buf, err_cap, "Not signed in to Last.fm");
            return -2;
        }
        (st.cfg.clone(), st.pending_np.take())
    };

    // 2. Send ephemeral `track.updateNowPlaying` if queued
    if let Some(np) = np_snapshot {
        send_now_playing_http(&g.transport, &cfg_snapshot, &np);
    }

    // 3. Flush offline scrobble cache in batches of up to 50
    let mut total_flushed: i32 = 0;
    loop {
        let batch = {
            let Ok(st) = g.state.lock() else {
                break;
            };
            if st.queue.is_empty() {
                break;
            }
            st.queue.peek_normalized_batch(now_unix)
        };
        if batch.is_empty() {
            break;
        }

        let batch_len = batch.len();
        match send_scrobble_batch_http(&g.transport, &cfg_snapshot, &batch) {
            Ok(_accepted) => {
                let mut dropped = batch_len;
                if let Ok(mut st) = g.state.lock() {
                    dropped = st.queue.drop_matching_front(&batch);
                    st.last_error.clear();
                }
                total_flushed += dropped as i32;
                if dropped == 0 {
                    break;
                }
            }
            Err(fail) => {
                let msg = fail.message();
                write_c_str(err_buf, err_cap, &msg);
                if let Ok(mut st) = g.state.lock() {
                    if fail.is_invalid_session() {
                        st.auth_invalid = true;
                    }
                    st.last_error = msg;
                }
                return if fail.is_transient() { -1 } else { -2 };
            }
        }
    }

    total_flushed
}
