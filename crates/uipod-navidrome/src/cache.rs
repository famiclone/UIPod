//! Persistent SD-card audio & cover-art cache/downloads in human-readable folder hierarchy,
//! pinning (`.uipod_pinned`), LRU eviction, in-flight deduplication, and background
//! `std::thread` prefetch + Subsonic scrobble worker.

use std::collections::{HashSet, VecDeque};
use std::fs;
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, SystemTime};

use crate::config::{NavidromeConfig, StreamQuality};
use crate::md5::md5_hex;
use crate::url::ParsedServerUrl;

const PINNED_MANIFEST_NAME: &str = ".uipod_pinned";
const COVERS_FALLBACK_DIR: &str = ".covers";
/// Keep full local track paths comfortably below Rockbox `MAX_PATH` (260),
/// leaving headroom for temporary `.part<N>` suffixes during atomic writes.
const MAX_FULL_TRACK_PATH_BYTES: usize = 232;

/// Transport trait implemented by the C OpenSSL/socket bridge in production
/// and by deterministic mock transports in unit tests.
pub trait HttpTransport: Send + Sync + 'static {
    fn get_bytes(
        &self,
        is_https: bool,
        host: &str,
        port: u16,
        path_and_query: &str,
        allow_self_signed: bool,
        timeout_ms: i32,
    ) -> Result<Vec<u8>, String>;

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
    ) -> Result<(), String>;
}

