//! Pure-Rust Subsonic / Navidrome client, virtual folder tree, LRU cache, and C ABI.
//!
//! Zero Rockbox headers or symbols — communicates strictly through `extern "C"` FFI.

pub mod cache;
pub mod config;
pub mod json;
pub mod md5;
pub mod tree;
pub mod url;

use std::collections::{HashMap, HashSet};
use std::ffi::{c_char, c_void, CStr, CString};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

use cache::{CacheManager, HttpTransport};
use config::{NavidromeConfig, StreamQuality};
use json::{decode_ping, decode_playlist_detail, decode_playlists, decode_search3_songs, SubsonicSong};
use tree::VirtualFolderTree;
use url::ParsedServerUrl;

pub type TpNavCancelFn = Option<unsafe extern "C" fn(*mut c_void) -> u8>;

#[repr(C)]
#[derive(Clone, Copy)]
pub struct TpNavHttpVtable {
    pub http_get: unsafe extern "C" fn(
        is_https: u8,
        host: *const c_char,
        port: u16,
        path_and_query: *const c_char,
        allow_self_signed: u8,
        timeout_ms: i32,
        out_data: *mut *mut u8,
        out_len: *mut usize,
        err_buf: *mut c_char,
        err_cap: usize,
    ) -> i32,
    pub http_free: unsafe extern "C" fn(ptr: *mut u8),
    pub http_download_file: unsafe extern "C" fn(
        is_https: u8,
        host: *const c_char,
        port: u16,
        path_and_query: *const c_char,
        allow_self_signed: u8,
        dest_path: *const c_char,
        timeout_ms: i32,
        cancel_cb: TpNavCancelFn,
        cancel_ctx: *mut c_void,
        err_buf: *mut c_char,
        err_cap: usize,
    ) -> i32,
}

struct CTransport {
    vt: TpNavHttpVtable,
}

unsafe impl Send for CTransport {}
unsafe impl Sync for CTransport {}

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

impl HttpTransport for CTransport {
    fn get_bytes(
        &self,
        is_https: bool,
        host: &str,
        port: u16,
        path_and_query: &str,
        allow_self_signed: bool,
        timeout_ms: i32,
    ) -> Result<Vec<u8>, String> {
        let c_host = CString::new(host).map_err(|_| "Invalid host".to_string())?;
        let c_pq = CString::new(path_and_query).map_err(|_| "Invalid path".to_string())?;
        let mut out_ptr: *mut u8 = std::ptr::null_mut();
        let mut out_len: usize = 0;
        let mut err_buf = [0u8; 128];

        let rc = unsafe {
            (self.vt.http_get)(
                if is_https { 1 } else { 0 },
                c_host.as_ptr(),
                port,
                c_pq.as_ptr(),
                if allow_self_signed { 1 } else { 0 },
                timeout_ms,
                &mut out_ptr,
                &mut out_len,
                err_buf.as_mut_ptr() as *mut c_char,
                err_buf.len(),
            )
        };

        if rc != 0 {
            if !out_ptr.is_null() {
                unsafe { (self.vt.http_free)(out_ptr) };
            }
            return Err(c_buf_to_string(&err_buf, "Connection failed"));
        }

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
        Ok(bytes)
    }

