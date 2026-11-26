mod app;
mod library;
mod plugins;
mod themes;

use std::collections::HashMap;
use uipod_codecs::gsf::{load_gsf_image_with, GsfDecoder};
use uipod_codecs::midi::{
    clear_custom_soundfont, set_custom_soundfont_bytes, MidiDecoder, ParsedMidi,
};
use uipod_codecs::nsf_bridge::format_nsf_subtrack_title;
use uipod_codecs::psf::{infer_track_and_title_from_path, PsfFile, PSF_VERSION_GSF};
use uipod_codecs::{
    get_nsf_active_chip_flags, get_nsf_apu_telemetry, get_nsf_channel_mute_mask,
    get_nsf_endless_loop, reset_nsf_runtime_session, set_nsf_channel_mute_mask,
    set_nsf_endless_loop, set_nsf_tempo_ratio, NsfDecoder, RustAudioDecoder,
};
use uipod_dj::{
    build_in_memory_deck_track, CrossfadeMode, DjConfig, DjTelemetry, DualDeckMixer,
    MaxTempoStretch, MixerPhase, TransitionLength,
};
use uipod_nsf::{scan_nsf_container, SubtrackUri, TrackKind};
use uipod_plugin_sdk::{DjDeckId, PluginApuTelemetry, PluginChannelState};
use uipod_track_intel::{
    analyze_mono_window, analyze_nsf_bytes, analyze_pcm_s16_buffer, audio_tap_push_apu,
    audio_tap_push_s16, compute_contour_256_from_mono, compute_track_key_hash, decode_record_bytes,
    encode_record_bytes, normalize_track_id, publish_active_analysis, publish_live_frame,
    read_active_analysis, write_fixed_utf8, AnalyzeRequest, LiveTelemetryEngine,
    TrackAnalysisRecord, TrackIntelDb, TrackKeyInput, SUBTRACK_NONE,
};
use uipod_ui::{ThemeColors, UiEngine, VizEngine, VIZ_PRESETS};
use wasm_bindgen::prelude::*;
#[cfg(target_arch = "wasm32")]
use wasm_bindgen::JsCast;

use crate::app::{UIPodApp, AUTOEQ_PRESETS};
use crate::themes::{derive_4shade, THEME_PRESETS};

fn json_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 8);
    for ch in s.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => {}
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => {}
            c => out.push(c),
        }
    }
    out
}

#[wasm_bindgen]
pub struct UIPodWasm {
    ui: UiEngine,
    viz: VizEngine,
    app: UIPodApp,
    gsf_libs: HashMap<String, Vec<u8>>,
    nsf_containers: HashMap<String, Vec<u8>>,
    active_nsf: Option<NsfDecoder>,
    dj_mixer: DualDeckMixer,
    dj_out_buf: Vec<i16>,
    manual_dj_out_buf: Vec<i16>,
    dj_midpoint_crossed: bool,
    intel_db: TrackIntelDb,
    intel_live: LiveTelemetryEngine,
    track_intel_keys: HashMap<usize, u64>,
    active_intel_key: u64,
}

#[wasm_bindgen]
impl UIPodWasm {
    #[cfg(target_arch = "wasm32")]
    #[wasm_bindgen(constructor)]
    pub fn new(canvas: web_sys::HtmlCanvasElement) -> Result<UIPodWasm, JsValue> {
        let ui = UiEngine::new_from_html_canvas(&canvas)
            .map_err(|e| JsValue::from_str(&format!("UiEngine init error: {e}")))?;

        let attrs = web_sys::WebGlContextAttributes::new();
        attrs.set_stencil(true);
        attrs.set_antialias(false);
        let gl_ctx = canvas
            .get_context_with_context_options("webgl2", &attrs)
            .map_err(|e| JsValue::from_str(&format!("WebGL2 context error: {e:?}")))?
            .ok_or_else(|| JsValue::from_str("WebGL2 context unavailable"))?
            .dyn_into::<web_sys::WebGl2RenderingContext>()
            .map_err(|_| JsValue::from_str("Not a WebGL2RenderingContext"))?;

        let viz = unsafe { VizEngine::new_from_webgl2(gl_ctx) }
            .map_err(|e| JsValue::from_str(&format!("VizEngine init error: {e}")))?;

        Ok(Self {
            ui,
            viz,
            app: UIPodApp::new(),
            gsf_libs: HashMap::new(),
            nsf_containers: HashMap::new(),
            active_nsf: None,
            dj_mixer: DualDeckMixer::new(48_000),
            dj_out_buf: vec![0i16; 8192 * 2],
            manual_dj_out_buf: vec![0i16; 8192 * 2],
            dj_midpoint_crossed: false,
            intel_db: TrackIntelDb::new_in_memory(),
            intel_live: LiveTelemetryEngine::new(),
            track_intel_keys: HashMap::new(),
            active_intel_key: 0,
        })
    }

    /// Advances the UIPod OS, plugins, transitions, and WebGL2 renderer by one frame.
    pub fn render(
        &mut self,
        dt_sec: f32,
        pcm_interleaved: &[i16],
        sample_rate: u32,
        win_w: u32,
        win_h: u32,
    ) {
        let now_ms = self.app.clock_ms;
        if self.app.audio_playing && !pcm_interleaved.is_empty() {
            audio_tap_push_s16(pcm_interleaved, sample_rate, 18_000, now_ms);
            if get_nsf_active_chip_flags().is_some() {
                audio_tap_push_apu(&get_nsf_apu_telemetry());
            }
        }
        let status = if self.app.audio_playing {
            1u8
        } else if self.app.has_resumable {
            2u8
        } else {
            0u8
        };
        let apu_opt = if status == 1 && get_nsf_active_chip_flags().is_some() {
            let raw = get_nsf_apu_telemetry();
            let mut chs = [PluginChannelState::default(); 8];
            for (i, c) in raw.channels.iter().enumerate() {
                chs[i] = PluginChannelState {
                    active: c.active,
                    muted: c.muted,
                    onset_trigger: c.onset_trigger,
                    duty_or_mode: c.duty_or_mode,
                    volume: c.volume,
                    freq_hz: c.freq_hz,
                };
            }
            Some(PluginApuTelemetry {
                has_apu_telemetry: 1,
                channel_count: raw.channel_count,
                chip_flags: raw.chip_flags,
                reserved: 0,
                mute_mask: raw.mute_mask,
                pad: 0,
                channels: chs,
            })
        } else {
            None
        };
        let active_rec = read_active_analysis();
        let live_frame = self.intel_live.step(
            status,
            self.active_intel_key,
            self.app.pos_ms,
            self.app.dur_ms,
            18.0,
            dt_sec,
            pcm_interleaved,
            sample_rate,
            apu_opt,
            active_rec.as_ref(),
        );
        publish_live_frame(&live_frame);

        self.app.tick_and_render(
            dt_sec,
            pcm_interleaved,
            sample_rate,
            win_w,
            win_h,
            &mut self.ui,
            &mut self.viz,
        );
    }

