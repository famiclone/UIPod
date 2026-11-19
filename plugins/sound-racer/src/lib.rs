//! Real-time & cached `Sound Racer` music-highway arcade plugin for UIPod (`PluginKind::MenuDriven`).
//!
//! Features:
//! - Launch prompt (`Start from Beginning` vs `Continue`).
//! - Per-track persistent highway cache (`trk_<hash>.bin`, `SRT1` binary format at 50 ms resolution)
//!   recorded on the fly from `pcmbuf` lookahead on first play and replayed deterministically
//!   with zero lookahead DSP overhead on subsequent runs.
//! - Per-track score reset at the start of every song, full-run completion verification
//!   (start-to-finish runs save per-track `Hi-Score`), and conditional `Hi-Score` HUD display.

use std::collections::VecDeque;
use std::path::Path;

use uipod_plugin_sdk::{
    btn, uipod_declare_plugin, AudioTrackInfo, FontWeight, HostContext, MenuItem, MenuOutcome,
    PluginCategory, PluginKind, PluginTheme, TextAlign, CONTENT_TOP_Y,
};

const SLOT_MS: u32 = 50;
const MAX_TRACK_SLOTS: usize = 36_000; // Up to 30-minute tracks (`216 KB` max)
const TRAVEL_TIME_MS: u32 = 1_150;
const TRAVEL_SLOTS: usize = (TRAVEL_TIME_MS / SLOT_MS) as usize; // 23 slots
const JUMP_DURATION_MS: u64 = 360;
const CLUSTER_FUSE_MS: u64 = 320;

const TRAY_COLS: usize = 3;
const TRAY_ROWS: usize = 7;
const MAX_VIS_BLOCKS: usize = 32;

