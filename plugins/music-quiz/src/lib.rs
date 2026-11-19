//! Classic iPod-inspired `Music Quiz` (`Вгадай мелодію`) game plugin for UIPod.
//!
//! Picks a random track from UIPod's unified Music library (`Local` + `Navidrome` + `Jellyfin`),
//! jumps directly into a random mid-song snippet (`20%..65%`), presents 4 distinct
//! `Artist - Track` choices, and awards `+1` point for each right guess.

use std::fs;
use std::path::Path;

use uipod_plugin_sdk::{
    btn, uipod_declare_plugin, FontWeight, HostContext, LibraryTrack, MenuItem, MenuOutcome,
    PluginCategory, PluginKind, TextAlign, CONTENT_TOP_Y, SCREEN_HEIGHT, SCREEN_WIDTH,
};

#[derive(Debug, Clone)]
struct QuizSong {
    path: String,
    label: String,
    norm_key: String,
    duration_ms: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TimerSetting {
    Sec15,
    Sec10,
    Off,
}

impl TimerSetting {
    fn duration_ms(self) -> Option<u64> {
        match self {
            Self::Sec15 => Some(15_000),
            Self::Sec10 => Some(10_000),
            Self::Off => None,
        }
    }

    fn next(self) -> Self {
        match self {
            Self::Sec15 => Self::Sec10,
            Self::Sec10 => Self::Off,
            Self::Off => Self::Sec15,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum GameLength {
    Endless,
    Rounds10,
    Rounds20,
}

impl GameLength {
    fn limit(self) -> Option<u32> {
        match self {
            Self::Endless => None,
            Self::Rounds10 => Some(10),
            Self::Rounds20 => Some(20),
        }
    }

    fn next(self) -> Self {
        match self {
            Self::Endless => Self::Rounds10,
            Self::Rounds10 => Self::Rounds20,
            Self::Rounds20 => Self::Endless,
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct QuizSave {
    best_score: u32,
    timer: TimerSetting,
    length: GameLength,
}

impl Default for QuizSave {
    fn default() -> Self {
        Self {
            best_score: 0,
            timer: TimerSetting::Sec15,
            length: GameLength::Endless,
        }
    }
}

/// Tiny deterministic xorshift64 PRNG seeded from `ticks_ms()`.
struct Rng64(u64);

impl Rng64 {
    fn new(seed: u64) -> Self {
        Self(seed ^ 0x9E37_79B9_7F4A_7C15)
    }

    fn next_u64(&mut self) -> u64 {
        let mut x = self.0;
        if x == 0 {
            x = 0xD1B5_4A32_D192_ED03;
        }
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }

    fn gen_range(&mut self, upper: usize) -> usize {
        if upper <= 1 {
            0
        } else {
            (self.next_u64() as usize) % upper
        }
    }

    fn shuffle<T>(&mut self, slice: &mut [T]) {
        for i in (1..slice.len()).rev() {
            let j = self.gen_range(i + 1);
            slice.swap(i, j);
        }
    }
}

fn clean_filename_stem(path: &str) -> String {
    let base = path.rsplit('/').next().unwrap_or(path);
    let stem = match base.rfind('.') {
        Some(dot) if dot > 0 => &base[..dot],
        _ => base,
    };
    // Strip leading "01 - " or "01. " track number prefixes if present
    let trimmed = stem.trim();
    let bytes = trimmed.as_bytes();
    let mut digits = 0usize;
    while digits < bytes.len() && digits < 3 && bytes[digits].is_ascii_digit() {
        digits += 1;
    }
    if digits > 0 && digits < trimmed.len() {
        let rest = trimmed[digits..].trim_start();
        if let Some(after) = rest.strip_prefix('-').or_else(|| rest.strip_prefix('.')) {
            let candidate = after.trim();
            if !candidate.is_empty() {
                return candidate.to_string();
            }
        }
    }
    trimmed.to_string()
}

fn format_artist_track(artist_raw: &str, title_raw: &str, path: &str, unknown_artist: &str) -> String {
    let title = {
        let t = title_raw.trim();
        if t.is_empty() {
            clean_filename_stem(path)
        } else {
            t.to_string()
        }
    };
    let artist = {
        let a = artist_raw.trim();
        if a.is_empty() {
            unknown_artist
        } else {
            a
        }
    };
    format!("{artist} - {title}")
}

fn normalize_key(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut prev_ws = false;
    for ch in s.chars() {
        if ch.is_whitespace() {
            if !prev_ws && !out.is_empty() {
                out.push(' ');
            }
            prev_ws = true;
        } else {
            prev_ws = false;
            for lc in ch.to_lowercase() {
                out.push(lc);
            }
        }
    }
    out.trim().to_string()
}

fn build_song_pool(raw_tracks: Vec<LibraryTrack>, unknown_artist: &str) -> Vec<QuizSong> {
    let mut songs = Vec::with_capacity(raw_tracks.len());
    let mut seen_keys = std::collections::HashSet::new();

    for tr in raw_tracks {
        if tr.path.trim().is_empty() {
            continue;
        }
        let label = format_artist_track(&tr.artist, &tr.title, &tr.path, unknown_artist);
        let norm_key = normalize_key(&label);
        if norm_key.is_empty() || !seen_keys.insert(norm_key.clone()) {
            continue;
        }
        songs.push(QuizSong {
            path: tr.path,
            label,
            norm_key,
            duration_ms: tr.duration_ms,
        });
    }
    songs
}

/// Picks a random snippet offset inside `[20% .. 65%]` of `duration_ms` so intros/outros are skipped
/// and at least ~15s of music remains.
fn pick_snippet_start_ms(duration_ms: u32, rng: &mut Rng64) -> u32 {
    if duration_ms < 32_000 {
        return 0;
    }
    let min_ms = (duration_ms / 5).max(8_000);
    let max_by_pct = (duration_ms.saturating_mul(65)) / 100;
    let max_by_tail = duration_ms.saturating_sub(16_000);
    let max_ms = max_by_pct.min(max_by_tail);
    if max_ms <= min_ms {
        return min_ms.min(duration_ms / 3);
    }
    let span = (max_ms - min_ms) as usize;
    min_ms + (rng.gen_range(span + 1) as u32)
}

/// Builds 4 distinct choice indices into `pool` containing `target_idx` and returning `(choices, correct_slot)`.
fn build_four_choices(pool: &[QuizSong], target_idx: usize, rng: &mut Rng64) -> ([usize; 4], usize) {
    let mut chosen = [target_idx; 4];
    let mut count = 1usize;
    let mut attempts = 0usize;

    while count < 4 && attempts < 256 {
        attempts += 1;
        let cand = rng.gen_range(pool.len());
        let dup = chosen[..count]
            .iter()
            .any(|&existing| existing == cand || pool[existing].norm_key == pool[cand].norm_key);
        if !dup {
            chosen[count] = cand;
            count += 1;
        }
    }

    // Deterministic linear scan fallback if random attempts hit collisions on tiny 4-song libraries
    if count < 4 {
        for cand in 0..pool.len() {
            if count >= 4 {
                break;
            }
            let dup = chosen[..count]
                .iter()
                .any(|&existing| existing == cand || pool[existing].norm_key == pool[cand].norm_key);
            if !dup {
                chosen[count] = cand;
                count += 1;
            }
        }
    }

    rng.shuffle(&mut chosen);
    let correct_slot = chosen.iter().position(|&idx| idx == target_idx).unwrap_or(0);
    (chosen, correct_slot)
}

fn load_save(ctx: &HostContext) -> QuizSave {
    let path = ctx.data_dir().join("music_quiz.cfg");
    let Ok(txt) = fs::read_to_string(path) else {
        return QuizSave::default();
    };
    let mut sv = QuizSave::default();
    for line in txt.lines() {
        let Some((k, v)) = line.split_once('=') else {
            continue;
        };
        match k.trim() {
            "best" => {
                if let Ok(n) = v.trim().parse::<u32>() {
                    sv.best_score = n;
                }
            }
            "timer" => {
                sv.timer = match v.trim() {
                    "10" => TimerSetting::Sec10,
                    "off" => TimerSetting::Off,
                    _ => TimerSetting::Sec15,
                };
            }
            "length" => {
                sv.length = match v.trim() {
                    "10" => GameLength::Rounds10,
                    "20" => GameLength::Rounds20,
                    _ => GameLength::Endless,
                };
            }
            _ => {}
        }
    }
    sv
}

fn save_state(ctx: &HostContext, sv: QuizSave) {
    let path = ctx.data_dir().join("music_quiz.cfg");
    let timer_s = match sv.timer {
        TimerSetting::Sec15 => "15",
        TimerSetting::Sec10 => "10",
        TimerSetting::Off => "off",
    };
    let len_s = match sv.length {
        GameLength::Endless => "endless",
        GameLength::Rounds10 => "10",
        GameLength::Rounds20 => "20",
    };
    let body = format!("best={}\ntimer={}\nlength={}\n", sv.best_score, timer_s, len_s);
    let _ = fs::write(path, body);
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RoundPhase {
    Guessing,
    Reveal { picked: Option<usize>, won: bool, until_ms: u64 },
    Summary,
}

struct RoundData {
    song_idx: usize,
    choices: [usize; 4],
    correct_slot: usize,
    started_ms: u64,
    deferred_seek_done: bool,
}

fn truncate_to_width(
    ctx: &HostContext,
    text: &str,
    font_size: f32,
    weight: FontWeight,
    max_w: f32,
) -> String {
    if ctx.measure_text(text, font_size, weight) <= max_w {
        return text.to_string();
    }
    let ellipsis = "...";
    let ell_w = ctx.measure_text(ellipsis, font_size, weight);
    let budget = (max_w - ell_w).max(20.0);
    let mut acc = String::new();
    for ch in text.chars() {
        acc.push(ch);
        if ctx.measure_text(&acc, font_size, weight) > budget {
            acc.pop();
            break;
        }
    }
    format!("{acc}{ellipsis}")
}

fn draw_empty_library_notice(ctx: &mut HostContext) {
    loop {
        let theme = ctx.theme();
        let title = ctx.tr("Music Quiz", "Вгадай мелодію");
        ctx.begin_frame(theme.bg, true, title);

        ctx.stroke_rect(64.0, 210.0, 896.0, 340.0, 22.0, 4.0, theme.fg1);
        ctx.draw_text(
            512.0,
            325.0,
            ctx.tr("Need at least 4 local songs", "Потрібно мінімум 4 локальні треки"),
            34.0,
            FontWeight::Bold,
            TextAlign::Center,
            theme.fg,
        );
        ctx.draw_text(
            512.0,
            395.0,
            ctx.tr(
                "Add local music folders on SD card in Settings -> Library.",
                "Додайте локальні теки з музикою на SD-карті в Налаштування -> Бібліотека.",
            ),
            24.0,
            FontWeight::Regular,
            TextAlign::Center,
            theme.fg2,
        );
        ctx.draw_text(
            512.0,
            475.0,
            ctx.tr("[A / B / START] Exit", "[A / B / START] Вихід"),
            24.0,
            FontWeight::SemiBold,
            TextAlign::Center,
            theme.fg,
        );
        ctx.present();

        let inp = ctx.poll_input(25);
        if inp.should_exit
            || inp.menu_requested
            || inp.pressed(btn::A | btn::B | btn::START | btn::MENU)
        {
            break;
        }
    }
}

fn start_next_round(
    ctx: &HostContext,
    pool: &[QuizSong],
    deck: &mut Vec<usize>,
    deck_pos: &mut usize,
    rng: &mut Rng64,
) -> RoundData {
    // Pick the next local track in the shuffled deck whose file exists on disk
    let mut song_idx = 0usize;
    let max_tries = deck.len().max(1);
    for _ in 0..max_tries {
        if *deck_pos >= deck.len() {
            rng.shuffle(deck);
            *deck_pos = 0;
        }
        let cand = deck[*deck_pos];
        *deck_pos += 1;
        song_idx = cand;
        if Path::new(&pool[cand].path).is_file() {
            break;
        }
    }

    let song = &pool[song_idx];
    let start_ms = pick_snippet_start_ms(song.duration_ms, rng);
    let _ = ctx.audio_play_track(&song.path, start_ms);

    let (choices, correct_slot) = build_four_choices(pool, song_idx, rng);
    RoundData {
        song_idx,
        choices,
        correct_slot,
        started_ms: ctx.ticks_ms(),
        deferred_seek_done: song.duration_ms >= 32_000,
    }
}

fn run_pause_menu(
    ctx: &HostContext,
    save: &mut QuizSave,
    round_started_ms: &mut u64,
) -> Option<bool> {
    let pause_at = ctx.ticks_ms();
    let was_playing = ctx.audio_status() == 1;
    if was_playing {
        ctx.audio_toggle_play_pause();
    }

    let mut sel = 0usize;
    let res = loop {
        let timer_lbl = match save.timer {
            TimerSetting::Sec15 => ctx.tr("15s", "15с"),
            TimerSetting::Sec10 => ctx.tr("10s", "10с"),
            TimerSetting::Off => ctx.tr("Off", "Вимк."),
        };
        let len_lbl = match save.length {
            GameLength::Endless => ctx.tr("Endless", "Нескінченно"),
            GameLength::Rounds10 => "10",
            GameLength::Rounds20 => "20",
        };
        let items = [
            MenuItem::action(ctx.tr("Continue", "Продовжити")),
            MenuItem::action(ctx.tr("New Game", "Нова гра")),
            MenuItem::option(ctx.tr("Round Timer", "Таймер раунду"), timer_lbl),
            MenuItem::option(ctx.tr("Game Length", "Кількість раундів"), len_lbl),
            MenuItem::action(ctx.tr("Exit", "Вихід")),
        ];
        match ctx.show_menu(ctx.tr("Music Quiz", "Вгадай мелодію"), &items, sel) {
            MenuOutcome::Selected(0) | MenuOutcome::Cancelled => break Some(false),
            MenuOutcome::Selected(1) => break Some(true),
            MenuOutcome::Selected(2) => {
                sel = 2;
                save.timer = save.timer.next();
                save_state(ctx, *save);
            }
            MenuOutcome::Selected(3) => {
                sel = 3;
                save.length = save.length.next();
                save_state(ctx, *save);
            }
            MenuOutcome::Selected(_) | MenuOutcome::ForceExit => break None,
        }
    };

    let paused_dur = ctx.ticks_ms().saturating_sub(pause_at);
    *round_started_ms = round_started_ms.saturating_add(paused_dur);

    if matches!(res, Some(false)) && was_playing && ctx.audio_status() == 2 {
        ctx.audio_toggle_play_pause();
    }
    res
}

fn run_music_quiz(ctx: &mut HostContext, _file: Option<&Path>) -> i32 {
    // Snapshot pre-existing playback so exiting Music Quiz leaves audio tidy
    let prev_track = ctx.audio_track_info();

    let total_lib = ctx.library_track_count();
    let max_scan = 4096usize;
    let mut raw_tracks = Vec::with_capacity(total_lib.min(max_scan));
    if total_lib <= max_scan {
        for i in 0..total_lib {
            if let Some(t) = ctx.library_track_at(i) {
                if t.path.starts_with('/') {
                    raw_tracks.push(t);
                }
            }
        }
    } else {
        let mut rng_scan = Rng64::new(ctx.ticks_ms());
        let offset = rng_scan.gen_range(total_lib);
        for s in 0..max_scan {
            let idx = (offset + s.saturating_mul(total_lib) / max_scan) % total_lib;
            if let Some(t) = ctx.library_track_at(idx) {
                if t.path.starts_with('/') {
                    raw_tracks.push(t);
                }
            }
        }
    }

    let unknown_artist = ctx.tr("Unknown Artist", "Невідомий виконавець");
    let pool = build_song_pool(raw_tracks, unknown_artist);
    if pool.len() < 4 {
        draw_empty_library_notice(ctx);
        return 0;
    }

    let mut save = load_save(ctx);
    let mut rng = Rng64::new(ctx.ticks_ms());
    let mut deck: Vec<usize> = (0..pool.len()).collect();
    rng.shuffle(&mut deck);
    let mut deck_pos = 0usize;

    let mut score: u32 = 0;
    let mut round_num: u32 = 1;
    let mut cursor: usize = 0;
    let mut phase = RoundPhase::Guessing;
    let mut round = start_next_round(ctx, &pool, &mut deck, &mut deck_pos, &mut rng);

    let mut eq_bars = [0.12f32; 12];
    let mut pcm_buf = [0i16; 256 * 2];

    loop {
        let now = ctx.ticks_ms();

        // If track duration wasn't in DB tags, jump to a mid-song snippet as soon as codec reports length
        if !round.deferred_seek_done && matches!(phase, RoundPhase::Guessing) {
            if let Some(info) = ctx.audio_track_info() {
                if info.path == pool[round.song_idx].path && info.duration_ms >= 32_000 {
                    round.deferred_seek_done = true;
                    let jump_ms = pick_snippet_start_ms(info.duration_ms, &mut rng);
                    ctx.audio_seek_ms(jump_ms);
                    round.started_ms = ctx.ticks_ms();
                }
            }
        }

        // Sample live PCM to animate the mystery-melody mini equalizer
        let got_pcm = ctx.audio_read_pcm(0, &mut pcm_buf);
        if got_pcm >= 24 {
            let chunk = got_pcm / 12;
            for (b, slot) in eq_bars.iter_mut().enumerate() {
                let start = b * chunk;
                let end = ((b + 1) * chunk).min(got_pcm);
                let mut peak = 0.0f32;
                for f in start..end {
                    let l = (pcm_buf[2 * f] as f32).abs() / 32768.0;
                    let r = (pcm_buf[2 * f + 1] as f32).abs() / 32768.0;
                    peak = peak.max(0.5 * (l + r));
                }
                let target = (peak * 2.6).clamp(0.10, 1.0);
                *slot += (target - *slot) * 0.38;
            }
        } else {
            for slot in &mut eq_bars {
                *slot = (*slot * 0.88).max(0.08);
            }
        }

        // Check round timer expiration while guessing
        if matches!(phase, RoundPhase::Guessing) {
            if let Some(limit_ms) = save.timer.duration_ms() {
                if now.saturating_sub(round.started_ms) >= limit_ms {
                    phase = RoundPhase::Reveal {
                        picked: None,
                        won: false,
                        until_ms: now + 1_500,
                    };
                }
            }
        }

        // Auto-advance after reveal pause
        if let RoundPhase::Reveal { until_ms, .. } = phase {
            if now >= until_ms {
                if let Some(max_rounds) = save.length.limit() {
                    if round_num >= max_rounds {
                        phase = RoundPhase::Summary;
                    }
                }
                if !matches!(phase, RoundPhase::Summary) {
                    round_num += 1;
                    cursor = 0;
                    phase = RoundPhase::Guessing;
                    round = start_next_round(ctx, &pool, &mut deck, &mut deck_pos, &mut rng);
                }
            }
        }

        // ---- Render 1024x768 4-Shade GPU Canvas ----
        let theme = ctx.theme();
        let hdr_title = ctx.tr("Music Quiz", "Вгадай мелодію");
        ctx.begin_frame(theme.bg, true, hdr_title);

        if matches!(phase, RoundPhase::Summary) {
            // Final Score Summary Card (for 10 / 20 round games)
            let max_r = save.length.limit().unwrap_or(round_num);
            ctx.fill_rect(96.0, 184.0, 832.0, 460.0, 24.0, theme.bg);
            ctx.stroke_rect(96.0, 184.0, 832.0, 460.0, 24.0, 4.0, theme.fg);

            ctx.draw_text(
                512.0,
                256.0,
                ctx.tr("Quiz Complete!", "Гру завершено!"),
                42.0,
                FontWeight::Bold,
                TextAlign::Center,
                theme.fg,
            );

            let res_str = format!(
                "{}: {} / {}",
                ctx.tr("Score", "Рахунок"),
                score,
                max_r
            );
            ctx.draw_text(
                512.0,
                355.0,
                &res_str,
                52.0,
                FontWeight::Bold,
                TextAlign::Center,
                theme.fg,
            );

            let best_str = format!("{}: {}", ctx.tr("Best Score", "Рекорд"), save.best_score);
            ctx.draw_text(
                512.0,
                435.0,
                &best_str,
                28.0,
                FontWeight::SemiBold,
                TextAlign::Center,
                theme.fg2,
            );

            ctx.fill_rect(262.0, 515.0, 500.0, 74.0, 16.0, theme.fg);
            ctx.draw_text(
                512.0,
                552.0,
                ctx.tr("[A] Play Again    [START] Menu", "[A] Ще раз    [START] Меню"),
                26.0,
                FontWeight::SemiBold,
                TextAlign::Center,
                theme.bg,
            );
        } else {
            // Top HUD Card (Round + Live Mystery Equalizer + Score / Best)
            let hud_x = 32.0;
            let hud_y = CONTENT_TOP_Y;
            let hud_w = (SCREEN_WIDTH - 64) as f32;
            let hud_h = 110.0;
            ctx.stroke_rect(hud_x, hud_y, hud_w, hud_h, 18.0, 3.0, theme.fg1);

            let round_lbl = match save.length.limit() {
                Some(lim) => format!("{} {}/{}", ctx.tr("Round", "Раунд"), round_num, lim),
                None => format!("{} {}", ctx.tr("Round", "Раунд"), round_num),
            };
            ctx.draw_text(
                hud_x + 26.0,
                hud_y + 36.0,
                &round_lbl,
                30.0,
                FontWeight::Bold,
                TextAlign::Left,
                theme.fg,
            );

            let status_txt = match phase {
                RoundPhase::Guessing => {
                    ctx.tr("[X] Another snippet", "[X] Інший уривок")
                }
                RoundPhase::Reveal { won: true, .. } => {
                    ctx.tr("Correct! +1 point", "Правильно! +1 очко")
                }
                RoundPhase::Reveal { picked: None, .. } => {
                    ctx.tr("Time's up!", "Час вийшов!")
                }
                RoundPhase::Reveal { won: false, .. } => {
                    ctx.tr("Wrong answer!", "Неправильно!")
                }
                RoundPhase::Summary => "",
            };
            let status_col = match phase {
                RoundPhase::Guessing => theme.fg2,
                _ => theme.fg,
            };
            ctx.draw_text(
                hud_x + 26.0,
                hud_y + 78.0,
                status_txt,
                22.0,
                if matches!(phase, RoundPhase::Guessing) {
                    FontWeight::Medium
                } else {
                    FontWeight::Bold
                },
                TextAlign::Left,
                status_col,
            );

            // Center 12-bar live audio equalizer pill
            let eq_total_w = 12.0 * 10.0 + 11.0 * 6.0; // 186 px
            let eq_x0 = (SCREEN_WIDTH as f32 - eq_total_w) * 0.5;
            let eq_base_y = hud_y + hud_h - 18.0;
            let eq_max_h = 74.0;
            for (i, &amp) in eq_bars.iter().enumerate() {
                let bx = eq_x0 + (i as f32) * 16.0;
                let bh = (amp * eq_max_h).max(8.0);
                let col = if amp > 0.62 {
                    theme.fg
                } else if amp > 0.30 {
                    theme.fg2
                } else {
                    theme.fg1
                };
                ctx.fill_rect(bx, eq_base_y - bh, 10.0, bh, 4.0, col);
            }

            // Right: Score (+1 per right guess) & Best
            let score_lbl = format!("{}: {}", ctx.tr("Score", "Рахунок"), score);
            ctx.draw_text(
                hud_x + hud_w - 26.0,
                hud_y + 36.0,
                &score_lbl,
                32.0,
                FontWeight::Bold,
                TextAlign::Right,
                theme.fg,
            );
            let best_lbl = format!("{}: {}", ctx.tr("Best", "Рекорд"), save.best_score);
            ctx.draw_text(
                hud_x + hud_w - 26.0,
                hud_y + 78.0,
                &best_lbl,
                22.0,
                FontWeight::Medium,
                TextAlign::Right,
                theme.fg2,
            );

            // Progress / Countdown Bar
            let bar_y = hud_y + hud_h + 14.0; // 260
            let progress = match (phase, save.timer.duration_ms()) {
                (RoundPhase::Guessing, Some(limit_ms)) => {
                    let elapsed = now.saturating_sub(round.started_ms).min(limit_ms);
                    1.0 - (elapsed as f32) / (limit_ms as f32)
                }
                (RoundPhase::Reveal { .. }, Some(_)) => 0.0,
                _ => {
                    // Gentle rhythmic progress wave when timer is Off
                    let cyc = ((now.saturating_sub(round.started_ms) % 10_000) as f32) / 10_000.0;
                    cyc
                }
            };
            ctx.draw_progress_bar(32.0, bar_y, 960.0, 22.0, progress);

            // 4 Choice Cards (`Artist - Track name`)
            let cards_top = bar_y + 36.0; // 296
            let card_h = 96.0;
            let card_gap = 14.0;
            for slot in 0..4usize {
                let cy = cards_top + (slot as f32) * (card_h + card_gap);
                let song_label = &pool[round.choices[slot]].label;

                let (fill_rgb, border_rgb, border_w, text_rgb, badge_bg, badge_fg, right_tag) =
                    match phase {
                        RoundPhase::Guessing => {
                            if slot == cursor {
                                (theme.fg, theme.fg, 0.0, theme.bg, theme.bg, theme.fg, None)
                            } else {
                                (theme.bg, theme.fg1, 3.0, theme.fg, theme.fg1, theme.bg, None)
                            }
                        }
                        RoundPhase::Reveal { picked, won, .. } => {
                            if slot == round.correct_slot {
                                let badge = if won { "+1" } else { "✓" };
                                (
                                    theme.fg,
                                    theme.fg2,
                                    4.0,
                                    theme.bg,
                                    theme.bg,
                                    theme.fg,
                                    Some(badge),
                                )
                            } else if picked == Some(slot) {
                                (
                                    theme.fg1,
                                    theme.fg,
                                    4.0,
                                    theme.fg,
                                    theme.fg,
                                    theme.bg,
                                    Some("✗"),
                                )
                            } else {
                                (theme.bg, theme.fg1, 2.0, theme.fg2, theme.fg1, theme.bg, None)
                            }
                        }
                        RoundPhase::Summary => {
                            (theme.bg, theme.fg1, 2.0, theme.fg, theme.fg1, theme.bg, None)
                        }
                    };

                ctx.fill_rect(32.0, cy, 960.0, card_h, 16.0, fill_rgb);
                if border_w > 0.0 {
                    ctx.stroke_rect(32.0, cy, 960.0, card_h, 16.0, border_w, border_rgb);
                }

                // Numbered pill badge (`1`..`4`)
                let bx = 54.0;
                let by = cy + (card_h - 48.0) * 0.5;
                ctx.fill_rect(bx, by, 48.0, 48.0, 12.0, badge_bg);
                let num_s = format!("{}", slot + 1);
                ctx.draw_text(
                    bx + 24.0,
                    cy + card_h * 0.5,
                    &num_s,
                    26.0,
                    FontWeight::Bold,
                    TextAlign::Center,
                    badge_fg,
                );

                let reserve_right = if right_tag.is_some() { 96.0 } else { 28.0 };
                let text_x = bx + 68.0;
                let max_text_w = 992.0 - text_x - reserve_right;
                let fitted =
                    truncate_to_width(ctx, song_label, 30.0, FontWeight::SemiBold, max_text_w);
                ctx.draw_text(
                    text_x,
                    cy + card_h * 0.5,
                    &fitted,
                    30.0,
                    FontWeight::SemiBold,
                    TextAlign::Left,
                    text_rgb,
                );

                if let Some(tag) = right_tag {
                    ctx.draw_text(
                        964.0,
                        cy + card_h * 0.5,
                        tag,
                        32.0,
                        FontWeight::Bold,
                        TextAlign::Right,
                        text_rgb,
                    );
                }
            }

            let _ = SCREEN_HEIGHT;
        }

        ctx.present();

        // ---- Poll Input (~60 FPS) ----
        let inp = ctx.poll_input(16);
        if inp.should_exit {
            break;
        }

        if inp.menu_requested {
            match run_pause_menu(ctx, &mut save, &mut round.started_ms) {
                Some(false) => continue,
                Some(true) => {
                    score = 0;
                    round_num = 1;
                    cursor = 0;
                    rng.shuffle(&mut deck);
                    deck_pos = 0;
                    phase = RoundPhase::Guessing;
                    round = start_next_round(ctx, &pool, &mut deck, &mut deck_pos, &mut rng);
                    continue;
                }
                None => break,
            }
        }

        match phase {
            RoundPhase::Guessing => {
                if inp.pressed(btn::UP) {
                    cursor = (cursor + 3) % 4;
                } else if inp.pressed(btn::DOWN) {
                    cursor = (cursor + 1) % 4;
                } else if inp.pressed(btn::X) {
                    // Jump to another random snippet of the current mystery song
                    let dur = ctx
                        .audio_track_info()
                        .map(|t| t.duration_ms.max(pool[round.song_idx].duration_ms))
                        .unwrap_or(pool[round.song_idx].duration_ms);
                    let next_pos = pick_snippet_start_ms(dur, &mut rng);
                    ctx.audio_seek_ms(next_pos);
                } else if inp.pressed(btn::A) {
                    let won = cursor == round.correct_slot;
                    if won {
                        score += 1;
                        if score > save.best_score {
                            save.best_score = score;
                            save_state(ctx, save);
                        }
                    }
                    phase = RoundPhase::Reveal {
                        picked: Some(cursor),
                        won,
                        until_ms: ctx.ticks_ms() + 1_350,
                    };
                }
            }
            RoundPhase::Reveal { .. } => {
                if inp.pressed(btn::A) {
                    if let Some(max_rounds) = save.length.limit() {
                        if round_num >= max_rounds {
                            phase = RoundPhase::Summary;
                            continue;
                        }
                    }
                    round_num += 1;
                    cursor = 0;
                    phase = RoundPhase::Guessing;
                    round = start_next_round(ctx, &pool, &mut deck, &mut deck_pos, &mut rng);
                }
            }
            RoundPhase::Summary => {
                if inp.pressed(btn::A) {
                    score = 0;
                    round_num = 1;
                    cursor = 0;
                    rng.shuffle(&mut deck);
                    deck_pos = 0;
                    phase = RoundPhase::Guessing;
                    round = start_next_round(ctx, &pool, &mut deck, &mut deck_pos, &mut rng);
                } else if inp.pressed(btn::B) {
                    break;
                }
            }
        }
    }

    save_state(ctx, save);

    // Restore pre-quiz track state (or stop quiz snippet if nothing was playing before)
    if let Some(prev) = prev_track {
        if !prev.path.is_empty() {
            let _ = ctx.audio_play_track(&prev.path, prev.position_ms);
            if prev.status != 1 && ctx.audio_status() == 1 {
                ctx.audio_toggle_play_pause();
            }
        } else {
            ctx.audio_stop();
        }
    } else {
        ctx.audio_stop();
    }

    0
}

uipod_declare_plugin! {
    id: "music_quiz",
    name: "Music Quiz",
    name_uk: "Вгадай мелодію",
    author: "UIPod",
    version: "0.1.0",
    category: PluginCategory::Game,
    kind: PluginKind::MenuDriven,
    extensions: "",
    entry: run_music_quiz,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_artist_track_and_strips_track_prefix_from_untagged_files() {
        let lbl1 = format_artist_track("Daft Punk", "Veridis Quo", "/music/03.mp3", "Unknown");
        assert_eq!(lbl1, "Daft Punk - Veridis Quo");

        let lbl2 = format_artist_track("", "", "/music/04 - Midnight City.flac", "Unknown");
        assert_eq!(lbl2, "Unknown - Midnight City");
    }

    #[test]
    fn four_choices_are_distinct_and_include_exact_target() {
        let raw = vec![
            LibraryTrack {
                path: "/m/1.mp3".into(),
                artist: "Artist A".into(),
                title: "Song 1".into(),
                duration_ms: 180_000,
            },
            LibraryTrack {
                path: "/m/2.mp3".into(),
                artist: "Artist B".into(),
                title: "Song 2".into(),
                duration_ms: 200_000,
            },
            LibraryTrack {
                path: "/m/3.mp3".into(),
                artist: "Artist C".into(),
                title: "Song 3".into(),
                duration_ms: 210_000,
            },
            LibraryTrack {
                path: "/m/4.mp3".into(),
                artist: "Artist D".into(),
                title: "Song 4".into(),
                duration_ms: 240_000,
            },
            // Duplicate Artist - Title at different path should be deduplicated by build_song_pool
            LibraryTrack {
                path: "/m/4_remaster.flac".into(),
                artist: "artist d".into(),
                title: "SONG 4".into(),
                duration_ms: 240_000,
            },
        ];

        let pool = build_song_pool(raw, "Unknown");
        assert_eq!(pool.len(), 4);

        let mut rng = Rng64::new(42);
        for target in 0..pool.len() {
            let (choices, correct_slot) = build_four_choices(&pool, target, &mut rng);
            assert_eq!(choices[correct_slot], target);
            let mut uniq = choices.to_vec();
            uniq.sort_unstable();
            uniq.dedup();
            assert_eq!(uniq.len(), 4);
        }
    }

    #[test]
    fn snippet_offset_lands_mid_track_away_from_intro_and_outro() {
        let mut rng = Rng64::new(12345);
        for _ in 0..50 {
            let pos = pick_snippet_start_ms(200_000, &mut rng);
            assert!((40_000..=130_000).contains(&pos), "unexpected snippet offset: {pos}");
        }
        assert_eq!(pick_snippet_start_ms(20_000, &mut rng), 0);
    }
}
