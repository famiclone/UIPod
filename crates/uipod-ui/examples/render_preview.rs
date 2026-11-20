//! Headless macOS OpenGL 3.2 Core preview renderer for visual verification of `uipod-ui`.
//!
//! Usage:
//!   cargo run --offline --example render_preview -- <output_dir>

#[cfg(target_os = "macos")]
mod macos_gl {
    use std::ffi::{c_char, c_int, c_void, CStr, CString};
    use std::ptr;

    type CGLPixelFormatObj = *mut c_void;
    type CGLContextObj = *mut c_void;
    type CGLError = c_int;

    const K_CGL_PFA_OPENGL_PROFILE: u32 = 99;
    const K_CGL_OGLP_VERSION_3_2_CORE: u32 = 0x3200;
    const K_CGL_PFA_ACCELERATED: u32 = 73;
    const K_CGL_PFA_ALLOW_OFFLINE_RENDERERS: u32 = 96;

    #[link(name = "OpenGL", kind = "framework")]
    extern "C" {
        fn CGLChoosePixelFormat(
            attribs: *const u32,
            pix: *mut CGLPixelFormatObj,
            npix: *mut c_int,
        ) -> CGLError;
        fn CGLCreateContext(
            pix: CGLPixelFormatObj,
            share: CGLContextObj,
            ctx: *mut CGLContextObj,
        ) -> CGLError;
        fn CGLSetCurrentContext(ctx: CGLContextObj) -> CGLError;
        fn CGLDestroyPixelFormat(pix: CGLPixelFormatObj) -> CGLError;
    }

    extern "C" {
        fn dlopen(filename: *const c_char, flag: c_int) -> *mut c_void;
        fn dlsym(handle: *mut c_void, symbol: *const c_char) -> *const c_void;
    }

    pub struct HeadlessGl {
        gl_lib: *mut c_void,
    }

    impl HeadlessGl {
        pub unsafe fn init() -> Result<Self, String> {
            const K_CGL_PFA_RENDERER_ID: u32 = 70;
            const K_CGL_RENDERER_GENERIC_FLOAT_ID: u32 = 0x0002_0400;

            let force_sw = std::env::var_os("UIPOD_SW_GL").is_some();
            let attribs_hw = [
                K_CGL_PFA_OPENGL_PROFILE,
                K_CGL_OGLP_VERSION_3_2_CORE,
                K_CGL_PFA_ACCELERATED,
                K_CGL_PFA_ALLOW_OFFLINE_RENDERERS,
                0,
            ];
            let attribs_sw = [
                K_CGL_PFA_OPENGL_PROFILE,
                K_CGL_OGLP_VERSION_3_2_CORE,
                K_CGL_PFA_RENDERER_ID,
                K_CGL_RENDERER_GENERIC_FLOAT_ID,
                0,
            ];

            let mut pix: CGLPixelFormatObj = ptr::null_mut();
            let mut npix: c_int = 0;
            let mut err = if force_sw {
                CGLChoosePixelFormat(attribs_sw.as_ptr(), &mut pix, &mut npix)
            } else {
                CGLChoosePixelFormat(attribs_hw.as_ptr(), &mut pix, &mut npix)
            };
            if err != 0 || pix.is_null() {
                err = CGLChoosePixelFormat(attribs_sw.as_ptr(), &mut pix, &mut npix);
            }
            if err != 0 || pix.is_null() {
                return Err(format!("CGLChoosePixelFormat failed: {err}"));
            }

            let mut ctx: CGLContextObj = ptr::null_mut();
            let err_ctx = CGLCreateContext(pix, ptr::null_mut(), &mut ctx);
            CGLDestroyPixelFormat(pix);
            if err_ctx != 0 || ctx.is_null() {
                return Err(format!("CGLCreateContext failed: {err_ctx}"));
            }

            let err_cur = CGLSetCurrentContext(ctx);
            if err_cur != 0 {
                return Err(format!("CGLSetCurrentContext failed: {err_cur}"));
            }

            let path = CString::new(
                "/System/Library/Frameworks/OpenGL.framework/Versions/Current/OpenGL",
            )
            .unwrap();
            let gl_lib = dlopen(path.as_ptr(), 1);
            if gl_lib.is_null() {
                return Err("dlopen OpenGL.framework failed".to_string());
            }

            Ok(Self { gl_lib })
        }