// Viewport geometry (`1024 x 768`)
const VIEW_X: f32 = 32.0;
const VIEW_Y: f32 = CONTENT_TOP_Y + 42.0; // 178
const VIEW_W: f32 = 700.0;
const VIEW_H: f32 = 562.0;
const SHIP_BASE_Y: f32 = VIEW_Y + VIEW_H - 58.0; // 682

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RaceMode {
    Mono = 0,
    Cluster = 1,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Difficulty {
    Chill = 0,
    Normal = 1,
    Pro = 2,
}

impl Difficulty {
    fn min_spawn_gap_slots(self) -> usize {
        match self {
            Self::Chill => 5,  // 250 ms
            Self::Normal => 3, // 150 ms
            Self::Pro => 2,    // 100 ms
        }
    }

    fn flux_threshold_mult(self) -> f32 {
        match self {
            Self::Chill => 1.42,
            Self::Normal => 1.15,
            Self::Pro => 0.92,
        }
    }
}

#[derive(Clone, Copy)]
struct RacerOptions {
    mode: RaceMode,
    difficulty: Difficulty,
}

impl Default for RacerOptions {
    fn default() -> Self {
        Self {
            mode: RaceMode::Mono,
            difficulty: Difficulty::Normal,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum BlockKind {
    Heavy = 1,
    Medium = 2,
    Light = 3,
    Spike = 4,
}

impl BlockKind {
    fn base_points(self) -> u32 {
        match self {
            Self::Heavy => 35,
            Self::Medium => 25,
            Self::Light => 15,
            Self::Spike => 0,
        }
    }
}

#[derive(Clone, Copy)]
struct HighwayBlock {
    arrive_song_ms: u32,
    lane: usize, // 0, 1, 2
    kind: BlockKind,
    resolved: bool,
}

#[inline]
fn fnv1a64(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for &b in bytes {
        h ^= b as u64;
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    h
}

#[inline]
fn mix64(mut x: u64) -> u64 {
    x ^= x >> 30;
    x = x.wrapping_mul(0xbf58_476d_1ce4_e5b9);
    x ^= x >> 27;
    x = x.wrapping_mul(0x94d0_49bb_1331_11eb);
    x ^ (x >> 31)
}

fn path_ext_ascii_lower(path: &str) -> Option<&str> {
    let base = path.rsplit('/').next().unwrap_or(path);
    let dot = base.rfind('.')?;
    if dot == 0 || dot + 1 >= base.len() {
        None
    } else {
        Some(&base[dot + 1..])
    }
}

/// Multi-subtune chiptune formats where Rockbox's GME/SID codecs report
/// `id3->elapsed = subtune_idx * 1000` (frozen during playback) and
/// `id3->length = subtune_count * 1000` rather than song duration in ms.
fn is_subtune_chiptune(path: &str) -> bool {
    let Some(ext) = path_ext_ascii_lower(path) else {
        return false;
    };
    ext.eq_ignore_ascii_case("nsf")
        || ext.eq_ignore_ascii_case("nsfe")
        || ext.eq_ignore_ascii_case("gbs")
        || ext.eq_ignore_ascii_case("sid")
        || ext.eq_ignore_ascii_case("hes")
        || ext.eq_ignore_ascii_case("ay")
        || ext.eq_ignore_ascii_case("kss")
        || ext.eq_ignore_ascii_case("sgc")
        || ext.eq_ignore_ascii_case("sap")
        || ext.eq_ignore_ascii_case("vtx")
}

/// Synthesized chiptune / tracker formats whose playback clock runs continuously
/// at 1.0x from `pcmbuf` without fixed PCM file duration or monotonic `id3->elapsed`.
fn is_chiptune_or_module(path: &str) -> bool {
    if is_subtune_chiptune(path) {
        return true;
    }
    let Some(ext) = path_ext_ascii_lower(path) else {
        return false;
    };
    ext.eq_ignore_ascii_case("spc")
        || ext.eq_ignore_ascii_case("vgm")
        || ext.eq_ignore_ascii_case("vgz")
        || ext.eq_ignore_ascii_case("mod")
        || ext.eq_ignore_ascii_case("s3m")
        || ext.eq_ignore_ascii_case("xm")
        || ext.eq_ignore_ascii_case("it")
}

fn compute_track_hash(tr: &AudioTrackInfo) -> u64 {
    if !tr.path.is_empty() {
        fnv1a64(tr.path.as_bytes())
    } else {
        let id_src = format!("{}:{}", tr.artist, tr.title);
        fnv1a64(id_src.as_bytes())
    }
}

/// Compact 6-byte quantized audio feature frame for one `50 ms` slice of a song.
#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
struct TrackSlot {
    flags: u8, // bit 0 = analyzed
    rms: u8,
    low: u8,
    mid: u8,
    high: u8,
    pan: i8,
}

impl TrackSlot {
    #[inline]
    const fn is_analyzed(self) -> bool {
        (self.flags & 1) != 0
    }

    fn from_features(f: AudioFeatures) -> Self {
        Self {
            flags: 1,
            rms: (f.rms.clamp(0.0, 1.0) * 255.0) as u8,
            low: (f.low.clamp(0.0, 1.0) * 255.0) as u8,
            mid: (f.mid.clamp(0.0, 1.0) * 255.0) as u8,
            high: (f.high.clamp(0.0, 1.0) * 255.0) as u8,
            pan: (f.pan.clamp(-1.0, 1.0) * 127.0) as i8,
        }
    }

    fn to_features(self) -> AudioFeatures {
        AudioFeatures {
            rms: (self.rms as f32) / 255.0,
            low: (self.low as f32) / 255.0,
            mid: (self.mid as f32) / 255.0,
            high: (self.high as f32) / 255.0,
            pan: (self.pan as f32) / 127.0,
            bars: [0.0; 16],
        }
    }
}

/// Persistent per-song highway map + full-run `Hi-Score` (`SRT1` binary format).
struct TrackMap {
    hash: u64,
    duration_ms: u32,
    hi_score_mono: u32,
    hi_score_cluster: u32,
    slots: Vec<TrackSlot>,
    dirty: bool,
}

impl TrackMap {
    fn new_empty(hash: u64, duration_ms: u32) -> Self {
        let count = (((duration_ms / SLOT_MS) as usize) + 8).clamp(64, MAX_TRACK_SLOTS);
        Self {
            hash,
            duration_ms,
            hi_score_mono: 0,
            hi_score_cluster: 0,
            slots: vec![TrackSlot::default(); count],
            dirty: false,
        }
    }

    fn storage_key(hash: u64) -> String {
        format!("tracks/trk_{hash:016x}.bin")
    }

    fn hi_score(&self, mode: RaceMode) -> u32 {
        match mode {
            RaceMode::Mono => self.hi_score_mono,
            RaceMode::Cluster => self.hi_score_cluster,
        }
    }

    /// Updates `Hi-Score` for `mode`. Returns `true` if a new record was set!
    fn update_hi_score(&mut self, mode: RaceMode, score: u32) -> bool {
        if score == 0 {
            return false;
        }
        let slot = match mode {
            RaceMode::Mono => &mut self.hi_score_mono,
            RaceMode::Cluster => &mut self.hi_score_cluster,
        };
        if score > *slot {
            *slot = score;
            self.dirty = true;
            true
        } else {
            false
        }
    }

    fn reset_hi_score(&mut self, mode: RaceMode) {
        match mode {
            RaceMode::Mono => self.hi_score_mono = 0,
            RaceMode::Cluster => self.hi_score_cluster = 0,
        }
        self.dirty = true;
    }

    fn ensure_slot_len(&mut self, idx: usize) {
        if idx >= self.slots.len() && idx < MAX_TRACK_SLOTS {
            self.slots.resize((idx + 64).min(MAX_TRACK_SLOTS), TrackSlot::default());
        }
    }

    fn to_bytes(&self) -> Vec<u8> {
        let n = self.slots.len().min(MAX_TRACK_SLOTS);
        let mut out = Vec::with_capacity(24 + n * 6);
        out.extend_from_slice(b"SRT1");
        out.extend_from_slice(&self.duration_ms.to_le_bytes());
        out.extend_from_slice(&self.hi_score_mono.to_le_bytes());
        out.extend_from_slice(&self.hi_score_cluster.to_le_bytes());
        out.extend_from_slice(&(n as u32).to_le_bytes());
        out.extend_from_slice(&0u32.to_le_bytes());
        for s in &self.slots[..n] {
            out.push(s.flags);
            out.push(s.rms);
            out.push(s.low);
            out.push(s.mid);
            out.push(s.high);
            out.push(s.pan as u8);
        }
        out
    }

    fn from_bytes(hash: u64, raw: &[u8]) -> Option<Self> {
        if raw.len() < 24 || &raw[0..4] != b"SRT1" {
            return None;
        }
        let duration_ms = u32::from_le_bytes(raw[4..8].try_into().ok()?);
        let hi_score_mono = u32::from_le_bytes(raw[8..12].try_into().ok()?);
        let hi_score_cluster = u32::from_le_bytes(raw[12..16].try_into().ok()?);
        let count = (u32::from_le_bytes(raw[16..20].try_into().ok()?) as usize).min(MAX_TRACK_SLOTS);
        if raw.len() < 24 + count * 6 {
            return None;
        }
        let mut slots = Vec::with_capacity(count);
        for i in 0..count {
            let off = 24 + i * 6;
            slots.push(TrackSlot {
                flags: raw[off],
                rms: raw[off + 1],
                low: raw[off + 2],
                mid: raw[off + 3],
                high: raw[off + 4],
                pan: raw[off + 5] as i8,
            });
        }
        Some(Self {
            hash,
            duration_ms,
            hi_score_mono,
            hi_score_cluster,
            slots,
            dirty: false,
        })
    }

    fn load_or_new(ctx: &HostContext, hash: u64, duration_ms: u32) -> Self {
        if hash == 0 {
            return Self::new_empty(0, duration_ms);
        }
        let key = Self::storage_key(hash);
        if let Some(bytes) = ctx.load_plugin_data("sound_racer", &key) {
            if let Some(mut loaded) = Self::from_bytes(hash, &bytes) {
                if duration_ms > loaded.duration_ms {
                    loaded.duration_ms = duration_ms;
                    let want = (((duration_ms / SLOT_MS) as usize) + 8).min(MAX_TRACK_SLOTS);
                    if want > loaded.slots.len() {
                        loaded.slots.resize(want, TrackSlot::default());
                    }
                }
                return loaded;
            }
        }
        Self::new_empty(hash, duration_ms)
    }

    fn save_if_dirty(&mut self, ctx: &HostContext) {
        if !self.dirty || self.hash == 0 {
            return;
        }
        let key = Self::storage_key(self.hash);
        let bytes = self.to_bytes();
        if ctx.save_plugin_data("sound_racer", &key, &bytes) {
            self.dirty = false;
        }
    }
}

/// Real-time 3-band + stereo DSP snapshot extracted from S16 stereo frames.
#[derive(Clone, Copy, Default)]
struct AudioFeatures {
    rms: f32,
    low: f32,
    mid: f32,
    high: f32,
    pan: f32,
    bars: [f32; 16],
}

fn analyze_stereo_window(samples: &[i16], n_frames: usize) -> AudioFeatures {
    let n = n_frames.min(samples.len() / 2);
    if n == 0 {
        return AudioFeatures::default();
    }

    let inv = 1.0 / 32768.0_f32;
    let mut lp_slow = 0.0_f32;
    let mut lp_fast = 0.0_f32;
    let mut prev_mono = 0.0_f32;

    let mut sum_sq = 0.0_f32;
    let mut low_sq = 0.0_f32;
    let mut mid_sq = 0.0_f32;
    let mut high_sq = 0.0_f32;
    let mut l_abs = 0.0_f32;
    let mut r_abs = 0.0_f32;

    let mut bars = [0.0_f32; 16];
    let chunk_sz = (n / 8).max(1);

    for i in 0..n {
        let l = (samples[i * 2] as f32) * inv;
        let r = (samples[i * 2 + 1] as f32) * inv;
        let mono = (l + r) * 0.5;

        let la = l.abs();
        let ra = r.abs();
        l_abs += la;
        r_abs += ra;

        lp_slow += 0.055 * (mono - lp_slow);
        lp_fast += 0.36 * (mono - lp_fast);
        let low = lp_slow;
        let mid = lp_fast - lp_slow;
        let high = mono - prev_mono;
        prev_mono = mono;

        sum_sq += mono * mono;
        low_sq += low * low;
        mid_sq += mid * mid;
        high_sq += high * high;

        let seg = (i / chunk_sz).min(7);
        let band_mix = match seg {
            0 | 1 => low.abs() * 2.1,
            2 | 3 => mid.abs() * 1.9,
            4 | 5 => (mid.abs() + high.abs()) * 1.3,
            _ => high.abs() * 1.8,
        };
        let l_weight = 0.7 + 0.6 * (la / (la + ra + 0.001));
        let r_weight = 1.4 - l_weight;
        bars[7 - seg] += band_mix * l_weight;
        bars[8 + seg] += band_mix * r_weight;
    }

    let nf = n as f32;
    let inv_chunk = 1.0 / (chunk_sz as f32);
    for slot in &mut bars {
        *slot = (*slot * inv_chunk).sqrt().clamp(0.0, 1.0);
    }

    let tot_lr = l_abs + r_abs;
    let pan = if tot_lr > 1e-4 {
        ((r_abs - l_abs) / tot_lr).clamp(-1.0, 1.0)
    } else {
        0.0
    };

    AudioFeatures {
        rms: (sum_sq / nf).sqrt(),
        low: (low_sq / nf).sqrt() * 1.7,
        mid: (mid_sq / nf).sqrt() * 1.9,
        high: (high_sq / nf).sqrt() * 1.6,
        pan,
        bars,
    }
}

/// 3 x 7 Collector Matrix with zero-allocation stack flood-fill cluster matching!
struct CollectorTray {
    cells: [[u8; TRAY_ROWS]; TRAY_COLS],
    matched_mask: [[bool; TRAY_ROWS]; TRAY_COLS],
    fuse_until_ms: Option<u64>,
}

impl CollectorTray {
    fn new() -> Self {
        Self {
            cells: [[0; TRAY_ROWS]; TRAY_COLS],
            matched_mask: [[false; TRAY_ROWS]; TRAY_COLS],
            fuse_until_ms: None,
        }
    }

    fn clear_all(&mut self) {
        self.cells = [[0; TRAY_ROWS]; TRAY_COLS];
        self.matched_mask = [[false; TRAY_ROWS]; TRAY_COLS];
        self.fuse_until_ms = None;
    }

    fn column_height(&self, col: usize) -> usize {
        self.cells[col].iter().take_while(|&&v| v != 0).count()
    }

    fn push_block(&mut self, col: usize, kind: BlockKind, now_ms: u64) -> bool {
        let h = self.column_height(col);
        if h >= TRAY_ROWS {
            self.cells[col] = [0; TRAY_ROWS];
            self.recompute_matches(now_ms);
            return true;
        }
        self.cells[col][h] = kind as u8;
        self.recompute_matches(now_ms);
        false
    }

    fn recompute_matches(&mut self, now_ms: u64) {
        let mut visited = [[false; TRAY_ROWS]; TRAY_COLS];
        let mut mask = [[false; TRAY_ROWS]; TRAY_COLS];
        let mut any_match = false;

        let mut stack = [(0usize, 0usize); TRAY_COLS * TRAY_ROWS];
        let mut comp = [(0usize, 0usize); TRAY_COLS * TRAY_ROWS];

        for c in 0..TRAY_COLS {
            for r in 0..TRAY_ROWS {
                let val = self.cells[c][r];
                if val == 0 || val == (BlockKind::Spike as u8) || visited[c][r] {
                    continue;
                }

                let mut sp = 1usize;
                let mut cp = 0usize;
                stack[0] = (c, r);
                visited[c][r] = true;

                while sp > 0 {
                    sp -= 1;
                    let (cx, cy) = stack[sp];
                    comp[cp] = (cx, cy);
                    cp += 1;

                    let neighbors = [
                        (cx.wrapping_sub(1), cy),
                        (cx + 1, cy),
                        (cx, cy.wrapping_sub(1)),
                        (cx, cy + 1),
                    ];
                    for (nx, ny) in neighbors {
                        if nx < TRAY_COLS
                            && ny < TRAY_ROWS
                            && !visited[nx][ny]
                            && self.cells[nx][ny] == val
                        {
                            visited[nx][ny] = true;
                            stack[sp] = (nx, ny);
                            sp += 1;
                        }
                    }
                }

                if cp >= 3 {
                    any_match = true;
                    for &(mx, my) in &comp[..cp] {
                        mask[mx][my] = true;
                        for (ax, ay) in [
                            (mx.wrapping_sub(1), my),
                            (mx + 1, my),
                            (mx, my.wrapping_sub(1)),
                            (mx, my + 1),
                        ] {
                            if ax < TRAY_COLS
                                && ay < TRAY_ROWS
                                && self.cells[ax][ay] == (BlockKind::Spike as u8)
                            {
                                mask[ax][ay] = true;
                            }
                        }
                    }
                }
            }
        }

        if any_match {
            let newly_expanded = mask != self.matched_mask;
            self.matched_mask = mask;
            if self.fuse_until_ms.is_none() || newly_expanded {
                self.fuse_until_ms = Some(now_ms + CLUSTER_FUSE_MS);
            }
        } else {
            self.matched_mask = [[false; TRAY_ROWS]; TRAY_COLS];
            self.fuse_until_ms = None;
        }
    }

    fn poll_cash_in(&mut self, now_ms: u64, force: bool) -> Option<(usize, u32)> {
        let fuse = self.fuse_until_ms?;
        if !force && now_ms < fuse {
            return None;
        }

        let mut count = 0usize;
        let mut pts = 0u32;
        for c in 0..TRAY_COLS {
            for r in 0..TRAY_ROWS {
                if self.matched_mask[c][r] {
                    let v = self.cells[c][r];
                    self.cells[c][r] = 0;
                    if v != (BlockKind::Spike as u8) {
                        count += 1;
                        pts += match v {
                            1 => BlockKind::Heavy.base_points(),
                            2 => BlockKind::Medium.base_points(),
                            _ => BlockKind::Light.base_points(),
                        };
                    }
                }
            }
            let mut col_compact = [0u8; TRAY_ROWS];
            let mut dst = 0usize;
            for r in 0..TRAY_ROWS {
                if self.cells[c][r] != 0 {
                    col_compact[dst] = self.cells[c][r];
                    dst += 1;
                }
            }
            self.cells[c] = col_compact;
        }

        self.matched_mask = [[false; TRAY_ROWS]; TRAY_COLS];
        self.fuse_until_ms = None;
        self.recompute_matches(now_ms);

        if count > 0 {
            let size_bonus = (count as u32).saturating_sub(2);
            Some((count, pts * size_bonus))
        } else {
            None
        }
    }
}

/// Cached HUD strings so zero `format!` or `measure_text` calls run on unchanged frames!
struct HudCache {
    track_hash: u64,
    track_display: String,
    last_pos_sec: u32,
    last_dur_sec: u32,
    time_display: String,
    progress: f32,
    has_track: bool,

    last_score: u32,
    last_hi_score: u32,
    last_chain: u32,
    last_eligible: bool,
    score_hdr: String,
    score_str: String,
    hi_str: String,
}

impl HudCache {
    fn new() -> Self {
        Self {
            track_hash: 0,
            track_display: String::new(),
            last_pos_sec: u32::MAX,
            last_dur_sec: u32::MAX,
            time_display: String::new(),
            progress: 0.0,
            has_track: false,
            last_score: u32::MAX,
            last_hi_score: u32::MAX,
            last_chain: u32::MAX,
            last_eligible: true,
            score_hdr: String::new(),
            score_str: String::new(),
            hi_str: String::new(),
        }
    }

    fn sync_score(
        &mut self,
        ctx: &HostContext,
        score: u32,
        hi_score: u32,
        chain: u32,
        full_run_eligible: bool,
    ) {
        if self.last_chain != chain || self.score_hdr.is_empty() {
            self.last_chain = chain;
            self.score_hdr = format!("{}   x{}", ctx.tr("SCORE", "РАХУНОК"), chain);
        }
        if self.last_score != score || self.score_str.is_empty() {
            self.last_score = score;
            self.score_str = score.to_string();
        }
        if self.last_hi_score != hi_score || self.last_eligible != full_run_eligible {
            self.last_hi_score = hi_score;
            self.last_eligible = full_run_eligible;
            if hi_score > 0 {
                self.hi_str = format!("Hi-Score: {hi_score}");
            } else if !full_run_eligible {
                self.hi_str = ctx.tr("Mid-track run", "Не з початку").to_string();
            } else {
                self.hi_str.clear();
            }
        }
    }

    fn update_time(&mut self, pos_ms: u32, dur_ms: u32) {
        self.progress = if dur_ms > 0 {
            ((pos_ms as f32) / (dur_ms as f32)).clamp(0.0, 1.0)
        } else {
            ((pos_ms % 120_000) as f32) / 120_000.0
        };
        let pos_s = pos_ms / 1000;
        let dur_s = if dur_ms > 0 { dur_ms / 1000 } else { u32::MAX - 1 };
        if pos_s != self.last_pos_sec || dur_s != self.last_dur_sec {
            self.last_pos_sec = pos_s;
            self.last_dur_sec = dur_s;
            if dur_ms > 0 {
                self.time_display = format!(
                    "{:02}:{:02} / {:02}:{:02}",
                    pos_s / 60,
                    pos_s % 60,
                    dur_s / 60,
                    dur_s % 60
                );
            } else {
                self.time_display = format!("{:02}:{:02}", pos_s / 60, pos_s % 60);
            }
        }
    }

    fn set_track_label(&mut self, ctx: &HostContext, hash: u64, tr: &AudioTrackInfo) {
        self.has_track = true;
        if hash == self.track_hash && !self.track_display.is_empty() {
            return;
        }
        self.track_hash = hash;
        let raw_lbl = if tr.artist.is_empty() {
            tr.title.clone()
        } else {
            format!("{} — {}", tr.artist, tr.title)
        };
        if ctx.measure_text(&raw_lbl, 25.0, FontWeight::SemiBold) <= 680.0 {
            self.track_display = raw_lbl;
        } else {
            let chars: Vec<char> = raw_lbl.chars().collect();
            let mut lo = 4usize;
            let mut hi = chars.len();
            let mut fitted = String::from("…");
            while lo <= hi {
                let mid = (lo + hi) / 2;
                let mut cand: String = chars[..mid].iter().collect();
                cand.push('…');
                if ctx.measure_text(&cand, 25.0, FontWeight::SemiBold) <= 680.0 {
                    fitted = cand;
                    lo = mid + 1;
                } else if mid == 0 {
                    break;
                } else {
                    hi = mid - 1;
                }
            }
            self.track_display = fitted;
        }
    }
}

struct RacerState {
    opts: RacerOptions,
    score: u32,
    chain: u32,
    streak_hits: u32,
    lane: usize,
    ship_pos: f32,
    jump_until_ms: Option<u64>,
    blocks: VecDeque<HighwayBlock>,
    tray: CollectorTray,

    // Current track map & run eligibility
    map: TrackMap,
    free_clock: bool,
    subtune_mode: bool,
    frozen_pos_polls: u8,
    smooth_song_ms: f32,
    clock_rate: f32,
    last_reported_pos_ms: u32,
    max_song_ms: u32,
    full_run_eligible: bool,
    completion_announced: bool,

    // Deterministic slot generator state
    next_gen_slot: usize,
    last_spawn_slot: usize,
    ema_energy: f32,
    ema_flux: f32,
    prev_slot_feat: AudioFeatures,
    stream_lane: usize,

    // Live 60 FPS visuals
    live_bars: [f32; 16],
    live_kick: f32,
    road_curve: f32,
    target_curve: f32,
    intensity: f32,
    scroll_phase: f32,
    callout: Option<(String, u64)>,
}

impl RacerState {
    fn new(
        opts: RacerOptions,
        map: TrackMap,
        start_pos_ms: u32,
        reported_pos_ms: u32,
        free_clock: bool,
        subtune_mode: bool,
        eligible: bool,
    ) -> Self {
        let start_slot = (start_pos_ms / SLOT_MS) as usize;
        Self {
            opts,
            score: 0,
            chain: 1,
            streak_hits: 0,
            lane: 1,
            ship_pos: 1.0,
            jump_until_ms: None,
            blocks: VecDeque::with_capacity(48),
            tray: CollectorTray::new(),
            map,
            free_clock,
            subtune_mode,
            frozen_pos_polls: 0,
            smooth_song_ms: start_pos_ms as f32,
            clock_rate: 1.0,
            last_reported_pos_ms: reported_pos_ms,
            max_song_ms: start_pos_ms,
            full_run_eligible: eligible,
            completion_announced: false,
            next_gen_slot: start_slot.saturating_add(2),
            last_spawn_slot: 0,
            ema_energy: 0.08,
            ema_flux: 0.025,
            prev_slot_feat: AudioFeatures::default(),
            stream_lane: 1,
            live_bars: [0.0; 16],
            live_kick: 0.0,
            road_curve: 0.0,
            target_curve: 0.0,
            intensity: 0.35,
            scroll_phase: 0.0,
            callout: None,
        }
    }

    /// Saves full-run `Hi-Score` if the outgoing track was played from beginning to end!
    fn finalize_track_if_completed(&mut self, ctx: &HostContext) {
        if self.free_clock {
            return;
        }
        if self.full_run_eligible
            && self.map.duration_ms >= 5_000
            && self.max_song_ms.saturating_add(3_500) >= self.map.duration_ms
        {
            let _ = self.map.update_hi_score(self.opts.mode, self.score);
        }
        self.map.save_if_dirty(ctx);
    }

    /// Resets points & highway for a newly started track (or `Start from Beginning`).
    fn start_on_track(
        &mut self,
        ctx: &HostContext,
        new_map: TrackMap,
        start_pos_ms: u32,
        reported_pos_ms: u32,
        free_clock: bool,
        subtune_mode: bool,
        eligible: bool,
    ) {
        self.finalize_track_if_completed(ctx);
        self.score = 0;
        self.chain = 1;
        self.streak_hits = 0;
        self.blocks.clear();
        self.tray.clear_all();
        self.jump_until_ms = None;
        self.map = new_map;
        self.free_clock = free_clock;
        self.subtune_mode = subtune_mode;
        self.frozen_pos_polls = 0;
        self.smooth_song_ms = start_pos_ms as f32;
        self.clock_rate = 1.0;
        self.last_reported_pos_ms = reported_pos_ms;
        self.max_song_ms = start_pos_ms;
        self.full_run_eligible = eligible;
        self.completion_announced = false;
        let start_slot = (start_pos_ms / SLOT_MS) as usize;
        self.next_gen_slot = start_slot.saturating_add(2);
        self.last_spawn_slot = 0;
        self.ema_energy = 0.08;
        self.ema_flux = 0.025;
        self.prev_slot_feat = AudioFeatures::default();
        self.stream_lane = 1;
    }

    fn add_score(&mut self, pts: u32) {
        self.score = self.score.saturating_add(pts);
    }

    fn is_airborne(&self, now_ms: u64) -> bool {
        self.jump_until_ms.map(|end| now_ms < end).unwrap_or(false)
    }

    fn jump_height_frac(&self, now_ms: u64) -> f32 {
        let Some(end) = self.jump_until_ms else {
            return 0.0;
        };
        if now_ms >= end {
            return 0.0;
        }
        let rem = (end - now_ms) as f32 / (JUMP_DURATION_MS as f32);
        let t = 1.0 - rem;
        (4.0 * t * (1.0 - t)).clamp(0.0, 1.0)
    }

    /// Deterministically generates road curve, slope, and highway blocks for `slot_idx`
    /// (`arrive_song_ms = slot_idx * 50`), seeded by `(track_hash ^ slot_idx)` so cached
    /// replays produce the exact same level layout every time!
    fn step_slot_deterministic(&mut self, slot_idx: usize, feat: AudioFeatures) {
        let arrive_song_ms = (slot_idx as u32).saturating_mul(SLOT_MS);
        if self.map.duration_ms > 1_200 && arrive_song_ms + 450 >= self.map.duration_ms {
            return;
        }
        if feat.rms < 0.0025 {
            return;
        }

        self.ema_energy = self.ema_energy * 0.94 + feat.rms * 0.06;
        let rel_intensity = (feat.rms / (self.ema_energy.max(0.02) * 1.45)).clamp(0.0, 1.25);
        self.intensity = (self.intensity * 0.88 + rel_intensity * 0.12).clamp(0.05, 1.0);

        let spectral_tilt = (feat.high - feat.low).clamp(-0.5, 0.5);
        self.target_curve = (feat.pan * 1.6 + spectral_tilt * 0.9).clamp(-1.0, 1.0);

        let d_low = (feat.low - self.prev_slot_feat.low).max(0.0);
        let d_mid = (feat.mid - self.prev_slot_feat.mid).max(0.0);
        let d_high = (feat.high - self.prev_slot_feat.high).max(0.0);
        let flux = d_low * 1.55 + d_mid * 1.15 + d_high * 0.85;
        self.prev_slot_feat = feat;

        self.ema_flux = self.ema_flux * 0.90 + flux * 0.10;
        let thresh = (self.ema_flux * self.opts.difficulty.flux_threshold_mult()).max(0.016);

        let gap_ok = self.last_spawn_slot == 0
            || slot_idx.saturating_sub(self.last_spawn_slot)
                >= self.opts.difficulty.min_spawn_gap_slots();
        let sustained_due = feat.rms > 0.035
            && (self.last_spawn_slot == 0 || slot_idx.saturating_sub(self.last_spawn_slot) >= 10);

        if gap_ok && (flux > thresh || sustained_due) {
            self.last_spawn_slot = slot_idx;

            // Deterministic pseudo-random value anchored to `(track_hash, slot_idx)`!
            let r = (mix64(self.map.hash ^ (slot_idx as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15))
                >> 16) as u32;

            if d_low > d_mid * 1.3 && (r % 3 != 0) {
                self.stream_lane = 1;
            } else if feat.pan < -0.08 || (r % 5 == 0) {
                self.stream_lane = self.stream_lane.saturating_sub(1);
            } else if feat.pan > 0.08 || (r % 5 == 1) {
                self.stream_lane = (self.stream_lane + 1).min(2);
            } else if r % 4 == 0 {
                self.stream_lane = ((r >> 4) as usize) % 3;
            }

            let kind = match self.opts.mode {
                RaceMode::Mono => {
                    let spike_chance = match self.opts.difficulty {
                        Difficulty::Chill => 7,
                        Difficulty::Normal => 5,
                        Difficulty::Pro => 4,
                    };
                    if d_high > d_low * 1.15 && (r % spike_chance == 0) {
                        BlockKind::Spike
                    } else {
                        BlockKind::Heavy
                    }
                }
                RaceMode::Cluster => {
                    if feat.low >= feat.mid && feat.low >= feat.high * 0.9 {
                        BlockKind::Heavy
                    } else if feat.mid >= feat.high {
                        BlockKind::Medium
                    } else {
                        BlockKind::Light
                    }
                }
            };

            self.blocks.push_back(HighwayBlock {
                arrive_song_ms,
                lane: self.stream_lane,
                kind,
                resolved: false,
            });

            if self.opts.mode == RaceMode::Mono
                && kind == BlockKind::Heavy
                && flux > thresh * 2.1
                && (r % 3 == 0)
            {
                let other_lane = (self.stream_lane + 1 + ((r as usize) & 1)) % 3;
                self.blocks.push_back(HighwayBlock {
                    arrive_song_ms,
                    lane: other_lane,
                    kind: BlockKind::Spike,
                    resolved: false,
                });
            }
        }
    }

    /// Advances highway generation up to `song_pos_ms + TRAVEL_TIME_MS`:
    /// - Replays instantly from `self.map.slots[slot]` when already cached in `.bin`!
    /// - Otherwise samples `ctx.audio_read_pcm(ahead_ms)` on the fly and records into `self.map`!
    fn advance_horizon(&mut self, ctx: &HostContext, pcm_scratch: &mut [i16; 512]) {
        let cur_ms = self.smooth_song_ms.max(0.0) as u32;
        let horizon_slot = ((cur_ms + TRAVEL_TIME_MS) / SLOT_MS) as usize;
        let max_slot = if self.map.duration_ms > 0 {
            ((self.map.duration_ms / SLOT_MS) as usize).min(MAX_TRACK_SLOTS - 1)
        } else {
            horizon_slot.min(MAX_TRACK_SLOTS - 1)
        };
        let target_slot = horizon_slot.min(max_slot);

        // Cap work per frame (`<= 24` slots on initial prefill, `1` slot during normal 60 FPS play)
        let limit = (self.next_gen_slot + TRAVEL_SLOTS + 2).min(target_slot);
        while self.next_gen_slot <= limit {
            let s = self.next_gen_slot;
            self.map.ensure_slot_len(s);

            if s < self.map.slots.len() && self.map.slots[s].is_analyzed() {
                // Instant replay from cached trackmap!
                let feat = self.map.slots[s].to_features();
                self.step_slot_deterministic(s, feat);
                self.next_gen_slot += 1;
            } else {
                // First time visiting this slice of the song: sample `pcmbuf` lookahead!
                let slot_ms = (s as u32).saturating_mul(SLOT_MS);
                let want_ahead = slot_ms.saturating_sub(cur_ms);
                let mut n_frames = ctx.audio_read_pcm(want_ahead, pcm_scratch);
                if n_frames < 64 && want_ahead > 500 {
                    n_frames = ctx.audio_read_pcm(450, pcm_scratch);
                }
                if n_frames < 64 {
                    n_frames = ctx.audio_read_pcm(0, pcm_scratch);
                }
                if n_frames >= 64 {
                    let feat = analyze_stereo_window(pcm_scratch, n_frames);
                    if s < self.map.slots.len() {
                        self.map.slots[s] = TrackSlot::from_features(feat);
                        if !self.free_clock {
                            self.map.dirty = true;
                        }
                    }
                    self.step_slot_deterministic(s, feat);
                    self.next_gen_slot += 1;
                } else {
                    // Wait until `pcmbuf` has decoded up to this timestamp
                    break;
                }
            }
        }
    }
}

#[inline]
fn project_road(z: f32, lane_offset: f32, road_curve: f32, intensity: f32) -> (f32, f32, f32) {
    let zc = z.clamp(0.0, 1.0);
    let inv = 1.0 - zc;
    let p = inv * inv;
    let scale = 0.09 + 0.91 * p;

    let vp_x = VIEW_X + VIEW_W * 0.5 + road_curve * 105.0;
    let vp_y = (VIEW_Y + 78.0) + (intensity - 0.45) * 44.0;

    let base_cx = VIEW_X + VIEW_W * 0.5;
    let center_x = base_cx + (vp_x - base_cx) * (zc * zc);
    let hill_arch = (4.0 * zc * inv) * (0.45 - intensity) * 26.0;
    let center_y = vp_y + (SHIP_BASE_Y - vp_y) * p - hill_arch;

    let road_half_w = 24.0 + 246.0 * p;
    let lane_w = (road_half_w * 2.0) / 3.0;
    let sx = center_x + lane_offset * lane_w;

    (sx, center_y, scale)
}

fn draw_highway_block(
    ctx: &HostContext,
    t: PluginTheme,
    cx: f32,
    cy: f32,
    scale: f32,
    kind: BlockKind,
) {
    let w = (98.0 * scale).max(7.0);
    let h = (38.0 * scale).max(4.5);
    let lift = (18.0 * scale).max(2.5);
    let r = (5.0 * scale).max(0.0);
    let x = cx - w * 0.5;
    let y_front = cy - h * 0.5;
    let y_top = y_front - lift;

    match kind {
        BlockKind::Spike => {
            ctx.fill_rect(x, y_top, w, h + lift, r, t.fg1);
            ctx.stroke_rect(x, y_top, w, h + lift, r, (2.8 * scale).max(1.5), t.fg);
            let pad = (8.0 * scale).max(2.0);
            let sw = (3.0 * scale).max(1.5);
            ctx.draw_line(
                x + pad,
                y_top + pad,
                x + w - pad,
                y_top + h + lift - pad,
                sw,
                t.fg,
            );
            ctx.draw_line(
                x + w - pad,
                y_top + pad,
                x + pad,
                y_top + h + lift - pad,
                sw,
                t.fg,
            );
        }
        BlockKind::Heavy => {
            ctx.fill_rect(x, y_top, w, h + lift, r, t.fg);
            if w > 18.0 {
                let inset = (4.5 * scale).max(1.5);
                ctx.fill_rect(
                    x + inset,
                    y_top + inset,
                    w - inset * 2.0,
                    (4.0 * scale).max(1.5),
                    0.0,
                    t.bg,
                );
            }
        }
        BlockKind::Medium => {
            ctx.fill_rect(x, y_top, w, h + lift, r, t.fg2);
            ctx.stroke_rect(x, y_top, w, h + lift, r, (2.4 * scale).max(1.2), t.fg);
            if w > 18.0 {
                ctx.fill_rect(
                    x + w * 0.22,
                    y_top + h * 0.45,
                    w * 0.56,
                    (4.0 * scale).max(1.5),
                    0.0,
                    t.bg,
                );
            }
        }
        BlockKind::Light => {
            ctx.fill_rect(x, y_top, w, h + lift, r, t.fg1);
            ctx.stroke_rect(x, y_top, w, h + lift, r, (2.4 * scale).max(1.2), t.fg);
            if w > 18.0 {
                let ps = (9.0 * scale).max(3.0);
                ctx.fill_rect(cx - ps * 0.5, y_top + (h - ps) * 0.5, ps, ps, 2.0, t.fg);
            }
        }
    }
}

fn draw_sound_racer(ctx: &HostContext, state: &RacerState, hud: &HudCache, now_ms: u64) {
    let t = ctx.theme();
    ctx.begin_frame(t.bg, true, "Sound Racer");

    // ---- Top Live Track Ribbon (`y = 132..170`) ----
    if hud.has_track {
        ctx.draw_text(
            VIEW_X,
            148.0,
            &hud.track_display,
            25.0,
            FontWeight::SemiBold,
            TextAlign::Left,
            t.fg,
        );
        ctx.draw_text(
            992.0,
            142.0,
            &hud.time_display,
            22.0,
            FontWeight::Medium,
            TextAlign::Right,
            t.fg2,
        );
        ctx.draw_progress_bar(752.0, 158.0, 240.0, 10.0, hud.progress);
    } else {
        ctx.draw_text(
            VIEW_X,
            148.0,
            ctx.tr(
                "No track playing  —  Press A to start music",
                "Музика на паузі  —  Натисни A, щоб увімкнути трек",
            ),
            25.0,
            FontWeight::SemiBold,
            TextAlign::Left,
            t.fg2,
        );
    }

    // ---- Main 3D Highway Viewport (`700 x 562`) ----
    ctx.stroke_rect(VIEW_X, VIEW_Y, VIEW_W, VIEW_H, 10.0, 3.5, t.fg1);

    let (vp_x, vp_y, _) = project_road(1.0, 0.0, state.road_curve, state.intensity);

    let sun_r = 16.0 + state.live_kick * 20.0;
    ctx.fill_circle(vp_x, vp_y - 10.0, sun_r, t.fg1);
    ctx.fill_circle(vp_x, vp_y - 10.0, sun_r * 0.56, t.bg);

    let sky_span = 460.0_f32;
    let bar_step = sky_span / 16.0;
    let bar_w = bar_step - 6.0;
    for (i, &lvl) in state.live_bars.iter().enumerate() {
        let bx = (vp_x - sky_span * 0.5 + (i as f32) * bar_step)
            .clamp(VIEW_X + 10.0, VIEW_X + VIEW_W - bar_w - 10.0);
        let bh = (4.0 + lvl * 54.0).min(vp_y - VIEW_Y - 10.0).max(3.0);
        let shade = if lvl > 0.62 {
            t.fg
        } else if lvl > 0.30 {
            t.fg2
        } else {
            t.fg1
        };
        ctx.fill_rect(bx, vp_y - bh, bar_w, bh, 0.0, shade);
    }

    ctx.draw_line(
        VIEW_X + 8.0,
        vp_y,
        VIEW_X + VIEW_W - 8.0,
        vp_y,
        2.0,
        t.fg1,
    );

    for i in 0..9 {
        let z = ((i as f32 + state.scroll_phase) / 9.0).fract();
        let (lx, ly, sc) = project_road(z, -1.5, state.road_curve, state.intensity);
        let (rx, ry, _) = project_road(z, 1.5, state.road_curve, state.intensity);
        ctx.draw_line(lx, ly, rx, ry, (2.0 * sc).max(1.0), t.fg1);
    }

    const RAIL_SEGS: usize = 8;
    for &lane_edge in &[-1.5_f32, -0.5, 0.5, 1.5] {
        let is_outer = lane_edge.abs() > 1.0;
        let color = if is_outer { t.fg } else { t.fg2 };
        let base_w = if is_outer { 4.8 } else { 2.2 };
        let (mut px, mut py, mut psc) =
            project_road(0.0, lane_edge, state.road_curve, state.intensity);
        for s in 1..=RAIL_SEGS {
            let z1 = (s as f32) / (RAIL_SEGS as f32);
            let (nx, ny, nsc) = project_road(z1, lane_edge, state.road_curve, state.intensity);
            ctx.draw_line(px, py, nx, ny, (base_w * psc).max(1.3), color);
            px = nx;
            py = ny;
            psc = nsc;
        }
    }

    // Zero-allocation stack sort of visible blocks (far -> near) locked to `smooth_song_ms`!
    let mut vis = [(0.0_f32, 0usize, BlockKind::Heavy); MAX_VIS_BLOCKS];
    let mut vis_n = 0usize;
    let inv_travel = 1.0 / (TRAVEL_TIME_MS as f32);
    for b in &state.blocks {
        if b.resolved {
            continue;
        }
        let z = ((b.arrive_song_ms as f32) - state.smooth_song_ms) * inv_travel;
        if (-0.08..=1.02).contains(&z) && vis_n < MAX_VIS_BLOCKS {
            vis[vis_n] = (z.clamp(0.0, 1.0), b.lane, b.kind);
            vis_n += 1;
        }
    }
    vis[..vis_n].sort_unstable_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));

    for &(z, lane, kind) in &vis[..vis_n] {
        let lane_off = (lane as f32) - 1.0;
        let (bx, by, sc) = project_road(z, lane_off, state.road_curve, state.intensity);
        draw_highway_block(ctx, t, bx, by, sc, kind);
    }

    // ---- Player Hovercraft Ship (`z = 0.0`) ----
    let ship_lane_off = state.ship_pos - 1.0;
    let (ship_x, ship_ground_y, _) =
        project_road(0.0, ship_lane_off, state.road_curve, state.intensity);
    let jump_frac = state.jump_height_frac(now_ms);
    let jump_px = jump_frac * 58.0;

    if jump_px > 2.0 {
        let sh_w = 88.0 * (1.0 - jump_frac * 0.35);
        ctx.fill_rect(
            ship_x - sh_w * 0.5,
            ship_ground_y + 6.0,
            sh_w,
            12.0,
            5.0,
            t.fg1,
        );
    }

    let sy = ship_ground_y - jump_px;
    ctx.draw_line(ship_x, sy - 26.0, ship_x - 46.0, sy + 14.0, 4.5, t.fg);
    ctx.draw_line(ship_x, sy - 26.0, ship_x + 46.0, sy + 14.0, 4.5, t.fg);
    ctx.draw_line(ship_x - 46.0, sy + 14.0, ship_x + 46.0, sy + 14.0, 4.5, t.fg);
    ctx.fill_rect(ship_x - 20.0, sy - 8.0, 40.0, 22.0, 5.0, t.fg);
    let thrust_r = 5.5 + state.live_kick * 6.0;
    ctx.fill_circle(ship_x, sy + 14.0, thrust_r, t.fg2);
    ctx.fill_rect(ship_x - 4.0, sy - 1.0, 8.0, 8.0, 2.0, t.bg);

    if let Some((ref txt, until)) = state.callout {
        if now_ms < until {
            ctx.draw_text(
                VIEW_X + VIEW_W * 0.5,
                VIEW_Y + 34.0,
                txt,
                28.0,
                FontWeight::Bold,
                TextAlign::Center,
                t.fg,
            );
        }
    }

    // ---- Right Sidebar (`x = 752..992`): Score (+ conditional Hi-Score) + Collector + Slope ----
    let rx = 752.0_f32;
    let rw = 240.0_f32;

    ctx.stroke_rect(rx, VIEW_Y, rw, 134.0, 10.0, 3.5, t.fg1);
    ctx.draw_text(
        rx + rw * 0.5,
        VIEW_Y + 26.0,
        &hud.score_hdr,
        22.0,
        FontWeight::Bold,
        TextAlign::Center,
        t.fg2,
    );
    let score_cy = if hud.hi_str.is_empty() {
        VIEW_Y + 80.0
    } else {
        VIEW_Y + 68.0
    };
    ctx.draw_text(
        rx + rw * 0.5,
        score_cy,
        &hud.score_str,
        38.0,
        FontWeight::Bold,
        TextAlign::Center,
        t.fg,
    );
    if !hud.hi_str.is_empty() {
        let beating = state.map.hi_score(state.opts.mode) > 0
            && state.full_run_eligible
            && state.score > state.map.hi_score(state.opts.mode);
        ctx.draw_text(
            rx + rw * 0.5,
            VIEW_Y + 110.0,
            &hud.hi_str,
            20.0,
            FontWeight::SemiBold,
            TextAlign::Center,
            if beating { t.fg } else { t.fg2 },
        );
    }

    // Fast 3 x 7 Collector Matrix
    let tray_y = VIEW_Y + 148.0;
    let tray_h = 292.0_f32;
    ctx.stroke_rect(rx, tray_y, rw, tray_h, 10.0, 3.5, t.fg1);
    ctx.draw_text(
        rx + rw * 0.5,
        tray_y + 22.0,
        ctx.tr("COLLECTOR (3+)", "КОЛЕКТОР (3+)"),
        20.0,
        FontWeight::SemiBold,
        TextAlign::Center,
        t.fg2,
    );

    let gx = rx + 18.0;
    let gy = tray_y + 42.0;
    let gw = rw - 36.0;
    let gh = tray_h - 58.0;
    let cw = gw / (TRAY_COLS as f32);
    let ch = gh / (TRAY_ROWS as f32);

    ctx.stroke_rect(gx, gy, gw, gh, 4.0, 2.0, t.fg1);
    for c in 1..TRAY_COLS {
        let lx = gx + (c as f32) * cw;
        ctx.draw_line(lx, gy, lx, gy + gh, 1.5, t.fg1);
    }
    for r in 1..TRAY_ROWS {
        let ly = gy + (r as f32) * ch;
        ctx.draw_line(gx, ly, gx + gw, ly, 1.5, t.fg1);
    }

    let marker_x = gx + (state.lane as f32) * cw + cw * 0.22;
    ctx.fill_rect(marker_x, tray_y + tray_h - 11.0, cw * 0.56, 5.0, 2.0, t.fg);

    let flash_on = (now_ms / 70) % 2 == 0;
    for col in 0..TRAY_COLS {
        let cx = gx + (col as f32) * cw + 3.0;
        let cell_w = cw - 6.0;
        for row in 0..TRAY_ROWS {
            let v = state.tray.cells[col][row];
            if v == 0 {
                continue;
            }
            let cy = gy + ((TRAY_ROWS - 1 - row) as f32) * ch + 2.5;
            let cell_h = ch - 5.0;
            let matched = state.tray.matched_mask[col][row];

            if matched && flash_on {
                ctx.fill_rect(cx, cy, cell_w, cell_h, 3.0, t.fg);
                ctx.fill_rect(cx + 4.0, cy + 4.0, cell_w - 8.0, cell_h - 8.0, 1.5, t.bg);
            } else {
                match v {
                    1 => ctx.fill_rect(cx, cy, cell_w, cell_h, 3.0, t.fg),
                    2 => ctx.fill_rect(cx, cy, cell_w, cell_h, 3.0, t.fg2),
                    3 => {
                        ctx.fill_rect(cx, cy, cell_w, cell_h, 3.0, t.fg1);
                        ctx.stroke_rect(cx, cy, cell_w, cell_h, 3.0, 2.0, t.fg);
                    }
                    _ => {
                        ctx.draw_line(
                            cx + 4.0,
                            cy + 3.0,
                            cx + cell_w - 4.0,
                            cy + cell_h - 3.0,
                            2.2,
                            t.fg,
                        );
                        ctx.draw_line(
                            cx + cell_w - 4.0,
                            cy + 3.0,
                            cx + 4.0,
                            cy + cell_h - 3.0,
                            2.2,
                            t.fg,
                        );
                    }
                }
            }
        }
    }

    // Slope & Controls card (`y = 632..740`)
    let bot_y = tray_y + tray_h + 14.0;
    let bot_h = VIEW_Y + VIEW_H - bot_y;
    ctx.stroke_rect(rx, bot_y, rw, bot_h, 10.0, 3.5, t.fg1);

    let slope_lbl = if state.intensity > 0.68 {
        ctx.tr("Slope: ▼ RUSH!", "Схил: ▼ ДРАЙВ!")
    } else if state.intensity > 0.36 {
        ctx.tr("Slope: ► Groove", "Схил: ► Ритм")
    } else {
        ctx.tr("Slope: ▲ Climb", "Схил: ▲ Підйом")
    };
    ctx.draw_text(
        rx + rw * 0.5,
        bot_y + 26.0,
        slope_lbl,
        22.0,
        FontWeight::Bold,
        TextAlign::Center,
        t.fg,
    );
    ctx.draw_text(
        rx + rw * 0.5,
        bot_y + 58.0,
        ctx.tr("←/→: Lane  •  A: Jump", "←/→: Смуга  •  A: Стрибок"),
        19.0,
        FontWeight::Medium,
        TextAlign::Center,
        t.fg2,
    );
    ctx.draw_text(
        rx + rw * 0.5,
        bot_y + 84.0,
        ctx.tr("Start / Menu: Pause", "Start / Menu: Пауза"),
        19.0,
        FontWeight::Medium,
        TextAlign::Center,
        t.fg2,
    );

    ctx.present();
}

fn load_options(ctx: &HostContext) -> RacerOptions {
    if let Some(raw) = ctx
        .load_plugin_data("sound_racer", "options.txt")
        .and_then(|b| String::from_utf8(b).ok())
    {
        let mut it = raw.split_whitespace();
        let mode = match it.next().and_then(|v| v.parse::<u8>().ok()).unwrap_or(0) {
            1 => RaceMode::Cluster,
            _ => RaceMode::Mono,
        };
        let diff = match it.next().and_then(|v| v.parse::<u8>().ok()).unwrap_or(1) {
            0 => Difficulty::Chill,
            2 => Difficulty::Pro,
            _ => Difficulty::Normal,
        };
        return RacerOptions {
            mode,
            difficulty: diff,
        };
    }
    RacerOptions::default()
}

fn save_options(ctx: &HostContext, opts: RacerOptions) {
    let s = format!("{} {}\n", opts.mode as u8, opts.difficulty as u8);
    let _ = ctx.save_plugin_data("sound_racer", "options.txt", s.as_bytes());
}

fn build_map_for_track(ctx: &HostContext, tr: Option<&AudioTrackInfo>) -> (TrackMap, bool, bool) {
    let Some(t) = tr else {
        return (TrackMap::new_empty(0, 0), false, false);
    };
    let hash = compute_track_hash(t);
    let free = is_chiptune_or_module(&t.path);
    let subtune = is_subtune_chiptune(&t.path) && t.duration_ms > 0 && t.duration_ms != 150_000;
    if free {
        (TrackMap::new_empty(hash, 0), true, subtune)
    } else {
        (TrackMap::load_or_new(ctx, hash, t.duration_ms), false, false)
    }
}

/// Initial 2-item launch menu shown when entering `Sound Racer`:
/// `0` = `Start from Beginning`, `1` = `Continue`.
/// Returns `None` if user cancelled with `B` / `ForceExit`.
fn prompt_launch_mode(
    ctx: &HostContext,
    track: Option<&AudioTrackInfo>,
    hi_score: u32,
) -> Option<bool> {
    let start_badge = if hi_score > 0 {
        Some(format!("Hi: {hi_score}"))
    } else {
        None
    };
    let cont_badge = track.and_then(|tr| {
        if is_subtune_chiptune(&tr.path) && tr.duration_ms > 0 && tr.duration_ms != 150_000 {
            let sub = ((tr.position_ms + 500) / 1000) + 1;
            let tot = (tr.duration_ms / 1000).max(sub);
            Some(format!("#{sub}/{tot}"))
        } else if is_chiptune_or_module(&tr.path) {
            None
        } else {
            let s = tr.position_ms / 1000;
            Some(format!("{:02}:{:02}", s / 60, s % 60))
        }
    });

    let items = [
        match start_badge.as_deref() {
            Some(b) => MenuItem::option(
                ctx.tr("Start song from beginning", "Почати трек спочатку"),
                b,
            ),
            None => MenuItem::action(ctx.tr("Start song from beginning", "Почати трек спочатку")),
        },
        match cont_badge.as_deref() {
            Some(b) => MenuItem::option(ctx.tr("Continue", "Продовжити"), b),
            None => MenuItem::action(ctx.tr("Continue", "Продовжити")),
        },
    ];

    match ctx.show_menu("Sound Racer", &items, 0) {
        MenuOutcome::Selected(0) => Some(true),
        MenuOutcome::Selected(1) => Some(false),
        MenuOutcome::Selected(_) | MenuOutcome::Cancelled | MenuOutcome::ForceExit => None,
    }
}

fn open_options_menu(ctx: &HostContext, state: &mut RacerState) -> bool {
    let mut sel = 0usize;
    loop {
        let mode_lbl = match state.opts.mode {
            RaceMode::Mono => ctx.tr("Mono", "Моно"),
            RaceMode::Cluster => ctx.tr("Cluster (3-Tone)", "Кластери"),
        };
        let diff_lbl = match state.opts.difficulty {
            Difficulty::Chill => ctx.tr("Chill", "Легка"),
            Difficulty::Normal => ctx.tr("Normal", "Звичайна"),
            Difficulty::Pro => ctx.tr("Pro", "Профі"),
        };
        let hi = state.map.hi_score(state.opts.mode);
        let hi_lbl = if hi > 0 {
            hi.to_string()
        } else {
            String::from("—")
        };
        let items = [
            MenuItem::option(ctx.tr("Game Mode", "Режим гри"), mode_lbl),
            MenuItem::option(ctx.tr("Difficulty", "Складність"), diff_lbl),
            MenuItem::option(
                ctx.tr("Reset Track Hi-Score", "Скинути рекорд треку"),
                &hi_lbl,
            ),
        ];
        match ctx.show_menu(ctx.tr("Options", "Налаштування"), &items, sel) {
            MenuOutcome::Selected(0) => {
                sel = 0;
                state.opts.mode = match state.opts.mode {
                    RaceMode::Mono => RaceMode::Cluster,
                    RaceMode::Cluster => RaceMode::Mono,
                };
                state.blocks.clear();
                state.tray.clear_all();
                state.next_gen_slot = ((state.smooth_song_ms.max(0.0) as u32) / SLOT_MS) as usize + 2;
                state.last_spawn_slot = 0;
                save_options(ctx, state.opts);
            }
            MenuOutcome::Selected(1) => {
                sel = 1;
                state.opts.difficulty = match state.opts.difficulty {
                    Difficulty::Chill => Difficulty::Normal,
                    Difficulty::Normal => Difficulty::Pro,
                    Difficulty::Pro => Difficulty::Chill,
                };
                save_options(ctx, state.opts);
            }
            MenuOutcome::Selected(2) => {
                sel = 2;
                state.map.reset_hi_score(state.opts.mode);
                state.map.save_if_dirty(ctx);
                ctx.show_splash(ctx.tr("Track Hi-Score reset", "Рекорд треку скинуто"), 700);
            }
            MenuOutcome::Selected(_) | MenuOutcome::Cancelled => return false,
            MenuOutcome::ForceExit => return true,
        }
    }
}

/// Automatically pauses music while the in-game `START` / `MENU` pause menu is open.
/// Returns `(should_exit, restarted_from_zero)`.
fn open_system_menu(ctx: &HostContext, state: &mut RacerState) -> (bool, bool) {
    let pause_started_ms = ctx.ticks_ms();
    let was_playing = ctx.audio_status() == 1;
    if was_playing {
        ctx.audio_toggle_play_pause();
    }
    state.map.save_if_dirty(ctx);

    let shift_timers = |st: &mut RacerState, end_ms: u64| {
        let delta = end_ms.saturating_sub(pause_started_ms);
        if let Some(ref mut t) = st.jump_until_ms {
            *t = t.saturating_add(delta);
        }
        if let Some(ref mut t) = st.tray.fuse_until_ms {
            *t = t.saturating_add(delta);
        }
        if let Some((_, ref mut exp)) = st.callout {
            *exp = exp.saturating_add(delta);
        }
    };

    let mut sel = 0usize;
    loop {
        let items = [
            MenuItem::action(ctx.tr("Continue", "Продовжити")),
            MenuItem::action(ctx.tr("Start song from beginning", "Почати трек спочатку")),
            MenuItem::action(ctx.tr("Next Track", "Наступний трек")),
            MenuItem::action(ctx.tr("Previous Track", "Попередній трек")),
            MenuItem::submenu(ctx.tr("Options", "Налаштування")),
            MenuItem::action(ctx.tr("Exit", "Вихід")),
        ];
        match ctx.show_menu("Sound Racer", &items, sel) {
            MenuOutcome::Selected(0) => {
                if (was_playing || ctx.audio_status() == 2) && ctx.audio_status() != 1 {
                    ctx.audio_toggle_play_pause();
                }
                shift_timers(state, ctx.ticks_ms());
                return (false, false);
            }
            MenuOutcome::Cancelled => {
                if was_playing && ctx.audio_status() == 2 {
                    ctx.audio_toggle_play_pause();
                }
                shift_timers(state, ctx.ticks_ms());
                return (false, false);
            }
            MenuOutcome::Selected(1) => {
                let restart_seek_ms = if state.subtune_mode {
                    ((state.last_reported_pos_ms + 500) / 1000) * 1000
                } else {
                    0
                };
                ctx.audio_seek_ms(restart_seek_ms);
                ctx.sleep_ms(40);
                let hash = state.map.hash;
                let dur = state.map.duration_ms;
                let free = state.free_clock;
                let subtune = state.subtune_mode;
                if !free {
                    state.map.save_if_dirty(ctx);
                }
                let reloaded = if free {
                    TrackMap::new_empty(hash, 0)
                } else {
                    TrackMap::load_or_new(ctx, hash, dur)
                };
                state.start_on_track(ctx, reloaded, 0, restart_seek_ms, free, subtune, true);
                return (false, true);
            }
            MenuOutcome::Selected(2) => {
                state.finalize_track_if_completed(ctx);
                if ctx.audio_status() == 2 {
                    ctx.audio_toggle_play_pause();
                }
                ctx.audio_next();
                ctx.sleep_ms(40);
                return (false, false);
            }
            MenuOutcome::Selected(3) => {
                state.finalize_track_if_completed(ctx);
                if ctx.audio_status() == 2 {
                    ctx.audio_toggle_play_pause();
                }
                ctx.audio_prev();
                ctx.sleep_ms(40);
                return (false, false);
            }
            MenuOutcome::Selected(4) => {
                sel = 4;
                if open_options_menu(ctx, state) {
                    state.finalize_track_if_completed(ctx);
                    if was_playing && ctx.audio_status() == 2 {
                        ctx.audio_toggle_play_pause();
                    }
                    return (true, false);
                }
            }
            MenuOutcome::Selected(_) | MenuOutcome::ForceExit => {
                state.finalize_track_if_completed(ctx);
                if was_playing && ctx.audio_status() == 2 {
                    ctx.audio_toggle_play_pause();
                }
                return (true, false);
            }
        }
    }
}

fn run_sound_racer(ctx: &mut HostContext, _file: Option<&Path>) -> i32 {
    let opts = load_options(ctx);
    let init_tr = ctx.audio_track_info();
    let (init_map, init_free, init_subtune) = build_map_for_track(ctx, init_tr.as_ref());
    let init_hash = init_map.hash;

    // 1. Mandatory 2-item launch menu: `Start song from beginning` vs `Continue`
    let Some(from_beginning) = prompt_launch_mode(
        ctx,
        init_tr.as_ref(),
        init_map.hi_score(opts.mode),
    ) else {
        return 0;
    };

    if from_beginning {
        let seek_target = if init_subtune {
            let raw = init_tr.as_ref().map(|t| t.position_ms).unwrap_or(0);
            ((raw + 500) / 1000) * 1000
        } else {
            0
        };
        ctx.audio_seek_ms(seek_target);
        ctx.sleep_ms(45);
    } else if ctx.audio_status() != 1 {
        ctx.audio_toggle_play_pause();
        ctx.sleep_ms(30);
    }

    let live_tr = ctx.audio_track_info().or(init_tr);
    let active_hash = live_tr.as_ref().map(compute_track_hash).unwrap_or(init_hash);
    let (active_map, active_free, active_subtune) = if active_hash == init_hash {
        (init_map, init_free, init_subtune)
    } else {
        build_map_for_track(ctx, live_tr.as_ref())
    };

    let reported_pos_ms = live_tr.as_ref().map(|t| t.position_ms).unwrap_or(0);
    let start_pos_ms = if from_beginning || active_free {
        0
    } else {
        reported_pos_ms
    };
    let eligible = !active_free && (from_beginning || start_pos_ms <= 2_500);

    let mut state = RacerState::new(
        opts,
        active_map,
        start_pos_ms,
        reported_pos_ms,
        active_free,
        active_subtune,
        eligible,
    );
    let mut hud = HudCache::new();
    if let Some(ref tr) = live_tr {
        hud.set_track_label(ctx, active_hash, tr);
        let disp_dur = if active_free { 0 } else { tr.duration_ms };
        hud.update_time(start_pos_ms, disp_dur);
    }

    let mut pcm_live = [0i16; 384];
    let mut pcm_ahead = [0i16; 512];

    if ctx.audio_status() == 1 {
        state.advance_horizon(ctx, &mut pcm_ahead);
    }
    hud.sync_score(
        ctx,
        state.score,
        state.map.hi_score(state.opts.mode),
        state.chain,
        state.full_run_eligible,
    );

    let mut prev_frame_ms = ctx.ticks_ms();
    let mut last_track_poll_ms = prev_frame_ms;

    loop {
        let frame_start = ctx.ticks_ms();
        let dt_ms = frame_start.saturating_sub(prev_frame_ms).min(60) as f32;
        prev_frame_ms = frame_start;

        let inp = ctx.poll_input(0);
        if inp.should_exit {
            state.finalize_track_if_completed(ctx);
            return 0;
        }
        if inp.menu_requested {
            let (exit_now, restarted) = open_system_menu(ctx, &mut state);
            if exit_now {
                return 0;
            }
            prev_frame_ms = ctx.ticks_ms();
            if restarted && ctx.audio_status() == 1 {
                state.advance_horizon(ctx, &mut pcm_ahead);
            }
            if let Some(tr) = ctx.audio_track_info() {
                let h = compute_track_hash(&tr);
                if h != state.map.hash {
                    let (nmap, nfree, nsub) = build_map_for_track(ctx, Some(&tr));
                    let sp = if nfree { 0 } else { tr.position_ms };
                    state.start_on_track(
                        ctx,
                        nmap,
                        sp,
                        tr.position_ms,
                        nfree,
                        nsub,
                        !nfree && tr.position_ms <= 3_000,
                    );
                }
                hud.set_track_label(ctx, state.map.hash, &tr);
                let disp_dur = if state.free_clock { 0 } else { tr.duration_ms };
                hud.update_time(state.smooth_song_ms.max(0.0) as u32, disp_dur);
            }
            hud.sync_score(
                ctx,
                state.score,
                state.map.hi_score(state.opts.mode),
                state.chain,
                state.full_run_eligible,
            );
            continue;
        }

        if inp.pressed(btn::LEFT | btn::L) && state.lane > 0 {
            state.lane -= 1;
        } else if inp.pressed(btn::RIGHT | btn::R) && state.lane < 2 {
            state.lane += 1;
        }

        if inp.pressed(btn::L2) {
            state.finalize_track_if_completed(ctx);
            ctx.audio_prev();
        } else if inp.pressed(btn::R2) {
            state.finalize_track_if_completed(ctx);
            ctx.audio_next();
        }

        let astat = ctx.audio_status();
        let playing = astat == 1;

        if inp.pressed(btn::A) {
            if !playing {
                ctx.audio_toggle_play_pause();
            } else if !state.is_airborne(frame_start) {
                state.jump_until_ms = Some(frame_start + JUMP_DURATION_MS);
            }
        }

        if inp.pressed(btn::B) {
            if let Some((cnt, base_pts)) = state.tray.poll_cash_in(frame_start, true) {
                let gained = base_pts * state.chain;
                state.add_score(gained);
                state.chain = (state.chain + 1).min(8);
                state.callout = Some((
                    format!("{} x{}  +{}", ctx.tr("CASH IN", "КАСКАД"), cnt, gained),
                    frame_start + 950,
                ));
            }
        }

        let target_lane = state.lane as f32;
        state.ship_pos += (target_lane - state.ship_pos) * 0.34;
        state.road_curve += (state.target_curve - state.road_curve) * 0.055;

        if playing {
            state.smooth_song_ms += dt_ms * state.clock_rate;
            let cur_u32 = state.smooth_song_ms.max(0.0) as u32;
            if cur_u32 > state.max_song_ms {
                state.max_song_ms = cur_u32;
            }
            state.scroll_phase = (state.scroll_phase + 0.05 + state.intensity * 0.07).fract();

            let n_live = ctx.audio_read_pcm(0, &mut pcm_live);
            if n_live >= 32 {
                let lf = analyze_stereo_window(&pcm_live, n_live);
                state.live_kick = (state.live_kick * 0.72).max((lf.low * 1.35).clamp(0.0, 1.0));
                for (dst, &src) in state.live_bars.iter_mut().zip(lf.bars.iter()) {
                    *dst = if src > *dst {
                        *dst * 0.35 + src * 0.65
                    } else {
                        *dst * 0.82 + src * 0.18
                    };
                }
            } else {
                state.live_kick *= 0.85;
                for dst in &mut state.live_bars {
                    *dst *= 0.85;
                }
            }

            state.advance_horizon(ctx, &mut pcm_ahead);

            // Check full-run completion as the song enters its final ~850 ms!
            if state.full_run_eligible
                && state.map.duration_ms >= 5_000
                && cur_u32.saturating_add(850) >= state.map.duration_ms
            {
                let is_new_record = state.map.update_hi_score(state.opts.mode, state.score);
                if !state.completion_announced {
                    state.completion_announced = true;
                    state.callout = Some((
                        if is_new_record {
                            format!(
                                "{}  {}!",
                                ctx.tr("NEW HI-SCORE:", "НОВИЙ РЕКОРД:"),
                                state.score
                            )
                        } else {
                            format!(
                                "{}  {}",
                                ctx.tr("TRACK COMPLETE!", "ТРЕК ПРОЙДЕНО!"),
                                state.score
                            )
                        },
                        frame_start + 2_200,
                    ));
                }
            }
        } else {
            state.live_kick *= 0.85;
            for dst in &mut state.live_bars {
                *dst *= 0.88;
            }
            // If playback stopped naturally at the end of a playlist after finishing the track:
            if astat == 0 && state.map.hash != 0 {
                state.finalize_track_if_completed(ctx);
            }
        }

        // Resolve blocks arriving at `smooth_song_ms`
        let airborne = state.is_airborne(frame_start);
        let ship_lane = (state.ship_pos.round() as i32).clamp(0, 2) as usize;
        let song_now = state.smooth_song_ms;

        for idx in 0..state.blocks.len() {
            let b = state.blocks[idx];
            if b.resolved {
                continue;
            }
            let dt = song_now - (b.arrive_song_ms as f32);
            if dt >= -35.0 && dt <= 75.0 && b.lane == ship_lane && !airborne {
                state.blocks[idx].resolved = true;
                if b.kind == BlockKind::Spike {
                    state.chain = 1;
                    state.streak_hits = 0;
                    let _ = state.tray.push_block(ship_lane, BlockKind::Spike, frame_start);
                    state.callout = Some((
                        ctx.tr("SPIKE HIT!  Chain x1", "ШИП!  Комбо скинуто").to_string(),
                        frame_start + 800,
                    ));
                } else {
                    state.streak_hits += 1;
                    if state.streak_hits % 6 == 0 {
                        state.chain = (state.chain + 1).min(8);
                    }
                    let pickup_pts = 15 * state.chain;
                    state.add_score(pickup_pts);
                    let overflowed = state.tray.push_block(ship_lane, b.kind, frame_start);
                    if overflowed {
                        state.chain = 1;
                        state.streak_hits = 0;
                        state.callout = Some((
                            ctx.tr("COLUMN OVERFILL!", "ПЕРЕПОВНЕННЯ!").to_string(),
                            frame_start + 850,
                        ));
                    }
                }
            } else if dt > 85.0 {
                state.blocks[idx].resolved = true;
                if state.opts.mode == RaceMode::Mono && b.kind == BlockKind::Heavy {
                    state.streak_hits = 0;
                }
            }
        }

        while state.blocks.front().map(|b| b.resolved).unwrap_or(false) {
            state.blocks.pop_front();
        }

        if let Some((cnt, base_pts)) = state.tray.poll_cash_in(frame_start, false) {
            let gained = base_pts * state.chain;
            state.add_score(gained);
            state.chain = (state.chain + 1).min(8);
            state.callout = Some((
                format!("{} x{}  +{}", ctx.tr("CLUSTER", "КЛАСТЕР"), cnt, gained),
                frame_start + 950,
            ));
        }

        // Poll track metadata & phase-lock `smooth_song_ms` ~4 times/sec
        if frame_start.saturating_sub(last_track_poll_ms) >= 250 {
            last_track_poll_ms = frame_start;
            if let Some(tr) = ctx.audio_track_info() {
                let h = compute_track_hash(&tr);
                if h != state.map.hash {
                    // Song changed! Finalize previous track's Hi-Score & map, reset score to 0, load new track's map!
                    let carry_banner = state.callout.clone();
                    let (new_map, nfree, nsub) = build_map_for_track(ctx, Some(&tr));
                    let sp = if nfree { 0 } else { tr.position_ms };
                    let from_start = !nfree && tr.position_ms <= 3_500;
                    state.start_on_track(
                        ctx,
                        new_map,
                        sp,
                        tr.position_ms,
                        nfree,
                        nsub,
                        from_start,
                    );
                    state.callout = carry_banner;
                    hud.set_track_label(ctx, h, &tr);
                    if playing {
                        state.advance_horizon(ctx, &mut pcm_ahead);
                    }
                } else if state.free_clock {
                    // Synthesized chiptune / tracker (`.nsf`, `.gbs`, `.sid`, `.spc`, `.vgm`, `.mod`, ...):
                    // keep highway locked at full 1.0x real-time speed driven directly by `pcmbuf`!
                    state.clock_rate = 1.0;
                    if state.subtune_mode {
                        let prev_sub = (state.last_reported_pos_ms + 500) / 1000;
                        let cur_sub = (tr.position_ms + 500) / 1000;
                        if cur_sub != prev_sub {
                            state.start_on_track(
                                ctx,
                                TrackMap::new_empty(h, 0),
                                0,
                                tr.position_ms,
                                true,
                                true,
                                false,
                            );
                            if playing {
                                state.advance_horizon(ctx, &mut pcm_ahead);
                            }
                        }
                    }
                    state.last_reported_pos_ms = tr.position_ms;
                    hud.set_track_label(ctx, h, &tr);
                } else {
                    if state.map.duration_ms == 0 && tr.duration_ms > 0 {
                        state.map.duration_ms = tr.duration_ms;
                    }
                    if state.last_reported_pos_ms > 4_500 && tr.position_ms < 1_200 {
                        let hash = state.map.hash;
                        let dur = state.map.duration_ms.max(tr.duration_ms);
                        state.map.save_if_dirty(ctx);
                        let reloaded = TrackMap::load_or_new(ctx, hash, dur);
                        state.start_on_track(
                            ctx,
                            reloaded,
                            tr.position_ms,
                            tr.position_ms,
                            false,
                            false,
                            true,
                        );
                    } else {
                        let jump = (tr.position_ms as i64) - (state.last_reported_pos_ms as i64);
                        let err = (tr.position_ms as f32) - state.smooth_song_ms;
                        if jump.abs() > 3_000 && err.abs() > 3_000.0 {
                            state.smooth_song_ms = tr.position_ms as f32;
                            state.clock_rate = 1.0;
                            state.frozen_pos_polls = 0;
                            state.blocks.clear();
                            state.next_gen_slot =
                                ((tr.position_ms / SLOT_MS) as usize).saturating_add(2);
                            state.last_spawn_slot = 0;
                        } else if playing {
                            if jump == 0 {
                                state.frozen_pos_polls = state.frozen_pos_polls.saturating_add(1);
                            } else {
                                state.frozen_pos_polls = 0;
                            }
                            if state.frozen_pos_polls >= 2 {
                                // Stream/loop with static `elapsed`: run smoothly at 1.0x
                                state.clock_rate = 1.0;
                            } else {
                                let target_rate = (1.0 + err / 550.0).clamp(0.04, 2.0);
                                state.clock_rate = state.clock_rate * 0.65 + target_rate * 0.35;
                            }
                        }
                        state.last_reported_pos_ms = tr.position_ms;
                    }
                    hud.set_track_label(ctx, h, &tr);
                }
                let disp_dur = if state.free_clock { 0 } else { tr.duration_ms };
                hud.update_time(state.smooth_song_ms.max(0.0) as u32, disp_dur);
            } else {
                hud.has_track = false;
            }
        } else if hud.has_track {
            let disp_dur = if state.free_clock { 0 } else { state.map.duration_ms };
            hud.update_time(state.smooth_song_ms.max(0.0) as u32, disp_dur);
        }

        hud.sync_score(
            ctx,
            state.score,
            state.map.hi_score(state.opts.mode),
            state.chain,
            state.full_run_eligible,
        );

        let now = ctx.ticks_ms();
        draw_sound_racer(ctx, &state, &hud, now);

        let elapsed = ctx.ticks_ms().saturating_sub(frame_start);
        if elapsed < 16 {
            ctx.sleep_ms((16 - elapsed) as u32);
        } else {
            ctx.sleep_ms(0);
        }
    }
}

uipod_declare_plugin! {
    id: "sound_racer",
    name: "Sound Racer",
    name_uk: "Sound Racer",
    author: "UIPod",
    version: "0.2.0",
    category: PluginCategory::Game,
    kind: PluginKind::MenuDriven,
    extensions: "",
    entry: run_sound_racer,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dsp_analyzer_distinguishes_bass_from_high_transients_and_pan() {
        let mut bass = vec![0i16; 512];
        for i in 0..256 {
            let s = ((i as f32 * 0.08).sin() * 24000.0) as i16;
            bass[i * 2] = s;
            bass[i * 2 + 1] = s / 4;
        }
        let bf = analyze_stereo_window(&bass, 256);
        assert!(bf.low > bf.high);
        assert!(bf.pan < -0.2);

        let mut treble = vec![0i16; 512];
        for i in 0..256 {
            let s = if i % 2 == 0 { 18000 } else { -18000 };
            treble[i * 2] = s / 4;
            treble[i * 2 + 1] = s;
        }
        let tf = analyze_stereo_window(&treble, 256);
        assert!(tf.high > tf.low * 2.0);
        assert!(tf.pan > 0.2);
    }

    #[test]
    fn collector_tray_matches_connected_cluster_and_applies_gravity() {
        let mut tray = CollectorTray::new();
        assert!(!tray.push_block(0, BlockKind::Light, 10));
        assert!(!tray.push_block(0, BlockKind::Heavy, 20));
        assert!(!tray.push_block(0, BlockKind::Heavy, 30));
        assert!(tray.fuse_until_ms.is_none());

        assert!(!tray.push_block(1, BlockKind::Light, 40));
        assert!(!tray.push_block(1, BlockKind::Heavy, 50));
        assert!(tray.fuse_until_ms.is_some());

        let cashed = tray.poll_cash_in(50 + CLUSTER_FUSE_MS + 1, false);
        assert_eq!(cashed.map(|c| c.0), Some(3));
        assert_eq!(tray.column_height(0), 1);
        assert_eq!(tray.column_height(1), 1);
    }

    #[test]
    fn track_map_persists_slots_hi_scores_and_replays_deterministically() {
        let mut map = TrackMap::new_empty(0xDEAD_BEEF_CAFE_1234, 180_000);
        assert_eq!(map.hi_score(RaceMode::Mono), 0);
        assert!(map.update_hi_score(RaceMode::Mono, 8450));
        assert!(!map.update_hi_score(RaceMode::Mono, 5000));
        assert_eq!(map.hi_score(RaceMode::Mono), 8450);

        for s in 0..80 {
            map.slots[s] = TrackSlot {
                flags: 1,
                rms: if s % 4 == 0 { 210 } else { 60 },
                low: if s % 4 == 0 { 230 } else { 40 },
                mid: 90,
                high: 50,
                pan: ((s as i32 % 11) - 5) as i8 * 12,
            };
        }

        let encoded = map.to_bytes();
        let decoded = TrackMap::from_bytes(0xDEAD_BEEF_CAFE_1234, &encoded).unwrap();
        assert_eq!(decoded.hi_score(RaceMode::Mono), 8450);
        assert_eq!(decoded.slots[12], map.slots[12]);

        // Verify two runs over the same `TrackMap` spawn identical block sequences!
        let opts = RacerOptions::default();
        let mut run_a = RacerState::new(opts, map, 0, 0, false, false, true);
        let mut run_b = RacerState::new(opts, decoded, 0, 0, false, false, true);
        for s in 0..80 {
            let fa = run_a.map.slots[s].to_features();
            let fb = run_b.map.slots[s].to_features();
            run_a.step_slot_deterministic(s, fa);
            run_b.step_slot_deterministic(s, fb);
        }
        assert!(!run_a.blocks.is_empty());
        assert_eq!(run_a.blocks.len(), run_b.blocks.len());
        for (ba, bb) in run_a.blocks.iter().zip(run_b.blocks.iter()) {
            assert_eq!(ba.arrive_song_ms, bb.arrive_song_ms);
            assert_eq!(ba.lane, bb.lane);
            assert_eq!(ba.kind, bb.kind);
        }
    }

    #[test]
    fn detects_nsf_and_chiptune_formats_for_1x_clock() {
        assert!(is_subtune_chiptune("/mnt/SDCARD/Music/Castlevania.NSF"));
        assert!(is_chiptune_or_module("/mnt/SDCARD/Music/Castlevania.NSF"));
        assert!(is_chiptune_or_module("/mnt/SDCARD/Music/ChronoTrigger.spc"));
        assert!(is_chiptune_or_module("/mnt/SDCARD/Music/SpaceDebris.mod"));
        assert!(!is_chiptune_or_module("/mnt/SDCARD/Music/DaftPunk.flac"));
    }
}