    /// Hot-loads `NotoSansCJK-Regular.ttf` fallback bytes behind `Geologica` without blocking boot.
    pub fn load_cjk_font(&mut self, bytes: &[u8]) -> bool {
        self.ui.add_cjk_fallback_bytes(bytes).is_ok()
    }

    /// Hot-loads `NotoSerifCJK-Regular.ttf` fallback bytes behind `Noto Serif` without blocking boot.
    pub fn load_serif_cjk_font(&mut self, bytes: &[u8]) -> bool {
        self.ui.add_serif_cjk_fallback_bytes(bytes).is_ok()
    }

    /// Registers an `.epub`, `.fb2`, `.fb2.zip`, `.mobi`, `.md`, or `.txt` file into the Web Simulator's
    /// virtual SD card (`Main -> Browse`) and optionally opens it immediately in the E-Book Reader.
    pub fn register_ebook_file(&mut self, file_name: &str, bytes: &[u8], open_now: bool) -> usize {
        self.app
            .register_ebook_file(file_name, bytes, open_now, &mut self.ui)
    }

    /* ---- Hardware Inputs (TrimUI Brick) ---- */

    pub fn button_down(&mut self, mask: u32) {
        self.app.button_down(mask);
    }

    pub fn button_up(&mut self, mask: u32) {
        self.app.button_up(mask);
    }

    pub fn button_long_press(&mut self, mask: u32) {
        self.app.button_long_press(mask, &mut self.ui);
    }

    pub fn volume_step(&mut self, delta: i32) {
        self.app.adjust_volume(delta, &mut self.ui);
    }

    pub fn set_hold(&mut self, held: bool) {
        self.app.set_hold(held);
    }

    /* ---- Web Audio Engine Synchronization ---- */

    pub fn sync_audio_state(&mut self, playing: bool, pos_ms: u32, dur_ms: u32) {
        self.app.sync_audio_state(playing, pos_ms, dur_ms);
    }

    /// Drains pending audio/system commands as a JSON array string (`[]` when empty).
    pub fn take_commands(&mut self) -> String {
        if self.app.outbound_cmds.is_empty() {
            return "[]".to_string();
        }
        let mut out = String::from("[");
        for (i, cmd) in self.app.outbound_cmds.drain(..).enumerate() {
            if i > 0 {
                out.push(',');
            }
            out.push_str(&cmd);
        }
        out.push(']');
        out
    }

    pub fn get_state_json(&self) -> String {
        self.app.state_json(&self.viz)
    }

    /* ---- Studio Bridge APIs (Theme Studio, Visualizer Lab, DSP & Uploads) ---- */

    pub fn jump_to_screen(&mut self, screen_id: &str) {
        self.app
            .jump_to_screen(screen_id, &mut self.ui, &mut self.viz);
    }

    pub fn set_theme_preset(&mut self, idx: usize) {
        if let Some(preset) = THEME_PRESETS.get(idx) {
            self.app.theme = preset.colors;
            self.app.theme_idx = Some(idx);
            self.app.state_dirty = true;
        }
    }

    pub fn set_custom_theme(&mut self, bg: u32, fg: u32, fg1: u32, fg2: u32, auto_derive: bool) {
        self.app.theme = if auto_derive {
            derive_4shade(bg & 0xFFFFFF, fg & 0xFFFFFF)
        } else {
            ThemeColors {
                bg_rgb: bg & 0xFFFFFF,
                fg_rgb: fg & 0xFFFFFF,
                fg1_rgb: fg1 & 0xFFFFFF,
                fg2_rgb: fg2 & 0xFFFFFF,
            }
        };
        self.app.theme_idx = None;
        self.app.state_dirty = true;
    }

    pub fn set_language(&mut self, uk: bool) {
        self.app.uk = uk;
        self.app.state_dirty = true;
    }

    pub fn set_mono_art(&mut self, mono: bool) {
        self.app.mono_album_art = mono;
        self.app.state_dirty = true;
    }

    pub fn set_alt_layout(&mut self, alt: bool) {
        self.app.alt_now_playing = alt;
        self.app.state_dirty = true;
    }

    pub fn set_transition(&mut self, kind: u8) {
        self.app.transition_kind = kind % 6;
        self.app.state_dirty = true;
    }

    pub fn set_viz_preset(&mut self, idx: usize) {
        let clamped = idx % VIZ_PRESETS.len();
        self.app.viz_preset = clamped;
        self.app.viz_custom = false;
        self.viz.set_preset(clamped);
        self.app.state_dirty = true;
    }

    pub fn set_viz_fine_grain(&mut self, _fine: bool) {
        // Locked to crisp default render resolution; kept as no-op for ABI compatibility.
        self.viz.set_fine_grain(true);
    }

    pub fn set_viz_invert(&mut self, invert: bool) {
        self.app.viz_invert = invert;
        self.viz.set_invert(invert);
        self.app.state_dirty = true;
    }

    pub fn compile_custom_viz_shader(&mut self, glsl_body: &str) -> Result<(), JsValue> {
        unsafe { self.viz.compile_custom_shader(glsl_body) }
            .map_err(|e| JsValue::from_str(&e))?;
        self.app.viz_custom = true;
        self.app.state_dirty = true;
        Ok(())
    }