        pub unsafe fn get_proc_address(&self, name: &CStr) -> *const c_void {
            dlsym(self.gl_lib, name.as_ptr())
        }
    }
}

use std::fs::File;
use std::io::BufWriter;
use std::path::{Path, PathBuf};

use uipod_ui::renderer::{HEIGHT, WIDTH};
use uipod_ui::{
    FileIconKind, HeaderSourceIcon, HeaderState, KeyboardKey, KeyboardState, ListRow,
    ListScreenState, NowPlayingState, PlayMode, RightAccessory, ThemeColors, UiEngine,
};

fn write_png(path: &Path, rgba: &[u8]) {
    let file = File::create(path).expect("create PNG file");
    let w = BufWriter::new(file);
    let mut encoder = png::Encoder::new(w, WIDTH as u32, HEIGHT as u32);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    let mut writer = encoder.write_header().expect("write PNG header");
    writer.write_image_data(rgba).expect("write PNG data");
}

/// Generates a synthetic colorful 256x256 vinyl label cover image to test monochrome+contrast filtering.
fn make_vinyl_test_art() -> Vec<u8> {
    let mut rgb = vec![0u8; 256 * 256 * 3];
    for y in 0..256 {
        for x in 0..256 {
            let dx = x as f32 - 127.5;
            let dy = y as f32 - 127.5;
            let dist = (dx * dx + dy * dy).sqrt();
            let (r, g, b) = if dist < 11.0 {
                // Center spindle hole
                (15, 15, 18)
            } else if dist < 122.0 {
                // Warm cream vinyl paper label with subtle decorative bands & print lines
                let band = if (y > 42 && y < 68 && x > 64 && x < 192)
                    || (y > 95 && y < 112 && x > 50 && x < 206)
                    || (dist > 114.0 && dist < 117.0)
                {
                    90u8
                } else if (y % 11 == 0) && y > 140 && y < 210 && x > 76 && x < 180 {
                    120u8
                } else {
                    236u8
                };
                (band, band.saturating_sub(8), band.saturating_sub(24))
            } else {
                // Dark vinyl outer rim
                let groove = if (dist as u32) % 4 == 0 { 28 } else { 12 };
                (groove, groove, groove + 4)
            };
            let idx = (y * 256 + x) * 3;
            rgb[idx] = r;
            rgb[idx + 1] = g;
            rgb[idx + 2] = b;
        }
    }
    rgb
}

