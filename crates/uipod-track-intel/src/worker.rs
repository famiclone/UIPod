//! Background worker (`uipod-intel-bg`) & synchronous on-demand game-entry gate (`worker.rs`).
//!
//! Note: Only touches `Mutex` / `Condvar` on track-change control events and background DB writes;
//! **never** called from the real-time audio thread or 60 FPS visualizer/plugin render reads!

#[cfg(not(target_arch = "wasm32"))]
use std::collections::VecDeque;
#[cfg(not(target_arch = "wasm32"))]
use std::path::PathBuf;
#[cfg(not(target_arch = "wasm32"))]
use std::sync::{Arc, Condvar, Mutex, OnceLock};
#[cfg(not(target_arch = "wasm32"))]
use std::time::{Duration, Instant};

#[cfg(not(target_arch = "wasm32"))]
use crate::analyzer::{analyze_file_fast, analyze_pcm_s16_buffer, AnalyzeRequest};
#[cfg(not(target_arch = "wasm32"))]
use crate::db::{analysis_flag, TrackAnalysisRecord, TrackIntelDb};
#[cfg(not(target_arch = "wasm32"))]
use crate::live::publish_active_analysis;

/// Optional host callback allowing `uipod-intel-bg` to inspect prefetched/decoded stream PCM
/// from Rockbox's `pcmbuf` when a remote stream is not yet finalized on disk.
#[cfg(not(target_arch = "wasm32"))]
pub type StreamPcmFallbackFn = fn(out_stereo: &mut [i16]) -> (usize, u32);

#[cfg(not(target_arch = "wasm32"))]
struct WorkerState {
    db: TrackIntelDb,
    active_key_hash: u64,
    in_flight_hash: u64,
    queue: VecDeque<AnalyzeRequest>,
    boost_requested: bool,
    shutdown: bool,
    stream_fallback: Option<StreamPcmFallbackFn>,
}

#[cfg(not(target_arch = "wasm32"))]
pub struct TrackIntelCoordinator {
    inner: Arc<(Mutex<WorkerState>, Condvar)>,
}

#[cfg(not(target_arch = "wasm32"))]
static GLOBAL_INTEL: OnceLock<TrackIntelCoordinator> = OnceLock::new();

#[cfg(all(unix, not(target_arch = "wasm32")))]
fn lower_current_thread_priority() {
    extern "C" {
        fn nice(inc: i32) -> i32;
    }
    unsafe {
        let _ = nice(10);
    }
}

#[cfg(all(not(unix), not(target_arch = "wasm32")))]
fn lower_current_thread_priority() {}