    pub fn set_dsp_config(
        &mut self,
        eq_enabled: bool,
        autoeq_idx: usize,
        tape_enabled: bool,
        wow: u8,
        hiss: u8,
        sat: u8,
        hf: u8,
        stop_start: bool,
        speed_pct: u16,
        menu_click: bool,
    ) {
        self.app.dsp.eq_enabled = eq_enabled;
        if self.app.dsp.autoeq_idx != autoeq_idx % AUTOEQ_PRESETS.len() {
            self.app.apply_autoeq_preset(autoeq_idx);
        }
        self.app.dsp.tape_enabled = tape_enabled;
        self.app.dsp.tape_wow = wow.min(100);
        self.app.dsp.tape_hiss = hiss.min(100);
        self.app.dsp.tape_sat = sat.min(100);
        self.app.dsp.tape_hf_rolloff = hf.min(100);
        self.app.dsp.tape_stop_start = stop_start;
        self.app.dsp.speed_pct = speed_pct.clamp(50, 150);
        self.app.dsp.menu_click = menu_click;
        self.app.state_dirty = true;
    }

    pub fn add_user_track(
        &mut self,
        title: String,
        artist: String,
        album: String,
        codec: String,
        bitdepth: String,
        samplerate: String,
        duration_ms: u32,
        art_bytes: Option<Vec<u8>>,
    ) -> usize {
        self.app.add_user_track(
            title,
            artist,
            album,
            codec,
            bitdepth,
            samplerate,
            duration_ms,
            art_bytes.as_deref(),
        )
    }

    pub fn select_and_play_track(&mut self, idx: usize) {
        if idx < self.app.tracks.len() {
            self.app.current_track = idx;
            self.app.pos_ms = 0;
            self.app.dur_ms = self.app.tracks[idx].duration_ms;
            self.app.audio_playing = true;
            self.app.has_resumable = true;
            self.app.play_mode = uipod_ui::PlayMode::Play;
            self.app.state_dirty = true;
        }
    }

    /// Store a `.gsflib` sample/ROM library in memory (matched case-insensitively by basename).
    pub fn register_gsflib(&mut self, filename: &str, bytes: &[u8]) {
        let base = filename
            .rsplit(['/', '\\'])
            .next()
            .unwrap_or(filename)
            .trim()
            .to_ascii_lowercase();
        if !base.is_empty() {
            self.gsf_libs.insert(base, bytes.to_vec());
        }
    }

    /// Parse `.gsf` / `.minigsf` `[TAG]` metadata (merging fallback tags from registered `.gsflib`).
    pub fn inspect_gsf_json(&self, filename: &str, bytes: &[u8]) -> String {
        let Ok(mut tags) = PsfFile::parse_tags_from_slice(bytes, Some(PSF_VERSION_GSF)) else {
            return r#"{"ok":false}"#.to_string();
        };
        if let Some(ref lib_rel) = tags.lib {
            let lib_key = lib_rel
                .rsplit(['/', '\\'])
                .next()
                .unwrap_or(lib_rel)
                .trim()
                .to_ascii_lowercase();
            if let Some(lib_bytes) = self.gsf_libs.get(&lib_key) {
                if let Ok(parent_tags) =
                    PsfFile::parse_tags_from_slice(lib_bytes, Some(PSF_VERSION_GSF))
                {
                    tags.merge_fallback_from(&parent_tags);
                }
            }
        }
        let (inf_track, inf_title) = infer_track_and_title_from_path(filename);
        let track_num = tags.track_num.or(inf_track).unwrap_or(0);
        let title = tags
            .title
            .clone()
            .or(inf_title)
            .unwrap_or_else(|| "GSF Track".to_string());
        let artist = tags
            .artist
            .clone()
            .unwrap_or_else(|| "Unknown Artist".to_string());
        let album = tags
            .game
            .clone()
            .unwrap_or_else(|| "Game Boy Advance".to_string());
        let dur_ms = tags.total_duration_ms();

        format!(
            r#"{{"ok":true,"title":"{}","artist":"{}","album":"{}","trackNum":{},"durationMs":{}}}"#,
            json_escape(&title),
            json_escape(&artist),
            json_escape(&album),
            track_num,
            dur_ms
        )
    }

    /// Synthesize a `.gsf` / `.minigsf` track into 44,100 Hz interleaved stereo S16 samples.
    pub fn decode_gsf_s16(&self, filename: &str, bytes: &[u8]) -> Vec<i16> {
        let libs = &self.gsf_libs;
        let Ok(img) = load_gsf_image_with(bytes, Some(filename), &mut |lib_rel| {
            let key = lib_rel
                .rsplit(['/', '\\'])
                .next()
                .unwrap_or(lib_rel)
                .trim()
                .to_ascii_lowercase();
            let data = libs.get(&key)?.clone();
            Some((format!("wasm:{key}:{}", data.len()), data))
        }) else {
            return Vec::new();
        };

        let mut dec = GsfDecoder::from_image(img);
        let total_frames = ((dec.duration_ms() as usize) * 44_100) / 1000;
        let mut out = Vec::with_capacity(total_frames * 2);
        let mut chunk = [0i16; 4096 * 2];
        loop {
            let n = dec.render_stereo_s16(&mut chunk, false);
            if n == 0 {
                break;
            }
            out.extend_from_slice(&chunk[..n * 2]);
        }
        out
    }

    /// Activate a custom `.sf2` SoundFont in memory (returns `true` if valid).
    pub fn register_soundfont(&mut self, filename: &str, bytes: &[u8]) -> bool {
        set_custom_soundfont_bytes(filename, bytes).is_ok()
    }

    /// Restore bundled default `Mobile_0100_base.sf2` SoundFont.
    pub fn reset_soundfont(&mut self) {
        clear_custom_soundfont();
    }