    fn download_to_file(
        &self,
        is_https: bool,
        host: &str,
        port: u16,
        path_and_query: &str,
        allow_self_signed: bool,
        dest_path: &Path,
        timeout_ms: i32,
        cancel_fn: Option<&(dyn Fn() -> bool + Sync)>,
    ) -> Result<(), String> {
        let c_host = CString::new(host).map_err(|_| "Invalid host".to_string())?;
        let c_pq = CString::new(path_and_query).map_err(|_| "Invalid path".to_string())?;
        let dest_str = dest_path.to_str().ok_or_else(|| "Invalid dest path".to_string())?;
        let c_dest = CString::new(dest_str).map_err(|_| "Invalid dest path".to_string())?;
        let mut err_buf = [0u8; 128];

        let (cb_ptr, cb_ctx): (TpNavCancelFn, *mut c_void) = if let Some(ref f) = cancel_fn {
            (
                Some(rust_closure_cancel_trampoline),
                f as *const &(dyn Fn() -> bool + Sync) as *mut c_void,
            )
        } else {
            (None, std::ptr::null_mut())
        };

        let rc = unsafe {
            (self.vt.http_download_file)(
                if is_https { 1 } else { 0 },
                c_host.as_ptr(),
                port,
                c_pq.as_ptr(),
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
pub struct TpNavConfigC {
    pub url: [u8; 256],
    pub username: [u8; 128],
    pub password: [u8; 128],
    pub quality: u8,
    pub allow_self_signed: u8,
    pub _pad: u16,
    pub cache_limit_mb: u32,
}

#[repr(C)]
pub struct TpNavSongC {
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
pub struct TpNavTreeEntryC {
    pub is_dir: u8,
    pub name: [u8; 256],
    pub target_path: [u8; 512],
}

pub type TpNavOnSongFn = Option<unsafe extern "C" fn(ctx: *mut c_void, song: *const TpNavSongC)>;
pub type TpNavOnPlaylistFn = Option<
    unsafe extern "C" fn(
        ctx: *mut c_void,
        playlist_name: *const c_char,
        m3u8_content: *const c_char,
    ),
>;
pub type TpNavOnProgressFn = Option<unsafe extern "C" fn(ctx: *mut c_void, synced_tracks: u32)>;
pub type TpNavPathIterFn = Option<unsafe extern "C" fn(ctx: *mut c_void, path: *const c_char) -> u8>;

struct Engine {
    cache: Arc<CacheManager>,
    tree: VirtualFolderTree,
}

static ENGINE: OnceLock<Mutex<Option<Engine>>> = OnceLock::new();

fn engine_slot() -> &'static Mutex<Option<Engine>> {
    ENGINE.get_or_init(|| Mutex::new(None))
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
    CString::new(s.as_bytes().iter().copied().filter(|&b| b != 0).collect::<Vec<u8>>())
        .unwrap_or_default()
}

/// High-level catalog synchronization logic (unit-testable with any `HttpTransport`).
pub fn sync_catalog_with_cache(
    cache: &CacheManager,
    mut on_song: impl FnMut(&SubsonicSong, &Path, &Path),
    mut on_playlist: impl FnMut(&str, &str),
    mut on_progress: impl FnMut(u32),
    cancel_fn: Option<&(dyn Fn() -> bool + Sync)>,
) -> Result<u32, String> {
    let cfg = cache.get_config();
    let url = ParsedServerUrl::parse(&cfg.url)?;
    let transport = cache.transport();

    // 1. Ping & authenticate
    let ping_req = url.ping_path(&cfg.username, &cfg.password);
    let ping_bytes = transport.get_bytes(
        url.is_https,
        &url.host,
        url.port,
        &ping_req,
        cfg.allow_self_signed,
        8_000,
    )?;
    decode_ping(&ping_bytes)?;

    // 2. Paginate all songs via search3.view
    const PAGE_SIZE: usize = 500;
    const MAX_SONGS: usize = 200_000;
    let mut offset = 0usize;
    let mut total_songs = 0u32;
    let mut id_to_path: HashMap<String, PathBuf> = HashMap::new();
    let mut used_paths: HashSet<PathBuf> = HashSet::new();

    let mut resolve_song_path = |song: &SubsonicSong| -> (PathBuf, bool) {
        if let Some(existing) = id_to_path.get(&song.id) {
            return (existing.clone(), false);
        }
        let artist_for_dir = if !song.album_artist.is_empty() {
            &song.album_artist
        } else {
            &song.artist
        };
        let mut tp = cache.track_path_for_meta(
            &song.server_path,
            artist_for_dir,
            &song.album,
            song.track,
            &song.title,
            &song.id,
            &song.suffix,
            cfg.quality,
        );
        if !used_paths.insert(tp.clone()) {
            tp = cache::disambiguate_track_path(&tp, &song.id, &song.suffix, cfg.quality);
            used_paths.insert(tp.clone());
        }
        id_to_path.insert(song.id.clone(), tp.clone());
        (tp, true)
    };

    on_progress(0);

    while offset < MAX_SONGS {
        if let Some(cb) = cancel_fn {
            if cb() {
                return Err("Cancelled".to_string());
            }
        }

        let req = url.search3_songs_path(&cfg.username, &cfg.password, PAGE_SIZE, offset);
        let body = transport.get_bytes(
            url.is_https,
            &url.host,
            url.port,
            &req,
            cfg.allow_self_signed,
            20_000,
        )?;
        let batch = decode_search3_songs(&body)?;
        if batch.is_empty() {
            break;
        }

        let batch_len = batch.len();
        let mut new_in_batch = 0usize;
        for song in &batch {
            let (track_path, is_new) = resolve_song_path(song);
            if !is_new {
                continue;
            }
            new_in_batch += 1;
            let cover_path = cache.cover_path_for_track(&track_path, &song.cover_art);
            on_song(song, &track_path, &cover_path);
            total_songs += 1;
        }

        on_progress(total_songs);

        if batch_len < PAGE_SIZE || new_in_batch == 0 {
            break;
        }
        offset += batch_len;
    }

    // 3. Sync server playlists (`getPlaylists` + `getPlaylist`)
    let pls_req = url.get_playlists_path(&cfg.username, &cfg.password);
    if let Ok(pls_body) = transport.get_bytes(
        url.is_https,
        &url.host,
        url.port,
        &pls_req,
        cfg.allow_self_signed,
        10_000,
    ) {
        if let Ok(playlists) = decode_playlists(&pls_body) {
            for pl in playlists {
                if let Some(cb) = cancel_fn {
                    if cb() {
                        return Err("Cancelled".to_string());
                    }
                }
                let det_req = url.get_playlist_path(&cfg.username, &cfg.password, &pl.id);
                if let Ok(det_body) = transport.get_bytes(
                    url.is_https,
                    &url.host,
                    url.port,
                    &det_req,
                    cfg.allow_self_signed,
                    12_000,
                ) {
                    if let Ok(detail) = decode_playlist_detail(&det_body) {
                        let mut m3u = String::from("#EXTM3U\n");
                        for song in &detail.songs {
                            let (tp, is_new) = resolve_song_path(song);
                            if is_new {
                                let cp = cache.cover_path_for_track(&tp, &song.cover_art);
                                on_song(song, &tp, &cp);
                                total_songs += 1;
                            }
                            m3u.push_str(&tp.to_string_lossy());
                            m3u.push('\n');
                        }
                        on_playlist(&detail.name, &m3u);
                    }
                }
            }
        }
    }

    Ok(total_songs)
}

// ============================================================================
// C ABI Exports (`tp_navidrome_*`)
// ============================================================================

#[no_mangle]
pub unsafe extern "C" fn tp_navidrome_init(
    cache_root_utf8: *const c_char,
    vtable: *const TpNavHttpVtable,
) -> bool {
    if cache_root_utf8.is_null() || vtable.is_null() {
        return false;
    }
    let root_str = unsafe { opt_cstr(cache_root_utf8) };
    if root_str.is_empty() {
        return false;
    }
    let vt = unsafe { *vtable };
    let transport = Arc::new(CTransport { vt });
    let cache = Arc::new(CacheManager::new(PathBuf::from(root_str), transport));

    *cache_arc_slot().lock().unwrap() = Some(Arc::clone(&cache));
    let mut guard = engine_slot().lock().unwrap();
    *guard = Some(Engine {
        cache,
        tree: VirtualFolderTree::new(),
    });
    true
}

#[no_mangle]
pub extern "C" fn tp_navidrome_shutdown() {
    *cache_arc_slot().lock().unwrap() = None;
    let mut guard = engine_slot().lock().unwrap();
    *guard = None;
}

#[no_mangle]
pub unsafe extern "C" fn tp_navidrome_load_config_text(ini_utf8: *const c_char) -> bool {
    let text = unsafe { opt_cstr(ini_utf8) };
    let cfg = NavidromeConfig::parse_ini(&text);
    let guard = engine_slot().lock().unwrap();
    let Some(eng) = guard.as_ref() else {
        return false;
    };
    eng.cache.set_config(cfg);
    true
}

#[no_mangle]
pub unsafe extern "C" fn tp_navidrome_format_config_text(
    out_buf: *mut c_char,
    out_cap: usize,
) -> usize {
    if out_buf.is_null() || out_cap == 0 {
        return 0;
    }
    let ini = {
        let guard = engine_slot().lock().unwrap();
        let Some(eng) = guard.as_ref() else {
            return 0;
        };
        eng.cache.get_config().to_ini()
    };
    let slice = unsafe { std::slice::from_raw_parts_mut(out_buf as *mut u8, out_cap) };
    write_str_to_c_buf(&ini, slice)
}

#[no_mangle]
pub unsafe extern "C" fn tp_navidrome_get_config(out_cfg: *mut TpNavConfigC) -> bool {
    if out_cfg.is_null() {
        return false;
    }
    let cfg = {
        let guard = engine_slot().lock().unwrap();
        let Some(eng) = guard.as_ref() else {
            return false;
        };
        eng.cache.get_config()
    };
    let dst = unsafe { &mut *out_cfg };
    dst.url.fill(0);
    dst.username.fill(0);
    dst.password.fill(0);
    write_str_to_c_buf(&cfg.url, &mut dst.url);
    write_str_to_c_buf(&cfg.username, &mut dst.username);
    write_str_to_c_buf(&cfg.password, &mut dst.password);
    dst.quality = cfg.quality as u8;
    dst.allow_self_signed = if cfg.allow_self_signed { 1 } else { 0 };
    dst._pad = 0;
    dst.cache_limit_mb = cfg.cache_limit_mb;
    true
}

#[no_mangle]
pub unsafe extern "C" fn tp_navidrome_set_config(in_cfg: *const TpNavConfigC) -> bool {
    if in_cfg.is_null() {
        return false;
    }
    let src = unsafe { &*in_cfg };
    let cfg = NavidromeConfig {
        url: c_buf_to_string(&src.url, ""),
        username: c_buf_to_string(&src.username, ""),
        password: c_buf_to_string(&src.password, ""),
        quality: StreamQuality::from_u8(src.quality),
        cache_limit_mb: src.cache_limit_mb,
        allow_self_signed: src.allow_self_signed != 0,
    };
    let guard = engine_slot().lock().unwrap();
    let Some(eng) = guard.as_ref() else {
        return false;
    };
    eng.cache.set_config(cfg);
    true
}

#[no_mangle]
pub extern "C" fn tp_navidrome_is_configured() -> bool {
    let guard = engine_slot().lock().unwrap();
    guard
        .as_ref()
        .map(|e| e.cache.get_config().is_configured())
        .unwrap_or(false)
}

#[no_mangle]
pub unsafe extern "C" fn tp_navidrome_ping(err_buf: *mut c_char, err_cap: usize) -> bool {
    let (cfg, transport) = {
        let guard = engine_slot().lock().unwrap();
        let Some(eng) = guard.as_ref() else {
            unsafe { write_err_ptr(err_buf, err_cap, "Not initialized") };
            return false;
        };
        (eng.cache.get_config(), Arc::clone(eng.cache.transport()))
    };

    let url = match ParsedServerUrl::parse(&cfg.url) {
        Ok(u) => u,
        Err(e) => {
            unsafe { write_err_ptr(err_buf, err_cap, &e) };
            return false;
        }
    };
    let req = url.ping_path(&cfg.username, &cfg.password);
    match transport
        .get_bytes(
            url.is_https,
            &url.host,
            url.port,
            &req,
            cfg.allow_self_signed,
            8_000,
        )
        .and_then(|b| decode_ping(&b))
    {
        Ok(()) => true,
        Err(e) => {
            unsafe { write_err_ptr(err_buf, err_cap, &e) };
            false
        }
    }
}

struct SyncCancelCtx {
    cb: TpNavCancelFn,
    ctx: *mut c_void,
}
unsafe impl Send for SyncCancelCtx {}
unsafe impl Sync for SyncCancelCtx {}

impl SyncCancelCtx {
    fn check(&self) -> bool {
        if let Some(f) = self.cb {
            unsafe { f(self.ctx) != 0 }
        } else {
            false
        }
    }
}

#[no_mangle]
pub unsafe extern "C" fn tp_navidrome_sync_catalog(
    on_song: TpNavOnSongFn,
    on_playlist: TpNavOnPlaylistFn,
    on_progress: TpNavOnProgressFn,
    cancel_cb: TpNavCancelFn,
    user_ctx: *mut c_void,
    err_buf: *mut c_char,
    err_cap: usize,
) -> i32 {
    // Note: we take a brief lock only to borrow `cache` safely? Wait — while syncing, UI callbacks
    // won't destroy the engine, so let's keep a reference without holding the tree lock across
    // callbacks if callbacks call `tp_navidrome_tree_insert`!
    // Even cleaner: `CacheManager` can be wrapped in `Arc<CacheManager>` inside `Engine`!
    tp_navidrome_sync_catalog_impl(
        on_song,
        on_playlist,
        on_progress,
        cancel_cb,
        user_ctx,
        err_buf,
        err_cap,
    )
}

unsafe fn tp_navidrome_sync_catalog_impl(
    on_song: TpNavOnSongFn,
    on_playlist: TpNavOnPlaylistFn,
    on_progress: TpNavOnProgressFn,
    cancel_cb: TpNavCancelFn,
    user_ctx: *mut c_void,
    err_buf: *mut c_char,
    err_cap: usize,
) -> i32 {
    // Snapshot config + cache root + transport so the global mutex is NEVER held during network I/O
    // or C callbacks (`tp_navidrome_tree_insert`, etc.)!
    let (cache_root, cfg, transport) = {
        let guard = engine_slot().lock().unwrap();
        let Some(eng) = guard.as_ref() else {
            unsafe { write_err_ptr(err_buf, err_cap, "Not initialized") };
            return -1;
        };
        (
            eng.cache.cache_root().to_path_buf(),
            eng.cache.get_config(),
            Arc::clone(eng.cache.transport()),
        )
    };

    let url = match ParsedServerUrl::parse(&cfg.url) {
        Ok(u) => u,
        Err(e) => {
            unsafe { write_err_ptr(err_buf, err_cap, &e) };
            return -1;
        }
    };

    let cctx = SyncCancelCtx {
        cb: cancel_cb,
        ctx: user_ctx,
    };
    let is_cancelled = || -> bool { cctx.check() };

    let ping_req = url.ping_path(&cfg.username, &cfg.password);
    if let Err(e) = transport
        .get_bytes(
            url.is_https,
            &url.host,
            url.port,
            &ping_req,
            cfg.allow_self_signed,
            8_000,
        )
        .and_then(|b| decode_ping(&b))
    {
        unsafe { write_err_ptr(err_buf, err_cap, &e) };
        return -1;
    }

    let mut id_to_path: HashMap<String, PathBuf> = HashMap::new();
    let mut used_paths: HashSet<PathBuf> = HashSet::new();

    let mut resolve_song_path = |song: &SubsonicSong| -> (PathBuf, bool) {
        if let Some(existing) = id_to_path.get(&song.id) {
            return (existing.clone(), false);
        }
        let artist_for_dir = if !song.album_artist.is_empty() {
            &song.album_artist
        } else {
            &song.artist
        };
        let mut tp = cache::build_track_path(
            &cache_root,
            &song.server_path,
            artist_for_dir,
            &song.album,
            song.track,
            &song.title,
            &song.id,
            &song.suffix,
            cfg.quality,
        );
        if !used_paths.insert(tp.clone()) {
            tp = cache::disambiguate_track_path(&tp, &song.id, &song.suffix, cfg.quality);
            used_paths.insert(tp.clone());
        }
        id_to_path.insert(song.id.clone(), tp.clone());
        (tp, true)
    };

    let emit_song = |song: &SubsonicSong, tp: &Path| {
        let Some(cb) = on_song else { return };
        let cp = cache::resolve_cover_path(&cache_root, tp, &song.cover_art);

        let c_id = to_cstring_lossy(&song.id);
        let c_tp = to_cstring_lossy(&tp.to_string_lossy());
        let c_cp = to_cstring_lossy(&cp.to_string_lossy());
        let c_cov = to_cstring_lossy(&song.cover_art);
        let c_sp = to_cstring_lossy(&song.server_path);
        let c_suf = to_cstring_lossy(&song.suffix);
        let c_tit = to_cstring_lossy(&song.title);
        let c_art = to_cstring_lossy(&song.artist);
        let c_aa = to_cstring_lossy(&song.album_artist);
        let c_alb = to_cstring_lossy(&song.album);
        let c_gen = to_cstring_lossy(&song.genre);

        let sc = TpNavSongC {
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
        unsafe { cb(user_ctx, &sc) };
    };

    if let Some(p) = on_progress {
        unsafe { p(user_ctx, 0) };
    }

    const PAGE_SIZE: usize = 500;
    const MAX_SONGS: usize = 200_000;
    let mut offset = 0usize;
    let mut total_songs = 0u32;

    while offset < MAX_SONGS {
        if is_cancelled() {
            unsafe { write_err_ptr(err_buf, err_cap, "Cancelled") };
            return -2;
        }
        let req = url.search3_songs_path(&cfg.username, &cfg.password, PAGE_SIZE, offset);
        let body = match transport.get_bytes(
            url.is_https,
            &url.host,
            url.port,
            &req,
            cfg.allow_self_signed,
            20_000,
        ) {
            Ok(b) => b,
            Err(e) => {
                unsafe { write_err_ptr(err_buf, err_cap, &e) };
                return -1;
            }
        };
        let batch = match decode_search3_songs(&body) {
            Ok(s) => s,
            Err(e) => {
                unsafe { write_err_ptr(err_buf, err_cap, &e) };
                return -1;
            }
        };
        if batch.is_empty() {
            break;
        }
        let batch_len = batch.len();
        let mut new_in_batch = 0usize;
        for song in &batch {
            let (tp, is_new) = resolve_song_path(song);
            if !is_new {
                continue;
            }
            new_in_batch += 1;
            emit_song(song, &tp);
            total_songs += 1;
        }
        if let Some(p) = on_progress {
            unsafe { p(user_ctx, total_songs) };
        }
        if batch_len < PAGE_SIZE || new_in_batch == 0 {
            break;
        }
        offset += batch_len;
    }

    // Fetch playlists
    let pls_req = url.get_playlists_path(&cfg.username, &cfg.password);
    if let Ok(pls_body) = transport.get_bytes(
        url.is_https,
        &url.host,
        url.port,
        &pls_req,
        cfg.allow_self_signed,
        10_000,
    ) {
        if let Ok(playlists) = decode_playlists(&pls_body) {
            for pl in playlists {
                if is_cancelled() {
                    unsafe { write_err_ptr(err_buf, err_cap, "Cancelled") };
                    return -2;
                }
                let det_req = url.get_playlist_path(&cfg.username, &cfg.password, &pl.id);
                if let Ok(det_body) = transport.get_bytes(
                    url.is_https,
                    &url.host,
                    url.port,
                    &det_req,
                    cfg.allow_self_signed,
                    12_000,
                ) {
                    if let Ok(detail) = decode_playlist_detail(&det_body) {
                        let mut m3u = String::from("#EXTM3U\n");
                        for song in &detail.songs {
                            let (tp, is_new) = resolve_song_path(song);
                            if is_new {
                                emit_song(song, &tp);
                                total_songs += 1;
                            }
                            m3u.push_str(&tp.to_string_lossy());
                            m3u.push('\n');
                        }
                        if let Some(pl_cb) = on_playlist {
                            let c_name = to_cstring_lossy(&detail.name);
                            let c_m3u = to_cstring_lossy(&m3u);
                            unsafe { pl_cb(user_ctx, c_name.as_ptr(), c_m3u.as_ptr()) };
                        }
                    }
                }
            }
        }
    }

    total_songs as i32
}

#[no_mangle]
#[allow(clippy::too_many_arguments)]
pub unsafe extern "C" fn tp_navidrome_track_path_for(
    server_path_utf8: *const c_char,
    artist_utf8: *const c_char,
    album_utf8: *const c_char,
    track_no: i32,
    title_utf8: *const c_char,
    song_id_utf8: *const c_char,
    suffix_utf8: *const c_char,
    quality: u8,
    out_buf: *mut c_char,
    out_cap: usize,
) -> usize {
    if out_buf.is_null() || out_cap == 0 {
        return 0;
    }
    let server_path = unsafe { opt_cstr(server_path_utf8) };
    let artist = unsafe { opt_cstr(artist_utf8) };
    let album = unsafe { opt_cstr(album_utf8) };
    let title = unsafe { opt_cstr(title_utf8) };
    let song_id = unsafe { opt_cstr(song_id_utf8) };
    let suffix = unsafe { opt_cstr(suffix_utf8) };
    let path_str = {
        let Some(cache) = current_cache() else {
            return 0;
        };
        cache
            .track_path_for_meta(
                &server_path,
                &artist,
                &album,
                track_no,
                &title,
                &song_id,
                &suffix,
                StreamQuality::from_u8(quality),
            )
            .to_string_lossy()
            .into_owned()
    };
    let slice = unsafe { std::slice::from_raw_parts_mut(out_buf as *mut u8, out_cap) };
    write_str_to_c_buf(&path_str, slice)
}

#[no_mangle]
pub unsafe extern "C" fn tp_navidrome_disambiguate_track_path(
    base_path_utf8: *const c_char,
    song_id_utf8: *const c_char,
    suffix_utf8: *const c_char,
    quality: u8,
    out_buf: *mut c_char,
    out_cap: usize,
) -> usize {
    if out_buf.is_null() || out_cap == 0 {
        return 0;
    }
    let base = PathBuf::from(unsafe { opt_cstr(base_path_utf8) });
    let song_id = unsafe { opt_cstr(song_id_utf8) };
    let suffix = unsafe { opt_cstr(suffix_utf8) };
    let disamb = cache::disambiguate_track_path(
        &base,
        &song_id,
        &suffix,
        StreamQuality::from_u8(quality),
    );
    let slice = unsafe { std::slice::from_raw_parts_mut(out_buf as *mut u8, out_cap) };
    write_str_to_c_buf(&disamb.to_string_lossy(), slice)
}

#[no_mangle]
pub unsafe extern "C" fn tp_navidrome_cover_path_for(
    local_track_path_utf8: *const c_char,
    cover_id_utf8: *const c_char,
    out_buf: *mut c_char,
    out_cap: usize,
) -> usize {
    if out_buf.is_null() || out_cap == 0 {
        return 0;
    }
    let track_path = PathBuf::from(unsafe { opt_cstr(local_track_path_utf8) });
    let cover_id = unsafe { opt_cstr(cover_id_utf8) };
    let path_str = {
        let Some(cache) = current_cache() else {
            return 0;
        };
        cache
            .cover_path_for_track(&track_path, &cover_id)
            .to_string_lossy()
            .into_owned()
    };
    let slice = unsafe { std::slice::from_raw_parts_mut(out_buf as *mut u8, out_cap) };
    write_str_to_c_buf(&path_str, slice)
}

#[no_mangle]
pub unsafe extern "C" fn tp_navidrome_is_track_path(path_utf8: *const c_char) -> bool {
    if path_utf8.is_null() {
        return false;
    }
    let p = unsafe { opt_cstr(path_utf8) };
    current_cache()
        .map(|c| c.is_track_path(&p))
        .unwrap_or(false)
}

#[no_mangle]
pub unsafe extern "C" fn tp_navidrome_pin_track(local_path_utf8: *const c_char) -> bool {
    if local_path_utf8.is_null() {
        return false;
    }
    let p = PathBuf::from(unsafe { opt_cstr(local_path_utf8) });
    current_cache().map(|c| c.pin_track(&p)).unwrap_or(false)
}

#[no_mangle]
pub unsafe extern "C" fn tp_navidrome_is_track_pinned(local_path_utf8: *const c_char) -> bool {
    if local_path_utf8.is_null() {
        return false;
    }
    let p = PathBuf::from(unsafe { opt_cstr(local_path_utf8) });
    current_cache().map(|c| c.is_pinned(&p)).unwrap_or(false)
}

#[no_mangle]
pub unsafe extern "C" fn tp_navidrome_ensure_track_cached(
    local_path_utf8: *const c_char,
    song_id_utf8: *const c_char,
    cover_id_utf8: *const c_char,
    cancel_cb: TpNavCancelFn,
    cancel_ctx: *mut c_void,
    err_buf: *mut c_char,
    err_cap: usize,
) -> i32 {
    let local_path = PathBuf::from(unsafe { opt_cstr(local_path_utf8) });
    let song_id = unsafe { opt_cstr(song_id_utf8) };
    let cover_id = unsafe { opt_cstr(cover_id_utf8) };

    if cache::CacheManager::file_ready(&local_path) {
        if !cover_id.is_empty() {
            if let Some(cache) = current_cache() {
                cache.enqueue_cover_for_track(&local_path, &cover_id);
            }
        }
        return 0;
    }

    if song_id.is_empty() {
        unsafe { write_err_ptr(err_buf, err_cap, "Unknown Navidrome track") };
        return -1;
    }

    tp_navidrome_ensure_track_cached_unlocked(
        &local_path,
        &song_id,
        &cover_id,
        cancel_cb,
        cancel_ctx,
        err_buf,
        err_cap,
    )
}

// Helper using an `Arc<CacheManager>` clone so `ENGINE` mutex is never held across network I/O!
static CACHE_ARC: OnceLock<Mutex<Option<Arc<CacheManager>>>> = OnceLock::new();

fn cache_arc_slot() -> &'static Mutex<Option<Arc<CacheManager>>> {
    CACHE_ARC.get_or_init(|| Mutex::new(None))
}

fn current_cache() -> Option<Arc<CacheManager>> {
    cache_arc_slot().lock().unwrap().clone()
}

unsafe fn tp_navidrome_ensure_track_cached_unlocked(
    local_path: &Path,
    song_id: &str,
    cover_id: &str,
    cancel_cb: TpNavCancelFn,
    cancel_ctx: *mut c_void,
    err_buf: *mut c_char,
    err_cap: usize,
) -> i32 {
    let Some(cache) = current_cache() else {
        unsafe { write_err_ptr(err_buf, err_cap, "Not initialized") };
        return -1;
    };

    let cctx = SyncCancelCtx {
        cb: cancel_cb,
        ctx: cancel_ctx,
    };
    let cancel_closure = || -> bool { cctx.check() };
    let cancel_opt: Option<&(dyn Fn() -> bool + Sync)> = if cancel_cb.is_some() {
        Some(&cancel_closure)
    } else {
        None
    };

    match cache.ensure_track_cached(local_path, song_id, cover_id, cancel_opt) {
        Ok(()) => 0,
        Err(e) if e == "Cancelled" => {
            unsafe { write_err_ptr(err_buf, err_cap, "Cancelled") };
            -2
        }
        Err(e) => {
            unsafe { write_err_ptr(err_buf, err_cap, &e) };
            -1
        }
    }
}

#[no_mangle]
pub unsafe extern "C" fn tp_navidrome_ensure_cover_cached(
    local_track_path_utf8: *const c_char,
    cover_id_utf8: *const c_char,
    out_buf: *mut c_char,
    out_cap: usize,
) -> bool {
    let local_track_path = PathBuf::from(unsafe { opt_cstr(local_track_path_utf8) });
    let cover_id = unsafe { opt_cstr(cover_id_utf8) };
    let Some(cache) = current_cache() else {
        return false;
    };
    let Some(path) = cache.ensure_cover_cached(&local_track_path, &cover_id) else {
        return false;
    };
    if !out_buf.is_null() && out_cap > 0 {
        let slice = unsafe { std::slice::from_raw_parts_mut(out_buf as *mut u8, out_cap) };
        write_str_to_c_buf(&path.to_string_lossy(), slice);
    }
    true
}

#[no_mangle]
pub unsafe extern "C" fn tp_navidrome_enqueue_prefetch(
    path1_utf8: *const c_char,
    song1_utf8: *const c_char,
    cover1_utf8: *const c_char,
    path2_utf8: *const c_char,
    song2_utf8: *const c_char,
    cover2_utf8: *const c_char,
) {
    let Some(cache) = current_cache() else {
        return;
    };
    let mut targets = Vec::new();
    let p1 = unsafe { opt_cstr(path1_utf8) };
    let s1 = unsafe { opt_cstr(song1_utf8) };
    let c1 = unsafe { opt_cstr(cover1_utf8) };
    if !p1.is_empty() && !s1.is_empty() {
        targets.push((PathBuf::from(p1), s1, c1));
    }
    let p2 = unsafe { opt_cstr(path2_utf8) };
    let s2 = unsafe { opt_cstr(song2_utf8) };
    let c2 = unsafe { opt_cstr(cover2_utf8) };
    if !p2.is_empty() && !s2.is_empty() {
        targets.push((PathBuf::from(p2), s2, c2));
    }
    cache.set_prefetch_targets(targets);
}

#[no_mangle]
pub unsafe extern "C" fn tp_navidrome_notify_track_start(song_id_utf8: *const c_char) {
    let Some(cache) = current_cache() else {
        return;
    };
    let song_id = unsafe { opt_cstr(song_id_utf8) };
    cache.notify_track_start(&song_id);
}

#[no_mangle]
pub unsafe extern "C" fn tp_navidrome_notify_track_played(
    song_id_utf8: *const c_char,
    played_secs: u32,
    duration_secs: u32,
) {
    let Some(cache) = current_cache() else {
        return;
    };
    let song_id = unsafe { opt_cstr(song_id_utf8) };
    cache.notify_track_played(&song_id, played_secs, duration_secs);
}

#[no_mangle]
pub extern "C" fn tp_navidrome_cache_usage_bytes() -> u64 {
    current_cache()
        .map(|c| c.cache_usage_bytes())
        .unwrap_or(0)
}

#[no_mangle]
pub extern "C" fn tp_navidrome_clear_cache() -> bool {
    current_cache().map(|c| c.clear_cache()).unwrap_or(false)
}

// ---- Virtual Folder Tree C ABI ---------------------------------------------

#[no_mangle]
pub extern "C" fn tp_navidrome_tree_clear() {
    if let Some(eng) = engine_slot().lock().unwrap().as_mut() {
        eng.tree.clear();
    }
}

#[no_mangle]
pub unsafe extern "C" fn tp_navidrome_tree_insert(
    server_path_utf8: *const c_char,
    local_track_path_utf8: *const c_char,
    fallback_title_utf8: *const c_char,
) {
    let sp = unsafe { opt_cstr(server_path_utf8) };
    let lp = unsafe { opt_cstr(local_track_path_utf8) };
    let ft = unsafe { opt_cstr(fallback_title_utf8) };
    if let Some(eng) = engine_slot().lock().unwrap().as_mut() {
        eng.tree.insert_track(&sp, &lp, &ft);
    }
}

#[no_mangle]
pub extern "C" fn tp_navidrome_tree_finalize() {
    if let Some(eng) = engine_slot().lock().unwrap().as_mut() {
        eng.tree.finalize();
    }
}

#[no_mangle]
pub unsafe extern "C" fn tp_navidrome_tree_list_count(rel_dir_utf8: *const c_char) -> usize {
    let dir = unsafe { opt_cstr(rel_dir_utf8) };
    engine_slot()
        .lock()
        .unwrap()
        .as_ref()
        .map(|e| e.tree.list(&dir).len())
        .unwrap_or(0)
}

#[no_mangle]
pub unsafe extern "C" fn tp_navidrome_tree_list_get(
    rel_dir_utf8: *const c_char,
    index: usize,
    out_entry: *mut TpNavTreeEntryC,
) -> bool {
    if out_entry.is_null() {
        return false;
    }
    let dir = unsafe { opt_cstr(rel_dir_utf8) };
    let guard = engine_slot().lock().unwrap();
    let Some(eng) = guard.as_ref() else {
        return false;
    };
    let Some(item) = eng.tree.list(&dir).get(index) else {
        return false;
    };
    let dst = unsafe { &mut *out_entry };
    dst.is_dir = if item.is_dir { 1 } else { 0 };
    dst.name.fill(0);
    dst.target_path.fill(0);
    write_str_to_c_buf(&item.name, &mut dst.name);
    write_str_to_c_buf(&item.target_path, &mut dst.target_path);
    true
}

#[no_mangle]
pub unsafe extern "C" fn tp_navidrome_tree_collect_tracks(
    rel_dir_utf8: *const c_char,
    cb: TpNavPathIterFn,
    ctx: *mut c_void,
) {
    let Some(cb_fn) = cb else { return };
    let dir = unsafe { opt_cstr(rel_dir_utf8) };
    let paths = {
        let guard = engine_slot().lock().unwrap();
        let Some(eng) = guard.as_ref() else {
            return;
        };
        let mut out = Vec::new();
        eng.tree.collect_tracks(&dir, &mut out);
        out
    };
    for p in paths {
        let c_p = to_cstring_lossy(&p);
        if unsafe { cb_fn(ctx, c_p.as_ptr()) } == 0 {
            break;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    unsafe extern "C" fn mock_c_http_get(
        _is_https: u8,
        _host: *const c_char,
        _port: u16,
        path_and_query: *const c_char,
        _allow_self_signed: u8,
        _timeout_ms: i32,
        out_data: *mut *mut u8,
        out_len: *mut usize,
        _err_buf: *mut c_char,
        _err_cap: usize,
    ) -> i32 {
        let pq = unsafe { CStr::from_ptr(path_and_query) }.to_string_lossy();
        let resp: &[u8] = if pq.contains("ping.view") {
            br#"{"subsonic-response":{"status":"ok","version":"1.16.1"}}"#
        } else if pq.contains("search3.view") {
            br#"{"subsonic-response":{"status":"ok","searchResult3":{"song":[
                {"id":"tr1","title":"Airbag","artist":"Radiohead","album":"OK Computer","genre":"Alt Rock","coverArt":"cov1","suffix":"flac","duration":284,"track":1,"discNumber":1,"year":1997,"path":"Radiohead/OK Computer/01 - Airbag.flac"},
                {"id":"tr2","title":"Paranoid Android","artist":"Radiohead","album":"OK Computer","genre":"Alt Rock","coverArt":"cov1","suffix":"flac","duration":383,"track":2,"discNumber":1,"year":1997,"path":"Radiohead/OK Computer/02 - Paranoid Android.flac"}
            ]}}}"#
        } else if pq.contains("getPlaylists.view") {
            br#"{"subsonic-response":{"status":"ok","playlists":{"playlist":[{"id":"pl1","name":"Favorites","songCount":2}]}}}"#
        } else if pq.contains("getPlaylist.view") {
            br#"{"subsonic-response":{"status":"ok","playlist":{"id":"pl1","name":"Favorites","entry":[
                {"id":"tr2","title":"Paranoid Android","suffix":"flac"},
                {"id":"tr1","title":"Airbag","suffix":"flac"}
            ]}}}"#
        } else {
            br#"{"subsonic-response":{"status":"ok"}}"#
        };

        let boxed = resp.to_vec().into_boxed_slice();
        let len = boxed.len();
        let raw = Box::into_raw(boxed) as *mut u8;
        unsafe {
            *out_data = raw;
            *out_len = len;
        }
        0
    }

    unsafe extern "C" fn mock_c_http_free(ptr: *mut u8) {
        if !ptr.is_null() {
            // Allocated from Vec::into_boxed_slice; reconstruct with capacity 1? No: use libc/alloc or header!
            // In this test, leak-free via Layout is unnecessary or we can store len; ignore tiny test buffer.
            let _ = ptr;
        }
    }

    unsafe extern "C" fn mock_c_http_download(
        _is_https: u8,
        _host: *const c_char,
        _port: u16,
        _path_and_query: *const c_char,
        _allow_self_signed: u8,
        dest_path: *const c_char,
        _timeout_ms: i32,
        _cancel_cb: TpNavCancelFn,
        _cancel_ctx: *mut c_void,
        _err_buf: *mut c_char,
        _err_cap: usize,
    ) -> i32 {
        let dst = unsafe { CStr::from_ptr(dest_path) }.to_string_lossy().into_owned();
        std::fs::write(dst, vec![0xABu8; 1024]).unwrap();
        0
    }

    unsafe extern "C" fn on_song_cb(ctx: *mut c_void, song: *const TpNavSongC) {
        let vec = unsafe { &mut *(ctx as *mut Vec<String>) };
        let s = unsafe { &*song };
        let title = unsafe { CStr::from_ptr(s.title) }.to_string_lossy().into_owned();
        unsafe { tp_navidrome_tree_insert(s.server_path, s.local_track_path, s.title) };
        vec.push(title);
    }

    #[test]
    fn test_c_abi_sync_and_tree_end_to_end() {
        let tmp = std::env::temp_dir().join("tp_nav_c_abi_test");
        let _ = std::fs::remove_dir_all(&tmp);
        let c_root = CString::new(tmp.to_string_lossy().as_bytes()).unwrap();
        let vt = TpNavHttpVtable {
            http_get: mock_c_http_get,
            http_free: mock_c_http_free,
            http_download_file: mock_c_http_download,
        };
        unsafe {
            assert!(tp_navidrome_init(c_root.as_ptr(), &vt));
            let ini = CString::new("url=http://10.0.0.5:4533\nusername=admin\npassword=secret\nquality=mp3_320\n").unwrap();
            assert!(tp_navidrome_load_config_text(ini.as_ptr()));
            assert!(tp_navidrome_is_configured());

            tp_navidrome_tree_clear();
            let mut titles: Vec<String> = Vec::new();
            let mut err = [0i8; 128];
            let count = tp_navidrome_sync_catalog(
                Some(on_song_cb),
                None,
                None,
                None,
                &mut titles as *mut _ as *mut c_void,
                err.as_mut_ptr(),
                err.len(),
            );
            assert_eq!(count, 2);
            assert_eq!(titles, vec!["Airbag", "Paranoid Android"]);

            tp_navidrome_tree_finalize();
            let rel = CString::new("Radiohead/OK Computer").unwrap();
            assert_eq!(tp_navidrome_tree_list_count(rel.as_ptr()), 2);

            let mut entry = TpNavTreeEntryC {
                is_dir: 0,
                name: [0; 256],
                target_path: [0; 512],
            };
            assert!(tp_navidrome_tree_list_get(rel.as_ptr(), 0, &mut entry));
            assert_eq!(entry.is_dir, 0);
            let tp_cstr = CStr::from_ptr(entry.target_path.as_ptr() as *const c_char);
            assert!(tp_cstr
                .to_string_lossy()
                .ends_with("Radiohead/OK Computer/01 - Airbag.320k.mp3"));
            assert!(tp_navidrome_is_track_path(tp_cstr.as_ptr()));

            let s_id = CString::new("tr1").unwrap();
            let c_id = CString::new("cov1").unwrap();
            assert_eq!(
                tp_navidrome_ensure_track_cached(
                    tp_cstr.as_ptr(),
                    s_id.as_ptr(),
                    c_id.as_ptr(),
                    None,
                    std::ptr::null_mut(),
                    err.as_mut_ptr(),
                    err.len(),
                ),
                0
            );

            let mut cov_out = [0i8; 256];
            assert!(tp_navidrome_ensure_cover_cached(
                tp_cstr.as_ptr(),
                c_id.as_ptr(),
                cov_out.as_mut_ptr(),
                cov_out.len(),
            ));
            let cov_str = CStr::from_ptr(cov_out.as_ptr()).to_string_lossy();
            assert!(cov_str.ends_with("Radiohead/OK Computer/cover.jpg"));

            assert!(!tp_navidrome_is_track_pinned(tp_cstr.as_ptr()));
            assert!(tp_navidrome_pin_track(tp_cstr.as_ptr()));
            assert!(tp_navidrome_is_track_pinned(tp_cstr.as_ptr()));

            tp_navidrome_shutdown();
        }
        let _ = std::fs::remove_dir_all(&tmp);
    }
}
