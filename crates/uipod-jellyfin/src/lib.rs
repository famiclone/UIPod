//! Pure-Rust Jellyfin client, hardware-aware video stream/transcode planner,
//! SD-card video & audio downloader/cache, and `extern "C"` FFI for UIPod.

pub mod cache;
pub mod config;
pub mod json;
pub mod url;

use std::collections::{HashMap, HashSet};
use std::ffi::{c_char, c_void, CStr, CString};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

use cache::{HttpResponse, JfCacheManager, JfHttpTransport};
use config::{AudioQuality, JellyfinConfig, VideoQuality};
use json::{
    decode_audio_items_page, decode_playlists_list, decode_video_items_page, decode_views,
    JellyfinAudioSong, JellyfinVideoItem, JellyfinView,
};
use url::{plan_video_stream, ParsedJellyfinUrl, VideoPlanInput};

pub type TpJfCancelFn = Option<unsafe extern "C" fn(*mut c_void) -> u8>;

#[repr(C)]
#[derive(Clone, Copy)]
pub struct TpJfHttpVtable {
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
    pub http_download_ex: unsafe extern "C" fn(
        is_https: u8,
        host: *const c_char,
        port: u16,
        path_and_query: *const c_char,
        extra_headers: *const c_char,
        allow_self_signed: u8,
        dest_path: *const c_char,
        timeout_ms: i32,
        cancel_cb: TpJfCancelFn,
        cancel_ctx: *mut c_void,
        err_buf: *mut c_char,
        err_cap: usize,
    ) -> i32,
}

struct CJfTransport {
    vt: TpJfHttpVtable,
}

unsafe impl Send for CJfTransport {}
unsafe impl Sync for CJfTransport {}

unsafe extern "C" fn rust_closure_cancel_trampoline(ctx: *mut c_void) -> u8 {
    if ctx.is_null() {
        return 0;
    }
    let cb_ref = unsafe { &*(ctx as *const &(dyn Fn() -> bool + Sync)) };
    if cb_ref() {
        1
    } else {
        0
    }
}