    /// Parse `.mid` / `.midi` / `.kar` / `.rmi` metadata and exact duration.
    pub fn inspect_midi_json(&self, filename: &str, bytes: &[u8]) -> String {
        let Ok(parsed) = ParsedMidi::parse(bytes) else {
            return r#"{"ok":false}"#.to_string();
        };
        let (inf_track, inf_title) = infer_track_and_title_from_path(filename);
        let track_num = inf_track.unwrap_or(0);
        let title = parsed
            .meta
            .title
            .or(inf_title)
            .unwrap_or_else(|| "MIDI Track".to_string());
        let artist = parsed
            .meta
            .artist
            .unwrap_or_else(|| "MIDI Synthesizer".to_string());
        format!(
            r#"{{"ok":true,"title":"{}","artist":"{}","album":"SoundFont MIDI","trackNum":{},"durationMs":{}}}"#,
            json_escape(&title),
            json_escape(&artist),
            track_num,
            parsed.meta.duration_ms
        )
    }

    /// Synthesize a `.mid` / `.midi` / `.kar` / `.rmi` track using the active `.sf2` SoundFont.
    pub fn decode_midi_s16(&self, bytes: &[u8]) -> Vec<i16> {
        let Ok(parsed) = ParsedMidi::parse(bytes) else {
            return Vec::new();
        };
        let mut dec = MidiDecoder::new(parsed);
        let total_frames = ((dec.duration_ms() as usize) * 44_100) / 1000;
        let mut out = Vec::with_capacity(total_frames * 2);
        let mut chunk = [0i16; 4096 * 2];
        loop {
            let n = dec.render_stereo_s16(&mut chunk, false);
            if n == 0 {
                break;
            }
            out.extend_from_slice(&chunk[..n * 2]);
        }
        out
    }

    /* ---- Native Rust NSF/NSFe Streaming Decoder, Scanner & Runtime Controls ---- */