fn main() {
    #[cfg(not(target_os = "macos"))]
    {
        eprintln!("render_preview example requires macOS CGL");
        std::process::exit(1);
    }

    #[cfg(target_os = "macos")]
    unsafe {
        let out_dir = std::env::args()
            .nth(1)
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("/tmp"));

        let gl = macos_gl::HeadlessGl::init().expect("init headless CGL");
        let cjk = Path::new("assets/fonts/PixelMplus12-Regular.ttf");
        let mut engine = UiEngine::new(|s| gl.get_proc_address(s), Some(cjk))
            .expect("init UiEngine");

        // 1. Main Menu screen (exact ui.figcraft [215] "UIPod Mini")
        let main_state = ListScreenState {
            theme: ThemeColors::default(),
            header: HeaderState {
                title: "UIPod",
                legend: "",
                play_mode: PlayMode::Play,
                battery_percent: 69,
                is_charging: true,
                is_hold: false,
                is_fx: true,
                is_downloading: true,
                download_done: 3,
                download_total: 12,
                source_icon: HeaderSourceIcon::Navidrome,
            },
            rows: vec![
                ListRow {
                    label: "Files",
                    selected: true,
                    file_icon: FileIconKind::None,
                    right: RightAccessory::Chevron,
                },
                ListRow {
                    label: "Extras",
                    selected: false,
                    file_icon: FileIconKind::None,
                    right: RightAccessory::Chevron,
                },
                ListRow {
                    label: "Artists",
                    selected: false,
                    file_icon: FileIconKind::None,
                    right: RightAccessory::Chevron,
                },
                ListRow {
                    label: "Albums",
                    selected: false,
                    file_icon: FileIconKind::None,
                    right: RightAccessory::Chevron,
                },
                ListRow {
                    label: "Genres",
                    selected: false,
                    file_icon: FileIconKind::None,
                    right: RightAccessory::Chevron,
                },
                ListRow {
                    label: "Videos",
                    selected: false,
                    file_icon: FileIconKind::None,
                    right: RightAccessory::Chevron,
                },
                ListRow {
                    label: "Settings",
                    selected: false,
                    file_icon: FileIconKind::None,
                    right: RightAccessory::Chevron,
                },
                ListRow {
                    label: "Now Playing",
                    selected: false,
                    file_icon: FileIconKind::None,
                    right: RightAccessory::None,
                },
            ],
            total_items: 8,
            start_item: 0,
            visible_items: 8,
        };
        let rgba_main = engine.render_list_rgba(&main_state);
        let main_path = out_dir.join("preview_main.png");
        write_png(&main_path, &rgba_main);

        // 2. Settings-Appearance (#1172 Active4 < UIPod Classic >) + Ukrainian showcase
        let appearance_state = ListScreenState {
            theme: ThemeColors::default(),
            header: HeaderState {
                title: "Вигляд",
                legend: "",
                play_mode: PlayMode::Pause,
                battery_percent: 54,
                is_charging: false,
                is_hold: false,
                ..Default::default()
            },
            rows: vec![
                ListRow {
                    label: "Тема",
                    selected: true,
                    file_icon: FileIconKind::None,
                    right: RightAccessory::CycleValue("UIPod Classic"),
                },
                ListRow {
                    label: "Кастомізація теми",
                    selected: false,
                    file_icon: FileIconKind::None,
                    right: RightAccessory::Chevron,
                },
                ListRow {
                    label: "Мова",
                    selected: false,
                    file_icon: FileIconKind::None,
                    right: RightAccessory::CycleValue("Українська"),
                },
                ListRow {
                    label: "Ч/Б обкладинка альбому",
                    selected: false,
                    file_icon: FileIconKind::None,
                    right: RightAccessory::Toggle(true),
                },
                ListRow {
                    label: "Головне меню",
                    selected: false,
                    file_icon: FileIconKind::None,
                    right: RightAccessory::Chevron,
                },
            ],
            total_items: 5,
            start_item: 0,
            visible_items: 8,
        };
        let rgba_app = engine.render_list_rgba(&appearance_state);
        let app_path = out_dir.join("preview_appearance_ua.png");
        write_png(&app_path, &rgba_app);

        // 3. Files (#694) + Add directory + Directory (#742) icons showcase
        let files_state = ListScreenState {
            theme: ThemeColors::default(),
            header: HeaderState {
                title: "Files",
                legend: "",
                play_mode: PlayMode::Play,
                battery_percent: 69,
                is_charging: true,
                is_hold: false,
                ..Default::default()
            },
            rows: vec![
                ListRow {
                    label: "Directory 1",
                    selected: true,
                    file_icon: FileIconKind::from_tag("folder"),
                    right: RightAccessory::Chevron,
                },
                ListRow {
                    label: "Directory 2",
                    selected: false,
                    file_icon: FileIconKind::from_tag("folder"),
                    right: RightAccessory::Chevron,
                },
                ListRow {
                    label: "Add directory",
                    selected: false,
                    file_icon: FileIconKind::from_tag("add_dir"),
                    right: RightAccessory::Chevron,
                },
                ListRow {
                    label: "Rezidue - Inner City.mp3",
                    selected: false,
                    file_icon: FileIconKind::from_tag("music"),
                    right: RightAccessory::Chevron,
                },
            ],
            total_items: 4,
            start_item: 0,
            visible_items: 8,
        };
        let rgba_files = engine.render_list_rgba(&files_state);
        let files_path = out_dir.join("preview_files_settings.png");
        write_png(&files_path, &rgba_files);

        // 3. Now Playing screen (exact ui.figcraft [233] & user's reference photo)
        let art = make_vinyl_test_art();
        let np_state = NowPlayingState {
            theme: ThemeColors::default(),
            header: HeaderState {
                title: "Now Playing",
                legend: "",
                play_mode: PlayMode::Play,
                battery_percent: 54,
                is_charging: false,
                is_hold: false,
                source_icon: HeaderSourceIcon::MicroSd,
                ..Default::default()
            },
            track_pos_label: "21 of 500",
            eq_enabled: true,
            title: "Inner City Blues (Jeep Mix)",
            artist: "Rezidue",
            album: "Inner City Blues / Dropin' Rezidue Mixed Edition",
            codec_label: "FLAC",
            bitdepth_label: "24bit",
            samplerate_label: "192kHz",
            progress: 68.0 / 195.0,
            elapsed_label: "1:08",
            remaining_label: "-2:07",
            center_status_label: "",
            art_rgb24: Some(&art),
            mono_album_art: true,
            alt_layout: false,
        };
        let rgba_np = engine.render_now_playing_rgba(&np_state);
        let np_path = out_dir.join("preview_now_playing.png");
        write_png(&np_path, &rgba_np);

        // 4. Volume Popup screen (exact ui.figcraft [10977] / [3214])
        let rgba_vp = engine.render_volume_popup_rgba(&np_state, 220.0 / 352.0);
        write_png(&out_dir.join("code_volume_popup.png"), &rgba_vp);

        // 5. Context Menu - Video Player (exact ui.figcraft [11301])
        let vid_ctx_rows = vec![
            ListRow {
                label: "Aspect Fit",
                file_icon: FileIconKind::None,
                selected: true,
                right: RightAccessory::CycleValue("Fit"),
            },
            ListRow {
                label: "Audio Track",
                file_icon: FileIconKind::None,
                selected: false,
                right: RightAccessory::CycleValue("Track 1 (EN)"),
            },
            ListRow {
                label: "Subtitles",
                file_icon: FileIconKind::None,
                selected: false,
                right: RightAccessory::CycleValue("Off"),
            },
        ];
        let vid_ctx_state = ListScreenState {
            theme: ThemeColors::default(),
            header: HeaderState {
                title: "Video Settings",
                legend: "",
                play_mode: PlayMode::Play,
                battery_percent: 54,
                is_charging: false,
                is_hold: false,
                source_icon: HeaderSourceIcon::None,
                ..Default::default()
            },
            rows: vid_ctx_rows,
            total_items: 3,
            start_item: 0,
            visible_items: 8,
        };
        let rgba_vctx = engine.render_list_rgba(&vid_ctx_state);
        write_png(&out_dir.join("code_video_context_menu.png"), &rgba_vctx);

        let qwerty_rows: [&[&str]; 5] = [
            &["1", "2", "3", "4", "5", "6", "7", "8", "9", "0"],
            &["Q", "W", "E", "R", "T", "Y", "U", "I", "O", "P"],
            &["A", "S", "D", "F", "G", "H", "J", "K", "L"],
            &["Z", "X", "C", "V", "B", "N", "M", "-"],
            &["CAPS", "sym", "space", "del", "enter"],
        ];
        let mut kbd_rows: [Vec<KeyboardKey>; 5] = Default::default();
        for (ri, r) in qwerty_rows.iter().enumerate() {
            kbd_rows[ri] = r
                .iter()
                .enumerate()
                .map(|(ci, &lbl)| KeyboardKey {
                    label: lbl,
                    selected: ri == 1 && ci == 4,
                    is_modifier: ri == 4,
                    is_active_toggle: ri == 4 && ci == 0,
                })
                .collect();
        }
        let kbd_state = KeyboardState {
            theme: ThemeColors::default(),
            text: "#D6D8D5",
            rows: kbd_rows,
        };
        let rgba_kbd = engine.render_keyboard_rgba(&kbd_state);
        let kbd_path = out_dir.join("preview_keyboard.png");
        write_png(&kbd_path, &rgba_kbd);

        println!("Rendered previews to {}", out_dir.display());
    }
}