/// Make an arbitrary Subsonic ID safe for vfat/exFAT filenames while preserving round-trip lookup
/// when already alphanumeric/hyphen/underscore.
pub fn safe_filename_id(raw_id: &str) -> String {
    let trimmed = raw_id.trim();
    if !trimmed.is_empty()
        && trimmed.len() <= 96
        && trimmed
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
    {
        return trimmed.to_string();
    }
    format!("h_{}", md5_hex(trimmed.as_bytes()))
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
    // Avoid leading dots so real music folders/files never masquerade as hidden dotfiles.
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

/// Return the filename extension (including leading `.`) for a track at `quality`.
///
/// - `Original` preserves the server's native extension (`.flac`, `.mp3`, `.m4a`, ...)
/// - `Mp3_320` uses `.320k.mp3`
/// - `Mp3_192` uses `.192k.mp3`
pub fn file_suffix_for_quality(original_suffix: &str, quality: StreamQuality) -> String {
    match quality {
        StreamQuality::Mp3_320 => ".320k.mp3".to_string(),
        StreamQuality::Mp3_192 => ".192k.mp3".to_string(),
        StreamQuality::Original => {
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

fn split_server_filename_stem(raw_file: &str) -> &str {
    if let Some((stem, ext)) = raw_file.rsplit_once('.') {
        let trimmed_stem = stem.trim();
        if !trimmed_stem.is_empty()
            && !ext.is_empty()
            && ext.len() <= 8
            && ext.chars().all(|c| c.is_ascii_alphanumeric())
        {
            return trimmed_stem;
        }
    }
    raw_file
}

/// Build a vfat-safe, human-readable local path under `cache_root` from server path / tags.
#[allow(clippy::too_many_arguments)]
pub fn build_track_path(
    cache_root: &Path,
    server_path: &str,
    artist: &str,
    album: &str,
    track_no: i32,
    title: &str,
    song_id: &str,
    original_suffix: &str,
    quality: StreamQuality,
) -> PathBuf {
    let segs: Vec<&str> = server_path
        .split(['/', '\\'])
        .map(|s| s.trim())
        .filter(|s| !s.is_empty() && *s != "." && *s != "..")
        .collect();

    let (dirs, stem) = if !segs.is_empty() {
        let raw_file = segs[segs.len() - 1];
        let raw_dirs = &segs[..segs.len() - 1];
        let clean_dirs: Vec<String> = raw_dirs
            .iter()
            .map(|d| sanitize_vfat_component(d, 60))
            .collect();
        let raw_stem = split_server_filename_stem(raw_file);
        let clean_stem = sanitize_vfat_component(raw_stem, 80);
        (clean_dirs, clean_stem)
    } else {
        let art = if artist.trim().is_empty() {
            "Unknown Artist"
        } else {
            artist.trim()
        };
        let alb = if album.trim().is_empty() {
            "Unknown Album"
        } else {
            album.trim()
        };
        let raw_stem = if !title.trim().is_empty() {
            if track_no > 0 {
                format!("{track_no:02} - {}", title.trim())
            } else {
                title.trim().to_string()
            }
        } else if !song_id.trim().is_empty() {
            safe_filename_id(song_id)
        } else {
            "Track".to_string()
        };
        (
            vec![
                sanitize_vfat_component(art, 60),
                sanitize_vfat_component(alb, 60),
            ],
            sanitize_vfat_component(&raw_stem, 80),
        )
    };

    let ext_part = file_suffix_for_quality(original_suffix, quality);
    let mut candidate = cache_root.to_path_buf();
    for d in &dirs {
        candidate.push(d);
    }
    candidate.push(format!("{stem}{ext_part}"));

    if candidate.to_string_lossy().len() <= MAX_FULL_TRACK_PATH_BYTES {
        return candidate;
    }

    // Fallback compaction for extraordinarily deep/long server hierarchies so paths
    // always fit safely inside Rockbox `MAX_PATH` (260).
    let hash6 = &md5_hex(song_id.as_bytes())[..6];
    let mut compact = cache_root.to_path_buf();
    if dirs.len() >= 2 {
        compact.push(sanitize_vfat_component(&dirs[0], 36));
        compact.push(sanitize_vfat_component(&dirs[dirs.len() - 1], 36));
    } else if dirs.len() == 1 {
        compact.push(sanitize_vfat_component(&dirs[0], 48));
    }
    let short_stem = sanitize_vfat_component(&stem, 44);
    compact.push(format!("{short_stem} [{hash6}]{ext_part}"));
    compact
}

/// Disambiguate a track path when two distinct `song_id`s map to the exact same local filename.
pub fn disambiguate_track_path(
    base_path: &Path,
    song_id: &str,
    original_suffix: &str,
    quality: StreamQuality,
) -> PathBuf {
    let ext_part = file_suffix_for_quality(original_suffix, quality);
    let file_name = base_path
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("Track");
    let stem = file_name.strip_suffix(&ext_part).unwrap_or(file_name);
    let hash6 = &md5_hex(song_id.as_bytes())[..6];
    let short_stem = sanitize_vfat_component(stem, 64);
    let parent = base_path.parent().unwrap_or_else(|| Path::new(""));
    parent.join(format!("{short_stem} [{hash6}]{ext_part}"))
}

/// Resolve the album art path (`<album_dir>/cover.jpg`, or `<cache_root>/.covers/<id>.jpg`
/// for loose root-level tracks) for `track_path`.
pub fn resolve_cover_path(cache_root: &Path, track_path: &Path, cover_id: &str) -> PathBuf {
    if cover_id.trim().is_empty() {
        return PathBuf::new();
    }
    if let Some(parent) = track_path.parent() {
        if parent != cache_root && parent.starts_with(cache_root) {
            return parent.join("cover.jpg");
        }
    }
    let safe_id = safe_filename_id(cover_id);
    cache_root
        .join(COVERS_FALLBACK_DIR)
        .join(format!("{safe_id}.jpg"))
}

#[derive(Debug, Clone)]
pub enum BgJob {
    PrefetchTrack {
        local_path: PathBuf,
        song_id: String,
        quality: StreamQuality,
        cover_id: String,
        cover_dest: PathBuf,
    },
    FetchCover {
        cover_id: String,
        dest_path: PathBuf,
    },
    Scrobble {
        song_id: String,
        submission: bool,
    },
}

struct SharedState {
    config: NavidromeConfig,
    /// Explicitly downloaded tracks protected from LRU eviction and `Clear Cache`.
    pinned: HashSet<PathBuf>,
    /// Local file paths currently being downloaded (either foreground or background).
    in_flight: HashSet<PathBuf>,
    /// Last error for an in-flight path if it failed.
    recent_err: Option<(PathBuf, String)>,
    /// Background queue: scrobbles are preserved; pending prefetches are replaced on track change.
    bg_queue: VecDeque<BgJob>,
    /// Prevent duplicate `submission=true` scrobbles for the same track session.
    last_scrobbled_song: String,
    shutdown: bool,
}

static PART_SEQ: AtomicU64 = AtomicU64::new(1);

pub struct CacheManager {
    cache_root: PathBuf,
    transport: Arc<dyn HttpTransport>,
    shared: Arc<(Mutex<SharedState>, Condvar)>,
    prefetch_done: Arc<AtomicBool>,
    worker: Option<JoinHandle<()>>,
}

impl CacheManager {
    pub fn new(cache_root: PathBuf, transport: Arc<dyn HttpTransport>) -> Self {
        let _ = fs::create_dir_all(&cache_root);
        let pinned = Self::load_pinned_set(&cache_root);

        let shared = Arc::new((
            Mutex::new(SharedState {
                config: NavidromeConfig::default(),
                pinned,
                in_flight: HashSet::new(),
                recent_err: None,
                bg_queue: VecDeque::new(),
                last_scrobbled_song: String::new(),
                shutdown: false,
            }),
            Condvar::new(),
        ));
        let prefetch_done = Arc::new(AtomicBool::new(false));

        let worker_transport = Arc::clone(&transport);
        let worker_shared = Arc::clone(&shared);
        let worker_pf_done = Arc::clone(&prefetch_done);
        let worker = thread::Builder::new()
            .name("tp-navidrome-bg".to_string())
            .spawn(move || {
                Self::worker_loop(worker_transport, worker_shared, worker_pf_done);
            })
            .ok();

        Self {
            cache_root,
            transport,
            shared,
            prefetch_done,
            worker,
        }
    }

    pub fn take_prefetch_done(&self) -> bool {
        if self.prefetch_done.load(Ordering::Acquire) {
            self.prefetch_done.swap(false, Ordering::AcqRel)
        } else {
            false
        }
    }

    fn load_pinned_set(cache_root: &Path) -> HashSet<PathBuf> {
        let manifest = cache_root.join(PINNED_MANIFEST_NAME);
        let Ok(text) = fs::read_to_string(manifest) else {
            return HashSet::new();
        };
        let mut set = HashSet::new();
        for line in text.lines() {
            let rel = line.trim();
            if rel.is_empty() || rel.starts_with('#') {
                continue;
            }
            set.insert(cache_root.join(rel));
        }
        set
    }

    fn save_pinned_set(cache_root: &Path, pinned: &HashSet<PathBuf>) -> bool {
        let _ = fs::create_dir_all(cache_root);
        let mut rels: Vec<String> = pinned
            .iter()
            .filter_map(|p| p.strip_prefix(cache_root).ok())
            .map(|r| r.to_string_lossy().replace('\\', "/"))
            .filter(|s| !s.is_empty())
            .collect();
        rels.sort();
        rels.dedup();

        let mut body = String::new();
        for r in rels {
            body.push_str(&r);
            body.push('\n');
        }

        let tmp = cache_root.join(format!("{PINNED_MANIFEST_NAME}.tmp"));
        let dst = cache_root.join(PINNED_MANIFEST_NAME);
        if fs::write(&tmp, body.as_bytes()).is_err() {
            let _ = fs::remove_file(&tmp);
            return false;
        }
        if fs::rename(&tmp, &dst).is_err() {
            let _ = fs::remove_file(&tmp);
            return false;
        }
        true
    }

    pub fn cache_root(&self) -> &Path {
        &self.cache_root
    }

    pub fn transport(&self) -> &Arc<dyn HttpTransport> {
        &self.transport
    }

    pub fn get_config(&self) -> NavidromeConfig {
        self.shared.0.lock().unwrap().config.clone()
    }

    pub fn set_config(&self, cfg: NavidromeConfig) {
        let limit_mb = cfg.cache_limit_mb;
        {
            let mut st = self.shared.0.lock().unwrap();
            st.config = cfg;
        }
        if limit_mb > 0 {
            self.enforce_lru_limit(limit_mb, None);
        }
    }

    /// Pin an explicitly downloaded track so neither LRU eviction nor `Clear Cache` removes it.
    pub fn pin_track(&self, local_path: &Path) -> bool {
        if !local_path.starts_with(&self.cache_root) {
            return false;
        }
        let snapshot = {
            let mut st = self.shared.0.lock().unwrap();
            if !st.pinned.insert(local_path.to_path_buf()) {
                return true;
            }
            st.pinned.clone()
        };
        Self::save_pinned_set(&self.cache_root, &snapshot)
    }

    pub fn is_pinned(&self, local_path: &Path) -> bool {
        self.shared.0.lock().unwrap().pinned.contains(local_path)
    }

    pub fn is_busy(&self) -> bool {
        !self.shared.0.lock().unwrap().in_flight.is_empty()
    }

    #[allow(clippy::too_many_arguments)]
    pub fn track_path_for_meta(
        &self,
        server_path: &str,
        artist: &str,
        album: &str,
        track_no: i32,
        title: &str,
        song_id: &str,
        original_suffix: &str,
        quality: StreamQuality,
    ) -> PathBuf {
        build_track_path(
            &self.cache_root,
            server_path,
            artist,
            album,
            track_no,
            title,
            song_id,
            original_suffix,
            quality,
        )
    }

    pub fn cover_path_for_track(&self, track_path: &Path, cover_id: &str) -> PathBuf {
        resolve_cover_path(&self.cache_root, track_path, cover_id)
    }

    pub fn is_track_path(&self, path: &str) -> bool {
        let p = Path::new(path);
        let Ok(rel) = p.strip_prefix(&self.cache_root) else {
            return false;
        };
        let mut count = 0usize;
        for comp in rel.components() {
            let Component::Normal(os) = comp else {
                return false;
            };
            let s = os.to_string_lossy();
            if s.is_empty() || s.starts_with('.') {
                return false;
            }
            count += 1;
        }
        if count == 0 {
            return false;
        }
        let Some(fname) = p.file_name().and_then(|s| s.to_str()) else {
            return false;
        };
        if fname.eq_ignore_ascii_case("cover.jpg") || fname.eq_ignore_ascii_case("folder.jpg") {
            return false;
        }
        if p.extension()
            .and_then(|s| s.to_str())
            .map(|e| e.starts_with("part"))
            .unwrap_or(false)
        {
            return false;
        }
        true
    }

    /// Infer `StreamQuality` from `<stem>.320k.mp3` / `<stem>.192k.mp3` / native `<stem>.<ext>`.
    pub fn quality_from_track_path(&self, path: &Path) -> StreamQuality {
        let fname = path
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or_default()
            .to_ascii_lowercase();
        if fname.ends_with(".320k.mp3") {
            StreamQuality::Mp3_320
        } else if fname.ends_with(".192k.mp3") {
            StreamQuality::Mp3_192
        } else {
            StreamQuality::Original
        }
    }

    pub fn file_ready(path: &Path) -> bool {
        fs::metadata(path)
            .map(|m| m.is_file() && m.len() > 0)
            .unwrap_or(false)
    }

    /// Ensure `local_path` exists in cache, downloading from `stream.view` if missing,
    /// or waiting on the background worker if it is already prefetching `local_path`.
    pub fn ensure_track_cached(
        &self,
        local_path: &Path,
        song_id: &str,
        cover_id: &str,
        cancel_fn: Option<&(dyn Fn() -> bool + Sync)>,
    ) -> Result<(), String> {
        if Self::file_ready(local_path) {
            if !cover_id.is_empty() {
                self.enqueue_cover_for_track(local_path, cover_id);
            }
            return Ok(());
        }

        let cfg = loop {
            if let Some(cb) = cancel_fn {
                if cb() {
                    return Err("Cancelled".to_string());
                }
            }

            let mut st = self.shared.0.lock().unwrap();
            if Self::file_ready(local_path) {
                return Ok(());
            }
            if st.in_flight.contains(local_path) {
                // Background worker is already downloading this exact track! Wait briefly & recheck.
                let (guard, _) = self
                    .shared
                    .1
                    .wait_timeout(st, Duration::from_millis(60))
                    .unwrap();
                st = guard;
                if Self::file_ready(local_path) {
                    return Ok(());
                }
                if !st.in_flight.contains(local_path) {
                    if let Some((ref ep, ref msg)) = st.recent_err {
                        if ep == local_path {
                            return Err(msg.clone());
                        }
                    }
                }
                continue;
            }

            st.in_flight.insert(local_path.to_path_buf());
            break st.config.clone();
        };

        let quality = self.quality_from_track_path(local_path);
        let res = Self::download_track_internal(
            &self.transport,
            &cfg,
            local_path,
            song_id,
            quality,
            cancel_fn,
        );

        {
            let mut st = self.shared.0.lock().unwrap();
            st.in_flight.remove(local_path);
            if let Err(ref e) = res {
                st.recent_err = Some((local_path.to_path_buf(), e.clone()));
            }
            self.shared.1.notify_all();
        }

        if res.is_ok() {
            if cfg.cache_limit_mb > 0 {
                self.enforce_lru_limit(cfg.cache_limit_mb, Some(local_path));
            }
            if !cover_id.is_empty() {
                self.enqueue_cover_for_track(local_path, cover_id);
            }
        }
        res
    }

    pub fn ensure_cover_cached(&self, local_track_path: &Path, cover_id: &str) -> Option<PathBuf> {
        let dest = self.cover_path_for_track(local_track_path, cover_id);
        if dest.as_os_str().is_empty() {
            return None;
        }
        if Self::file_ready(&dest) {
            return Some(dest);
        }
        let cfg = self.get_config();
        if Self::download_cover_internal(&self.transport, &cfg, &dest, cover_id).is_ok()
            && Self::file_ready(&dest)
        {
            Some(dest)
        } else {
            None
        }
    }

    pub fn enqueue_cover_for_track(&self, local_track_path: &Path, cover_id: &str) {
        let dest = self.cover_path_for_track(local_track_path, cover_id);
        if dest.as_os_str().is_empty() || Self::file_ready(&dest) {
            return;
        }
        let mut st = self.shared.0.lock().unwrap();
        let already_queued = st.bg_queue.iter().any(|job| match job {
            BgJob::FetchCover { dest_path, .. } => dest_path == &dest,
            BgJob::PrefetchTrack { cover_dest, .. } => cover_dest == &dest,
            _ => false,
        });
        if !already_queued {
            st.bg_queue.push_front(BgJob::FetchCover {
                cover_id: cover_id.to_string(),
                dest_path: dest,
            });
            self.shared.1.notify_one();
        }
    }

    /// Replace stale upcoming-track prefetches with the new next 1-3 tracks in the queue,
    /// while preserving any queued Subsonic scrobbles.
    pub fn set_prefetch_targets(&self, targets: Vec<(PathBuf, String, String)>) {
        let mut st = self.shared.0.lock().unwrap();
        st.bg_queue
            .retain(|job| matches!(job, BgJob::Scrobble { .. } | BgJob::FetchCover { .. }));
        let mut seen_paths = HashSet::new();
        for (local_path, song_id, cover_id) in targets {
            if !seen_paths.insert(local_path.clone()) {
                continue;
            }
            let cover_dest = self.cover_path_for_track(&local_path, &cover_id);
            if song_id.is_empty() || Self::file_ready(&local_path) {
                if !cover_id.is_empty()
                    && !cover_dest.as_os_str().is_empty()
                    && !Self::file_ready(&cover_dest)
                {
                    st.bg_queue.push_back(BgJob::FetchCover {
                        cover_id,
                        dest_path: cover_dest,
                    });
                }
                continue;
            }
            let quality = self.quality_from_track_path(&local_path);
            st.bg_queue.push_back(BgJob::PrefetchTrack {
                local_path,
                song_id,
                quality,
                cover_id,
                cover_dest,
            });
        }
        self.shared.1.notify_one();
    }

    fn push_priority_job(queue: &mut VecDeque<BgJob>, job: BgJob) {
        let pos = queue
            .iter()
            .position(|j| matches!(j, BgJob::PrefetchTrack { .. }))
            .unwrap_or(queue.len());
        queue.insert(pos, job);
    }

    pub fn notify_track_start(&self, song_id: &str) {
        if song_id.is_empty() {
            return;
        }
        let mut st = self.shared.0.lock().unwrap();
        st.last_scrobbled_song.clear();
        Self::push_priority_job(
            &mut st.bg_queue,
            BgJob::Scrobble {
                song_id: song_id.to_string(),
                submission: false,
            },
        );
        self.shared.1.notify_one();
    }

    /// Standard Subsonic / Last.fm scrobble rule: track duration >= 30s and
    /// played >= min(duration / 2, 240s).
    pub fn notify_track_played(&self, song_id: &str, played_secs: u32, duration_secs: u32) {
        if song_id.is_empty() || duration_secs < 30 {
            return;
        }
        let threshold = (duration_secs / 2).clamp(15, 240);
        if played_secs < threshold {
            return;
        }
        let mut st = self.shared.0.lock().unwrap();
        if st.last_scrobbled_song == song_id {
            return;
        }
        st.last_scrobbled_song = song_id.to_string();
        Self::push_priority_job(
            &mut st.bg_queue,
            BgJob::Scrobble {
                song_id: song_id.to_string(),
                submission: true,
            },
        );
        self.shared.1.notify_one();
    }

    fn pinned_snapshot(&self) -> (HashSet<PathBuf>, HashSet<PathBuf>) {
        let pinned_files = self.shared.0.lock().unwrap().pinned.clone();
        let mut pinned_dirs = HashSet::new();
        for p in &pinned_files {
            if Self::file_ready(p) {
                if let Some(parent) = p.parent() {
                    pinned_dirs.insert(parent.to_path_buf());
                }
            }
        }
        (pinned_files, pinned_dirs)
    }

    fn is_protected_file(
        path: &Path,
        pinned_files: &HashSet<PathBuf>,
        pinned_dirs: &HashSet<PathBuf>,
    ) -> bool {
        let Some(fname) = path.file_name().and_then(|s| s.to_str()) else {
            return true;
        };
        if fname == PINNED_MANIFEST_NAME || fname.starts_with(".uipod_pinned") {
            return true;
        }
        if pinned_files.contains(path) {
            return true;
        }
        if fname.eq_ignore_ascii_case("cover.jpg") {
            if let Some(parent) = path.parent() {
                if pinned_dirs.contains(parent) {
                    return true;
                }
            }
        }
        false
    }

    /// Bytes occupied by transient (unpinned) cached audio & covers.
    pub fn cache_usage_bytes(&self) -> u64 {
        let (pinned_files, pinned_dirs) = self.pinned_snapshot();
        let mut total = 0u64;
        Self::walk_files(&self.cache_root, &mut |path, len, _| {
            if Self::is_protected_file(path, &pinned_files, &pinned_dirs) {
                return;
            }
            total = total.saturating_add(len);
        });
        total
    }

    /// Remove all unpinned cached audio & covers and prune empty directories,
    /// while preserving explicitly `Download`ed tracks and their album `cover.jpg`.
    pub fn clear_cache(&self) -> bool {
        {
            let mut st = self.shared.0.lock().unwrap();
            st.bg_queue
                .retain(|job| matches!(job, BgJob::Scrobble { .. }));
        }
        let (pinned_files, pinned_dirs) = self.pinned_snapshot();
        let mut files = Vec::new();
        Self::walk_files(&self.cache_root, &mut |path, _, _| {
            if !Self::is_protected_file(path, &pinned_files, &pinned_dirs) {
                files.push(path.to_path_buf());
            }
        });
        for f in files {
            let _ = fs::remove_file(f);
        }
        Self::prune_empty_dirs(&self.cache_root, true);
        let _ = fs::create_dir_all(&self.cache_root);
        true
    }

    pub fn enforce_lru_limit(&self, limit_mb: u32, protect: Option<&Path>) {
        if limit_mb == 0 {
            return;
        }
        let cap_bytes = (limit_mb as u64).saturating_mul(1024 * 1024);
        let (pinned_files, _) = self.pinned_snapshot();
        let mut entries: Vec<(PathBuf, u64, SystemTime)> = Vec::new();
        let mut total = 0u64;

        Self::walk_files(&self.cache_root, &mut |path, len, mtime| {
            let Some(path_str) = path.to_str() else {
                return;
            };
            if !self.is_track_path(path_str) || pinned_files.contains(path) {
                return;
            }
            total = total.saturating_add(len);
            entries.push((path.to_path_buf(), len, mtime));
        });

        if total <= cap_bytes {
            return;
        }

        // Oldest mtime first
        entries.sort_by_key(|(_, _, mtime)| *mtime);
        let mut touched_dirs = HashSet::new();
        for (path, len, _) in entries {
            if total <= cap_bytes {
                break;
            }
            if protect.map(|p| p == path.as_path()).unwrap_or(false) {
                continue;
            }
            if fs::remove_file(&path).is_ok() {
                total = total.saturating_sub(len);
                if let Some(parent) = path.parent() {
                    touched_dirs.insert(parent.to_path_buf());
                }
            }
        }

        // If an album folder lost all of its audio tracks, clean up its orphan `cover.jpg`
        // and prune empty folders.
        for dir in touched_dirs {
            if dir != self.cache_root && Self::dir_has_only_cover(&dir) {
                let _ = fs::remove_file(dir.join("cover.jpg"));
            }
        }
        Self::prune_empty_dirs(&self.cache_root, true);
    }

    fn dir_has_only_cover(dir: &Path) -> bool {
        let Ok(rd) = fs::read_dir(dir) else {
            return false;
        };
        let mut saw_cover = false;
        for entry in rd.flatten() {
            let fname = entry.file_name();
            let s = fname.to_string_lossy();
            if s.eq_ignore_ascii_case("cover.jpg") {
                saw_cover = true;
            } else {
                return false;
            }
        }
        saw_cover
    }

    fn prune_empty_dirs(dir: &Path, is_root: bool) -> bool {
        let Ok(rd) = fs::read_dir(dir) else {
            return false;
        };
        let mut empty = true;
        for entry in rd.flatten() {
            let path = entry.path();
            if path.is_dir() {
                if !Self::prune_empty_dirs(&path, false) {
                    empty = false;
                }
            } else {
                empty = false;
            }
        }
        if empty && !is_root {
            fs::remove_dir(dir).is_ok()
        } else {
            false
        }
    }

    fn walk_files(dir: &Path, cb: &mut dyn FnMut(&Path, u64, SystemTime)) {
        let Ok(rd) = fs::read_dir(dir) else {
            return;
        };
        for entry in rd.flatten() {
            let path = entry.path();
            let Ok(meta) = entry.metadata() else {
                continue;
            };
            if meta.is_dir() {
                Self::walk_files(&path, cb);
            } else if meta.is_file() {
                let mtime = meta.modified().unwrap_or(SystemTime::UNIX_EPOCH);
                cb(&path, meta.len(), mtime);
            }
        }
    }

    fn download_track_internal(
        transport: &Arc<dyn HttpTransport>,
        cfg: &NavidromeConfig,
        local_path: &Path,
        song_id: &str,
        quality: StreamQuality,
        cancel_fn: Option<&(dyn Fn() -> bool + Sync)>,
    ) -> Result<(), String> {
        let url = ParsedServerUrl::parse(&cfg.url)?;
        if let Some(parent) = local_path.parent() {
            let _ = fs::create_dir_all(parent);
        }
        let seq = PART_SEQ.fetch_add(1, Ordering::Relaxed);
        let part_path = local_path.with_extension(format!("part{seq}"));
        let req_path = url.stream_path(&cfg.username, &cfg.password, song_id, quality);

        let res = transport.download_to_file(
            url.is_https,
            &url.host,
            url.port,
            &req_path,
            cfg.allow_self_signed,
            &part_path,
            45_000,
            cancel_fn,
        );
        if let Err(e) = res {
            let _ = fs::remove_file(&part_path);
            return Err(e);
        }

        // Guard against Subsonic returning a tiny JSON/XML error payload with HTTP 200!
        let Ok(meta) = fs::metadata(&part_path) else {
            let _ = fs::remove_file(&part_path);
            return Err("Empty stream".to_string());
        };
        if meta.len() < 256 {
            if let Ok(bytes) = fs::read(&part_path) {
                let _ = fs::remove_file(&part_path);
                if bytes.starts_with(b"{") || bytes.starts_with(b"<?xml") {
                    if let Err(sub_err) = crate::json::decode_ping(&bytes) {
                        return Err(sub_err);
                    }
                    return Err("Stream error response".to_string());
                }
            }
            let _ = fs::remove_file(&part_path);
            return Err("Truncated stream".to_string());
        }

        fs::rename(&part_path, local_path).map_err(|e| {
            let _ = fs::remove_file(&part_path);
            format!("Cache write failed: {e}")
        })
    }

    fn download_cover_internal(
        transport: &Arc<dyn HttpTransport>,
        cfg: &NavidromeConfig,
        dest: &Path,
        cover_id: &str,
    ) -> Result<(), String> {
        let url = ParsedServerUrl::parse(&cfg.url)?;
        if let Some(parent) = dest.parent() {
            let _ = fs::create_dir_all(parent);
        }
        let seq = PART_SEQ.fetch_add(1, Ordering::Relaxed);
        let part_path = dest.with_extension(format!("part{seq}"));
        let req_path = url.cover_art_path(&cfg.username, &cfg.password, cover_id, 512);
        transport.download_to_file(
            url.is_https,
            &url.host,
            url.port,
            &req_path,
            cfg.allow_self_signed,
            &part_path,
            10_000,
            None,
        )?;
        let len = fs::metadata(&part_path).map(|m| m.len()).unwrap_or(0);
        if len < 32 {
            let _ = fs::remove_file(&part_path);
            return Err("Invalid cover art".to_string());
        }
        // Check magic bytes so a JSON error isn't cached as an image
        if let Ok(head) = fs::read(&part_path) {
            if head.starts_with(b"{") || head.starts_with(b"<?xml") {
                let _ = fs::remove_file(&part_path);
                return Err("Cover art not found".to_string());
            }
        }
        fs::rename(&part_path, dest).map_err(|e| {
            let _ = fs::remove_file(&part_path);
            format!("Cover rename failed: {e}")
        })
    }

    fn worker_loop(
        transport: Arc<dyn HttpTransport>,
        shared: Arc<(Mutex<SharedState>, Condvar)>,
        prefetch_done: Arc<AtomicBool>,
    ) {
        loop {
            let (job, cfg) = {
                let mut st = shared.0.lock().unwrap();
                while st.bg_queue.is_empty() && !st.shutdown {
                    st = shared.1.wait(st).unwrap();
                }
                if st.shutdown {
                    break;
                }
                let job = st.bg_queue.pop_front().unwrap();
                let cfg = st.config.clone();
                (job, cfg)
            };

            if !cfg.is_configured() {
                continue;
            }

            match job {
                BgJob::Scrobble {
                    song_id,
                    submission,
                } => {
                    if let Ok(url) = ParsedServerUrl::parse(&cfg.url) {
                        let req =
                            url.scrobble_path(&cfg.username, &cfg.password, &song_id, submission);
                        let _ = transport.get_bytes(
                            url.is_https,
                            &url.host,
                            url.port,
                            &req,
                            cfg.allow_self_signed,
                            6_000,
                        );
                    }
                }
                BgJob::FetchCover {
                    cover_id,
                    dest_path,
                } => {
                    if !dest_path.as_os_str().is_empty() && !Self::file_ready(&dest_path) {
                        let _ =
                            Self::download_cover_internal(&transport, &cfg, &dest_path, &cover_id);
                    }
                }
                BgJob::PrefetchTrack {
                    local_path,
                    song_id,
                    quality,
                    cover_id,
                    cover_dest,
                } => {
                    if !cover_id.is_empty()
                        && !cover_dest.as_os_str().is_empty()
                        && !Self::file_ready(&cover_dest)
                    {
                        let _ =
                            Self::download_cover_internal(&transport, &cfg, &cover_dest, &cover_id);
                    }

                    let should_dl = {
                        let mut st = shared.0.lock().unwrap();
                        if Self::file_ready(&local_path) || st.in_flight.contains(&local_path) {
                            false
                        } else {
                            st.in_flight.insert(local_path.clone());
                            true
                        }
                    };

                    if should_dl {
                        let res = Self::download_track_internal(
                            &transport,
                            &cfg,
                            &local_path,
                            &song_id,
                            quality,
                            None,
                        );
                        let ok = res.is_ok();
                        {
                            let mut st = shared.0.lock().unwrap();
                            st.in_flight.remove(&local_path);
                            if let Err(e) = res {
                                st.recent_err = Some((local_path.clone(), e));
                            }
                            shared.1.notify_all();
                        }
                        if ok {
                            prefetch_done.store(true, Ordering::Release);
                        }
                    }
                }
            }
        }
    }
}

impl Drop for CacheManager {
    fn drop(&mut self) {
        {
            let mut st = self.shared.0.lock().unwrap();
            st.shutdown = true;
            self.shared.1.notify_all();
        }
        if let Some(h) = self.worker.take() {
            let _ = h.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;

    struct MockTransport {
        downloads: AtomicUsize,
        scrobbles: Mutex<Vec<(String, bool)>>,
    }

    impl HttpTransport for MockTransport {
        fn get_bytes(
            &self,
            _is_https: bool,
            _host: &str,
            _port: u16,
            path_and_query: &str,
            _allow_self_signed: bool,
            _timeout_ms: i32,
        ) -> Result<Vec<u8>, String> {
            if path_and_query.contains("scrobble.view") {
                let sub = path_and_query.contains("submission=true");
                self.scrobbles
                    .lock()
                    .unwrap()
                    .push((path_and_query.to_string(), sub));
            }
            Ok(br#"{"subsonic-response":{"status":"ok"}}"#.to_vec())
        }

        fn download_to_file(
            &self,
            _is_https: bool,
            _host: &str,
            _port: u16,
            _path_and_query: &str,
            _allow_self_signed: bool,
            dest_path: &Path,
            _timeout_ms: i32,
            _cancel_fn: Option<&(dyn Fn() -> bool + Sync)>,
        ) -> Result<(), String> {
            self.downloads.fetch_add(1, Ordering::SeqCst);
            let dummy = vec![0xFFu8; 2048];
            fs::write(dest_path, dummy).map_err(|e| e.to_string())
        }
    }

    #[test]
    fn test_vfat_sanitization_and_hierarchy() {
        assert_eq!(sanitize_vfat_component("AC:DC?", 60), "AC_DC_");
        assert_eq!(sanitize_vfat_component("Album... ", 60), "Album");
        assert_eq!(sanitize_vfat_component("CON", 60), "_CON");

        let root = Path::new("/mnt/SDCARD/Media/Music/navidrome");
        let p_orig = build_track_path(
            root,
            "Radiohead/OK Computer/01 - Airbag.flac",
            "Radiohead",
            "OK Computer",
            1,
            "Airbag",
            "tr1",
            "flac",
            StreamQuality::Original,
        );
        assert_eq!(
            p_orig,
            PathBuf::from("/mnt/SDCARD/Media/Music/navidrome/Radiohead/OK Computer/01 - Airbag.flac")
        );
        assert_eq!(
            resolve_cover_path(root, &p_orig, "cov1"),
            PathBuf::from("/mnt/SDCARD/Media/Music/navidrome/Radiohead/OK Computer/cover.jpg")
        );

        let p_320 = build_track_path(
            root,
            "Radiohead/OK Computer/01 - Airbag.flac",
            "Radiohead",
            "OK Computer",
            1,
            "Airbag",
            "tr1",
            "flac",
            StreamQuality::Mp3_320,
        );
        assert_eq!(
            p_320,
            PathBuf::from(
                "/mnt/SDCARD/Media/Music/navidrome/Radiohead/OK Computer/01 - Airbag.320k.mp3"
            )
        );
    }

    #[test]
    fn test_cache_download_pinning_and_scrobble() {
        let dir = std::env::temp_dir().join(format!(
            "uipod_nav_test_{}_{}",
            std::process::id(),
            PART_SEQ.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = fs::remove_dir_all(&dir);
        let mock = Arc::new(MockTransport {
            downloads: AtomicUsize::new(0),
            scrobbles: Mutex::new(Vec::new()),
        });
        let mgr = CacheManager::new(dir.clone(), mock.clone());
        mgr.set_config(NavidromeConfig {
            url: "http://192.168.1.50:4533".to_string(),
            username: "admin".to_string(),
            password: "pw".to_string(),
            quality: StreamQuality::Mp3_320,
            cache_limit_mb: 512,
            allow_self_signed: true,
            ..Default::default()
        });

        let p_pinned = mgr.track_path_for_meta(
            "Daft Punk/Discovery/01 - One More Time.flac",
            "Daft Punk",
            "Discovery",
            1,
            "One More Time",
            "song1",
            "flac",
            StreamQuality::Mp3_320,
        );
        let p_cached = mgr.track_path_for_meta(
            "Boards of Canada/Geogaddi/01 - Ready Lets Go.flac",
            "Boards of Canada",
            "Geogaddi",
            1,
            "Ready Lets Go",
            "song2",
            "flac",
            StreamQuality::Mp3_320,
        );
        assert!(p_pinned
            .to_string_lossy()
            .ends_with("Daft Punk/Discovery/01 - One More Time.320k.mp3"));
        assert_eq!(
            mgr.quality_from_track_path(&p_pinned),
            StreamQuality::Mp3_320
        );
        assert!(mgr.is_track_path(&p_pinned.to_string_lossy()));

        mgr.ensure_track_cached(&p_pinned, "song1", "", None)
            .unwrap();
        let cov = mgr.ensure_cover_cached(&p_pinned, "cov1").unwrap();
        assert!(cov.ends_with("Daft Punk/Discovery/cover.jpg"));
        assert!(!mgr.is_track_path(&cov.to_string_lossy()));

        mgr.ensure_track_cached(&p_cached, "song2", "", None)
            .unwrap();
        assert_eq!(mock.downloads.load(Ordering::SeqCst), 3);

        // Pin `p_pinned` and verify `clear_cache` deletes `p_cached` + its folder
        // while keeping `p_pinned` + `Daft Punk/Discovery/cover.jpg` intact!
        assert!(mgr.pin_track(&p_pinned));
        assert!(mgr.is_pinned(&p_pinned));
        assert_eq!(mgr.cache_usage_bytes(), 2048); // only unpinned `p_cached` counts as transient cache

        mgr.clear_cache();
        assert_eq!(mgr.cache_usage_bytes(), 0);
        assert!(CacheManager::file_ready(&p_pinned));
        assert!(CacheManager::file_ready(&cov));
        assert!(!CacheManager::file_ready(&p_cached));
        assert!(!p_cached.parent().unwrap().exists());

        // Scrobble rules: 10s of 200s should NOT submit=true; 120s of 200s SHOULD submit=true once
        mgr.notify_track_start("song1");
        mgr.notify_track_played("song1", 10, 200);
        mgr.notify_track_played("song1", 120, 200);
        mgr.notify_track_played("song1", 150, 200); // deduplicated!

        thread::sleep(Duration::from_millis(80));
        let sc = mock.scrobbles.lock().unwrap().clone();
        assert_eq!(sc.len(), 2);
        assert!(!sc[0].1); // Now Playing
        assert!(sc[1].1); // Scrobbled once

        drop(mgr);

        // Reopen CacheManager to verify `.uipod_pinned` persisted on disk
        let mgr2 = CacheManager::new(dir.clone(), mock);
        assert!(mgr2.is_pinned(&p_pinned));
        drop(mgr2);

        let _ = fs::remove_dir_all(dir);
    }
}
