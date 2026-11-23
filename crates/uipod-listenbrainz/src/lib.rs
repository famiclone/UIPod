//! Pure-Rust ListenBrainz scrobbling client, persistent offline scrobble cache,
//! and C ABI for UIPod.
//!
//! Zero Rockbox headers or symbols — communicates strictly through `extern "C"` FFI.

pub mod config;
pub mod json;
pub mod queue;

use std::ffi::{c_char, CStr, CString};
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use config::{
    format_auth_header_value, normalize_base_url, sanitize_token, ListenBrainzConfig, ParsedLbUrl,
};
use json::{
    decode_error_body, decode_submit_response, decode_validate_token, encode_playing_now_json,
    encode_scrobbles_json, LbApiError, NowPlayingMeta,
};
use queue::{ActiveTrackSession, QueuedScrobble, ScrobbleQueue};

const LB_USER_AGENT: &str = "UIPod/0.5";

#[repr(C)]
#[derive(Clone, Copy)]
pub struct TpLbHttpVtable {
    pub http_request: unsafe extern "C" fn(
        is_https: u8,
        host: *const c_char,
        port: u16,
        method: *const c_char,
        path_and_query: *const c_char,
        extra_headers: *const c_char,
        req_body: *const u8,
        req_body_len: usize,
        allow_self_signed: u8,
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
pub struct TpLbConfigC {
    pub enabled: u8,
    pub now_playing: u8,
    pub scrobble_streaming: u8,
    pub allow_self_signed: u8,
    pub api_url: [c_char; 192],
    pub user_token: [c_char; 128],
    pub username: [c_char; 96],
}

#[derive(Debug, Clone)]
struct PendingNowPlaying {
    artist: String,
    track: String,
    album: String,
    album_artist: String,
    track_number: u32,
    duration_ms: u64,
}

#[derive(Clone, Copy)]
struct Transport {
    vt: TpLbHttpVtable,
}

unsafe impl Send for Transport {}
unsafe impl Sync for Transport {}

#[derive(Debug)]
enum RequestFailure {
    Network(String),
    HttpStatus(i32, String),
    Api(LbApiError),
}

impl RequestFailure {
    fn is_transient(&self) -> bool {
        match self {
            Self::Network(_) => true,
            Self::HttpStatus(code, _) => *code >= 500 || *code == 429 || *code == 408,
            Self::Api(err) => err.code >= 500 || err.code == 429 || err.code == 408,
        }
    }

    fn is_unauthorized(&self) -> bool {
        match self {
            Self::HttpStatus(code, _) => *code == 401 || *code == 403,
            Self::Api(err) => err.code == 401 || err.code == 403,
            Self::Network(_) => false,
        }
    }

    fn is_bad_request(&self) -> bool {
        match self {
            Self::HttpStatus(code, _) => *code == 400,
            Self::Api(err) => err.code == 400,
            Self::Network(_) => false,
        }
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

fn build_lb_headers(raw_token: &str, is_post: bool) -> Result<String, RequestFailure> {
    let auth_val = format_auth_header_value(raw_token).ok_or_else(|| {
        RequestFailure::Api(LbApiError {
            code: 401,
            message: "Set User Token first".to_string(),
        })
    })?;
    let mut hdr = String::with_capacity(192);
    hdr.push_str("Authorization: ");
    hdr.push_str(&auth_val);
    hdr.push_str("\r\nAccept: application/json\r\n");
    if is_post {
        hdr.push_str("Content-Type: application/json\r\n");
    }
    hdr.push_str("User-Agent: ");
    hdr.push_str(LB_USER_AGENT);
    hdr.push_str("\r\n");
    Ok(hdr)
}

impl Transport {
    fn request_lb(
        &self,
        url: &ParsedLbUrl,
        allow_self_signed: bool,
        token: &str,
        method: &str,
        endpoint_suffix: &str,
        json_body: Option<&str>,
        timeout_ms: i32,
    ) -> Result<Vec<u8>, RequestFailure> {
        let is_post = method.eq_ignore_ascii_case("POST");
        let pq = url.endpoint_path(endpoint_suffix);
        let hdrs = build_lb_headers(token, is_post)?;

        let c_host =
            CString::new(url.host.as_str()).map_err(|_| RequestFailure::Network("Invalid host".into()))?;
        let c_method =
            CString::new(method).map_err(|_| RequestFailure::Network("Invalid method".into()))?;
        let c_pq =
            CString::new(pq.as_str()).map_err(|_| RequestFailure::Network("Invalid path".into()))?;
        let c_hdr =
            CString::new(hdrs.as_str()).map_err(|_| RequestFailure::Network("Invalid headers".into()))?;

        let (body_ptr, body_len) = match json_body {
            Some(b) if !b.is_empty() => (b.as_bytes().as_ptr(), b.len()),
            _ => (std::ptr::null(), 0usize),
        };

        let mut status: i32 = 0;
        let mut out_ptr: *mut u8 = std::ptr::null_mut();
        let mut out_len: usize = 0;
        let mut err_buf = [0u8; 160];

        let rc = unsafe {
            (self.vt.http_request)(
                if url.is_https { 1 } else { 0 },
                c_host.as_ptr(),
                url.port,
                c_method.as_ptr(),
                c_pq.as_ptr(),
                c_hdr.as_ptr(),
                body_ptr,
                body_len,
                if allow_self_signed { 1 } else { 0 },
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

        if status > 0 && (status < 200 || status >= 300) {
            if let Some(mut api_err) = decode_error_body(&resp_bytes) {
                if api_err.code <= 0 {
                    api_err.code = status;
                }
                eprintln!(
                    "[uipod-listenbrainz] {} {}{} -> HTTP {} (code {}): {}",
                    method, url.host, pq, status, api_err.code, api_err.message
                );
                return Err(RequestFailure::Api(LbApiError {
                    code: api_err.code,
                    message: format!("HTTP {status}: {}", api_err.message),
                }));
            }

            let msg = if status == 404 || status == 405 {
                format!("Wrong server URL (HTTP {status} on {})", url.host)
            } else if status == 401 || status == 403 {
                format!("HTTP {status} on {} (check token or server URL)", url.host)
            } else {
                format!("HTTP {status} ({})", url.host)
            };
            eprintln!(
                "[uipod-listenbrainz] {} {}{} -> {} (non-JSON response, {} bytes)",
                method,
                url.host,
                pq,
                msg,
                resp_bytes.len()
            );
            return Err(RequestFailure::HttpStatus(status, msg));
        }

        if rc != 0 {
            return Err(RequestFailure::Network(c_buf_to_string(
                &err_buf,
                "Connection failed",
            )));
        }

        Ok(resp_bytes)
    }
}

struct ListenBrainzState {
    cfg: ListenBrainzConfig,
    queue: ScrobbleQueue,
    active: Option<ActiveTrackSession>,
    pending_np: Option<PendingNowPlaying>,
    auth_invalid: bool,
    last_error: String,
}

struct GlobalListenBrainz {
    transport: Transport,
    state: Mutex<ListenBrainzState>,
}

static GLOBAL_LB: OnceLock<GlobalListenBrainz> = OnceLock::new();

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

// ---- Core Engine Logic (testable independent of `OnceLock`) -----------------

fn validate_token_with(
    transport: &Transport,
    state: &Mutex<ListenBrainzState>,
    err_buf: *mut c_char,
    err_cap: usize,
) -> i32 {
    let (url_str, allow_self_signed, token) = {
        let Ok(st) = state.lock() else {
            return -2;
        };
        (
            st.cfg.effective_api_url().to_string(),
            st.cfg.allow_self_signed,
            st.cfg.clean_token(),
        )
    };

    if token.is_empty() {
        write_c_str(err_buf, err_cap, "Set User Token first");
        return -2;
    }

    let parsed_url = match ParsedLbUrl::parse(&url_str) {
        Ok(u) => u,
        Err(e) => {
            write_c_str(err_buf, err_cap, &e);
            return -2;
        }
    };

    match transport.request_lb(
        &parsed_url,
        allow_self_signed,
        &token,
        "GET",
        "/1/validate-token",
        None,
        8_000,
    ) {
        Ok(resp) => match decode_validate_token(&resp) {
            Ok(vt) if vt.valid => {
                if let Ok(mut st) = state.lock() {
                    if !vt.user_name.trim().is_empty() {
                        st.cfg.username = vt.user_name.trim().to_string();
                    }
                    st.auth_invalid = false;
                    st.last_error.clear();
                }
                0
            }
            Ok(vt) => {
                let msg = if vt.message.trim().is_empty() {
                    "Invalid token".to_string()
                } else {
                    vt.message
                };
                if let Ok(mut st) = state.lock() {
                    st.auth_invalid = true;
                    st.cfg.username.clear();
                    st.last_error = msg.clone();
                }
                write_c_str(err_buf, err_cap, &msg);
                -2
            }
            Err(e) => {
                write_c_str(err_buf, err_cap, &e.message);
                -2
            }
        },
        Err(fail) => {
            let msg = fail.message();
            write_c_str(err_buf, err_cap, &msg);
            if let Ok(mut st) = state.lock() {
                if fail.is_unauthorized() {
                    st.auth_invalid = true;
                    st.cfg.username.clear();
                }
                st.last_error = msg;
            }
            if fail.is_transient() {
                -1
            } else {
                -2
            }
        }
    }
}

fn send_now_playing_http(
    transport: &Transport,
    url: &ParsedLbUrl,
    cfg: &ListenBrainzConfig,
    np: &PendingNowPlaying,
) -> Result<(), RequestFailure> {
    let body = encode_playing_now_json(&NowPlayingMeta {
        artist: &np.artist,
        track: &np.track,
        album: &np.album,
        album_artist: &np.album_artist,
        track_number: np.track_number,
        duration_ms: np.duration_ms,
    });
    let resp = transport.request_lb(
        url,
        cfg.allow_self_signed,
        &cfg.clean_token(),
        "POST",
        "/1/submit-listens",
        Some(&body),
        6_000,
    )?;
    decode_submit_response(&resp).map_err(RequestFailure::Api)
}

fn send_scrobble_batch_http(
    transport: &Transport,
    url: &ParsedLbUrl,
    cfg: &ListenBrainzConfig,
    batch: &[QueuedScrobble],
) -> Result<usize, RequestFailure> {
    let body = encode_scrobbles_json(batch);
    let resp = transport.request_lb(
        url,
        cfg.allow_self_signed,
        &cfg.clean_token(),
        "POST",
        "/1/submit-listens",
        Some(&body),
        10_000,
    )?;
    decode_submit_response(&resp).map_err(RequestFailure::Api)?;
    Ok(batch.len())
}

fn sync_now_with(
    transport: &Transport,
    state: &Mutex<ListenBrainzState>,
    now_unix: u64,
    out_cfg_updated: *mut bool,
    err_buf: *mut c_char,
    err_cap: usize,
    inter_batch_pause_ms: u64,
) -> i32 {
    if !out_cfg_updated.is_null() {
        unsafe { *out_cfg_updated = false };
    }

    // 1. If `user_token` was supplied via `listenbrainz.cfg` without cached `username`,
    //    validate it once so UI displays `@username` and invalid tokens are caught early.
    let needs_initial_validate = {
        let Ok(st) = state.lock() else {
            return -2;
        };
        st.cfg.enabled
            && st.cfg.is_authenticated()
            && !st.auth_invalid
            && st.cfg.username.trim().is_empty()
    };
    if needs_initial_validate {
        let vrc = validate_token_with(transport, state, err_buf, err_cap);
        if vrc == 0 {
            if !out_cfg_updated.is_null() {
                unsafe { *out_cfg_updated = true };
            }
        } else {
            return vrc;
        }
    }

    let (cfg_snapshot, np_snapshot) = {
        let Ok(mut st) = state.lock() else {
            return -2;
        };
        let np = if st.cfg.now_playing {
            st.pending_np.take()
        } else {
            st.pending_np = None;
            None
        };
        if !st.cfg.enabled || !st.cfg.is_authenticated() || st.auth_invalid {
            let msg = if st.auth_invalid {
                if st.last_error.is_empty() {
                    "Invalid ListenBrainz token"
                } else {
                    st.last_error.as_str()
                }
            } else {
                "Not signed in to ListenBrainz"
            };
            write_c_str(err_buf, err_cap, msg);
            return -2;
        }
        (st.cfg.clone(), np)
    };

    let parsed_url = match ParsedLbUrl::parse(&cfg_snapshot.effective_api_url()) {
        Ok(u) => u,
        Err(e) => {
            write_c_str(err_buf, err_cap, &e);
            return -2;
        }
    };

    // 2. Send ephemeral `playing_now` if queued (never cached offline on network failure)
    if let Some(np) = np_snapshot {
        if let Err(fail) = send_now_playing_http(transport, &parsed_url, &cfg_snapshot, &np) {
            if fail.is_unauthorized() {
                let msg = fail.message();
                write_c_str(err_buf, err_cap, &msg);
                if let Ok(mut st) = state.lock() {
                    st.auth_invalid = true;
                    st.last_error = msg;
                }
                return -2;
            }
        }
    }

    // 3. Flush persistent offline scrobble cache (`single` for 1, `import` for batches up to 50)
    let mut total_flushed: i32 = 0;
    let mut batch_idx: usize = 0;
    loop {
        let batch = {
            let Ok(st) = state.lock() else {
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

        if batch_idx > 0 && inter_batch_pause_ms > 0 {
            std::thread::sleep(Duration::from_millis(inter_batch_pause_ms));
        }
        batch_idx += 1;

        match send_scrobble_batch_http(transport, &parsed_url, &cfg_snapshot, &batch) {
            Ok(_) => {
                let mut dropped = batch.len();
                if let Ok(mut st) = state.lock() {
                    dropped = st.queue.drop_matching_front(&batch);
                    st.last_error.clear();
                }
                total_flushed += dropped as i32;
                if dropped == 0 {
                    break;
                }
            }
            Err(fail) if fail.is_bad_request() => {
                // Isolate 400 Bad Request so a single malformed item never blocks the queue forever
                if batch.len() > 1 {
                    match send_scrobble_batch_http(
                        transport,
                        &parsed_url,
                        &cfg_snapshot,
                        &batch[..1],
                    ) {
                        Ok(_) => {
                            if let Ok(mut st) = state.lock() {
                                let d = st.queue.drop_matching_front(&batch[..1]);
                                total_flushed += d as i32;
                            }
                            continue;
                        }
                        Err(single_fail) if single_fail.is_bad_request() => {
                            let msg = single_fail.message();
                            eprintln!(
                                "[uipod-listenbrainz] dropping malformed scrobble ({} - {}): {}",
                                batch[0].artist, batch[0].track, msg
                            );
                            if let Ok(mut st) = state.lock() {
                                st.last_error = msg;
                                st.queue.drop_front_one();
                            }
                            continue;
                        }
                        Err(other_fail) => {
                            let msg = other_fail.message();
                            write_c_str(err_buf, err_cap, &msg);
                            if let Ok(mut st) = state.lock() {
                                if other_fail.is_unauthorized() {
                                    st.auth_invalid = true;
                                }
                                st.last_error = msg;
                            }
                            return if other_fail.is_transient() { -1 } else { -2 };
                        }
                    }
                } else {
                    let msg = fail.message();
                    eprintln!(
                        "[uipod-listenbrainz] dropping malformed scrobble ({} - {}): {}",
                        batch[0].artist, batch[0].track, msg
                    );
                    write_c_str(err_buf, err_cap, &msg);
                    if let Ok(mut st) = state.lock() {
                        st.last_error = msg;
                        st.queue.drop_front_one();
                    }
                    continue;
                }
            }
            Err(fail) => {
                let msg = fail.message();
                write_c_str(err_buf, err_cap, &msg);
                if let Ok(mut st) = state.lock() {
                    if fail.is_unauthorized() {
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

// ---- C ABI (`tp_listenbrainz_*`) --------------------------------------------

#[no_mangle]
pub unsafe extern "C" fn tp_listenbrainz_init(
    queue_file_path: *const c_char,
    vt: *const TpLbHttpVtable,
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

    if let Some(g) = GLOBAL_LB.get() {
        if let Some(p) = store_path {
            if let Ok(mut st) = g.state.lock() {
                st.queue.set_store_path(p);
            }
        }
        return true;
    }

    let _ = GLOBAL_LB.set(GlobalListenBrainz {
        transport: Transport { vt: unsafe { *vt } },
        state: Mutex::new(ListenBrainzState {
            cfg: ListenBrainzConfig::default(),
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
pub unsafe extern "C" fn tp_listenbrainz_load_config_text(ini_text: *const c_char) -> bool {
    let Some(g) = GLOBAL_LB.get() else {
        return false;
    };
    let text = c_str_to_string(ini_text);
    let parsed = ListenBrainzConfig::parse_text(&text);
    let Ok(mut st) = g.state.lock() else {
        return false;
    };
    st.cfg = parsed;
    st.auth_invalid = false;
    true
}

#[no_mangle]
pub unsafe extern "C" fn tp_listenbrainz_format_config_text(
    out_buf: *mut c_char,
    out_cap: usize,
) -> usize {
    let Some(g) = GLOBAL_LB.get() else {
        return 0;
    };
    let Ok(st) = g.state.lock() else {
        return 0;
    };
    let formatted = st.cfg.format_text();
    write_c_str(out_buf, out_cap, &formatted)
}

#[no_mangle]
pub unsafe extern "C" fn tp_listenbrainz_get_config(out_cfg: *mut TpLbConfigC) -> bool {
    if out_cfg.is_null() {
        return false;
    }
    let Some(g) = GLOBAL_LB.get() else {
        return false;
    };
    let Ok(st) = g.state.lock() else {
        return false;
    };
    let mut c = TpLbConfigC {
        enabled: if st.cfg.enabled { 1 } else { 0 },
        now_playing: if st.cfg.now_playing { 1 } else { 0 },
        scrobble_streaming: if st.cfg.scrobble_streaming { 1 } else { 0 },
        allow_self_signed: if st.cfg.allow_self_signed { 1 } else { 0 },
        api_url: [0; 192],
        user_token: [0; 128],
        username: [0; 96],
    };
    write_fixed_c(&mut c.api_url, &st.cfg.effective_api_url());
    write_fixed_c(&mut c.user_token, &st.cfg.clean_token());
    write_fixed_c(&mut c.username, &st.cfg.username);
    unsafe { *out_cfg = c };
    true
}

#[no_mangle]
pub unsafe extern "C" fn tp_listenbrainz_set_config(in_cfg: *const TpLbConfigC) -> bool {
    if in_cfg.is_null() {
        return false;
    }
    let Some(g) = GLOBAL_LB.get() else {
        return false;
    };
    let c = unsafe { &*in_cfg };
    let Ok(mut st) = g.state.lock() else {
        return false;
    };
    let new_url = normalize_base_url(&read_fixed_c(&c.api_url));
    let new_token = sanitize_token(&read_fixed_c(&c.user_token));
    let token_changed =
        new_token != st.cfg.clean_token() || new_url != st.cfg.effective_api_url();

    st.cfg.enabled = c.enabled != 0;
    st.cfg.now_playing = c.now_playing != 0;
    st.cfg.scrobble_streaming = c.scrobble_streaming != 0;
    st.cfg.allow_self_signed = c.allow_self_signed != 0;
    st.cfg.api_url = new_url;
    st.cfg.user_token = new_token;
    if token_changed && st.cfg.user_token.is_empty() {
        st.cfg.username.clear();
    } else if !token_changed {
        st.cfg.username = read_fixed_c(&c.username).trim().to_string();
    } else {
        // New token or server URL entered: clear stale cached username until re-validated
        st.cfg.username.clear();
    }
    st.auth_invalid = false;
    true
}

#[no_mangle]
pub extern "C" fn tp_listenbrainz_is_authenticated() -> bool {
    GLOBAL_LB
        .get()
        .and_then(|g| {
            g.state
                .lock()
                .ok()
                .map(|s| s.cfg.is_authenticated() && !s.auth_invalid)
        })
        .unwrap_or(false)
}

#[no_mangle]
pub extern "C" fn tp_listenbrainz_has_token() -> bool {
    GLOBAL_LB
        .get()
        .and_then(|g| g.state.lock().ok().map(|s| s.cfg.has_token()))
        .unwrap_or(false)
}

#[no_mangle]
pub extern "C" fn tp_listenbrainz_is_token_invalid() -> bool {
    GLOBAL_LB
        .get()
        .and_then(|g| g.state.lock().ok().map(|s| s.auth_invalid))
        .unwrap_or(false)
}

#[no_mangle]
pub extern "C" fn tp_listenbrainz_cached_count() -> u32 {
    GLOBAL_LB
        .get()
        .and_then(|g| g.state.lock().ok().map(|s| s.queue.len() as u32))
        .unwrap_or(0)
}

#[no_mangle]
pub extern "C" fn tp_listenbrainz_has_active_track() -> bool {
    GLOBAL_LB
        .get()
        .and_then(|g| g.state.lock().ok().map(|s| s.active.is_some()))
        .unwrap_or(false)
}

#[no_mangle]
pub extern "C" fn tp_listenbrainz_clear_cache() {
    if let Some(g) = GLOBAL_LB.get() {
        if let Ok(mut st) = g.state.lock() {
            st.queue.clear();
        }
    }
}

#[no_mangle]
pub extern "C" fn tp_listenbrainz_logout() {
    if let Some(g) = GLOBAL_LB.get() {
        if let Ok(mut st) = g.state.lock() {
            st.cfg.user_token.clear();
            st.cfg.username.clear();
            st.auth_invalid = false;
            st.pending_np = None;
        }
    }
}

/// Validate the configured `user_token` against `GET {api_url}/1/validate-token`.
/// Returns `0` on valid token (`cfg.username` updated), `-1` on transient network failure,
/// `-2` on invalid token / bad URL.
#[no_mangle]
pub unsafe extern "C" fn tp_listenbrainz_validate_token(
    err_buf: *mut c_char,
    err_cap: usize,
) -> i32 {
    let Some(g) = GLOBAL_LB.get() else {
        write_c_str(err_buf, err_cap, "ListenBrainz not initialized");
        return -2;
    };
    validate_token_with(&g.transport, &g.state, err_buf, err_cap)
}

/// Non-blocking playback hook: called when a track starts playing (`audio_playlist_track_change`).
/// Returns `true` if background worker should wake up.
#[no_mangle]
pub unsafe extern "C" fn tp_listenbrainz_on_track_start(
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
    let Some(g) = GLOBAL_LB.get() else {
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

    if duration_ms > 30_000 && st.cfg.now_playing {
        st.pending_np = Some(PendingNowPlaying {
            artist: artist_s,
            track: track_s,
            album: album_s,
            album_artist: aa_s,
            track_number,
            duration_ms,
        });
    }

    true
}

/// Non-blocking periodic tick (~every 2s from background worker while a track is active).
/// Commits the scrobble to `listenbrainz_queue.tsv` immediately once `min(duration/2, 240s)` is reached.
#[no_mangle]
pub extern "C" fn tp_listenbrainz_on_playback_tick(now_mono_ms: u64, is_playing: bool) -> bool {
    let Some(g) = GLOBAL_LB.get() else {
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
        let allow_stream =
            !st.active.as_ref().map(|a| a.is_streaming).unwrap_or(false) || st.cfg.scrobble_streaming;
        if st.cfg.enabled && allow_stream {
            committed = st.queue.enqueue(scrob);
        }
    }
    committed
}

/// Non-blocking playback hook: called when a track ends or is skipped (`audio_playlist_track_finish`).
/// Returns `true` if pending scrobbles exist in the queue.
#[no_mangle]
pub extern "C" fn tp_listenbrainz_on_track_finish(now_mono_ms: u64) -> bool {
    let Some(g) = GLOBAL_LB.get() else {
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

/// Synchronously flush pending `Now Playing` (`playing_now`) and cached offline scrobbles
/// (`single` for 1 item, `import` for batches up to 50).
///
/// Returns:
/// - `>= 0`: Number of scrobbles flushed (0 if nothing queued).
/// - `-1`: Transient network / 429 / 5xx error (caller should back off; cached scrobbles are preserved).
/// - `-2`: Auth/config error (not signed in or 401 invalid token; cached scrobbles are preserved).
#[no_mangle]
pub unsafe extern "C" fn tp_listenbrainz_sync_now(
    now_unix: u64,
    out_cfg_updated: *mut bool,
    err_buf: *mut c_char,
    err_cap: usize,
) -> i32 {
    let Some(g) = GLOBAL_LB.get() else {
        if !out_cfg_updated.is_null() {
            unsafe { *out_cfg_updated = false };
        }
        write_c_str(err_buf, err_cap, "ListenBrainz not initialized");
        return -2;
    };
    sync_now_with(
        &g.transport,
        &g.state,
        now_unix,
        out_cfg_updated,
        err_buf,
        err_cap,
        250,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Clone, Debug)]
    struct CapturedReq {
        is_https: bool,
        host: String,
        port: u16,
        method: String,
        path: String,
        headers: String,
        body: String,
    }

    static TEST_SERIAL: Mutex<()> = Mutex::new(());
    static MOCK_REQS: Mutex<Vec<CapturedReq>> = Mutex::new(Vec::new());
    static MOCK_RESPS: Mutex<Vec<(i32, i32, String)>> = Mutex::new(Vec::new());

    unsafe extern "C" fn mock_http_req(
        is_https: u8,
        host: *const c_char,
        port: u16,
        method: *const c_char,
        path_and_query: *const c_char,
        extra_headers: *const c_char,
        req_body: *const u8,
        req_body_len: usize,
        _allow_self_signed: u8,
        _timeout_ms: i32,
        out_status: *mut i32,
        out_data: *mut *mut u8,
        out_len: *mut usize,
        err_buf: *mut c_char,
        err_cap: usize,
    ) -> i32 {
        let body_str = if !req_body.is_null() && req_body_len > 0 {
            String::from_utf8_lossy(unsafe { std::slice::from_raw_parts(req_body, req_body_len) })
                .into_owned()
        } else {
            String::new()
        };
        MOCK_REQS.lock().unwrap().push(CapturedReq {
            is_https: is_https != 0,
            host: c_str_to_string(host),
            port,
            method: c_str_to_string(method),
            path: c_str_to_string(path_and_query),
            headers: c_str_to_string(extra_headers),
            body: body_str,
        });

        let (rc, status, resp_body) = MOCK_RESPS.lock().unwrap().remove(0);
        if status <= 0 && rc != 0 {
            write_c_str(err_buf, err_cap, &resp_body);
            return rc;
        }
        unsafe { *out_status = status };
        if rc != 0 {
            write_c_str(err_buf, err_cap, &format!("HTTP {status}"));
        }
        let bytes = resp_body.into_bytes().into_boxed_slice();
        let len = bytes.len();
        let raw = Box::into_raw(bytes) as *mut u8;
        unsafe {
            *out_data = raw;
            *out_len = len;
        }
        rc
    }

    unsafe extern "C" fn mock_http_free(ptr: *mut u8) {
        if !ptr.is_null() {
            // Leak-free enough for tiny unit test buffers; allocated from boxed slice
            let _ = ptr;
        }
    }

    fn make_test_state(token: &str, username: &str) -> (Transport, Mutex<ListenBrainzState>) {
        let vt = TpLbHttpVtable {
            http_request: mock_http_req,
            http_free: mock_http_free,
        };
        let mut cfg = ListenBrainzConfig::default();
        cfg.user_token = token.to_string();
        cfg.username = username.to_string();
        (
            Transport { vt },
            Mutex::new(ListenBrainzState {
                cfg,
                queue: ScrobbleQueue::new(None),
                active: None,
                pending_np: None,
                auth_invalid: false,
                last_error: String::new(),
            }),
        )
    }

    #[test]
    fn test_end_to_end_validate_playing_now_and_import_and_errors() {
        let _guard = TEST_SERIAL.lock().unwrap();
        MOCK_REQS.lock().unwrap().clear();
        MOCK_RESPS.lock().unwrap().clear();

        let (transport, state) = make_test_state("uuid-tok-123", "");

        // Queue `playing_now` + 2 offline scrobbles
        {
            let mut st = state.lock().unwrap();
            st.pending_np = Some(PendingNowPlaying {
                artist: "C418".into(),
                track: "Sweden".into(),
                album: "Minecraft - Volume Alpha".into(),
                album_artist: String::new(),
                track_number: 3,
                duration_ms: 215_000,
            });
            st.queue.enqueue(QueuedScrobble {
                artist: "Koji Kondo".into(),
                track: "Overworld".into(),
                album: "Super Mario Bros.".into(),
                album_artist: String::new(),
                track_number: 1,
                duration_ms: 180_000,
                timestamp: 1_763_890_000,
            });
            st.queue.enqueue(QueuedScrobble {
                artist: "Tim Follin".into(),
                track: "Title Theme".into(),
                album: "Solstice (NSF)".into(),
                album_artist: String::new(),
                track_number: 1,
                duration_ms: 175_000,
                timestamp: 1_763_890_200,
            });
        }

        // Responses: 1) auto validate-token -> valid, 2) playing_now -> ok, 3) import -> ok
        {
            let mut resps = MOCK_RESPS.lock().unwrap();
            resps.push((
                0,
                200,
                r#"{"code":200,"message":"Token valid.","valid":true,"user_name":"brick_player"}"#
                    .into(),
            ));
            resps.push((0, 200, r#"{"status":"ok"}"#.into()));
            resps.push((0, 200, r#"{"status":"ok"}"#.into()));
        }

        let mut cfg_upd = false;
        let mut err = [0 as c_char; 128];
        let flushed = sync_now_with(
            &transport,
            &state,
            1_763_891_000,
            &mut cfg_upd,
            err.as_mut_ptr(),
            err.len(),
            0,
        );
        assert_eq!(flushed, 2);
        assert!(cfg_upd);
        assert_eq!(state.lock().unwrap().cfg.username, "brick_player");
        assert_eq!(state.lock().unwrap().queue.len(), 0);

        let reqs = MOCK_REQS.lock().unwrap().clone();
        assert_eq!(reqs.len(), 3);
        assert_eq!(reqs[0].method, "GET");
        assert_eq!(reqs[0].path, "/1/validate-token");
        assert!(reqs[0].headers.contains("Authorization: Token uuid-tok-123"));

        assert_eq!(reqs[1].method, "POST");
        assert_eq!(reqs[1].path, "/1/submit-listens");
        assert!(reqs[1].body.contains("\"listen_type\":\"playing_now\""));
        assert!(!reqs[1].body.contains("listened_at"));

        assert_eq!(reqs[2].method, "POST");
        assert!(reqs[2].body.contains("\"listen_type\":\"import\""));
        assert!(reqs[2].body.contains("\"listened_at\":1763890000"));
        assert!(reqs[2].body.contains("\"listened_at\":1763890200"));

        // Now test 401 Unauthorized: must preserve queue and mark auth_invalid
        {
            let mut st = state.lock().unwrap();
            st.queue.enqueue(QueuedScrobble {
                artist: "Nujabes".into(),
                track: "Feather".into(),
                album: "Modal Soul".into(),
                album_artist: String::new(),
                track_number: 1,
                duration_ms: 175_000,
                timestamp: 1_763_890_500,
            });
        }
        MOCK_RESPS.lock().unwrap().push((
            0,
            401,
            r#"{"code":401,"error":"You need to provide an Authorization header."}"#.into(),
        ));
        let rc_401 = sync_now_with(
            &transport,
            &state,
            1_763_891_000,
            std::ptr::null_mut(),
            err.as_mut_ptr(),
            err.len(),
            0,
        );
        assert_eq!(rc_401, -2);
        assert!(state.lock().unwrap().auth_invalid);
        assert_eq!(state.lock().unwrap().queue.len(), 1);

        // Reset auth_invalid; test 429 rate-limit -> transient (-1) & queue preserved
        state.lock().unwrap().auth_invalid = false;
        MOCK_RESPS
            .lock()
            .unwrap()
            .push((0, 429, r#"{"code":429,"error":"Too Many Requests"}"#.into()));
        let rc_429 = sync_now_with(
            &transport,
            &state,
            1_763_891_000,
            std::ptr::null_mut(),
            err.as_mut_ptr(),
            err.len(),
            0,
        );
        assert_eq!(rc_429, -1);
        assert!(!state.lock().unwrap().auth_invalid);
        assert_eq!(state.lock().unwrap().queue.len(), 1);

        // Test 400 poison-record isolation: 2 items in queue, first is rejected with 400,
        // second succeeds via `single` so queue never wedges permanently!
        {
            let mut st = state.lock().unwrap();
            st.queue.clear();
            st.queue.enqueue(QueuedScrobble {
                artist: "Bad\x07Artist".into(),
                track: "CorruptItem".into(),
                album: String::new(),
                album_artist: String::new(),
                track_number: 0,
                duration_ms: 180_000,
                timestamp: 1_763_890_600,
            });
            st.queue.enqueue(QueuedScrobble {
                artist: "Healthy Artist".into(),
                track: "Healthy Track".into(),
                album: String::new(),
                album_artist: String::new(),
                track_number: 1,
                duration_ms: 200_000,
                timestamp: 1_763_890_800,
            });
        }
        {
            let mut resps = MOCK_RESPS.lock().unwrap();
            // 1) import batch of 2 fails with 400
            resps.push((0, 400, r#"{"code":400,"error":"Bad payload"}"#.into()));
            // 2) isolated front item (`single`) also fails with 400 -> dropped!
            resps.push((0, 400, r#"{"code":400,"error":"Bad payload"}"#.into()));
            // 3) remaining healthy item (`single`) succeeds with 200 OK!
            resps.push((0, 200, r#"{"status":"ok"}"#.into()));
        }
        let flushed_after_400 = sync_now_with(
            &transport,
            &state,
            1_763_891_000,
            std::ptr::null_mut(),
            err.as_mut_ptr(),
            err.len(),
            0,
        );
        assert_eq!(flushed_after_400, 1);
        assert_eq!(state.lock().unwrap().queue.len(), 0);
    }

    #[test]
    fn test_c_transport_401_error_body_and_url_rewrite_end_to_end() {
        let _guard = TEST_SERIAL.lock().unwrap();
        MOCK_REQS.lock().unwrap().clear();
        MOCK_RESPS.lock().unwrap().clear();

        // Simulate user setting `https://listenbrainz.org/` (without `api.`) and a messy
        // `"Bearer my-clean-tok"\r\n` token — must hit `https://api.listenbrainz.org`
        // with exact `Authorization: Token my-clean-tok\r\n`.
        let (transport, state) = make_test_state("  \"Bearer my-clean-tok\"\r\n", "");
        {
            let mut st = state.lock().unwrap();
            st.cfg.api_url = "https://listenbrainz.org/".to_string();
            st.queue.enqueue(QueuedScrobble {
                artist: "Boards of Canada".into(),
                track: "Roygbiv".into(),
                album: "Music Has the Right to Children".into(),
                album_artist: String::new(),
                track_number: 10,
                duration_ms: 149_000,
                timestamp: 1_763_890_900,
            });
        }

        {
            let mut resps = MOCK_RESPS.lock().unwrap();
            resps.push((
                0,
                200,
                r#"{"code":200,"message":"Token valid.","valid":true,"user_name":"boc_fan"}"#
                    .into(),
            ));
            resps.push((0, 200, r#"{"status":"ok"}"#.into()));
        }

        let mut err = [0 as c_char; 160];
        let flushed = sync_now_with(
            &transport,
            &state,
            1_763_891_200,
            std::ptr::null_mut(),
            err.as_mut_ptr(),
            err.len(),
            0,
        );
        assert_eq!(flushed, 1);

        let reqs = MOCK_REQS.lock().unwrap().clone();
        assert_eq!(reqs.len(), 2);
        for r in &reqs {
            assert!(r.is_https);
            assert_eq!(r.host, "api.listenbrainz.org");
            assert_eq!(r.port, 443);
            assert!(
                r.headers.contains("Authorization: Token my-clean-tok\r\n"),
                "unexpected headers: {:?}",
                r.headers
            );
        }
        assert_eq!(reqs[0].path, "/1/validate-token");
        assert_eq!(reqs[1].path, "/1/submit-listens");

        // Now simulate real C `retrohh_http_request` behavior on HTTP 401:
        // returns `rc = -1` AND `out_status = 401` with JSON error body!
        // Must NOT be misclassified as transient `rc = -1`; must parse JSON error and set `auth_invalid`.
        MOCK_RESPS.lock().unwrap().push((
            -1,
            401,
            r#"{"code": 401, "error": "Invalid authorization token."}"#.into(),
        ));
        let vrc = validate_token_with(&transport, &state, err.as_mut_ptr(), err.len());
        assert_eq!(vrc, -2);
        assert!(state.lock().unwrap().auth_invalid);
        let err_msg = c_str_to_string(err.as_ptr());
        assert!(
            err_msg.contains("401") && err_msg.contains("Invalid authorization token."),
            "unexpected 401 message: {err_msg}"
        );

        // Simulate wrong custom server URL returning non-JSON HTML 404 (`rc = -1`, `status = 404`):
        // must report wrong server URL without marking token invalid!
        {
            let mut st = state.lock().unwrap();
            st.auth_invalid = false;
            st.cfg.api_url = "https://wrong-host.local/not-lb".to_string();
        }
        MOCK_RESPS
            .lock()
            .unwrap()
            .push((-1, 404, "<html><body>404 Not Found</body></html>".into()));
        let vrc_404 = validate_token_with(&transport, &state, err.as_mut_ptr(), err.len());
        assert_eq!(vrc_404, -2);
        assert!(!state.lock().unwrap().auth_invalid);
        let err_404 = c_str_to_string(err.as_ptr());
        assert!(
            err_404.contains("Wrong server URL") && err_404.contains("404"),
            "unexpected 404 message: {err_404}"
        );
    }
}
