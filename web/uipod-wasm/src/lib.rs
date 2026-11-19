mod app;
mod library;
mod plugins;
mod themes;

use uipod_ui::{ThemeColors, UiEngine, VizEngine, VIZ_PRESETS};
use wasm_bindgen::prelude::*;
use wasm_bindgen::JsCast;

use crate::app::{UIPodApp, AUTOEQ_PRESETS};
use crate::themes::{derive_4shade, THEME_PRESETS};

#[wasm_bindgen]
pub struct UIPodWasm {
    ui: UiEngine,
    viz: VizEngine,
    app: UIPodApp,
}

#[wasm_bindgen]
impl UIPodWasm {
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
        self.app.adjust_volume(delta);
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

    pub fn set_viz_fine_grain(&mut self, fine: bool) {
        self.viz.set_fine_grain(fine);
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
        self.app.dsp.autoeq_idx = autoeq_idx % AUTOEQ_PRESETS.len();
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
            self.app.play_mode = uipod_ui::PlayMode::Play;
            self.app.state_dirty = true;
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