    /// Store an `.nsf` / `.nsfe` container in memory and scan all subtracks (`Music` vs `Sfx`).
    pub fn register_nsf_container(&mut self, filename: &str, bytes: &[u8]) -> String {
        let Ok(meta) = scan_nsf_container(bytes) else {
            return r#"{"ok":false}"#.to_string();
        };
        let norm_full = filename.trim().replace('\\', "/").to_ascii_lowercase();
        let base_only = filename
            .rsplit(['/', '\\'])
            .next()
            .unwrap_or(filename)
            .trim()
            .to_ascii_lowercase();
        if !norm_full.is_empty() {
            self.nsf_containers.insert(norm_full, bytes.to_vec());
        }
        if !base_only.is_empty() {
            self.nsf_containers.insert(base_only.clone(), bytes.to_vec());
        }

        let game = if !meta.game_title.trim().is_empty() {
            meta.game_title.trim().to_string()
        } else {
            filename
                .rsplit(['/', '\\'])
                .next()
                .unwrap_or(filename)
                .rsplit_once('.')
                .map(|(s, _)| s)
                .unwrap_or("NSF Cartridge")
                .to_string()
        };
        let artist = if !meta.artist.trim().is_empty() {
            meta.artist.trim().to_string()
        } else {
            "Famicom / NES".to_string()
        };

        let mut subs_json = String::from("[");
        for (i, s) in meta.subtracks.iter().enumerate() {
            if i > 0 {
                subs_json.push(',');
            }
            let title = format_nsf_subtrack_title(&meta, s.index, filename);
            let kind_str = match s.kind {
                TrackKind::Music => "music",
                TrackKind::Sfx => "sfx",
            };
            let total_ms = s.duration_ms.saturating_add(s.fade_ms);
            let loop_str = s
                .loop_point_ms
                .map(|lp| lp.to_string())
                .unwrap_or_else(|| "null".to_string());
            subs_json.push_str(&format!(
                r#"{{"index":{},"title":"{}","kind":"{}","durationMs":{},"fadeMs":{},"loopPointMs":{}}}"#,
                s.index,
                json_escape(&title),
                kind_str,
                total_ms,
                s.fade_ms,
                loop_str
            ));
        }
        subs_json.push(']');

        format!(
            r#"{{"ok":true,"gameTitle":"{}","artist":"{}","copyright":"{}","chipFlags":{},"badge":"{}","totalSubtracks":{},"startingSubtrack":{},"subtracks":{}}}"#,
            json_escape(&game),
            json_escape(&artist),
            json_escape(meta.copyright.trim()),
            meta.chip_flags.0,
            meta.chip_flags.primary_badge_label(),
            meta.total_subtracks,
            meta.starting_subtrack,
            subs_json
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub fn add_nsf_subtrack(
        &mut self,
        uri: String,
        subtrack_index: u16,
        is_sfx: bool,
        chip_flags: u8,
        loop_point_ms: i32,
        title: String,
        artist: String,
        album: String,
        duration_ms: u32,
        art_bytes: Option<Vec<u8>>,
    ) -> usize {
        self.app.add_nsf_subtrack(
            uri,
            subtrack_index,
            is_sfx,
            chip_flags,
            (loop_point_ms >= 0).then_some(loop_point_ms as u32),
            title,
            artist,
            album,
            duration_ms,
            art_bytes.as_deref(),
        )
    }

    /// Open a canonical `Game.nsf#idx` (or bare `Game.nsf`) stream at `sample_rate` Hz.
    /// Automatically enforces per-container channel mute mask hygiene.
    /// Returns duration in milliseconds (`0` on error).
    pub fn open_nsf_stream(&mut self, uri: &str, sample_rate: u32) -> u32 {
        self.active_nsf = None;
        let parsed_uri = SubtrackUri::parse(uri);
        let norm_full = parsed_uri
            .file_path
            .trim()
            .replace('\\', "/")
            .to_ascii_lowercase();
        let base_only = parsed_uri
            .file_path
            .rsplit(['/', '\\'])
            .next()
            .unwrap_or(parsed_uri.file_path)
            .trim()
            .to_ascii_lowercase();

        let Some(bytes) = self
            .nsf_containers
            .get(&norm_full)
            .or_else(|| self.nsf_containers.get(&base_only))
            .cloned()
        else {
            return 0;
        };

        let Ok(dec) = NsfDecoder::open_with_rate(uri, &bytes, sample_rate.clamp(8_000, 192_000))
        else {
            return 0;
        };
        let dur = dec.duration_ms();
        self.active_nsf = Some(dec);
        dur
    }

    /// Render up to `frames` interleaved stereo S16 samples from the active NSF stream.
    pub fn render_nsf_stream_s16(&mut self, frames: usize) -> Vec<i16> {
        let Some(dec) = self.active_nsf.as_mut() else {
            return Vec::new();
        };
        let n_frames = frames.min(16_384);
        let mut out = vec![0i16; n_frames * 2];
        let rendered = dec.render_stereo_s16(&mut out, false);
        out.truncate(rendered * 2);
        out
    }

    pub fn seek_nsf_stream_ms(&mut self, target_ms: u32) {
        if let Some(dec) = self.active_nsf.as_mut() {
            dec.seek_ms(target_ms);
        }
    }

    pub fn nsf_stream_elapsed_ms(&self) -> u32 {
        self.active_nsf
            .as_ref()
            .map(|d| d.elapsed_ms())
            .unwrap_or(0)
    }

    /// Close active NSF decoder (keeping container session so stepping to the next subtrack
    /// of the same `.nsf` preserves channel mute toggles).
    pub fn close_nsf_stream(&mut self) {
        self.active_nsf = None;
    }

    /// Full NSF session reset (called when switching to a non-NSF track or stopping playback).
    pub fn reset_nsf_session(&mut self) {
        self.active_nsf = None;
        reset_nsf_runtime_session();
    }

    pub fn get_nsf_channel_mute_mask(&self) -> u16 {
        get_nsf_channel_mute_mask()
    }

    pub fn set_nsf_channel_mute_mask(&self, mask: u16) {
        set_nsf_channel_mute_mask(mask);
    }

    pub fn get_nsf_active_chip_flags(&self) -> i32 {
        get_nsf_active_chip_flags()
            .map(|f| f.0 as i32)
            .unwrap_or(-1)
    }

    pub fn get_nsf_channel_descriptors_json(&self) -> String {
        let Some(flags) = get_nsf_active_chip_flags() else {
            return "[]".to_string();
        };
        let descs = flags.channel_descriptors();
        let mut out = String::from("[");
        for (i, d) in descs.iter().enumerate() {
            if i > 0 {
                out.push(',');
            }
            out.push_str(&format!(
                r#"{{"bitIndex":{},"shortCode":"{}","displayName":"{}"}}"#,
                d.bit_index,
                json_escape(d.short_code),
                json_escape(d.display_name)
            ));
        }
        out.push(']');
        out
    }

    pub fn get_nsf_endless_loop(&self) -> bool {
        get_nsf_endless_loop()
    }

    pub fn set_nsf_endless_loop(&self, enabled: bool) {
        set_nsf_endless_loop(enabled);
    }

    pub fn get_nsf_telemetry_json(&self) -> String {
        if get_nsf_active_chip_flags().is_none() {
            return r#"{"active":false}"#.to_string();
        }
        let tel = get_nsf_apu_telemetry();
        let n = (tel.channel_count as usize).min(8);
        let mut ch_json = String::from("[");
        for i in 0..n {
            if i > 0 {
                ch_json.push(',');
            }
            let c = &tel.channels[i];
            ch_json.push_str(&format!(
                r#"{{"active":{},"muted":{},"onset":{},"duty":{},"volume":{:.4},"freqHz":{:.2}}}"#,
                c.active != 0,
                c.muted != 0,
                c.onset_trigger != 0,
                c.duty_or_mode,
                c.volume,
                c.freq_hz
            ));
        }
        ch_json.push(']');
        format!(
            r#"{{"active":true,"channelCount":{},"chipFlags":{},"muteMask":{},"channels":{}}}"#,
            tel.channel_count, tel.chip_flags, tel.mute_mask, ch_json
        )
    }

    /* ---- Dual-Deck Smart DJ Crossfade Engine (`uipod-dj`) ---- */

    pub fn set_dj_config(
        &mut self,
        mode: u8,
        length: u8,
        max_stretch: u8,
        keep_gapless: bool,
        quick_cut: bool,
    ) {
        self.app.dsp.dj_mode = mode.min(3);
        self.app.dsp.dj_length = length.min(2);
        self.app.dsp.dj_max_stretch = max_stretch.min(2);
        self.app.dsp.dj_keep_gapless = keep_gapless;
        self.app.dsp.dj_quick_cut = quick_cut;
        self.dj_mixer.set_config(DjConfig {
            mode: CrossfadeMode::from_u8(self.app.dsp.dj_mode),
            length: TransitionLength::from_u8(self.app.dsp.dj_length),
            max_stretch: MaxTempoStretch::from_u8(self.app.dsp.dj_max_stretch),
            keep_album_gapless: keep_gapless,
            quick_cut_on_next: quick_cut,
        });
        if self.app.dsp.dj_mode == 0 {
            self.dj_reset();
        }
        self.app.state_dirty = true;
    }

    #[allow(clippy::too_many_arguments)]
    pub fn dj_begin_transition(
        &mut self,
        sample_rate: u32,
        track_a_idx: usize,
        tail_a_s16: &[i16],
        track_b_idx: usize,
        intro_b_s16: &[i16],
        is_b_eof: bool,
        deck_b_is_native_nsf: bool,
        manual_quick_cut: bool,
    ) -> bool {
        let d = self.app.dsp;
        self.dj_mixer.set_config(DjConfig {
            mode: CrossfadeMode::from_u8(d.dj_mode),
            length: TransitionLength::from_u8(d.dj_length),
            max_stretch: MaxTempoStretch::from_u8(d.dj_max_stretch),
            keep_album_gapless: d.dj_keep_gapless,
            quick_cut_on_next: d.dj_quick_cut,
        });
        if d.dj_mode == 0 || tail_a_s16.len() < 4 || intro_b_s16.len() < 4 {
            return false;
        }

        let (album_a, num_a) = self
            .app
            .tracks
            .get(track_a_idx)
            .map(|t| {
                let n = t
                    .subtrack_index
                    .map(|s| s as i32 + 1)
                    .unwrap_or((track_a_idx + 1) as i32);
                (t.album.clone(), n)
            })
            .unwrap_or_default();
        let (album_b, num_b) = self
            .app
            .tracks
            .get(track_b_idx)
            .map(|t| {
                let n = t
                    .subtrack_index
                    .map(|s| s as i32 + 1)
                    .unwrap_or((track_b_idx + 1) as i32);
                (t.album.clone(), n)
            })
            .unwrap_or_default();

        self.dj_midpoint_crossed = false;
        let hint_a = self
            .track_intel_keys
            .get(&track_a_idx)
            .and_then(|&k| self.intel_db.get(k))
            .map(|r| (r.bpm, r.bpm_confidence, Some(r.first_beat_offset_ms)));
        let hint_b = self
            .track_intel_keys
            .get(&track_b_idx)
            .and_then(|&k| self.intel_db.get(k))
            .map(|r| (r.bpm, r.bpm_confidence, Some(r.first_beat_offset_ms)));
        let dur_a_ms = self
            .app
            .tracks
            .get(track_a_idx)
            .map(|t| t.duration_ms)
            .unwrap_or(0);
        let dur_b_ms = self
            .app
            .tracks
            .get(track_b_idx)
            .map(|t| t.duration_ms)
            .unwrap_or(0);
        let start_a_ms = dur_a_ms.saturating_sub(
            (((tail_a_s16.len() / 2) as u64 * 1000) / (sample_rate.max(1) as u64)) as u32,
        );
        self.dj_mixer
            .begin_deck_a_capture(sample_rate, &album_a, num_a, false, manual_quick_cut);
        self.dj_mixer
            .set_track_timing(start_a_ms, dur_a_ms, dur_b_ms);
        self.dj_mixer.set_deck_intel_hints(hint_a, hint_b);
        self.dj_mixer.push_deck_a_frames(tail_a_s16);
        self.dj_mixer
            .finish_deck_a_and_begin_deck_b(&album_b, num_b, deck_b_is_native_nsf);
        self.dj_mixer.push_deck_b_frames(intro_b_s16, is_b_eof);
        self.app.dj_telemetry = self.dj_mixer.telemetry();
        self.dj_mixer.phase() != MixerPhase::Idle
    }

    pub fn dj_set_track_timing(
        &mut self,
        deck_a_start_ms: u32,
        deck_a_dur_ms: u32,
        deck_b_dur_ms: u32,
    ) {
        self.dj_mixer
            .set_track_timing(deck_a_start_ms, deck_a_dur_ms, deck_b_dur_ms);
    }

    pub fn dj_audible_elapsed_ms(&self, downstream_frames: usize) -> u32 {
        self.dj_mixer
            .playhead_at_delay_frames(downstream_frames)
            .audible_elapsed_ms
    }

    pub fn dj_feed_deck_b_s16(&mut self, chunk_s16: &[i16], is_eof: bool) {
        self.dj_mixer.push_deck_b_frames(chunk_s16, is_eof);
    }

    pub fn dj_render_s16(&mut self, max_frames: usize) -> Vec<i16> {
        let want_samples = max_frames.saturating_mul(2);
        if want_samples == 0 {
            return Vec::new();
        }
        if self.dj_out_buf.len() < want_samples {
            self.dj_out_buf.resize(want_samples, 0);
        }
        let mut total_frames = 0usize;
        while total_frames < max_frames {
            let dst = &mut self.dj_out_buf[total_frames * 2..want_samples];
            let (frames, crossed) = self.dj_mixer.render_s16(dst);
            if crossed {
                self.dj_midpoint_crossed = true;
            }
            if frames == 0 {
                break;
            }
            total_frames += frames;
        }
        set_nsf_tempo_ratio(self.dj_mixer.recommended_nsf_tempo_b());
        self.app.dj_telemetry = self.dj_mixer.playhead_at_delay_frames(max_frames).telemetry;
        self.dj_out_buf[..total_frames * 2].to_vec()
    }

    pub fn dj_take_midpoint_crossed(&mut self) -> bool {
        if self.dj_midpoint_crossed {
            self.dj_midpoint_crossed = false;
            true
        } else {
            false
        }
    }

    pub fn dj_is_active(&self) -> bool {
        self.dj_mixer.phase() != MixerPhase::Idle
    }

    pub fn dj_reset(&mut self) {
        self.dj_mixer.reset();
        self.dj_midpoint_crossed = false;
        self.app.dj_telemetry = DjTelemetry::default();
        set_nsf_tempo_ratio(1.0);
    }

    /* ---- Manual Dual-Deck `DJ Mixer` Plugin WebAudio Bridge ---- */

    pub fn manual_dj_begin(&mut self, sample_rate: u32) {
        let sr = sample_rate.clamp(8_000, 192_000);
        crate::plugins::with_wasm_manual_dj(|c| {
            c.set_sample_rate(sr);
            c.reset_session();
            c.set_crossfader(0.0, true);
        });
    }

    pub fn manual_dj_load_deck_s16(
        &mut self,
        deck: u8,
        track_idx: usize,
        sample_rate: u32,
        pcm_s16: &[i16],
        start_ms: u32,
        start_paused: bool,
    ) {
        let sr = sample_rate.clamp(8_000, 192_000);
        let (title, artist, path) = self
            .app
            .tracks
            .get(track_idx)
            .map(|t| {
                let p = t.uri.clone().unwrap_or_else(|| t.title.clone());
                (t.title.clone(), t.artist.clone(), p)
            })
            .unwrap_or_else(|| ("Track".to_string(), String::new(), "track".to_string()));

        let mut bpm = 120.0f32;
        let mut first_beat_ms = 0u32;
        let mut contour = [140u8; 256];
        let mut got_intel = false;

        if let Some(&key) = self.track_intel_keys.get(&track_idx) {
            if let Some(rec) = self.intel_db.get(key) {
                if rec.bpm >= 50.0 && rec.bpm <= 220.0 {
                    bpm = rec.bpm;
                }
                first_beat_ms = rec.first_beat_offset_ms;
                contour = rec.energy_contour;
                got_intel = true;
            }
        }

        if !got_intel && pcm_s16.len() >= (sr as usize) * 4 {
            let win_s16_len = pcm_s16.len().min((sr as usize) * 16 * 2);
            let dur_ms = (((pcm_s16.len() / 2) as u64 * 1000) / (sr as u64)) as u32;
            let req = AnalyzeRequest {
                key_hash: (track_idx as u64).wrapping_add(1),
                fs_path: String::new(),
                subtrack_index: SUBTRACK_NONE,
                slice_start_ms: 0,
                duration_ms: dur_ms,
                sample_rate_hint: sr,
                title: title.clone(),
                artist: artist.clone(),
                album: String::new(),
                genre: String::new(),
                tag_tbpm: None,
                high_priority: true,
            };
            let rec = analyze_pcm_s16_buffer(&pcm_s16[..win_s16_len], sr, 0, &req);
            if rec.bpm >= 50.0 && rec.bpm <= 220.0 {
                bpm = rec.bpm;
            }
            first_beat_ms = rec.first_beat_offset_ms;
            contour = rec.energy_contour;
            self.track_intel_keys.insert(track_idx, rec.key_hash);
            self.intel_db.upsert(rec);
        }

        let loaded = build_in_memory_deck_track(
            &path,
            &title,
            &artist,
            sr,
            pcm_s16.to_vec(),
            bpm,
            first_beat_ms,
            contour,
        );
        let eff_start_ms = if start_paused && start_ms == 0 {
            first_beat_ms.min(loaded.duration_ms.saturating_sub(250))
        } else {
            start_ms
        };
        let start_frame = (((eff_start_ms as u64) * (sr as u64)) / 1000).min(loaded.total_frames);
        let deck_id = DjDeckId::from_u8(deck);

        crate::plugins::with_wasm_manual_dj(|c| {
            c.set_sample_rate(sr);
            c.enqueue_deck_track(deck_id, loaded, start_paused, start_frame);
            c.step_idle_controls();
        });
    }

    pub fn manual_dj_render_s16(&mut self, frames: usize, sample_rate: u32) -> Vec<i16> {
        if frames == 0 {
            return Vec::new();
        }
        let sr = sample_rate.clamp(8_000, 192_000);
        let need = frames * 2;
        if self.manual_dj_out_buf.len() < need {
            self.manual_dj_out_buf.resize(need, 0);
        }
        let buf = &mut self.manual_dj_out_buf[..need];
        crate::plugins::with_wasm_manual_dj(|c| {
            c.set_sample_rate(sr);
            let _ = c.render_s16(buf);
        });
        buf.to_vec()
    }

    pub fn manual_dj_render_f32_planar(
        &mut self,
        out_l: &mut [f32],
        out_r: &mut [f32],
        sample_rate: u32,
    ) -> usize {
        let frames = out_l.len().min(out_r.len());
        if frames == 0 {
            return 0;
        }
        let sr = sample_rate.clamp(8_000, 192_000);
        let need = frames * 2;
        if self.manual_dj_out_buf.len() < need {
            self.manual_dj_out_buf.resize(need, 0);
        }
        let buf = &mut self.manual_dj_out_buf[..need];
        crate::plugins::with_wasm_manual_dj(|c| {
            c.set_sample_rate(sr);
            let _ = c.render_s16(buf);
        });
        let inv = 1.0f32 / 32768.0f32;
        for i in 0..frames {
            out_l[i] = (buf[i * 2] as f32) * inv;
            out_r[i] = (buf[i * 2 + 1] as f32) * inv;
        }
        frames
    }

    /* ---- Unified Track Intelligence (`uipod-track-intel` Web Bridge) ---- */

    pub fn intel_compute_key_hex(&mut self, track_idx: usize, byte_size: u32) -> String {
        let Some(tr) = self.app.tracks.get(track_idx) else {
            return String::new();
        };
        let src = tr
            .uri
            .as_deref()
            .filter(|s| !s.is_empty())
            .unwrap_or(tr.title.as_str());
        let norm = normalize_track_id(src, tr.subtrack_index);
        let eff_size = if byte_size > 0 {
            byte_size as u64
        } else {
            tr.duration_ms as u64
        };
        let h = compute_track_key_hash(&TrackKeyInput {
            canonical_source_id: &norm.canonical_source_id,
            file_size: eff_size,
            mtime_sec: 0,
            subtrack_index: norm.subtrack_index,
        });
        self.track_intel_keys.insert(track_idx, h);
        format!("{h:016x}")
    }

    pub fn intel_activate_if_in_memory(&mut self, track_idx: usize) -> bool {
        let Some(&h) = self.track_intel_keys.get(&track_idx) else {
            return false;
        };
        if let Some(rec) = self.intel_db.get(h) {
            self.active_intel_key = h;
            publish_active_analysis(Some(&rec));
            true
        } else {
            false
        }
    }

    pub fn intel_load_cached_bytes(&mut self, track_idx: usize, raw_512: &[u8]) -> bool {
        let Some(rec) = decode_record_bytes(raw_512) else {
            return false;
        }
        ;
        self.track_intel_keys.insert(track_idx, rec.key_hash);
        self.intel_db.upsert(rec);
        self.active_intel_key = rec.key_hash;
        publish_active_analysis(Some(&rec));
        true
    }

    #[allow(clippy::too_many_arguments)]
    pub fn intel_analyze_mono_window(
        &mut self,
        track_idx: usize,
        byte_size: u32,
        sample_rate: u32,
        window_start_ms: u32,
        mono_f32: &[f32],
        full_contour_mono: &[f32],
    ) -> Vec<u8> {
        let hex = self.intel_compute_key_hex(track_idx, byte_size);
        if hex.is_empty() {
            return Vec::new();
        }
        let h = self.track_intel_keys[&track_idx];
        if let Some(existing) = self.intel_db.get(h) {
            self.active_intel_key = h;
            publish_active_analysis(Some(&existing));
            return encode_record_bytes(&existing).to_vec();
        }

        let metrics = analyze_mono_window(mono_f32, sample_rate, window_start_ms, None);
        let contour = if !full_contour_mono.is_empty() {
            compute_contour_256_from_mono(full_contour_mono)
        } else {
            compute_contour_256_from_mono(mono_f32)
        };
        let (dur_ms, sub_idx, title, artist, album, genre) = self
            .app
            .tracks
            .get(track_idx)
            .map(|t| {
                (
                    t.duration_ms,
                    t.subtrack_index.unwrap_or(SUBTRACK_NONE),
                    t.title.clone(),
                    t.artist.clone(),
                    t.album.clone(),
                    t.genre.clone(),
                )
            })
            .unwrap_or((0, SUBTRACK_NONE, String::new(), String::new(), String::new(), String::new()));

        let mut rec = TrackAnalysisRecord {
            key_hash: h,
            record_crc32: 0,
            subtrack_index: sub_idx,
            channels: 2,
            analysis_flags: 0,
            duration_ms: dur_ms,
            sample_rate: sample_rate.max(8_000),
            bpm: metrics.bpm,
            bpm_confidence: metrics.bpm_confidence,
            first_beat_offset_ms: metrics.first_beat_offset_ms,
            tag_tbpm: 0.0,
            tonal_balance: metrics.tonal_balance,
            avg_rms: metrics.avg_rms,
            crest_factor: metrics.crest_factor,
            kick_baseline: metrics.kick_baseline,
            snare_baseline: metrics.snare_baseline,
            ..Default::default()
        };
        write_fixed_utf8(&mut rec.title, &title);
        write_fixed_utf8(&mut rec.artist, &artist);
        write_fixed_utf8(&mut rec.album, &album);
        write_fixed_utf8(&mut rec.genre, &genre);
        rec.energy_contour = contour;

        self.intel_db.upsert(rec);
        self.active_intel_key = h;
        publish_active_analysis(Some(&rec));
        encode_record_bytes(&rec).to_vec()
    }

    pub fn intel_analyze_nsf_track(&mut self, track_idx: usize) -> Vec<u8> {
        let Some(tr) = self.app.tracks.get(track_idx).cloned() else {
            return Vec::new();
        };
        let Some(ref raw_uri) = tr.uri else {
            return Vec::new();
        };
        if raw_uri.is_empty() {
            return Vec::new();
        }
        let parsed_uri = SubtrackUri::parse(raw_uri);
        let cf = parsed_uri
            .file_path
            .trim()
            .replace('\\', "/")
            .to_ascii_lowercase();
        let base_only = cf.rsplit('/').next().unwrap_or(cf.as_str()).to_string();
        let Some(nsf_bytes) = self
            .nsf_containers
            .get(&cf)
            .or_else(|| self.nsf_containers.get(&base_only))
            .cloned()
        else {
            return Vec::new();
        };
        let _ = self.intel_compute_key_hex(track_idx, nsf_bytes.len() as u32);
        let h = self.track_intel_keys[&track_idx];
        if let Some(existing) = self.intel_db.get(h) {
            self.active_intel_key = h;
            publish_active_analysis(Some(&existing));
            return encode_record_bytes(&existing).to_vec();
        }
        let sub_idx = tr.subtrack_index.or(parsed_uri.subtrack_index);
        let req = AnalyzeRequest {
            key_hash: h,
            fs_path: cf,
            subtrack_index: sub_idx.unwrap_or(0),
            slice_start_ms: 0,
            duration_ms: tr.duration_ms,
            sample_rate_hint: 44_100,
            title: tr.title,
            artist: tr.artist,
            album: tr.album,
            genre: tr.genre,
            tag_tbpm: None,
            high_priority: true,
        };
        if let Some(rec) = analyze_nsf_bytes(&nsf_bytes, sub_idx, &req) {
            self.intel_db.upsert(rec);
            self.active_intel_key = h;
            publish_active_analysis(Some(&rec));
            encode_record_bytes(&rec).to_vec()
        } else {
            Vec::new()
        }
    }

    pub fn hud_begin_processing(&mut self, detail: &str) -> u32 {
        let title = if self.app.uk { "Обробка..." } else { "Processing..." };
        self.ui.hud_progress_begin(title, detail, None, self.app.clock_ms)
    }

    pub fn hud_end_processing(&mut self, handle: u32) {
        if handle != 0 {
            let _ = self.ui.hud_progress_end(handle, self.app.clock_ms);
        }
    }
}

/* ---- Static Metadata Exports for the Landing Page & Studio Editors ---- */

#[wasm_bindgen]
pub fn get_themes_json() -> String {
    let mut out = String::from("[");
    for (i, t) in THEME_PRESETS.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        out.push_str(&format!(
            r##"{{"index":{},"name":"{}","bg":"#{:06X}","fg":"#{:06X}","fg1":"#{:06X}","fg2":"#{:06X}"}}"##,
            i,
            t.name,
            t.colors.bg_rgb,
            t.colors.fg_rgb,
            t.colors.fg1_rgb,
            t.colors.fg2_rgb
        ));
    }
    out.push(']');
    out
}

#[wasm_bindgen]
pub fn get_viz_presets_json() -> String {
    let mut out = String::from("[");
    for (i, p) in VIZ_PRESETS.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        out.push_str(&format!(
            r#"{{"index":{},"id":"{}","nameEn":"{}","nameUk":"{}"}}"#,
            i, p.id, p.name_en, p.name_uk
        ));
    }
    out.push(']');
    out
}

#[wasm_bindgen]
pub fn get_viz_shader_header() -> String {
    VizEngine::scene_fs_header().to_string()
}

#[wasm_bindgen]
pub fn get_viz_preset_shader(idx: usize) -> String {
    VizEngine::preset_shader_source(idx).to_string()
}

#[wasm_bindgen]
pub fn get_autoeq_presets_json() -> String {
    let mut out = String::from("[");
    for (i, name) in AUTOEQ_PRESETS.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        out.push_str(&format!(r#""{name}""#));
    }
    out.push(']');
    out
}
