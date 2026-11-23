//! Persistent Offline Scrobble Cache (`listenbrainz_queue.tsv`) and Active Play Session tracker.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

pub const MAX_CACHED_SCROBBLES: usize = 5000;
pub const MAX_BATCH_SIZE: usize = 50;
/// 2020-01-01T00:00:00Z — timestamps below this mean handheld RTC was unset/1970 offline.
const MIN_VALID_UNIX_TS: u64 = 1_577_836_800;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QueuedScrobble {
    pub artist: String,
    pub track: String,
    pub album: String,
    pub album_artist: String,
    pub track_number: u32,
    pub duration_ms: u64,
    pub timestamp: u64,
}

fn sanitize_tsv(s: &str) -> String {
    s.chars()
        .map(|c| match c {
            '\t' | '\r' | '\n' | '\0' => ' ',
            other => other,
        })
        .collect::<String>()
        .trim()
        .to_string()
}

#[derive(Debug, Default)]
pub struct ScrobbleQueue {
    store_path: Option<PathBuf>,
    items: Vec<QueuedScrobble>,
}

impl ScrobbleQueue {
    pub fn new(store_path: Option<PathBuf>) -> Self {
        let mut q = Self {
            store_path,
            items: Vec::new(),
        };
        q.load_from_disk();
        q
    }

    pub fn set_store_path(&mut self, path: PathBuf) {
        self.store_path = Some(path);
        self.load_from_disk();
    }

    pub fn len(&self) -> usize {
        self.items.len()
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    pub fn items(&self) -> &[QueuedScrobble] {
        &self.items
    }

    pub fn enqueue(&mut self, mut item: QueuedScrobble) -> bool {
        item.artist = sanitize_tsv(&item.artist);
        item.track = sanitize_tsv(&item.track);
        item.album = sanitize_tsv(&item.album);
        item.album_artist = sanitize_tsv(&item.album_artist);

        if item.artist.is_empty() || item.track.is_empty() {
            return false;
        }

        // Guard against accidental duplicate enqueue of the exact same play
        if let Some(last) = self.items.last() {
            if last.artist == item.artist
                && last.track == item.track
                && last.timestamp.abs_diff(item.timestamp) <= 2
            {
                return false;
            }
        }

        if self.items.len() >= MAX_CACHED_SCROBBLES {
            let overflow = self.items.len() + 1 - MAX_CACHED_SCROBBLES;
            self.items.drain(0..overflow);
        }

        self.items.push(item);
        let _ = self.save_to_disk();
        true
    }

    /// Return up to `MAX_BATCH_SIZE` (`50`) scrobbles from the front of the queue,
    /// reconstructing unset-RTC (`< 2020-01-01`) timestamps relative to `now_unix`
    /// while preserving exact chronological order.
    pub fn peek_normalized_batch(&self, now_unix: u64) -> Vec<QueuedScrobble> {
        let count = self.items.len().min(MAX_BATCH_SIZE);
        let mut batch: Vec<QueuedScrobble> = self.items[..count].to_vec();
        if batch.is_empty() {
            return batch;
        }

        if now_unix < MIN_VALID_UNIX_TS {
            // If system clock is still unset (1970) when online, ensure positive fallback timestamps
            let fallback_now = MIN_VALID_UNIX_TS + 86_400;
            for (idx, s) in batch.iter_mut().enumerate() {
                if s.timestamp < MIN_VALID_UNIX_TS {
                    s.timestamp = fallback_now.saturating_sub(((count - idx) * 180) as u64);
                }
            }
            return batch;
        }

        let needs_unset_fix = batch.iter().any(|s| s.timestamp < MIN_VALID_UNIX_TS);
        if needs_unset_fix {
            let total_dur: u64 = batch
                .iter()
                .map(|s| ((s.duration_ms / 1000).max(120)).min(600))
                .sum();
            let mut cursor = now_unix
                .saturating_sub(total_dur + 30)
                .max(MIN_VALID_UNIX_TS);
            for s in &mut batch {
                if s.timestamp < MIN_VALID_UNIX_TS {
                    s.timestamp = cursor;
                }
                let step = ((s.duration_ms / 1000).max(120)).min(600);
                cursor = (cursor + step).min(now_unix.saturating_sub(5));
            }
        }

        // Prevent accidental future timestamps if RTC drifted slightly ahead
        for (idx, s) in batch.iter_mut().enumerate() {
            let ceiling = now_unix.saturating_sub((count - idx).saturating_sub(1) as u64);
            if s.timestamp > ceiling && ceiling >= MIN_VALID_UNIX_TS {
                s.timestamp = ceiling;
            }
        }

        batch
    }

    pub fn drop_matching_front(&mut self, submitted: &[QueuedScrobble]) -> usize {
        let mut dropped = 0usize;
        for sub in submitted {
            if let Some(front) = self.items.first() {
                if front.artist == sub.artist && front.track == sub.track {
                    self.items.remove(0);
                    dropped += 1;
                    continue;
                }
            }
            break;
        }
        if dropped > 0 {
            let _ = self.save_to_disk();
        }
        dropped
    }

    pub fn drop_front_one(&mut self) -> bool {
        if self.items.is_empty() {
            return false;
        }
        self.items.remove(0);
        let _ = self.save_to_disk();
        true
    }

    pub fn clear(&mut self) {
        self.items.clear();
        let _ = self.save_to_disk();
    }

    pub fn load_from_disk(&mut self) {
        let Some(path) = &self.store_path else {
            return;
        };
        let Ok(text) = fs::read_to_string(path) else {
            return;
        };
        self.items = Self::parse_tsv(&text);
    }

    pub fn save_to_disk(&self) -> Result<(), String> {
        let Some(path) = &self.store_path else {
            return Ok(());
        };
        Self::write_atomic(path, &self.format_tsv())
    }

    fn write_atomic(path: &Path, content: &str) -> Result<(), String> {
        if let Some(parent) = path.parent() {
            let _ = fs::create_dir_all(parent);
        }
        let tmp = PathBuf::from(format!("{}.new", path.display()));
        let mut f = fs::File::create(&tmp).map_err(|e| e.to_string())?;
        f.write_all(content.as_bytes()).map_err(|e| e.to_string())?;
        let _ = f.sync_all();
        drop(f);
        fs::rename(&tmp, path).map_err(|e| e.to_string())
    }

    pub fn format_tsv(&self) -> String {
        let mut out = String::with_capacity(64 + self.items.len() * 96);
        out.push_str("#UIPOD_LISTENBRAINZ_QUEUE_V1\n");
        for s in &self.items {
            out.push_str(&format!(
                "{}\t{}\t{}\t{}\t{}\t{}\t{}\n",
                s.timestamp,
                s.duration_ms,
                s.track_number,
                sanitize_tsv(&s.artist),
                sanitize_tsv(&s.track),
                sanitize_tsv(&s.album),
                sanitize_tsv(&s.album_artist),
            ));
        }
        out
    }

    pub fn parse_tsv(text: &str) -> Vec<QueuedScrobble> {
        let mut out = Vec::new();
        for raw in text.lines() {
            let line = raw.trim_end_matches(&['\r', '\n'][..]);
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let cols: Vec<&str> = line.splitn(7, '\t').collect();
            if cols.len() < 5 {
                continue;
            }
            let Ok(timestamp) = cols[0].trim().parse::<u64>() else {
                continue;
            };
            let duration_ms = cols[1].trim().parse::<u64>().unwrap_or(0);
            let track_number = cols[2].trim().parse::<u32>().unwrap_or(0);
            let artist = cols[3].trim().to_string();
            let track = cols[4].trim().to_string();
            let album = cols.get(5).map(|s| s.trim().to_string()).unwrap_or_default();
            let album_artist = cols.get(6).map(|s| s.trim().to_string()).unwrap_or_default();
            if artist.is_empty() || track.is_empty() {
                continue;
            }
            out.push(QueuedScrobble {
                artist,
                track,
                album,
                album_artist,
                track_number,
                duration_ms,
                timestamp,
            });
            if out.len() >= MAX_CACHED_SCROBBLES {
                break;
            }
        }
        out
    }
}

/// Tracks the currently playing song using the identical scrobble threshold rules as Last.fm:
/// - Track duration must be > 30 seconds (`> 30_000 ms`)
/// - Actual unpaused listening time must reach `min(duration / 2, 240_000 ms)`
#[derive(Debug, Clone)]
pub struct ActiveTrackSession {
    pub path: String,
    pub artist: String,
    pub track: String,
    pub album: String,
    pub album_artist: String,
    pub track_number: u32,
    pub duration_ms: u64,
    pub is_streaming: bool,
    pub started_unix: u64,
    pub accum_played_ms: u64,
    pub last_tick_mono_ms: u64,
    pub is_playing: bool,
    pub scrobbled: bool,
}

impl ActiveTrackSession {
    pub fn scrobble_threshold_ms(duration_ms: u64) -> Option<u64> {
        if duration_ms <= 30_000 {
            None
        } else {
            Some((duration_ms / 2).min(240_000))
        }
    }

