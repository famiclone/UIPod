//! Persistent SD-card video & audio cache/downloads for Jellyfin:
//! - Videos: `/mnt/SDCARD/Media/Videos/jellyfin` (`Movies/...`, `Shows/<Series>/Season <NN>/...`, `Videos/...`)
//! - Audio: `/mnt/SDCARD/Media/Music/jellyfin` (`<Artist>/<Album>/<01 - Title>.<ext>` + `cover.jpg`)
//! Includes VFAT sanitization, `MAX_PATH` bounding, atomic `.part` downloads, pinning, LRU eviction,
//! automatic 401 re-authentication, and background audio prefetch + session reporting worker.

use std::collections::{HashSet, VecDeque};
use std::fs;
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, SystemTime};

use crate::config::{AudioQuality, JellyfinConfig};
use crate::json::{
    decode_auth_response, decode_first_user_id, JellyfinVideoItem, JF_ITEM_EPISODE, JF_ITEM_MOVIE,
};
use crate::url::{
    json_escape_str, plan_video_stream, ParsedJellyfinUrl, VideoPlanInput,
};

const PINNED_MANIFEST_NAME: &str = ".uipod_pinned";
const COVERS_FALLBACK_DIR: &str = ".covers";
/// Keep full local file paths comfortably below Rockbox `MAX_PATH` (260),
/// leaving headroom for `.part<N>` suffixes during atomic writes.
const MAX_FULL_PATH_BYTES: usize = 228;

#[derive(Debug, Clone)]
pub struct HttpResponse {
    pub status: i32,
    pub body: Vec<u8>,
}

/// Transport trait implemented by the C OpenSSL/socket bridge in production
/// and by mock transports in unit tests.
pub trait JfHttpTransport: Send + Sync + 'static {
    #[allow(clippy::too_many_arguments)]
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
    ) -> Result<HttpResponse, String>;

    #[allow(clippy::too_many_arguments)]
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
    ) -> Result<(), String>;
}