impl JfHttpTransport for CJfTransport {
    fn request(
        &self,
        is_https: bool,
        host: &str,
        port: u16,
        method: &str,
        path_and_query: &str,
        extra_headers: &str,
        body: Option<&[u8]>,
        allow_self_signed: bool,
        timeout_ms: i32,
    ) -> Result<HttpResponse, String> {
        let c_host = CString::new(host).map_err(|_| "Invalid host".to_string())?;
        let c_method = CString::new(method).map_err(|_| "Invalid method".to_string())?;
        let c_pq = CString::new(path_and_query).map_err(|_| "Invalid path".to_string())?;
        let c_hdr = CString::new(extra_headers).map_err(|_| "Invalid headers".to_string())?;

        let (body_ptr, body_len) = match body {
            Some(b) if !b.is_empty() => (b.as_ptr(), b.len()),
            _ => (std::ptr::null(), 0usize),
        };

        let mut status: i32 = 0;
        let mut out_ptr: *mut u8 = std::ptr::null_mut();
        let mut out_len: usize = 0;
        let mut err_buf = [0u8; 128];

        let rc = unsafe {
            (self.vt.http_request)(
                if is_https { 1 } else { 0 },
                c_host.as_ptr(),
                port,
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

        let bytes = if !out_ptr.is_null() && out_len > 0 {
            let slice = unsafe { std::slice::from_raw_parts(out_ptr, out_len) };
            let vec = slice.to_vec();
            unsafe { (self.vt.http_free)(out_ptr) };
            vec
        } else {
            if !out_ptr.is_null() {
                unsafe { (self.vt.http_free)(out_ptr) };
            }
            Vec::new()
        };

        if status == 401 || status == 403 {
            return Ok(HttpResponse {
                status,
                body: bytes,
            });
        }

        if rc != 0 {
            return Err(c_buf_to_string(&err_buf, "Connection failed"));
        }

        Ok(HttpResponse {
            status: if status == 0 { 200 } else { status },
            body: bytes,
        })
    }

    fn download_to_file(
        &self,
        is_https: bool,
        host: &str,
        port: u16,
        path_and_query: &str,
        extra_headers: &str,
        allow_self_signed: bool,
        dest_path: &Path,
        timeout_ms: i32,
        cancel_fn: Option<&(dyn Fn() -> bool + Sync)>,
    ) -> Result<(), String> {
        let c_host = CString::new(host).map_err(|_| "Invalid host".to_string())?;
        let c_pq = CString::new(path_and_query).map_err(|_| "Invalid path".to_string())?;
        let c_hdr = CString::new(extra_headers).map_err(|_| "Invalid headers".to_string())?;
        let dest_str = dest_path
            .to_str()
            .ok_or_else(|| "Invalid dest path".to_string())?;
        let c_dest = CString::new(dest_str).map_err(|_| "Invalid dest path".to_string())?;
        let mut err_buf = [0u8; 128];

        let (cb_ptr, cb_ctx): (TpJfCancelFn, *mut c_void) = if let Some(ref f) = cancel_fn {
            (
                Some(rust_closure_cancel_trampoline),
                f as *const &(dyn Fn() -> bool + Sync) as *mut c_void,
            )
        } else {
            (None, std::ptr::null_mut())
        };

        let rc = unsafe {
            (self.vt.http_download_ex)(
                if is_https { 1 } else { 0 },
                c_host.as_ptr(),
                port,
                c_pq.as_ptr(),
                c_hdr.as_ptr(),
                if allow_self_signed { 1 } else { 0 },
                c_dest.as_ptr(),
                timeout_ms,
                cb_ptr,
                cb_ctx,
                err_buf.as_mut_ptr() as *mut c_char,
                err_buf.len(),
            )
        };

        if rc != 0 {
            return Err(c_buf_to_string(&err_buf, "Download failed"));
        }
        Ok(())
    }
}

#[repr(C)]
pub struct TpJfConfigC {
    pub url: [u8; 256],
    pub username: [u8; 128],
    pub password: [u8; 128],
    pub video_quality: u8,
    pub audio_quality: u8,
    pub allow_self_signed: u8,
    pub _pad: u8,
    pub cache_limit_mb: u32,
}

#[repr(C)]
pub struct TpJfViewC {
    pub id: *const c_char,
    pub name: *const c_char,
    pub collection_type: *const c_char,
    pub view_kind: u8,
}

#[repr(C)]
pub struct TpJfVideoItemC {
    pub id: *const c_char,
    pub view_id: *const c_char,
    pub item_type: u8,
    pub played: u8,
    pub name: *const c_char,
    pub series_id: *const c_char,
    pub series_name: *const c_char,
    pub season_id: *const c_char,
    pub season_name: *const c_char,
    pub season_number: i32,
    pub episode_number: i32,
    pub year: i32,
    pub duration_ms: u32,
    pub resume_ms: u32,
    pub media_source_id: *const c_char,
    pub container: *const c_char,
    pub video_codec: *const c_char,
    pub audio_codec: *const c_char,
    pub width: u32,
    pub height: u32,
    pub ratio_num: u32,
    pub ratio_den: u32,
    pub bit_depth: u32,
    pub bitrate: u32,
    pub chapters_csv: *const c_char,
    pub local_path: *const c_char,
}

#[repr(C)]
pub struct TpJfAudioSongC {
    pub id: *const c_char,
    pub local_track_path: *const c_char,
    pub local_cover_path: *const c_char,
    pub cover_id: *const c_char,
    pub server_path: *const c_char,
    pub suffix: *const c_char,
    pub title: *const c_char,
    pub artist: *const c_char,
    pub album_artist: *const c_char,
    pub album: *const c_char,
    pub genre: *const c_char,
    pub year: i32,
    pub track: i32,
    pub disc_number: i32,
    pub duration_secs: u32,
    pub size: u64,
    pub bit_rate: u32,
    pub bit_depth: u32,
    pub sampling_rate: u32,
}

#[repr(C)]
pub struct TpJfPlannedStreamC {
    pub is_https: u8,
    pub allow_self_signed: u8,
    pub is_transcoded: u8,
    pub _pad: u8,
    pub port: u16,
    pub _pad2: u16,
    pub host: [u8; 256],
    pub path_and_query: [u8; 1024],
    pub extra_headers: [u8; 512],
    pub download_ext: [u8; 16],
}

pub type TpJfOnViewFn = Option<unsafe extern "C" fn(ctx: *mut c_void, view: *const TpJfViewC)>;
pub type TpJfOnVideoFn = Option<unsafe extern "C" fn(ctx: *mut c_void, item: *const TpJfVideoItemC)>;
pub type TpJfOnSongFn = Option<unsafe extern "C" fn(ctx: *mut c_void, song: *const TpJfAudioSongC)>;
pub type TpJfOnPlaylistFn = Option<
    unsafe extern "C" fn(
        ctx: *mut c_void,
        playlist_name: *const c_char,
        m3u8_content: *const c_char,
    ),
>;
pub type TpJfOnProgressFn =
    Option<unsafe extern "C" fn(ctx: *mut c_void, synced_videos: u32, synced_tracks: u32)>;

static CACHE_SLOT: OnceLock<Mutex<Option<Arc<JfCacheManager>>>> = OnceLock::new();

fn cache_slot() -> &'static Mutex<Option<Arc<JfCacheManager>>> {
    CACHE_SLOT.get_or_init(|| Mutex::new(None))
}

fn current_cache() -> Option<Arc<JfCacheManager>> {
    cache_slot().lock().unwrap().clone()
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

fn write_str_to_c_buf(s: &str, out: &mut [u8]) -> usize {
    if out.is_empty() {
        return 0;
    }
    let bytes = s.as_bytes();
    let max_copy = bytes.len().min(out.len() - 1);
    out[..max_copy].copy_from_slice(&bytes[..max_copy]);
    out[max_copy] = 0;
    max_copy
}

unsafe fn write_err_ptr(err_buf: *mut c_char, err_cap: usize, msg: &str) {
    if err_buf.is_null() || err_cap == 0 {
        return;
    }
    let slice = unsafe { std::slice::from_raw_parts_mut(err_buf as *mut u8, err_cap) };
    write_str_to_c_buf(msg, slice);
}

unsafe fn opt_cstr(ptr: *const c_char) -> String {
    if ptr.is_null() {
        String::new()
    } else {
        unsafe { CStr::from_ptr(ptr) }
            .to_string_lossy()
            .into_owned()
    }
}

fn to_cstring_lossy(s: &str) -> CString {
    CString::new(
        s.as_bytes()
            .iter()
            .copied()
            .filter(|&b| b != 0)
            .collect::<Vec<u8>>(),
    )
    .unwrap_or_default()
}

struct SyncCancelCtx {
    cb: TpJfCancelFn,
    ctx: *mut c_void,
}

unsafe impl Send for SyncCancelCtx {}
unsafe impl Sync for SyncCancelCtx {}

impl SyncCancelCtx {
    fn is_cancelled(&self) -> bool {
        if let Some(f) = self.cb {
            unsafe { f(self.ctx) != 0 }
        } else {
            false
        }
    }
}

/// High-level Jellyfin library synchronization (unit-testable with any `JfHttpTransport`).
#[allow(clippy::too_many_arguments)]
pub fn sync_jellyfin_catalog(
    cache: &JfCacheManager,
    mut on_view: impl FnMut(&JellyfinView),
    mut on_video: impl FnMut(&str, &JellyfinVideoItem, &Path),
    mut on_song: impl FnMut(&JellyfinAudioSong, &Path, &Path),
    mut on_playlist: impl FnMut(&str, &str),
    mut on_progress: impl FnMut(u32, u32),
    cancel_fn: Option<&(dyn Fn() -> bool + Sync)>,
) -> Result<(u32, u32), String> {
    let (parsed, tok, _uid, cfg) = cache.ensure_authenticated(false)?;

    let views_bytes = cache.authed_get_json(|u, uid, _| u.user_views_path(uid), 12_000)?;
    let views = decode_views(&views_bytes)?;

    let mut total_videos = 0u32;
    let mut total_tracks = 0u32;
    on_progress(0, 0);

    let mut used_video_paths: HashSet<PathBuf> = HashSet::new();
    let mut seen_video_ids: HashSet<String> = HashSet::new();

    for view in &views {
        on_view(view);
    }

    // 1. Sync video items per non-music view
    const VIDEO_PAGE: usize = 200;
    const MAX_VIDEOS_PER_VIEW: usize = 50_000;
    for view in &views {
        if view.view_kind() == 2 {
            continue;
        }
        let mut start_idx = 0usize;
        while start_idx < MAX_VIDEOS_PER_VIEW {
            if let Some(cb) = cancel_fn {
                if cb() {
                    return Err("Cancelled".to_string());
                }
            }
            let view_id = view.id.clone();
            let page_bytes = cache.authed_get_json(
                |u, uid, _| u.video_items_path(uid, &view_id, start_idx, VIDEO_PAGE),
                25_000,
            )?;
            let (batch, total_count) = decode_video_items_page(&page_bytes)?;
            if batch.is_empty() {
                break;
            }
            let batch_len = batch.len();
            for item in &batch {
                if !seen_video_ids.insert(format!("{}:{}", view.id, item.id)) {
                    continue;
                }
                let local_path = if item.is_playable() {
                    let plan = plan_video_stream(
                        &parsed,
                        &VideoPlanInput {
                            item_id: &item.id,
                            media_source_id: &item.media_source_id,
                            container: &item.container,
                            video_codec: &item.video_codec,
                            audio_codec: &item.audio_codec,
                            width: item.width,
                            height: item.height,
                            bit_depth: item.bit_depth,
                            bitrate: item.bitrate,
                            quality: cfg.video_quality,
                            start_ms: 0,
                            play_session_id: "sync",
                            token: &tok,
                        },
                    );
                    let mut p = cache::build_video_local_path(
                        cache.video_root(),
                        item,
                        &plan.download_ext,
                    );
                    if !used_video_paths.insert(p.clone()) {
                        p = cache::disambiguate_media_path(&p, &item.id);
                        used_video_paths.insert(p.clone());
                    }
                    total_videos += 1;
                    p
                } else {
                    PathBuf::new()
                };
                on_video(&view.id, item, &local_path);
            }

            on_progress(total_videos, total_tracks);

            start_idx += batch_len;
            if batch_len < VIDEO_PAGE || (total_count > 0 && start_idx >= total_count) {
                break;
            }
        }
    }

    // 2. Sync audio tracks globally across user's accessible libraries
    const AUDIO_PAGE: usize = 300;
    const MAX_AUDIO: usize = 200_000;
    let mut audio_start = 0usize;
    let mut id_to_audio_path: HashMap<String, PathBuf> = HashMap::new();
    let mut used_audio_paths: HashSet<PathBuf> = HashSet::new();

    let mut resolve_song_path = |song: &JellyfinAudioSong| -> (PathBuf, bool) {
        if let Some(existing) = id_to_audio_path.get(&song.id) {
            return (existing.clone(), false);
        }
        let artist_dir = if !song.album_artist.trim().is_empty() {
            &song.album_artist
        } else {
            &song.artist
        };
        let mut tp = cache::build_audio_track_path(
            cache.audio_root(),
            artist_dir,
            &song.album,
            song.track,
            &song.title,
            &song.id,
            &song.suffix,
            cfg.audio_quality,
        );
        if !used_audio_paths.insert(tp.clone()) {
            tp = cache::disambiguate_media_path(&tp, &song.id);
            used_audio_paths.insert(tp.clone());
        }
        id_to_audio_path.insert(song.id.clone(), tp.clone());
        (tp, true)
    };

    while audio_start < MAX_AUDIO {
        if let Some(cb) = cancel_fn {
            if cb() {
                return Err("Cancelled".to_string());
            }
        }
        let page_bytes = cache.authed_get_json(
            |u, uid, _| u.audio_items_path(uid, audio_start, AUDIO_PAGE),
            25_000,
        )?;
        let (batch, total_count) = decode_audio_items_page(&page_bytes)?;
        if batch.is_empty() {
            break;
        }
        let batch_len = batch.len();
        for song in &batch {
            let (tp, is_new) = resolve_song_path(song);
            if !is_new {
                continue;
            }
            let cp = cache.cover_path_for_track(&tp, &song.cover_id);
            on_song(song, &tp, &cp);
            total_tracks += 1;
        }
        on_progress(total_videos, total_tracks);

        audio_start += batch_len;
        if batch_len < AUDIO_PAGE || (total_count > 0 && audio_start >= total_count) {
            break;
        }
    }

    // 3. Sync Jellyfin audio playlists -> M3U8
    if let Ok(pls_bytes) = cache.authed_get_json(|u, uid, _| u.playlists_path(uid), 10_000) {
        if let Ok(pls) = decode_playlists_list(&pls_bytes) {
            for pl in pls {
                if let Some(cb) = cancel_fn {
                    if cb() {
                        return Err("Cancelled".to_string());
                    }
                }
                let pl_id = pl.id.clone();
                if let Ok(items_bytes) = cache
                    .authed_get_json(|u, uid, _| u.playlist_items_path(&pl_id, uid), 12_000)
                {
                    if let Ok((songs, _)) = decode_audio_items_page(&items_bytes) {
                        if songs.is_empty() {
                            continue;
                        }
                        let mut m3u = String::from("#EXTM3U\n");
                        for song in &songs {
                            let (tp, is_new) = resolve_song_path(song);
                            if is_new {
                                let cp = cache.cover_path_for_track(&tp, &song.cover_id);
                                on_song(song, &tp, &cp);
                                total_tracks += 1;
                            }
                            m3u.push_str(&tp.to_string_lossy());
                            m3u.push('\n');
                        }
                        on_playlist(&pl.name, &m3u);
                    }
                }
            }
        }
    }

    Ok((total_videos, total_tracks))
}

// ============================================================================
// C ABI Exports (`tp_jellyfin_*`)
// ============================================================================

#[no_mangle]
pub unsafe extern "C" fn tp_jellyfin_init(
    video_root_utf8: *const c_char,
    audio_root_utf8: *const c_char,
    vtable: *const TpJfHttpVtable,
) -> bool {
    if video_root_utf8.is_null() || audio_root_utf8.is_null() || vtable.is_null() {
        return false;
    }
    let vroot = unsafe { opt_cstr(video_root_utf8) };
    let aroot = unsafe { opt_cstr(audio_root_utf8) };
    if vroot.is_empty() || aroot.is_empty() {
        return false;
    }
    let vt = unsafe { *vtable };
    let transport = Arc::new(CJfTransport { vt });
    let mgr = JfCacheManager::new(PathBuf::from(vroot), PathBuf::from(aroot), transport);
    *cache_slot().lock().unwrap() = Some(mgr);
    true
}

#[no_mangle]
pub extern "C" fn tp_jellyfin_shutdown() {
    if let Some(mgr) = cache_slot().lock().unwrap().take() {
        mgr.shutdown();
    }
}

#[no_mangle]
pub unsafe extern "C" fn tp_jellyfin_load_config_text(ini_utf8: *const c_char) -> bool {
    let text = unsafe { opt_cstr(ini_utf8) };
    let cfg = JellyfinConfig::parse_ini(&text);
    let Some(mgr) = current_cache() else {
        return false;
    };
    mgr.set_config(cfg);
    true
}

#[no_mangle]
pub unsafe extern "C" fn tp_jellyfin_format_config_text(
    out_buf: *mut c_char,
    out_cap: usize,
) -> usize {
    if out_buf.is_null() || out_cap == 0 {
        return 0;
    }
    let Some(mgr) = current_cache() else {
        return 0;
    };
    let ini = mgr.get_config().to_ini();
    let slice = unsafe { std::slice::from_raw_parts_mut(out_buf as *mut u8, out_cap) };
    write_str_to_c_buf(&ini, slice)
}

#[no_mangle]
pub unsafe extern "C" fn tp_jellyfin_get_config(out_cfg: *mut TpJfConfigC) -> bool {
    if out_cfg.is_null() {
        return false;
    }
    let Some(mgr) = current_cache() else {
        return false;
    };
    let cfg = mgr.get_config();
    let dst = unsafe { &mut *out_cfg };
    dst.url.fill(0);
    dst.username.fill(0);
    dst.password.fill(0);
    write_str_to_c_buf(&cfg.url, &mut dst.url);
    write_str_to_c_buf(&cfg.username, &mut dst.username);
    write_str_to_c_buf(&cfg.password, &mut dst.password);
    dst.video_quality = cfg.video_quality as u8;
    dst.audio_quality = cfg.audio_quality as u8;
    dst.allow_self_signed = if cfg.allow_self_signed { 1 } else { 0 };
    dst._pad = 0;
    dst.cache_limit_mb = cfg.cache_limit_mb;
    true
}

#[no_mangle]
pub unsafe extern "C" fn tp_jellyfin_set_config(in_cfg: *const TpJfConfigC) -> bool {
    if in_cfg.is_null() {
        return false;
    }
    let Some(mgr) = current_cache() else {
        return false;
    };
    let src = unsafe { &*in_cfg };
    let cfg = JellyfinConfig {
        url: c_buf_to_string(&src.url, ""),
        username: c_buf_to_string(&src.username, ""),
        password: c_buf_to_string(&src.password, ""),
        access_token: String::new(),
        user_id: String::new(),
        video_quality: VideoQuality::from_u8(src.video_quality),
        audio_quality: AudioQuality::from_u8(src.audio_quality),
        cache_limit_mb: src.cache_limit_mb,
        allow_self_signed: src.allow_self_signed != 0,
    };
    mgr.set_config(cfg);
    true
}

#[no_mangle]
pub extern "C" fn tp_jellyfin_is_configured() -> bool {
    current_cache()
        .map(|m| m.get_config().is_configured())
        .unwrap_or(false)
}

#[no_mangle]
pub unsafe extern "C" fn tp_jellyfin_sync_catalog(
    on_view: TpJfOnViewFn,
    on_video: TpJfOnVideoFn,
    on_song: TpJfOnSongFn,
    on_playlist: TpJfOnPlaylistFn,
    on_progress: TpJfOnProgressFn,
    cb_ctx: *mut c_void,
    cancel_cb: TpJfCancelFn,
    cancel_ctx: *mut c_void,
    out_videos: *mut u32,
    out_tracks: *mut u32,
    err_buf: *mut c_char,
    err_cap: usize,
) -> i32 {
    let Some(mgr) = current_cache() else {
        unsafe { write_err_ptr(err_buf, err_cap, "Jellyfin not initialized") };
        return -1;
    };

    let cctx = SyncCancelCtx {
        cb: cancel_cb,
        ctx: cancel_ctx,
    };
    let cancel_closure = || cctx.is_cancelled();
    let cancel_dyn: &(dyn Fn() -> bool + Sync) = &cancel_closure;

    let res = sync_jellyfin_catalog(
        &mgr,
        |v| {
            if let Some(f) = on_view {
                let c_id = to_cstring_lossy(&v.id);
                let c_name = to_cstring_lossy(&v.name);
                let c_ct = to_cstring_lossy(&v.collection_type);
                let vc = TpJfViewC {
                    id: c_id.as_ptr(),
                    name: c_name.as_ptr(),
                    collection_type: c_ct.as_ptr(),
                    view_kind: v.view_kind(),
                };
                unsafe { f(cb_ctx, &vc) };
            }
        },
        |view_id, item, local_path| {
            if let Some(f) = on_video {
                let c_id = to_cstring_lossy(&item.id);
                let c_vid = to_cstring_lossy(view_id);
                let c_name = to_cstring_lossy(&item.name);
                let c_ser_id = to_cstring_lossy(&item.series_id);
                let c_ser_name = to_cstring_lossy(&item.series_name);
                let c_sea_id = to_cstring_lossy(&item.season_id);
                let c_sea_name = to_cstring_lossy(&item.season_name);
                let c_msid = to_cstring_lossy(&item.media_source_id);
                let c_cont = to_cstring_lossy(&item.container);
                let c_vcod = to_cstring_lossy(&item.video_codec);
                let c_acod = to_cstring_lossy(&item.audio_codec);
                let c_chap = to_cstring_lossy(&item.chapters_csv);
                let c_lpath = to_cstring_lossy(&local_path.to_string_lossy());

                let ic = TpJfVideoItemC {
                    id: c_id.as_ptr(),
                    view_id: c_vid.as_ptr(),
                    item_type: item.item_type,
                    played: if item.played { 1 } else { 0 },
                    name: c_name.as_ptr(),
                    series_id: c_ser_id.as_ptr(),
                    series_name: c_ser_name.as_ptr(),
                    season_id: c_sea_id.as_ptr(),
                    season_name: c_sea_name.as_ptr(),
                    season_number: item.season_number,
                    episode_number: item.episode_number,
                    year: item.year,
                    duration_ms: item.duration_ms,
                    resume_ms: item.resume_ms,
                    media_source_id: c_msid.as_ptr(),
                    container: c_cont.as_ptr(),
                    video_codec: c_vcod.as_ptr(),
                    audio_codec: c_acod.as_ptr(),
                    width: item.width,
                    height: item.height,
                    ratio_num: item.ratio_num,
                    ratio_den: item.ratio_den,
                    bit_depth: item.bit_depth,
                    bitrate: item.bitrate,
                    chapters_csv: c_chap.as_ptr(),
                    local_path: c_lpath.as_ptr(),
                };
                unsafe { f(cb_ctx, &ic) };
            }
        },
        |song, tp, cp| {
            if let Some(f) = on_song {
                let c_id = to_cstring_lossy(&song.id);
                let c_tp = to_cstring_lossy(&tp.to_string_lossy());
                let c_cp = to_cstring_lossy(&cp.to_string_lossy());
                let c_cov = to_cstring_lossy(&song.cover_id);
                let c_sp = to_cstring_lossy(&song.server_path);
                let c_suf = to_cstring_lossy(&song.suffix);
                let c_tit = to_cstring_lossy(&song.title);
                let c_art = to_cstring_lossy(&song.artist);
                let c_aa = to_cstring_lossy(&song.album_artist);
                let c_alb = to_cstring_lossy(&song.album);
                let c_gen = to_cstring_lossy(&song.genre);

                let sc = TpJfAudioSongC {
                    id: c_id.as_ptr(),
                    local_track_path: c_tp.as_ptr(),
                    local_cover_path: c_cp.as_ptr(),
                    cover_id: c_cov.as_ptr(),
                    server_path: c_sp.as_ptr(),
                    suffix: c_suf.as_ptr(),
                    title: c_tit.as_ptr(),
                    artist: c_art.as_ptr(),
                    album_artist: c_aa.as_ptr(),
                    album: c_alb.as_ptr(),
                    genre: c_gen.as_ptr(),
                    year: song.year,
                    track: song.track,
                    disc_number: song.disc_number,
                    duration_secs: song.duration_secs,
                    size: song.size,
                    bit_rate: song.bit_rate,
                    bit_depth: song.bit_depth,
                    sampling_rate: song.sampling_rate,
                };
                unsafe { f(cb_ctx, &sc) };
            }
        },
        |pl_name, m3u| {
            if let Some(f) = on_playlist {
                let c_n = to_cstring_lossy(pl_name);
                let c_m = to_cstring_lossy(m3u);
                unsafe { f(cb_ctx, c_n.as_ptr(), c_m.as_ptr()) };
            }
        },
        |vids, trks| {
            if let Some(f) = on_progress {
                unsafe { f(cb_ctx, vids, trks) };
            }
        },
        Some(cancel_dyn),
    );

    match res {
        Ok((vids, trks)) => {
            if !out_videos.is_null() {
                unsafe { *out_videos = vids };
            }
            if !out_tracks.is_null() {
                unsafe { *out_tracks = trks };
            }
            0
        }
        Err(e) => {
            unsafe { write_err_ptr(err_buf, err_cap, &e) };
            if e == "Cancelled" {
                -2
            } else {
                -1
            }
        }
    }
}

#[no_mangle]
pub unsafe extern "C" fn tp_jellyfin_plan_video_stream(
    item_id_utf8: *const c_char,
    media_source_id_utf8: *const c_char,
    container_utf8: *const c_char,
    video_codec_utf8: *const c_char,
    audio_codec_utf8: *const c_char,
    width: u32,
    height: u32,
    bit_depth: u32,
    bitrate: u32,
    start_ms: u32,
    play_session_id_utf8: *const c_char,
    out_plan: *mut TpJfPlannedStreamC,
    err_buf: *mut c_char,
    err_cap: usize,
) -> i32 {
    if out_plan.is_null() {
        return -1;
    }
    let Some(mgr) = current_cache() else {
        unsafe { write_err_ptr(err_buf, err_cap, "Not initialized") };
        return -1;
    };

    let (parsed, tok, _uid, cfg) = match mgr.ensure_authenticated(false) {
        Ok(v) => v,
        Err(e) => {
            unsafe { write_err_ptr(err_buf, err_cap, &e) };
            return -1;
        }
    };

    let item_id = unsafe { opt_cstr(item_id_utf8) };
    let msid = unsafe { opt_cstr(media_source_id_utf8) };
    let container = unsafe { opt_cstr(container_utf8) };
    let vcodec = unsafe { opt_cstr(video_codec_utf8) };
    let acodec = unsafe { opt_cstr(audio_codec_utf8) };
    let sid = unsafe { opt_cstr(play_session_id_utf8) };

    let plan = plan_video_stream(
        &parsed,
        &VideoPlanInput {
            item_id: &item_id,
            media_source_id: &msid,
            container: &container,
            video_codec: &vcodec,
            audio_codec: &acodec,
            width,
            height,
            bit_depth,
            bitrate,
            quality: cfg.video_quality,
            start_ms,
            play_session_id: &sid,
            token: &tok,
        },
    );

    let headers = ParsedJellyfinUrl::stream_headers(Some(&tok));
    let dst = unsafe { &mut *out_plan };
    dst.is_https = if parsed.is_https { 1 } else { 0 };
    dst.allow_self_signed = if cfg.allow_self_signed { 1 } else { 0 };
    dst.is_transcoded = if plan.is_transcoded { 1 } else { 0 };
    dst._pad = 0;
    dst.port = parsed.port;
    dst._pad2 = 0;
    dst.host.fill(0);
    dst.path_and_query.fill(0);
    dst.extra_headers.fill(0);
    dst.download_ext.fill(0);
    write_str_to_c_buf(&parsed.host, &mut dst.host);
    write_str_to_c_buf(&plan.path_and_query, &mut dst.path_and_query);
    write_str_to_c_buf(&headers, &mut dst.extra_headers);
    write_str_to_c_buf(&plan.download_ext, &mut dst.download_ext);
    0
}

#[no_mangle]
pub unsafe extern "C" fn tp_jellyfin_resolve_downloaded_video(
    planned_local_path_utf8: *const c_char,
    out_buf: *mut c_char,
    out_cap: usize,
) -> usize {
    if planned_local_path_utf8.is_null() || out_buf.is_null() || out_cap == 0 {
        return 0;
    }
    let p_str = unsafe { opt_cstr(planned_local_path_utf8) };
    if p_str.is_empty() {
        return 0;
    }
    let Some(found) = cache::resolve_existing_video_path(Path::new(&p_str)) else {
        return 0;
    };
    let slice = unsafe { std::slice::from_raw_parts_mut(out_buf as *mut u8, out_cap) };
    write_str_to_c_buf(&found.to_string_lossy(), slice)
}

#[no_mangle]
pub unsafe extern "C" fn tp_jellyfin_delete_downloaded_video(
    planned_local_path_utf8: *const c_char,
) -> bool {
    if planned_local_path_utf8.is_null() {
        return false;
    }
    let p_str = unsafe { opt_cstr(planned_local_path_utf8) };
    if p_str.is_empty() {
        return false;
    }
    let mut removed = false;
    while let Some(found) = cache::resolve_existing_video_path(Path::new(&p_str)) {
        if fs::remove_file(&found).is_ok() {
            removed = true;
        } else {
            break;
        }
    }
    removed
}

#[no_mangle]
pub unsafe extern "C" fn tp_jellyfin_download_video(
    planned_local_path_utf8: *const c_char,
    item_id_utf8: *const c_char,
    media_source_id_utf8: *const c_char,
    container_utf8: *const c_char,
    video_codec_utf8: *const c_char,
    audio_codec_utf8: *const c_char,
    width: u32,
    height: u32,
    bit_depth: u32,
    bitrate: u32,
    cancel_cb: TpJfCancelFn,
    cancel_ctx: *mut c_void,
    err_buf: *mut c_char,
    err_cap: usize,
) -> i32 {
    let Some(mgr) = current_cache() else {
        unsafe { write_err_ptr(err_buf, err_cap, "Not initialized") };
        return -1;
    };
    let planned_str = unsafe { opt_cstr(planned_local_path_utf8) };
    let item_id = unsafe { opt_cstr(item_id_utf8) };
    if planned_str.is_empty() || item_id.is_empty() {
        unsafe { write_err_ptr(err_buf, err_cap, "Invalid video item") };
        return -1;
    }
    let msid = unsafe { opt_cstr(media_source_id_utf8) };
    let container = unsafe { opt_cstr(container_utf8) };
    let vcodec = unsafe { opt_cstr(video_codec_utf8) };
    let acodec = unsafe { opt_cstr(audio_codec_utf8) };

    let cctx = SyncCancelCtx {
        cb: cancel_cb,
        ctx: cancel_ctx,
    };
    let cancel_closure = || cctx.is_cancelled();
    let cancel_dyn: &(dyn Fn() -> bool + Sync) = &cancel_closure;

    match mgr.download_video_item(
        Path::new(&planned_str),
        &item_id,
        &msid,
        &container,
        &vcodec,
        &acodec,
        width,
        height,
        bit_depth,
        bitrate,
        Some(cancel_dyn),
    ) {
        Ok(_) => 0,
        Err(e) => {
            unsafe { write_err_ptr(err_buf, err_cap, &e) };
            if e == "Cancelled" {
                -2
            } else {
                -1
            }
        }
    }
}

#[no_mangle]
pub unsafe extern "C" fn tp_jellyfin_stop_encoding(play_session_id_utf8: *const c_char) {
    let sid = unsafe { opt_cstr(play_session_id_utf8) };
    if let Some(mgr) = current_cache() {
        mgr.stop_active_encoding(&sid);
    }
}

#[no_mangle]
pub unsafe extern "C" fn tp_jellyfin_report_session(
    endpoint_kind: u8,
    item_id_utf8: *const c_char,
    media_source_id_utf8: *const c_char,
    play_session_id_utf8: *const c_char,
    position_ms: u32,
    is_paused: u8,
) {
    let Some(mgr) = current_cache() else {
        return;
    };
    let item_id = unsafe { opt_cstr(item_id_utf8) };
    let msid = unsafe { opt_cstr(media_source_id_utf8) };
    let sid = unsafe { opt_cstr(play_session_id_utf8) };
    mgr.enqueue_session_report(
        endpoint_kind,
        &item_id,
        &msid,
        &sid,
        position_ms,
        is_paused != 0,
    );
}

#[no_mangle]
pub unsafe extern "C" fn tp_jellyfin_is_audio_track_path(path_utf8: *const c_char) -> bool {
    if path_utf8.is_null() {
        return false;
    }
    let p = unsafe { opt_cstr(path_utf8) };
    current_cache()
        .map(|m| m.is_audio_track_path(&p))
        .unwrap_or(false)
}

#[no_mangle]
pub unsafe extern "C" fn tp_jellyfin_ensure_audio_cached(
    local_path_utf8: *const c_char,
    song_id_utf8: *const c_char,
    suffix_utf8: *const c_char,
    cover_id_utf8: *const c_char,
    pin: u8,
    cancel_cb: TpJfCancelFn,
    cancel_ctx: *mut c_void,
    err_buf: *mut c_char,
    err_cap: usize,
) -> i32 {
    let Some(mgr) = current_cache() else {
        unsafe { write_err_ptr(err_buf, err_cap, "Not initialized") };
        return -1;
    };
    let local_path = PathBuf::from(unsafe { opt_cstr(local_path_utf8) });
    let song_id = unsafe { opt_cstr(song_id_utf8) };
    let suffix = unsafe { opt_cstr(suffix_utf8) };
    let cover_id = unsafe { opt_cstr(cover_id_utf8) };

    if JfCacheManager::file_ready(&local_path) && pin == 0 {
        if !cover_id.is_empty() {
            mgr.enqueue_cover_for_track(&local_path, &cover_id);
        }
        return 0;
    }
    if song_id.is_empty() {
        unsafe { write_err_ptr(err_buf, err_cap, "Unknown Jellyfin track") };
        return -1;
    }

    let cctx = SyncCancelCtx {
        cb: cancel_cb,
        ctx: cancel_ctx,
    };
    let cancel_closure = || cctx.is_cancelled();
    let cancel_dyn: &(dyn Fn() -> bool + Sync) = &cancel_closure;

    match mgr.prepare_audio_track(
        &local_path,
        &song_id,
        &suffix,
        &cover_id,
        pin != 0,
        Some(cancel_dyn),
    ) {
        Ok(_) => 0,
        Err(e) => {
            unsafe { write_err_ptr(err_buf, err_cap, &e) };
            if e == "Cancelled" {
                -2
            } else {
                -1
            }
        }
    }
}

#[no_mangle]
pub unsafe extern "C" fn tp_jellyfin_prefetch_audio(
    local_path_utf8: *const c_char,
    song_id_utf8: *const c_char,
    suffix_utf8: *const c_char,
    cover_id_utf8: *const c_char,
) {
    let Some(mgr) = current_cache() else {
        return;
    };
    let local_path = PathBuf::from(unsafe { opt_cstr(local_path_utf8) });
    let song_id = unsafe { opt_cstr(song_id_utf8) };
    let suffix = unsafe { opt_cstr(suffix_utf8) };
    let cover_id = unsafe { opt_cstr(cover_id_utf8) };
    if song_id.is_empty() {
        return;
    }
    mgr.enqueue_prefetch(local_path, song_id, suffix, cover_id);
}

#[no_mangle]
pub unsafe extern "C" fn tp_jellyfin_cover_path_for(
    local_track_path_utf8: *const c_char,
    cover_id_utf8: *const c_char,
    out_buf: *mut c_char,
    out_cap: usize,
) -> usize {
    if out_buf.is_null() || out_cap == 0 {
        return 0;
    }
    let Some(mgr) = current_cache() else {
        return 0;
    };
    let tp = PathBuf::from(unsafe { opt_cstr(local_track_path_utf8) });
    let cid = unsafe { opt_cstr(cover_id_utf8) };
    let cp = mgr.cover_path_for_track(&tp, &cid);
    if !cid.is_empty() && !JfCacheManager::file_ready(&cp) {
        let _ = mgr.ensure_cover_at_path(&cp, &cid);
    }
    if !JfCacheManager::file_ready(&cp) {
        return 0;
    }
    let slice = unsafe { std::slice::from_raw_parts_mut(out_buf as *mut u8, out_cap) };
    write_str_to_c_buf(&cp.to_string_lossy(), slice)
}

#[no_mangle]
pub extern "C" fn tp_jellyfin_cache_usage_bytes() -> u64 {
    current_cache().map(|m| m.usage_bytes()).unwrap_or(0)
}

#[no_mangle]
pub extern "C" fn tp_jellyfin_clear_cache(keep_pinned: u8) {
    if let Some(mgr) = current_cache() {
        mgr.clear_cache(keep_pinned != 0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::json::{JF_ITEM_EPISODE, JF_ITEM_MOVIE, JF_ITEM_SERIES};

    #[test]
    fn parses_jellyfin_urls_and_strips_web_suffix() {
        let u1 = ParsedJellyfinUrl::parse("http://192.168.1.50:8096/web/index.html#!/home.html")
            .unwrap();
        assert!(!u1.is_https);
        assert_eq!(u1.host, "192.168.1.50");
        assert_eq!(u1.port, 8096);
        assert_eq!(u1.base_path, "");

        let u2 = ParsedJellyfinUrl::parse("https://media.example.com/jellyfin/web").unwrap();
        assert!(u2.is_https);
        assert_eq!(u2.host, "media.example.com");
        assert_eq!(u2.port, 443);
        assert_eq!(u2.base_path, "/jellyfin");
    }

    #[test]
    fn plans_direct_stream_for_8bit_h264_and_transcodes_hevc_or_dts() {
        let srv = ParsedJellyfinUrl::parse("http://10.0.0.5:8096").unwrap();

        // 1) 1080p 8-bit H.264 + AAC in MP4 -> Direct Stream (`Static=true`)
        let p_direct = plan_video_stream(
            &srv,
            &VideoPlanInput {
                item_id: "mov1",
                media_source_id: "ms1",
                container: "mp4",
                video_codec: "h264",
                audio_codec: "aac",
                width: 1920,
                height: 1080,
                bit_depth: 8,
                bitrate: 8_000_000,
                quality: VideoQuality::Auto720p,
                start_ms: 120_000,
                play_session_id: "sid1",
                token: "tok123",
            },
        );
        assert!(!p_direct.is_transcoded);
        assert_eq!(p_direct.download_ext, "mp4");
        assert!(p_direct.path_and_query.contains("Static=true"));

        // 2) 4K 10-bit HEVC in MKV -> H.264 720p + stereo AAC transcode (`stream.ts`)
        let p_hevc = plan_video_stream(
            &srv,
            &VideoPlanInput {
                item_id: "mov2",
                media_source_id: "ms2",
                container: "mkv",
                video_codec: "hevc",
                audio_codec: "eac3",
                width: 3840,
                height: 2160,
                bit_depth: 10,
                bitrate: 25_000_000,
                quality: VideoQuality::Auto720p,
                start_ms: 60_000,
                play_session_id: "sid2",
                token: "tok123",
            },
        );
        assert!(p_hevc.is_transcoded);
        assert_eq!(p_hevc.download_ext, "ts");
        assert!(p_hevc.path_and_query.contains("VideoCodec=h264"));
        assert!(p_hevc.path_and_query.contains("StartTimeTicks=600000000"));
        assert!(p_hevc.path_and_query.contains("AllowVideoStreamCopy=false"));

        // 3) 1080p 8-bit H.264 with DTS audio at start_ms=0 -> stream.ts with AllowVideoStreamCopy=true!
        let p_dts = plan_video_stream(
            &srv,
            &VideoPlanInput {
                item_id: "mov3",
                media_source_id: "ms3",
                container: "mkv",
                video_codec: "h264",
                audio_codec: "dts",
                width: 1920,
                height: 1080,
                bit_depth: 8,
                bitrate: 10_000_000,
                quality: VideoQuality::Auto720p,
                start_ms: 0,
                play_session_id: "sid3",
                token: "tok123",
            },
        );
        assert!(p_dts.is_transcoded);
        assert!(p_dts.path_and_query.contains("AllowVideoStreamCopy=true"));
    }

    #[test]
    fn decodes_auth_views_and_video_items_and_builds_vfat_paths() {
        let auth_json = br#"{"AccessToken":"abc-token","User":{"Id":"u-42","Name":"famiclone"}}"#;
        let auth = json::decode_auth_response(auth_json).unwrap();
        assert_eq!(auth.access_token, "abc-token");
        assert_eq!(auth.user_id, "u-42");

        let views_json = br#"{"Items":[
            {"Id":"v1","Name":"Movies","CollectionType":"movies"},
            {"Id":"v2","Name":"Anime","CollectionType":"tvshows"},
            {"Id":"v3","Name":"Music","CollectionType":"music"},
            {"Id":"v4","Name":"BoxSets","CollectionType":"boxsets"}
        ]}"#;
        let views = decode_views(views_json).unwrap();
        assert_eq!(views.len(), 3);
        assert_eq!(views[0].view_kind(), 1);
        assert_eq!(views[2].view_kind(), 2);

        let items_json = br#"{
            "TotalRecordCount": 3,
            "Items": [
                {
                    "Id": "m1",
                    "Name": "Blade Runner: 2049",
                    "Type": "Movie",
                    "ProductionYear": 2017,
                    "RunTimeTicks": 98400000000,
                    "UserData": {"PlaybackPositionTicks": 1200000000, "Played": false},
                    "Path": "/media/movies/Blade.Runner.2049.mkv",
                    "Chapters": [{"StartPositionTicks": 0}, {"StartPositionTicks": 3000000000}],
                    "MediaSources": [{
                        "Id": "ms-m1",
                        "Container": "matroska,webm",
                        "Bitrate": 18000000,
                        "MediaStreams": [
                            {"Type":"Video","Codec":"hevc","Width":3840,"Height":1600,"AspectRatio":"2.40:1","BitDepth":10},
                            {"Type":"Audio","Codec":"truehd"}
                        ]
                    }]
                },
                {
                    "Id": "s1",
                    "Name": "Cowboy Bebop",
                    "Type": "Series"
                },
                {
                    "Id": "e1",
                    "Name": "Asteroid Blues?",
                    "Type": "Episode",
                    "SeriesId": "s1",
                    "SeriesName": "Cowboy Bebop",
                    "ParentIndexNumber": 1,
                    "IndexNumber": 1,
                    "RunTimeTicks": 15000000000,
                    "MediaSources": [{
                        "Id": "ms-e1",
                        "Container": "mov,mp4,m4a,3gp,3g2,mj2",
                        "Bitrate": 3200000,
                        "MediaStreams": [
                            {"Type":"Video","Codec":"h264","Width":1440,"Height":1080,"AspectRatio":"4:3","BitDepth":8},
                            {"Type":"Audio","Codec":"aac"}
                        ]
                    }]
                }
            ]
        }"#;
        let (items, total) = decode_video_items_page(items_json).unwrap();
        assert_eq!(total, 3);
        assert_eq!(items.len(), 3);
        assert_eq!(items[0].item_type, JF_ITEM_MOVIE);
        assert_eq!(items[0].duration_ms, 9_840_000);
        assert_eq!(items[0].resume_ms, 120_000);
        assert_eq!(items[0].chapters_csv, "0,300000");
        assert_eq!(items[0].ratio_num, 240);
        assert_eq!(items[0].ratio_den, 100);
        assert_eq!(items[1].item_type, JF_ITEM_SERIES);
        assert_eq!(items[2].item_type, JF_ITEM_EPISODE);

        let vroot = Path::new("/mnt/SDCARD/Media/Videos/jellyfin");
        let mp = cache::build_video_local_path(vroot, &items[0], "ts");
        assert_eq!(
            mp.to_str().unwrap(),
            "/mnt/SDCARD/Media/Videos/jellyfin/Movies/Blade Runner_ 2049 (2017).ts"
        );
        let ep = cache::build_video_local_path(vroot, &items[2], "mp4");
        assert_eq!(
            ep.to_str().unwrap(),
            "/mnt/SDCARD/Media/Videos/jellyfin/Shows/Cowboy Bebop/Season 01/S01E01 - Asteroid Blues_.mp4"
        );
    }
}