#[cfg(not(target_arch = "wasm32"))]
impl TrackIntelCoordinator {
    pub fn global() -> &'static Self {
        GLOBAL_INTEL.get_or_init(Self::new_unconfigured)
    }

    fn new_unconfigured() -> Self {
        let pair = Arc::new((
            Mutex::new(WorkerState {
                db: TrackIntelDb::new_in_memory(),
                active_key_hash: 0,
                in_flight_hash: 0,
                queue: VecDeque::with_capacity(8),
                boost_requested: false,
                shutdown: false,
                stream_fallback: None,
            }),
            Condvar::new(),
        ));

        let worker_pair = Arc::clone(&pair);
        let _ = std::thread::Builder::new()
            .name("uipod-intel-bg".to_string())
            .spawn(move || Self::worker_loop(worker_pair));

        Self { inner: pair }
    }

    /// Open (or switch) the persistent `track_intel.db` file and register an optional `pcmbuf` fallback.
    pub fn init(&self, db_path: PathBuf, stream_fallback: Option<StreamPcmFallbackFn>) {
        let opened = TrackIntelDb::open(&db_path);
        let (mtx, _) = &*self.inner;
        if let Ok(mut st) = mtx.lock() {
            st.db = opened;
            if stream_fallback.is_some() {
                st.stream_fallback = stream_fallback;
            }
        }
    }

    /// Query `TrackIntelDb` without altering the active playback slot (e.g. Auto-DJ Deck B prefetch).
    pub fn lookup(&self, key_hash: u64) -> Option<TrackAnalysisRecord> {
        if key_hash == 0 {
            return None;
        }
        let (mtx, _) = &*self.inner;
        mtx.lock().ok()?.db.get(key_hash)
    }

    /// Called when a track starts playing:
    /// - **Cache Hit:** Loads `TrackAnalysisRecord` into lock-free `ACTIVE_ANALYSIS_SLOT` in `< 0.05 ms` (`true`).
    /// - **Cache Miss:** Clears stale analysis and dispatches silent background job on `uipod-intel-bg` (`false`).
    pub fn on_track_start(&self, req: AnalyzeRequest) -> bool {
        let (mtx, cv) = &*self.inner;
        let Ok(mut st) = mtx.lock() else {
            return false;
        };

        st.active_key_hash = req.key_hash;
        if req.key_hash == 0 {
            publish_active_analysis(None);
            return true;
        }

        if let Some(rec) = st.db.get(req.key_hash) {
            publish_active_analysis(Some(&rec));
            return true;
        }

        publish_active_analysis(None);
        // Deduplicate & prioritize the active track at the front of the queue
        st.queue.retain(|q| q.key_hash != req.key_hash);
        if st.in_flight_hash != req.key_hash {
            st.queue.push_front(req);
            cv.notify_all();
        }
        false
    }

    /// Enqueue background pre-analysis for an upcoming playlist track (e.g. `NEXTTRACK_ID3` for Auto-DJ).
    pub fn prefetch_track(&self, req: AnalyzeRequest) {
        if req.key_hash == 0 {
            return;
        }
        let (mtx, cv) = &*self.inner;
        let Ok(mut st) = mtx.lock() else {
            return;
        };
        if st.db.contains(req.key_hash)
            || st.in_flight_hash == req.key_hash
            || st.queue.iter().any(|q| q.key_hash == req.key_hash)
        {
            return;
        }
        if st.queue.len() >= 8 {
            st.queue.pop_back();
        }
        st.queue.push_back(req);
        cv.notify_all();
    }

    /// Check whether `key_hash` is already persisted in `TrackIntelDb` (`0 ms`, non-blocking).
    pub fn is_ready(&self, key_hash: u64) -> bool {
        if key_hash == 0 {
            return true;
        }
        let (mtx, _) = &*self.inner;
        let Ok(mut st) = mtx.lock() else {
            return false;
        };
        if let Some(rec) = st.db.get(key_hash) {
            if st.active_key_hash == key_hash {
                publish_active_analysis(Some(&rec));
            }
            true
        } else {
            false
        }
    }

    /// Boost priority of `req` on `uipod-intel-bg` and wait up to `timeout` (`0` = non-blocking poll
    /// used inside 60 Hz `"Processing..."` HUD animation loops). Returns `Some(record)` once ready.
    pub fn ensure_analyzed(
        &self,
        mut req: AnalyzeRequest,
        timeout: Duration,
    ) -> Option<TrackAnalysisRecord> {
        if req.key_hash == 0 {
            return None;
        }
        req.high_priority = true;
        let deadline = Instant::now() + timeout;
        let (mtx, cv) = &*self.inner;
        let mut st = mtx.lock().ok()?;

        if let Some(rec) = st.db.get(req.key_hash) {
            if st.active_key_hash == req.key_hash {
                publish_active_analysis(Some(&rec));
            }
            return Some(rec);
        }

        st.boost_requested = true;
        if st.in_flight_hash != req.key_hash {
            st.queue.retain(|q| q.key_hash != req.key_hash);
            st.queue.push_front(req.clone());
            cv.notify_all();
        }

        while !timeout.is_zero() {
            if let Some(rec) = st.db.get(req.key_hash) {
                if st.active_key_hash == req.key_hash {
                    publish_active_analysis(Some(&rec));
                }
                return Some(rec);
            }
            let now = Instant::now();
            if now >= deadline {
                break;
            }
            let (guard, _) = cv.wait_timeout(st, deadline - now).ok()?;
            st = guard;
        }

        st.db.get(req.key_hash)
    }

    pub fn clear_active(&self) {
        let (mtx, _) = &*self.inner;
        if let Ok(mut st) = mtx.lock() {
            st.active_key_hash = 0;
        }
        publish_active_analysis(None);
    }

    fn worker_loop(pair: Arc<(Mutex<WorkerState>, Condvar)>) {
        lower_current_thread_priority();
        let (mtx, cv) = &*pair;

        loop {
            let (mut job, fallback_fn) = {
                let Ok(mut st) = mtx.lock() else {
                    return;
                };
                while st.queue.is_empty() && !st.shutdown {
                    st.boost_requested = false;
                    let Ok(guard) = cv.wait(st) else {
                        return;
                    };
                    st = guard;
                }
                if st.shutdown {
                    return;
                }
                let mut job = st.queue.pop_front().unwrap();
                if st.boost_requested {
                    job.high_priority = true;
                }
                st.in_flight_hash = job.key_hash;
                (job, st.stream_fallback)
            };

            if !job.high_priority {
                // Brief settle pause on normal track change so initial codec/UI transition gets 100% CPU
                std::thread::sleep(Duration::from_millis(35));
                if let Ok(st) = mtx.lock() {
                    if st.boost_requested {
                        job.high_priority = true;
                    }
                }
            }

            let mut result = analyze_file_fast(&job);

            // Fallback: active network stream or Rockbox-only codec (`Opus`, `SPC`, `MOD`, etc.)
            // analyzed directly from `pcmbuf` decoded RAM buffer with zero network sockets!
            if result.is_none() {
                if let Some(cb) = fallback_fn {
                    let mut pcm_buf = vec![0i16; 44_100 * 14 * 2];
                    let (got_frames, sr) = cb(&mut pcm_buf);
                    if got_frames >= (sr as usize) * 3 {
                        pcm_buf.truncate(got_frames * 2);
                        let mut rec = analyze_pcm_s16_buffer(&pcm_buf, sr, 0, &job);
                        rec.analysis_flags |= analysis_flag::STREAM_BUFFERED;
                        result = Some(rec);
                    }
                }
            }

            // Guaranteed record completion if `ensure_analyzed` is waiting on an unparsable/short file
            let final_rec = result.unwrap_or_else(|| {
                let mut fallback = TrackAnalysisRecord {
                    key_hash: job.key_hash,
                    subtrack_index: job.subtrack_index,
                    duration_ms: job.duration_ms,
                    sample_rate: job.sample_rate_hint.max(44_100),
                    bpm: job
                        .tag_tbpm
                        .map(crate::analyzer::fold_bpm_60_200)
                        .unwrap_or(crate::analyzer::DEFAULT_BPM),
                    bpm_confidence: if job.tag_tbpm.is_some() { 0.50 } else { 0.10 },
                    tag_tbpm: job.tag_tbpm.unwrap_or(0.0),
                    ..Default::default()
                };
                crate::db::write_fixed_utf8(&mut fallback.title, &job.title);
                crate::db::write_fixed_utf8(&mut fallback.artist, &job.artist);
                crate::db::write_fixed_utf8(&mut fallback.album, &job.album);
                crate::db::write_fixed_utf8(&mut fallback.genre, &job.genre);
                fallback
            });

            if let Ok(mut st) = mtx.lock() {
                st.in_flight_hash = 0;
                st.db.upsert(final_rec);
                if st.active_key_hash == final_rec.key_hash {
                    publish_active_analysis(Some(&final_rec));
                }
                cv.notify_all();
            }
        }
    }
}