fn fnv1a_hex(bytes: &[u8]) -> String {
    let mut h: u64 = 0xcbf29ce484222325;
    for &b in bytes {
        h ^= b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    format!("{h:016x}")
}

pub fn safe_filename_id(raw_id: &str) -> String {
    let trimmed = raw_id.trim();
    if !trimmed.is_empty()
        && trimmed.len() <= 64
        && trimmed
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
    {
        return trimmed.to_string();
    }
    format!("jf_{}", fnv1a_hex(trimmed.as_bytes()))
}

fn truncate_utf8(s: &str, max_bytes: usize) -> &str {
    if s.len() <= max_bytes {
        return s;
    }
    let mut end = max_bytes;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

fn is_fat_reserved_name(name: &str) -> bool {
    let stem = name.split('.').next().unwrap_or(name).trim();
    if stem.is_empty() {
        return false;
    }
    let up = stem.to_ascii_uppercase();
    matches!(
        up.as_str(),
        "CON"
            | "PRN"
            | "AUX"
            | "NUL"
            | "COM1"
            | "COM2"
            | "COM3"
            | "COM4"
            | "COM5"
            | "COM6"
            | "COM7"
            | "COM8"
            | "COM9"
            | "LPT1"
            | "LPT2"
            | "LPT3"
            | "LPT4"
            | "LPT5"
            | "LPT6"
            | "LPT7"
            | "LPT8"
            | "LPT9"
    )
}

/// Sanitize a single directory or filename stem component for FAT32/exFAT (`vfat`).
pub fn sanitize_vfat_component(raw: &str, max_bytes: usize) -> String {
    let mut mapped = String::with_capacity(raw.len());
    for c in raw.trim().chars() {
        if c < ' '
            || c == '\x7f'
            || matches!(c, '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*')
        {
            mapped.push('_');
        } else {
            mapped.push(c);
        }
    }

    let mut out = truncate_utf8(mapped.trim_start_matches(' '), max_bytes).to_string();
    while out.ends_with(' ') || out.ends_with('.') {
        out.pop();
    }
    while out.starts_with('.') {
        out.remove(0);
    }
    out = out.trim_start_matches(' ').to_string();

    if is_fat_reserved_name(&out) {
        let prefixed = format!("_{out}");
        out = truncate_utf8(&prefixed, max_bytes)
            .trim_end_matches([' ', '.'])
            .to_string();
    }

    if out.is_empty() || out == "." || out == ".." {
        "_".to_string()
    } else {
        out
    }
}

pub fn audio_file_suffix(original_suffix: &str, quality: AudioQuality) -> String {
    match quality {
        AudioQuality::Mp3_320 => ".320k.mp3".to_string(),
        AudioQuality::Mp3_192 => ".192k.mp3".to_string(),
        AudioQuality::Original => {
            let ext = quality.file_ext(original_suffix);
            let clean: String = ext
                .chars()
                .filter(|c| c.is_ascii_alphanumeric())
                .take(8)
                .collect();
            if clean.is_empty() {
                ".mp3".to_string()
            } else {
                format!(".{clean}")
            }
        }
    }
}

fn fit_path_with_suffix(dir: &Path, stem: &str, suffix_with_dot: &str, fallback_id: &str) -> PathBuf {
    let dir_len = dir.to_string_lossy().len() + 1;
    let max_stem = MAX_FULL_PATH_BYTES
        .saturating_sub(dir_len + suffix_with_dot.len())
        .clamp(8, 120);
    let clean_stem = sanitize_vfat_component(stem, max_stem);
    let candidate = dir.join(format!("{clean_stem}{suffix_with_dot}"));
    if candidate.to_string_lossy().len() <= MAX_FULL_PATH_BYTES {
        candidate
    } else {
        let short_id = sanitize_vfat_component(&safe_filename_id(fallback_id), 24);
        dir.join(format!("{short_id}{suffix_with_dot}"))
    }
}

/// Build a VFAT-safe local video path under `video_root` (`/mnt/SDCARD/Media/Videos/jellyfin`).
pub fn build_video_local_path(
    video_root: &Path,
    item: &JellyfinVideoItem,
    download_ext: &str,
) -> PathBuf {
    if !item.is_playable() {
        return PathBuf::new();
    }
    let clean_ext: String = download_ext
        .trim()
        .trim_start_matches('.')
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .take(6)
        .collect();
    let suffix = if clean_ext.is_empty() {
        ".mp4".to_string()
    } else {
        format!(".{clean_ext}")
    };

    match item.item_type {
        JF_ITEM_MOVIE => {
            let dir = video_root.join("Movies");
            let raw_stem = if item.year > 0 {
                format!("{} ({})", item.name.trim(), item.year)
            } else {
                item.name.trim().to_string()
            };
            fit_path_with_suffix(&dir, &raw_stem, &suffix, &item.id)
        }
        JF_ITEM_EPISODE => {
            let series_dir = if item.series_name.trim().is_empty() {
                "Unknown Series".to_string()
            } else {
                sanitize_vfat_component(&item.series_name, 52)
            };
            let season_dir = if item.season_number > 0 {
                format!("Season {:02}", item.season_number)
            } else if !item.season_name.trim().is_empty() {
                sanitize_vfat_component(&item.season_name, 28)
            } else {
                "Specials".to_string()
            };
            let dir = video_root.join("Shows").join(series_dir).join(season_dir);
            let raw_stem = if item.season_number >= 0 && item.episode_number > 0 {
                format!(
                    "S{:02}E{:02} - {}",
                    item.season_number.max(0),
                    item.episode_number,
                    item.name.trim()
                )
            } else if item.episode_number > 0 {
                format!("{:02} - {}", item.episode_number, item.name.trim())
            } else {
                item.name.trim().to_string()
            };
            fit_path_with_suffix(&dir, &raw_stem, &suffix, &item.id)
        }
        _ => {
            let dir = video_root.join("Videos");
            fit_path_with_suffix(&dir, item.name.trim(), &suffix, &item.id)
        }
    }
}

pub fn disambiguate_media_path(base_path: &Path, item_id: &str) -> PathBuf {
    let Some(parent) = base_path.parent() else {
        return base_path.to_path_buf();
    };
    let file_name = base_path
        .file_name()
        .map(|f| f.to_string_lossy().into_owned())
        .unwrap_or_else(|| "media.mp4".to_string());
    let (stem, dot_ext) = match file_name.rsplit_once('.') {
        Some((s, e)) => (s, format!(".{e}")),
        None => (file_name.as_str(), String::new()),
    };
    let short_tag: String = safe_filename_id(item_id).chars().take(8).collect();
    let tagged = format!("{stem} [{short_tag}]");
    fit_path_with_suffix(parent, &tagged, &dot_ext, item_id)
}

/// Build a VFAT-safe local audio path under `audio_root` (`/mnt/SDCARD/Media/Music/jellyfin`).
#[allow(clippy::too_many_arguments)]
pub fn build_audio_track_path(
    audio_root: &Path,
    artist: &str,
    album: &str,
    track_no: i32,
    title: &str,
    song_id: &str,
    original_suffix: &str,
    quality: AudioQuality,
) -> PathBuf {
    let suffix = audio_file_suffix(original_suffix, quality);
    let art_dir = sanitize_vfat_component(
        if artist.trim().is_empty() {
            "Unknown Artist"
        } else {
            artist
        },
        48,
    );
    let alb_dir = sanitize_vfat_component(
        if album.trim().is_empty() {
            "Unknown Album"
        } else {
            album
        },
        52,
    );
    let dir = audio_root.join(art_dir).join(alb_dir);
    let raw_title = if title.trim().is_empty() {
        safe_filename_id(song_id)
    } else {
        title.trim().to_string()
    };
    let stem = if track_no > 0 {
        format!("{track_no:02} - {raw_title}")
    } else {
        raw_title
    };
    fit_path_with_suffix(&dir, &stem, &suffix, song_id)
}

/// Check if `planned_path` or a sibling video container extension (`.mp4`, `.mkv`, `.ts`, `.avi`, `.mov`)
/// exists on disk with size > 0.
pub fn resolve_existing_video_path(planned_path: &Path) -> Option<PathBuf> {
    if JfCacheManager::file_ready(planned_path) {
        return Some(planned_path.to_path_buf());
    }
    let parent = planned_path.parent()?;
    let stem = planned_path.file_stem()?.to_string_lossy();
    for ext in ["ts", "mp4", "mkv", "avi", "mov", "m4v"] {
        let alt = parent.join(format!("{stem}.{ext}"));
        if JfCacheManager::file_ready(&alt) {
            return Some(alt);
        }
    }
    None
}

#[derive(Debug, Clone)]
enum WorkerJob {
    PrefetchAudio {
        local_path: PathBuf,
        song_id: String,
        original_suffix: String,
        cover_id: String,
    },
    FetchCover {
        local_track_path: PathBuf,
        cover_id: String,
    },
    ReportSession {
        endpoint_kind: u8, // 0 = Playing, 1 = Progress, 2 = Stopped
        item_id: String,
        media_source_id: String,
        play_session_id: String,
        position_ms: u32,
        is_paused: bool,
    },
    Shutdown,
}

struct WorkerQueue {
    jobs: VecDeque<WorkerJob>,
    stopped: bool,
}

pub struct JfCacheManager {
    video_root: PathBuf,
    audio_root: PathBuf,
    covers_fallback_dir: PathBuf,
    pinned_manifest_path: PathBuf,
    config: Mutex<JellyfinConfig>,
    pinned: Mutex<HashSet<String>>,
    inflight: Mutex<HashSet<PathBuf>>,
    inflight_cv: Condvar,
    part_seq: AtomicU64,
    prefetch_done: AtomicBool,
    transport: Arc<dyn JfHttpTransport>,
    queue: Mutex<WorkerQueue>,
    queue_cv: Condvar,
    worker: Mutex<Option<JoinHandle<()>>>,
}

impl JfCacheManager {
    pub fn new(
        video_root: PathBuf,
        audio_root: PathBuf,
        transport: Arc<dyn JfHttpTransport>,
    ) -> Arc<Self> {
        let covers_fallback_dir = audio_root.join(COVERS_FALLBACK_DIR);
        let pinned_manifest_path = audio_root.join(PINNED_MANIFEST_NAME);
        let initial_pinned = Self::load_pinned_manifest(&pinned_manifest_path, &audio_root);

        let mgr = Arc::new(Self {
            video_root,
            audio_root,
            covers_fallback_dir,
            pinned_manifest_path,
            config: Mutex::new(JellyfinConfig::default()),
            pinned: Mutex::new(initial_pinned),
            inflight: Mutex::new(HashSet::new()),
            inflight_cv: Condvar::new(),
            part_seq: AtomicU64::new(1),
            prefetch_done: AtomicBool::new(false),
            transport,
            queue: Mutex::new(WorkerQueue {
                jobs: VecDeque::new(),
                stopped: false,
            }),
            queue_cv: Condvar::new(),
            worker: Mutex::new(None),
        });

        mgr.cleanup_stale_part_files();

        let worker_mgr = Arc::clone(&mgr);
        if let Ok(handle) = thread::Builder::new()
            .name("uipod-jf-bg".to_string())
            .spawn(move || worker_mgr.worker_loop())
        {
            *mgr.worker.lock().unwrap() = Some(handle);
        }

        mgr
    }

    pub fn shutdown(&self) {
        {
            let mut q = self.queue.lock().unwrap();
            if q.stopped {
                return;
            }
            q.stopped = true;
            q.jobs.clear();
            q.jobs.push_back(WorkerJob::Shutdown);
            self.queue_cv.notify_all();
        }
        if let Some(handle) = self.worker.lock().unwrap().take() {
            let _ = handle.join();
        }
    }

    pub fn video_root(&self) -> &Path {
        &self.video_root
    }

    pub fn audio_root(&self) -> &Path {
        &self.audio_root
    }

    pub fn transport(&self) -> &Arc<dyn JfHttpTransport> {
        &self.transport
    }

    pub fn is_busy(&self) -> bool {
        !self.inflight.lock().unwrap().is_empty()
    }

    pub fn set_config(&self, mut new_cfg: JellyfinConfig) {
        let mut cur = self.config.lock().unwrap();
        let same_creds = cur.url.trim() == new_cfg.url.trim()
            && cur.username.trim() == new_cfg.username.trim()
            && cur.password == new_cfg.password;
        if same_creds {
            if new_cfg.access_token.is_empty() {
                new_cfg.access_token = cur.access_token.clone();
            }
            if new_cfg.user_id.is_empty() {
                new_cfg.user_id = cur.user_id.clone();
            }
        }
        *cur = new_cfg;
    }

    pub fn get_config(&self) -> JellyfinConfig {
        self.config.lock().unwrap().clone()
    }

    pub fn set_auth_tokens(&self, access_token: String, user_id: String) {
        let mut cur = self.config.lock().unwrap();
        cur.access_token = access_token;
        cur.user_id = user_id;
    }

    pub fn clear_auth_tokens(&self) {
        let mut cur = self.config.lock().unwrap();
        cur.access_token.clear();
        cur.user_id.clear();
    }

    /// Ensure we have a valid `(ParsedJellyfinUrl, access_token, user_id, JellyfinConfig)`.
    /// Performs `POST /Users/AuthenticateByName` (or API-key `GET /Users`) if needed.
    pub fn ensure_authenticated(
        &self,
        force_reauth: bool,
    ) -> Result<(ParsedJellyfinUrl, String, String, JellyfinConfig), String> {
        let cfg = self.get_config();
        let parsed = ParsedJellyfinUrl::parse(&cfg.url)?;

        if !force_reauth && !cfg.access_token.trim().is_empty() && !cfg.user_id.trim().is_empty() {
            return Ok((
                parsed,
                cfg.access_token.clone(),
                cfg.user_id.clone(),
                cfg,
            ));
        }

        let uname = cfg.username.trim();
        if !uname.is_empty() {
            let body = format!(
                "{{\"Username\":\"{}\",\"Pw\":\"{}\"}}",
                json_escape_str(uname),
                json_escape_str(&cfg.password)
            );
            let headers = ParsedJellyfinUrl::auth_headers(None, true);
            let resp = self.transport.request(
                parsed.is_https,
                &parsed.host,
                parsed.port,
                "POST",
                &parsed.auth_by_name_path(),
                &headers,
                Some(body.as_bytes()),
                cfg.allow_self_signed,
                12_000,
            )?;
            if resp.status == 401 || resp.status == 403 {
                return Err("Invalid username or password".to_string());
            }
            if !(200..300).contains(&resp.status) {
                return Err(format!("Auth HTTP {}", resp.status));
            }
            let auth = decode_auth_response(&resp.body)?;
            self.set_auth_tokens(auth.access_token.clone(), auth.user_id.clone());
            let updated_cfg = self.get_config();
            Ok((parsed, auth.access_token, auth.user_id, updated_cfg))
        } else {
            // API-key mode: `password` holds the Jellyfin API key (or existing `access_token`)
            let api_key = if !cfg.password.trim().is_empty() {
                cfg.password.trim().to_string()
            } else if !cfg.access_token.trim().is_empty() {
                cfg.access_token.trim().to_string()
            } else {
                return Err("Missing username or API key".to_string());
            };
            let headers = ParsedJellyfinUrl::auth_headers(Some(&api_key), false);
            let resp = self.transport.request(
                parsed.is_https,
                &parsed.host,
                parsed.port,
                "GET",
                &parsed.users_list_path(),
                &headers,
                None,
                cfg.allow_self_signed,
                10_000,
            )?;
            if resp.status == 401 || resp.status == 403 {
                return Err("Invalid API key".to_string());
            }
            if !(200..300).contains(&resp.status) {
                return Err(format!("Users HTTP {}", resp.status));
            }
            let uid = decode_first_user_id(&resp.body)?;
            self.set_auth_tokens(api_key.clone(), uid.clone());
            let updated_cfg = self.get_config();
            Ok((parsed, api_key, uid, updated_cfg))
        }
    }

    /// Perform an authenticated GET with automatic single retry on HTTP 401.
    pub fn authed_get_json(&self, build_path: impl Fn(&ParsedJellyfinUrl, &str, &str) -> String, timeout_ms: i32) -> Result<Vec<u8>, String> {
        let (parsed, tok, uid, cfg) = self.ensure_authenticated(false)?;
        let pq = build_path(&parsed, &uid, &tok);
        let headers = ParsedJellyfinUrl::auth_headers(Some(&tok), false);
        let resp = self.transport.request(
            parsed.is_https,
            &parsed.host,
            parsed.port,
            "GET",
            &pq,
            &headers,
            None,
            cfg.allow_self_signed,
            timeout_ms,
        )?;
        if resp.status == 401 && !cfg.username.trim().is_empty() {
            let (parsed2, tok2, uid2, cfg2) = self.ensure_authenticated(true)?;
            let pq2 = build_path(&parsed2, &uid2, &tok2);
            let headers2 = ParsedJellyfinUrl::auth_headers(Some(&tok2), false);
            let resp2 = self.transport.request(
                parsed2.is_https,
                &parsed2.host,
                parsed2.port,
                "GET",
                &pq2,
                &headers2,
                None,
                cfg2.allow_self_signed,
                timeout_ms,
            )?;
            if !(200..300).contains(&resp2.status) {
                return Err(format!("HTTP {}", resp2.status));
            }
            return Ok(resp2.body);
        }
        if resp.status == 401 || resp.status == 403 {
            return Err("Authentication failed".to_string());
        }
        if !(200..300).contains(&resp.status) {
            return Err(format!("HTTP {}", resp.status));
        }
        Ok(resp.body)
    }

    pub fn is_audio_track_path(&self, path_str: &str) -> bool {
        let p = Path::new(path_str);
        if !p.starts_with(&self.audio_root) {
            return false;
        }
        if p.starts_with(&self.covers_fallback_dir) {
            return false;
        }
        let Some(fname) = p.file_name().and_then(|n| n.to_str()) else {
            return false;
        };
        if fname.starts_with('.') || fname.eq_ignore_ascii_case("cover.jpg") || fname.contains(".part") {
            return false;
        }
        true
    }

    pub fn is_video_path(&self, path_str: &str) -> bool {
        let p = Path::new(path_str);
        if !p.starts_with(&self.video_root) {
            return false;
        }
        let Some(fname) = p.file_name().and_then(|n| n.to_str()) else {
            return false;
        };
        !fname.starts_with('.') && !fname.contains(".part")
    }

    pub fn cover_path_for_track(&self, local_track_path: &Path, cover_id: &str) -> PathBuf {
        if let Some(parent) = local_track_path.parent() {
            if parent.starts_with(&self.audio_root) && parent != self.audio_root {
                return parent.join("cover.jpg");
            }
        }
        if cover_id.trim().is_empty() {
            return PathBuf::new();
        }
        self.covers_fallback_dir
            .join(format!("{}.jpg", safe_filename_id(cover_id)))
    }

    pub fn file_ready(path: &Path) -> bool {
        fs::metadata(path)
            .map(|m| m.is_file() && m.len() > 0)
            .unwrap_or(false)
    }

    fn rel_audio_key(&self, path: &Path) -> Option<String> {
        let rel = path.strip_prefix(&self.audio_root).ok()?;
        let mut parts = Vec::new();
        for comp in rel.components() {
            if let Component::Normal(os) = comp {
                parts.push(os.to_string_lossy().into_owned());
            }
        }
        if parts.is_empty() {
            None
        } else {
            Some(parts.join("/"))
        }
    }

    fn load_pinned_manifest(manifest_path: &Path, audio_root: &Path) -> HashSet<String> {
        let mut set = HashSet::new();
        let Ok(text) = fs::read_to_string(manifest_path) else {
            return set;
        };
        for raw in text.lines() {
            let line = raw.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let full = audio_root.join(line);
            if Self::file_ready(&full) {
                set.insert(line.to_string());
            }
        }
        set
    }

    fn save_pinned_manifest_locked(&self, set: &HashSet<String>) {
        let mut sorted: Vec<&str> = set.iter().map(|s| s.as_str()).collect();
        sorted.sort_unstable();
        let mut out = String::new();
        for item in sorted {
            out.push_str(item);
            out.push('\n');
        }
        let tmp = self.audio_root.join(".uipod_pinned.tmp");
        if fs::write(&tmp, out.as_bytes()).is_ok() {
            let _ = fs::rename(&tmp, &self.pinned_manifest_path);
        }
    }

    pub fn pin_audio_track(&self, local_path: &Path) -> bool {
        let Some(key) = self.rel_audio_key(local_path) else {
            return false;
        };
        let mut set = self.pinned.lock().unwrap();
        if set.insert(key) {
            self.save_pinned_manifest_locked(&set);
        }
        true
    }

    pub fn is_audio_pinned(&self, local_path: &Path) -> bool {
        let Some(key) = self.rel_audio_key(local_path) else {
            return false;
        };
        self.pinned.lock().unwrap().contains(&key)
    }

    fn acquire_inflight(&self, target: &Path, cancel_fn: Option<&(dyn Fn() -> bool + Sync)>) -> Result<bool, String> {
        let mut guard = self.inflight.lock().unwrap();
        while guard.contains(target) {
            if let Some(cb) = cancel_fn {
                if cb() {
                    return Err("Cancelled".to_string());
                }
            }
            let (next_guard, _) = self
                .inflight_cv
                .wait_timeout(guard, Duration::from_millis(100))
                .unwrap();
            guard = next_guard;
            if Self::file_ready(target) {
                return Ok(false);
            }
        }
        if Self::file_ready(target) {
            return Ok(false);
        }
        guard.insert(target.to_path_buf());
        Ok(true)
    }

    fn release_inflight(&self, target: &Path) {
        let mut guard = self.inflight.lock().unwrap();
        guard.remove(target);
        self.inflight_cv.notify_all();
    }

    fn download_atomic(
        &self,
        is_https: bool,
        host: &str,
        port: u16,
        path_and_query: &str,
        extra_headers: &str,
        allow_self_signed: bool,
        dest: &Path,
        timeout_ms: i32,
        cancel_fn: Option<&(dyn Fn() -> bool + Sync)>,
    ) -> Result<(), String> {
        if let Some(parent) = dest.parent() {
            fs::create_dir_all(parent).map_err(|e| format!("mkdir failed: {e}"))?;
        }
        let seq = self.part_seq.fetch_add(1, Ordering::Relaxed);
        let fname = dest
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| "dl.bin".to_string());
        let part_path = dest
            .parent()
            .unwrap_or(&self.video_root)
            .join(format!(".{fname}.part{seq}"));

        let res = self.transport.download_to_file(
            is_https,
            host,
            port,
            path_and_query,
            extra_headers,
            allow_self_signed,
            &part_path,
            timeout_ms,
            cancel_fn,
        );

        if let Err(e) = res {
            let _ = fs::remove_file(&part_path);
            return Err(e);
        }

        if !Self::file_ready(&part_path) {
            let _ = fs::remove_file(&part_path);
            return Err("Empty response from Jellyfin".to_string());
        }

        fs::rename(&part_path, dest).map_err(|e| {
            let _ = fs::remove_file(&part_path);
            format!("Rename failed: {e}")
        })
    }

    pub fn ensure_cover_at_path(&self, cover_path: &Path, cover_id: &str) -> Option<PathBuf> {
        if cover_id.trim().is_empty() || cover_path.as_os_str().is_empty() {
            return None;
        }
        if Self::file_ready(cover_path) {
            return Some(cover_path.to_path_buf());
        }
        if !self.acquire_inflight(cover_path, None).ok()? {
            return Some(cover_path.to_path_buf());
        }
        let res = (|| -> Result<(), String> {
            let (parsed, tok, _uid, cfg) = self.ensure_authenticated(false)?;
            let pq = parsed.item_image_path(cover_id, 500, &tok);
            let headers = ParsedJellyfinUrl::stream_headers(Some(&tok));
            self.download_atomic(
                parsed.is_https,
                &parsed.host,
                parsed.port,
                &pq,
                &headers,
                cfg.allow_self_signed,
                cover_path,
                10_000,
                None,
            )
        })();
        self.release_inflight(cover_path);
        res.ok().map(|()| cover_path.to_path_buf())
    }

    #[allow(clippy::too_many_arguments)]
    pub fn prepare_audio_track(
        &self,
        local_path: &Path,
        song_id: &str,
        original_suffix: &str,
        cover_id: &str,
        pin: bool,
        cancel_fn: Option<&(dyn Fn() -> bool + Sync)>,
    ) -> Result<PathBuf, String> {
        if Self::file_ready(local_path) {
            if pin {
                self.pin_audio_track(local_path);
            }
            if !cover_id.is_empty() {
                self.enqueue_cover_for_track(local_path, cover_id);
            }
            return Ok(local_path.to_path_buf());
        }

        let acquired = self.acquire_inflight(local_path, cancel_fn)?;
        if !acquired {
            if pin {
                self.pin_audio_track(local_path);
            }
            return Ok(local_path.to_path_buf());
        }

        let res = (|| -> Result<(), String> {
            let (parsed, tok, _uid, cfg) = self.ensure_authenticated(false)?;
            let pq = parsed.audio_stream_path(song_id, original_suffix, cfg.audio_quality, &tok);
            let headers = ParsedJellyfinUrl::stream_headers(Some(&tok));
            self.download_atomic(
                parsed.is_https,
                &parsed.host,
                parsed.port,
                &pq,
                &headers,
                cfg.allow_self_signed,
                local_path,
                30_000,
                cancel_fn,
            )
        })();
        self.release_inflight(local_path);
        res?;

        if pin {
            self.pin_audio_track(local_path);
        } else {
            self.evict_audio_lru_if_needed(Some(local_path));
        }

        if !cover_id.is_empty() {
            let cp = self.cover_path_for_track(local_path, cover_id);
            let _ = self.ensure_cover_at_path(&cp, cover_id);
        }

        Ok(local_path.to_path_buf())
    }

    /// Download a Jellyfin video item (Direct Stream or transcoded `.ts`) to SD card for offline playback.
    #[allow(clippy::too_many_arguments)]
    pub fn download_video_item(
        &self,
        planned_local_path: &Path,
        item_id: &str,
        media_source_id: &str,
        container: &str,
        video_codec: &str,
        audio_codec: &str,
        width: u32,
        height: u32,
        bit_depth: u32,
        bitrate: u32,
        cancel_fn: Option<&(dyn Fn() -> bool + Sync)>,
    ) -> Result<PathBuf, String> {
        if let Some(existing) = resolve_existing_video_path(planned_local_path) {
            return Ok(existing);
        }

        let (parsed, tok, _uid, cfg) = self.ensure_authenticated(false)?;
        let dl_sid = format!("uipod-dl-{}", safe_filename_id(item_id));
        let plan = plan_video_stream(
            &parsed,
            &VideoPlanInput {
                item_id,
                media_source_id,
                container,
                video_codec,
                audio_codec,
                width,
                height,
                bit_depth,
                bitrate,
                quality: cfg.video_quality,
                start_ms: 0,
                play_session_id: &dl_sid,
                token: &tok,
            },
        );

        let dest = if let (Some(parent), Some(stem)) =
            (planned_local_path.parent(), planned_local_path.file_stem())
        {
            parent.join(format!("{}.{}", stem.to_string_lossy(), plan.download_ext))
        } else {
            planned_local_path.to_path_buf()
        };

        if Self::file_ready(&dest) {
            return Ok(dest);
        }

        let acquired = self.acquire_inflight(&dest, cancel_fn)?;
        if !acquired {
            return Ok(dest);
        }

        let headers = ParsedJellyfinUrl::stream_headers(Some(&tok));
        let res = self.download_atomic(
            parsed.is_https,
            &parsed.host,
            parsed.port,
            &plan.path_and_query,
            &headers,
            cfg.allow_self_signed,
            &dest,
            45_000,
            cancel_fn,
        );

        if plan.is_transcoded {
            let del_pq = parsed.active_encodings_delete_path(&dl_sid);
            let del_hdr = ParsedJellyfinUrl::auth_headers(Some(&tok), false);
            let _ = self.transport.request(
                parsed.is_https,
                &parsed.host,
                parsed.port,
                "DELETE",
                &del_pq,
                &del_hdr,
                None,
                cfg.allow_self_signed,
                4_000,
            );
        }

        self.release_inflight(&dest);
        res?;
        Ok(dest)
    }

    pub fn stop_active_encoding(&self, play_session_id: &str) {
        let Ok((parsed, tok, _uid, cfg)) = self.ensure_authenticated(false) else {
            return;
        };
        let pq = parsed.active_encodings_delete_path(play_session_id);
        let headers = ParsedJellyfinUrl::auth_headers(Some(&tok), false);
        let _ = self.transport.request(
            parsed.is_https,
            &parsed.host,
            parsed.port,
            "DELETE",
            &pq,
            &headers,
            None,
            cfg.allow_self_signed,
            3_000,
        );
    }

    pub fn enqueue_session_report(
        &self,
        endpoint_kind: u8,
        item_id: &str,
        media_source_id: &str,
        play_session_id: &str,
        position_ms: u32,
        is_paused: bool,
    ) {
        if item_id.trim().is_empty() {
            return;
        }
        let mut q = self.queue.lock().unwrap();
        if q.stopped || q.jobs.len() >= 32 {
            return;
        }
        q.jobs.push_back(WorkerJob::ReportSession {
            endpoint_kind,
            item_id: item_id.to_string(),
            media_source_id: if media_source_id.trim().is_empty() {
                item_id.to_string()
            } else {
                media_source_id.to_string()
            },
            play_session_id: play_session_id.to_string(),
            position_ms,
            is_paused,
        });
        self.queue_cv.notify_one();
    }

    pub fn clear_audio_prefetches(&self) {
        let mut q = self.queue.lock().unwrap();
        q.jobs
            .retain(|job| !matches!(job, WorkerJob::PrefetchAudio { .. }));
    }

    pub fn take_prefetch_done(&self) -> bool {
        if self.prefetch_done.load(Ordering::Acquire) {
            self.prefetch_done.swap(false, Ordering::AcqRel)
        } else {
            false
        }
    }

    pub fn enqueue_prefetch(
        &self,
        local_path: PathBuf,
        song_id: String,
        original_suffix: String,
        cover_id: String,
    ) {
        if Self::file_ready(&local_path) {
            return;
        }
        let mut q = self.queue.lock().unwrap();
        if q.stopped || q.jobs.len() >= 24 {
            return;
        }
        let already_queued = q.jobs.iter().any(|job| match job {
            WorkerJob::PrefetchAudio {
                local_path: existing,
                ..
            } => existing == &local_path,
            _ => false,
        });
        if already_queued {
            return;
        }
        q.jobs.push_back(WorkerJob::PrefetchAudio {
            local_path,
            song_id,
            original_suffix,
            cover_id,
        });
        self.queue_cv.notify_one();
    }

    pub fn enqueue_cover_for_track(&self, local_track_path: &Path, cover_id: &str) {
        if cover_id.trim().is_empty() {
            return;
        }
        let cp = self.cover_path_for_track(local_track_path, cover_id);
        if Self::file_ready(&cp) {
            return;
        }
        let mut q = self.queue.lock().unwrap();
        if q.stopped || q.jobs.len() >= 32 {
            return;
        }
        q.jobs.push_back(WorkerJob::FetchCover {
            local_track_path: local_track_path.to_path_buf(),
            cover_id: cover_id.to_string(),
        });
        self.queue_cv.notify_one();
    }

    fn worker_loop(&self) {
        loop {
            let job = {
                let mut q = self.queue.lock().unwrap();
                while q.jobs.is_empty() && !q.stopped {
                    q = self.queue_cv.wait(q).unwrap();
                }
                if q.stopped && q.jobs.is_empty() {
                    return;
                }
                q.jobs.pop_front()
            };

            let Some(job) = job else {
                continue;
            };

            match job {
                WorkerJob::Shutdown => return,
                WorkerJob::PrefetchAudio {
                    local_path,
                    song_id,
                    original_suffix,
                    cover_id,
                } => {
                    let was_ready = Self::file_ready(&local_path);
                    if self
                        .prepare_audio_track(
                            &local_path,
                            &song_id,
                            &original_suffix,
                            &cover_id,
                            false,
                            None,
                        )
                        .is_ok()
                        && !was_ready
                    {
                        self.prefetch_done.store(true, Ordering::Release);
                    }
                }
                WorkerJob::FetchCover {
                    local_track_path,
                    cover_id,
                } => {
                    let cp = self.cover_path_for_track(&local_track_path, &cover_id);
                    let _ = self.ensure_cover_at_path(&cp, &cover_id);
                }
                WorkerJob::ReportSession {
                    endpoint_kind,
                    item_id,
                    media_source_id,
                    play_session_id,
                    position_ms,
                    is_paused,
                } => {
                    self.send_session_report_sync(
                        endpoint_kind,
                        &item_id,
                        &media_source_id,
                        &play_session_id,
                        position_ms,
                        is_paused,
                    );
                }
            }
        }
    }

    fn send_session_report_sync(
        &self,
        endpoint_kind: u8,
        item_id: &str,
        media_source_id: &str,
        play_session_id: &str,
        position_ms: u32,
        is_paused: bool,
    ) {
        let Ok((parsed, tok, _uid, cfg)) = self.ensure_authenticated(false) else {
            return;
        };
        let pq = match endpoint_kind {
            0 => parsed.session_playing_path(),
            2 => parsed.session_stopped_path(),
            _ => parsed.session_progress_path(),
        };
        let ticks: u64 = (position_ms as u64).saturating_mul(10_000);
        let body = format!(
            "{{\"ItemId\":\"{}\",\"MediaSourceId\":\"{}\",\"PlaySessionId\":\"{}\",\"PositionTicks\":{},\"IsPaused\":{}}}",
            json_escape_str(item_id),
            json_escape_str(media_source_id),
            json_escape_str(play_session_id),
            ticks,
            if is_paused { "true" } else { "false" }
        );
        let headers = ParsedJellyfinUrl::auth_headers(Some(&tok), true);
        let _ = self.transport.request(
            parsed.is_https,
            &parsed.host,
            parsed.port,
            "POST",
            &pq,
            &headers,
            Some(body.as_bytes()),
            cfg.allow_self_signed,
            5_000,
        );
        if endpoint_kind == 2 && !play_session_id.is_empty() {
            self.stop_active_encoding(play_session_id);
        }
    }

    fn evict_audio_lru_if_needed(&self, protect: Option<&Path>) {
        let limit_mb = self.get_config().cache_limit_mb;
        if limit_mb == 0 {
            return;
        }
        let max_bytes = (limit_mb as u64).saturating_mul(1024 * 1024);

        let mut files: Vec<(PathBuf, u64, SystemTime)> = Vec::new();
        let mut total_unpinned: u64 = 0;
        let pinned_snapshot = self.pinned.lock().unwrap().clone();

        Self::walk_dir(&self.audio_root, &mut |p, meta| {
            let fname = p
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("");
            if fname.starts_with('.') || fname.contains(".part") {
                return;
            }
            if let Some(key) = self.rel_audio_key(p) {
                if pinned_snapshot.contains(&key) {
                    return;
                }
            }
            let sz = meta.len();
            total_unpinned = total_unpinned.saturating_add(sz);
            let mtime = meta.modified().unwrap_or(SystemTime::UNIX_EPOCH);
            files.push((p.to_path_buf(), sz, mtime));
        });

        if total_unpinned <= max_bytes {
            return;
        }

        files.sort_by_key(|(_, _, t)| *t);
        for (p, sz, _) in files {
            if total_unpinned <= max_bytes {
                break;
            }
            if protect.map(|pr| pr == p.as_path()).unwrap_or(false) {
                continue;
            }
            if fs::remove_file(&p).is_ok() {
                total_unpinned = total_unpinned.saturating_sub(sz);
            }
        }
    }

    fn walk_dir(dir: &Path, cb: &mut impl FnMut(&Path, &fs::Metadata)) {
        let Ok(rd) = fs::read_dir(dir) else {
            return;
        };
        for entry in rd.flatten() {
            let p = entry.path();
            let Ok(meta) = entry.metadata() else {
                continue;
            };
            if meta.is_dir() {
                Self::walk_dir(&p, cb);
            } else if meta.is_file() {
                cb(&p, &meta);
            }
        }
    }

    fn cleanup_stale_part_files(&self) {
        let mut parts = Vec::new();
        for root in [&self.video_root, &self.audio_root] {
            Self::walk_dir(root, &mut |p, _| {
                if let Some(name) = p.file_name().and_then(|n| n.to_str()) {
                    if name.starts_with('.') && name.contains(".part") {
                        parts.push(p.to_path_buf());
                    }
                }
            });
        }
        for p in parts {
            let _ = fs::remove_file(p);
        }
    }

    pub fn usage_bytes(&self) -> u64 {
        let mut total = 0u64;
        for root in [&self.video_root, &self.audio_root] {
            Self::walk_dir(root, &mut |p, meta| {
                let fname = p.file_name().and_then(|n| n.to_str()).unwrap_or("");
                if fname == PINNED_MANIFEST_NAME {
                    return;
                }
                total = total.saturating_add(meta.len());
            });
        }
        total
    }

    pub fn clear_cache(&self, keep_pinned: bool) {
        let pinned_snapshot = self.pinned.lock().unwrap().clone();
        let mut to_delete = Vec::new();

        Self::walk_dir(&self.audio_root, &mut |p, _| {
            let fname = p.file_name().and_then(|n| n.to_str()).unwrap_or("");
            if fname == PINNED_MANIFEST_NAME {
                if !keep_pinned {
                    to_delete.push(p.to_path_buf());
                }
                return;
            }
            if keep_pinned {
                if let Some(key) = self.rel_audio_key(p) {
                    if pinned_snapshot.contains(&key) {
                        return;
                    }
                }
            }
            to_delete.push(p.to_path_buf());
        });

        if !keep_pinned {
            Self::walk_dir(&self.video_root, &mut |p, _| {
                to_delete.push(p.to_path_buf());
            });
            self.pinned.lock().unwrap().clear();
        }

        for p in to_delete {
            let _ = fs::remove_file(p);
        }
        Self::prune_empty_dirs(&self.audio_root, true);
        Self::prune_empty_dirs(&self.video_root, true);
        let _ = fs::create_dir_all(&self.covers_fallback_dir);
    }

    fn prune_empty_dirs(dir: &Path, is_root: bool) {
        let Ok(rd) = fs::read_dir(dir) else {
            return;
        };
        for entry in rd.flatten() {
            let p = entry.path();
            if p.is_dir() {
                Self::prune_empty_dirs(&p, false);
            }
        }
        if !is_root {
            let _ = fs::remove_dir(dir);
        }
    }
}