    pub fn tick(&mut self, now_mono_ms: u64, is_playing: bool) {
        if self.is_playing && now_mono_ms > self.last_tick_mono_ms {
            let delta = (now_mono_ms - self.last_tick_mono_ms).min(15_000);
            self.accum_played_ms = self.accum_played_ms.saturating_add(delta);
        }
        self.last_tick_mono_ms = now_mono_ms;
        self.is_playing = is_playing;
    }

    pub fn ready_to_scrobble(&self) -> Option<QueuedScrobble> {
        if self.scrobbled || self.artist.trim().is_empty() || self.track.trim().is_empty() {
            return None;
        }
        let threshold = Self::scrobble_threshold_ms(self.duration_ms)?;
        if self.accum_played_ms < threshold {
            return None;
        }
        Some(QueuedScrobble {
            artist: self.artist.clone(),
            track: self.track.clone(),
            album: self.album.clone(),
            album_artist: self.album_artist.clone(),
            track_number: self.track_number,
            duration_ms: self.duration_ms,
            timestamp: self.started_unix,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_tsv_roundtrip_and_batch_normalization() {
        let mut q = ScrobbleQueue::new(None);
        assert!(q.enqueue(QueuedScrobble {
            artist: "Boards of Canada".to_string(),
            track: "Roygbiv".to_string(),
            album: "Music Has the Right to Children".to_string(),
            album_artist: "Boards of Canada".to_string(),
            track_number: 10,
            duration_ms: 149_000,
            timestamp: 120, // unset 1970 RTC offline play
        }));
        assert_eq!(q.len(), 1);

        let tsv = q.format_tsv();
        let parsed = ScrobbleQueue::parse_tsv(&tsv);
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].track, "Roygbiv");
        assert_eq!(parsed[0].duration_ms, 149_000);

        let now = 1_763_890_800;
        let batch = q.peek_normalized_batch(now);
        assert!(batch[0].timestamp >= MIN_VALID_UNIX_TS);
        assert!(batch[0].timestamp <= now);
    }

    #[test]
    fn test_scrobble_threshold_rules() {
        assert_eq!(ActiveTrackSession::scrobble_threshold_ms(30_000), None);
        assert_eq!(ActiveTrackSession::scrobble_threshold_ms(180_000), Some(90_000));
        assert_eq!(ActiveTrackSession::scrobble_threshold_ms(600_000), Some(240_000));
    }
}
