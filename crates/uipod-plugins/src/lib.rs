//! Pure-Rust native plugin engine for UIPod (`.uipod` / `.so` via `dlopen`).
//!
//! Responsibilities:
//! - Scans bundled (`/tmp/uipod/plugins`) and SD-card (`/mnt/SDCARD/Plugins/UIPod`,
//!   `/mnt/SDCARD/Media/Plugins`) directories for `.uipod` / `.so` plugins.
//! - Probes each library's `uipod_plugin_descriptor` symbol, validates `magic` and
//!   `api_version`, indexes metadata + `Viewer` file extensions in memory, and
//!   unloads (`dlclose`) immediately so idle plugins consume zero RAM.
//! - Dynamically loads, executes with `&UIPodPluginApi`, and cleanly unloads plugins
//!   launched from `Main -> Extras -> Plugins` (`Games` / `Apps` / `Demos` / `Viewers`)
//!   or triggered by opening a matching file (or `.uipod` binary) in `Files` / `Browse`.

use std::ffi::{c_char, c_int, c_void, CStr, CString};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

use uipod_plugin_sdk::{
    PluginCategory, PluginKind, UIPodPluginApi, UIPodPluginDescriptor, UIPOD_PLUGIN_API_VERSION,
    UIPOD_PLUGIN_ENTRY_SYMBOL, UIPOD_PLUGIN_MAGIC,
};

#[cfg(target_os = "macos")]
const RTLD_FLAGS: c_int = 2 | 4; // RTLD_NOW | RTLD_LOCAL
#[cfg(not(target_os = "macos"))]
const RTLD_FLAGS: c_int = 2; // RTLD_NOW | RTLD_LOCAL (0 on glibc/musl)

extern "C" {
    fn dlopen(filename: *const c_char, flags: c_int) -> *mut c_void;
    fn dlsym(handle: *mut c_void, symbol: *const c_char) -> *mut c_void;
    fn dlclose(handle: *mut c_void) -> c_int;
    fn dlerror() -> *const c_char;
}

type DescriptorFn = unsafe extern "C" fn() -> *const UIPodPluginDescriptor;

struct DlLibrary {
    handle: *mut c_void,
}

impl DlLibrary {
    fn open(path: &Path) -> Result<Self, String> {
        let s = path.to_string_lossy();
        let c_path = CString::new(s.as_bytes()).map_err(|_| "Invalid plugin path".to_string())?;
        unsafe {
            // Clear any stale dlerror
            let _ = dlerror();
            let handle = dlopen(c_path.as_ptr(), RTLD_FLAGS);
            if handle.is_null() {
                let err_ptr = dlerror();
                let msg = if err_ptr.is_null() {
                    "dlopen returned NULL".to_string()
                } else {
                    CStr::from_ptr(err_ptr).to_string_lossy().into_owned()
                };
                return Err(msg);
            }
            Ok(Self { handle })
        }
    }

    unsafe fn descriptor(&self) -> Result<&'static UIPodPluginDescriptor, String> {
        let _ = dlerror();
        let sym = dlsym(
            self.handle,
            UIPOD_PLUGIN_ENTRY_SYMBOL.as_ptr() as *const c_char,
        );
        if sym.is_null() {
            return Err("Missing uipod_plugin_descriptor symbol".to_string());
        }
        let get_desc: DescriptorFn = std::mem::transmute(sym);
        let ptr = get_desc();
        if ptr.is_null() {
            return Err("uipod_plugin_descriptor returned NULL".to_string());
        }
        let desc = &*ptr;
        if desc.magic != UIPOD_PLUGIN_MAGIC {
            return Err(format!("Invalid plugin magic: 0x{:08X}", desc.magic));
        }
        if desc.api_version != UIPOD_PLUGIN_API_VERSION && desc.api_version != 5 {
            return Err(format!(
                "Incompatible plugin ABI v{} (host v{})",
                desc.api_version, UIPOD_PLUGIN_API_VERSION
            ));
        }
        Ok(desc)
    }
}

impl Drop for DlLibrary {
    fn drop(&mut self) {
        if !self.handle.is_null() {
            unsafe {
                let _ = dlclose(self.handle);
            }
        }
    }
}

#[derive(Debug, Clone)]
pub struct PluginMeta {
    pub id: String,
    pub name: String,
    pub name_uk: String,
    pub author: String,
    pub version: String,
    pub category: PluginCategory,
    pub kind: PluginKind,
    pub extensions: Vec<String>,
    pub lib_path: PathBuf,
    pub is_user_sdcard: bool,
}

impl PluginMeta {
    pub fn display_name(&self, ukrainian: bool) -> &str {
        if ukrainian && !self.name_uk.is_empty() {
            &self.name_uk
        } else {
            &self.name
        }
    }

    pub fn matches_extension(&self, ext_lower: &str) -> bool {
        !ext_lower.is_empty() && self.extensions.iter().any(|e| e == ext_lower)
    }
}

#[inline]
unsafe fn read_cstr(p: *const c_char) -> String {
    if p.is_null() {
        String::new()
    } else {
        CStr::from_ptr(p).to_string_lossy().trim().to_string()
    }
}

pub fn parse_extensions_csv(raw: &str) -> Vec<String> {
    raw.split(|c: char| c == ',' || c == ';' || c.is_ascii_whitespace())
        .map(|part| part.trim().trim_start_matches('.').to_ascii_lowercase())
        .filter(|s| !s.is_empty())
        .collect()
}

pub fn is_plugin_binary_path(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .map(|e| e.eq_ignore_ascii_case("uipod"))
        .unwrap_or(false)
}

fn is_scannable_plugin_file(path: &Path) -> bool {
    let Some(ext) = path.extension().and_then(|e| e.to_str()) else {
        return false;
    };
    if ext.eq_ignore_ascii_case("uipod") {
        return true;
    }
    #[cfg(target_os = "android")]
    {
        if ext.eq_ignore_ascii_case("so") {
            return path
                .file_name()
                .and_then(|n| n.to_str())
                .map(|n| n.starts_with("libuipod_plugin_"))
                .unwrap_or(false);
        }
        false
    }
    #[cfg(not(target_os = "android"))]
    {
        ext.eq_ignore_ascii_case("so")
    }
}

fn probe_plugin_file(path: &Path, is_user_sdcard: bool) -> Result<PluginMeta, String> {
    let lib = DlLibrary::open(path)?;
    unsafe {
        let desc = lib.descriptor()?;
        let id = read_cstr(desc.id);
        let name = read_cstr(desc.name);
        if id.is_empty() || name.is_empty() {
            return Err("Plugin id or name is empty".to_string());
        }
        let name_uk = read_cstr(desc.name_uk);
        let author = read_cstr(desc.author);
        let version = read_cstr(desc.version);
        let category = PluginCategory::from_u32(desc.category);
        let kind = PluginKind::from_u32(desc.kind);
        let extensions = parse_extensions_csv(&read_cstr(desc.extensions));

        Ok(PluginMeta {
            id,
            name,
            name_uk,
            author,
            version,
            category,
            kind,
            extensions,
            lib_path: path.to_path_buf(),
            is_user_sdcard,
        })
    }
}

fn collect_plugin_files(dir: &Path, depth: usize, out: &mut Vec<PathBuf>) {
    if depth > 2 || !dir.is_dir() {
        return;
    }
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    let mut paths: Vec<PathBuf> = entries.flatten().map(|e| e.path()).collect();
    paths.sort();
    for p in paths {
        if let Some(name) = p.file_name().and_then(|n| n.to_str()) {
            if name.starts_with('.') {
                continue;
            }
        }
        if p.is_dir() {
            collect_plugin_files(&p, depth + 1, out);
        } else if is_scannable_plugin_file(&p) {
            out.push(p);
        }
    }
}

pub struct PluginRegistry {
    bundled_dir: PathBuf,
    user_dirs: Vec<PathBuf>,
    plugins: Vec<PluginMeta>,
}

#[cfg(target_os = "android")]
const DEFAULT_SDCARD_ROOT: &str = "/storage/emulated/0";
#[cfg(not(target_os = "android"))]
const DEFAULT_SDCARD_ROOT: &str = "/mnt/SDCARD";

impl PluginRegistry {
    fn new() -> Self {
        Self {
            bundled_dir: PathBuf::from("/tmp/uipod/plugins"),
            user_dirs: vec![
                PathBuf::from(format!("{DEFAULT_SDCARD_ROOT}/Plugins/UIPod")),
                PathBuf::from(format!("{DEFAULT_SDCARD_ROOT}/Media/Plugins")),
            ],
            plugins: Vec::new(),
        }
    }

    pub fn configure_dirs(&mut self, bundled: &Path, sd_primary: &Path) {
        self.bundled_dir = bundled.to_path_buf();
        self.user_dirs.clear();
        if !sd_primary.as_os_str().is_empty() {
            self.user_dirs.push(sd_primary.to_path_buf());
        }
        let alt = PathBuf::from(format!("{DEFAULT_SDCARD_ROOT}/Media/Plugins"));
        if alt != sd_primary {
            self.user_dirs.push(alt);
        }
    }

    pub fn rescan(&mut self) -> usize {
        if Path::new(DEFAULT_SDCARD_ROOT).is_dir() {
            if let Some(primary_sd) = self.user_dirs.first() {
                let _ = fs::create_dir_all(primary_sd);
            }
        }

        let mut list: Vec<PluginMeta> = Vec::new();

        // 1. Bundled plugins inside UIPod.pak (`/tmp/uipod/plugins`)
        let mut bundled_files = Vec::new();
        collect_plugin_files(&self.bundled_dir, 0, &mut bundled_files);
        for f in bundled_files {
            if let Ok(meta) = probe_plugin_file(&f, false) {
                if let Some(existing) = list.iter_mut().find(|m| m.id == meta.id) {
                    *existing = meta;
                } else {
                    list.push(meta);
                }
            }
        }

        // 2. User SD-card plugins (override bundled plugins with matching `id`)
        for udir in &self.user_dirs {
            let mut user_files = Vec::new();
            collect_plugin_files(udir, 0, &mut user_files);
            for f in user_files {
                if let Ok(meta) = probe_plugin_file(&f, true) {
                    if let Some(existing) = list.iter_mut().find(|m| m.id == meta.id) {
                        *existing = meta;
                    } else {
                        list.push(meta);
                    }
                }
            }
        }

        list.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
        self.plugins = list;
        self.plugins.len()
    }

    pub fn by_category(&self, cat: PluginCategory) -> Vec<PluginMeta> {
        self.plugins
            .iter()
            .filter(|p| p.category == cat)
            .cloned()
            .collect()
    }

    pub fn find_by_id(&self, id: &str) -> Option<PluginMeta> {
        self.plugins.iter().find(|p| p.id == id).cloned()
    }

    pub fn find_viewer_for_path(&self, file_path: &Path) -> Option<PluginMeta> {
        let ext = file_path
            .extension()
            .and_then(|e| e.to_str())?
            .to_ascii_lowercase();
        self.plugins
            .iter()
            .find(|p| p.category == PluginCategory::Viewer && p.matches_extension(&ext))
            .cloned()
    }
}

fn registry_slot() -> &'static Mutex<PluginRegistry> {
    static REG: OnceLock<Mutex<PluginRegistry>> = OnceLock::new();
    REG.get_or_init(|| Mutex::new(PluginRegistry::new()))
}

fn execute_library_path(
    lib_path: &Path,
    api: *const UIPodPluginApi,
    open_file: Option<&str>,
) -> Result<i32, String> {
    if api.is_null() {
        return Err("Host API table is NULL".to_string());
    }
    let lib = DlLibrary::open(lib_path)?;
    unsafe {
        let desc = lib.descriptor()?;
        let c_file = match open_file {
            Some(s) if !s.is_empty() => Some(
                CString::new(s.as_bytes()).map_err(|_| "Invalid target file path".to_string())?,
            ),
            _ => None,
        };
        let file_ptr = c_file.as_ref().map_or(std::ptr::null(), |c| c.as_ptr());
        Ok((desc.run)(api, file_ptr))
    }
}

/* ---- C-ABI FFI Surface for `apps/uipod_plugins.c` ---------------------- */

#[repr(C)]
pub struct UIPodPluginInfoC {
    pub id: [c_char; 64],
    pub title: [c_char; 128],
    pub author: [c_char; 64],
    pub version: [c_char; 32],
    pub category: u32,
    pub kind: u32,
    pub is_sdcard: bool,
}

fn write_fixed_cstr(src: &str, dst: &mut [c_char]) {
    if dst.is_empty() {
        return;
    }
    for slot in dst.iter_mut() {
        *slot = 0;
    }
    let bytes = src.as_bytes();
    let max = dst.len() - 1;
    let mut end = bytes.len().min(max);
    while end > 0 && !src.is_char_boundary(end) {
        end -= 1;
    }
    for (i, &b) in bytes[..end].iter().enumerate() {
        dst[i] = b as c_char;
    }
}

#[no_mangle]
pub unsafe extern "C" fn uipod_rs_plugins_init(
    bundled_dir: *const c_char,
    sdcard_dir: *const c_char,
) -> usize {
    let b_str = read_cstr(bundled_dir);
    let s_str = read_cstr(sdcard_dir);
    let Ok(mut guard) = registry_slot().lock() else {
        return 0;
    };
    if !b_str.is_empty() {
        guard.configure_dirs(Path::new(&b_str), Path::new(&s_str));
    }
    guard.rescan()
}

#[no_mangle]
pub extern "C" fn uipod_rs_plugins_rescan() -> usize {
    registry_slot()
        .lock()
        .map(|mut g| g.rescan())
        .unwrap_or(0)
}

#[no_mangle]
pub extern "C" fn uipod_rs_plugins_count_category(category: u32) -> usize {
    let cat = PluginCategory::from_u32(category);
    registry_slot()
        .lock()
        .map(|g| g.by_category(cat).len())
        .unwrap_or(0)
}

#[no_mangle]
pub unsafe extern "C" fn uipod_rs_plugins_list_category(
    category: u32,
    ukrainian: bool,
    out: *mut UIPodPluginInfoC,
    cap: usize,
) -> usize {
    if out.is_null() || cap == 0 {
        return 0;
    }
    let cat = PluginCategory::from_u32(category);
    let Ok(guard) = registry_slot().lock() else {
        return 0;
    };
    let mut items = guard.by_category(cat);
    items.sort_by(|a, b| {
        a.display_name(ukrainian)
            .to_lowercase()
            .cmp(&b.display_name(ukrainian).to_lowercase())
    });

    let n = items.len().min(cap);
    let out_slice = std::slice::from_raw_parts_mut(out, n);
    for (src, dst) in items.into_iter().take(n).zip(out_slice.iter_mut()) {
        write_fixed_cstr(&src.id, &mut dst.id);
        write_fixed_cstr(src.display_name(ukrainian), &mut dst.title);
        write_fixed_cstr(&src.author, &mut dst.author);
        write_fixed_cstr(&src.version, &mut dst.version);
        dst.category = src.category as u32;
        dst.kind = src.kind as u32;
        dst.is_sdcard = src.is_user_sdcard;
    }
    n
}

#[no_mangle]
pub unsafe extern "C" fn uipod_rs_plugins_is_menu_driven(
    plugin_id: *const c_char,
    open_path: *const c_char,
) -> bool {
    let id = read_cstr(plugin_id);
    if !id.is_empty() {
        return registry_slot()
            .lock()
            .ok()
            .and_then(|g| g.find_by_id(&id))
            .map(|m| m.kind == PluginKind::MenuDriven)
            .unwrap_or(false);
    }

    let path_str = read_cstr(open_path);
    if path_str.is_empty() {
        return false;
    }
    let p = Path::new(&path_str);
    if is_plugin_binary_path(p) {
        return probe_plugin_file(p, false)
            .map(|m| m.kind == PluginKind::MenuDriven)
            .unwrap_or(false);
    }
    registry_slot()
        .lock()
        .ok()
        .and_then(|g| g.find_viewer_for_path(p))
        .map(|m| m.kind == PluginKind::MenuDriven)
        .unwrap_or(false)
}

#[no_mangle]
pub unsafe extern "C" fn uipod_rs_plugins_run_by_id(
    plugin_id: *const c_char,
    api: *const UIPodPluginApi,
    file_path: *const c_char,
    err_out: *mut c_char,
    err_cap: usize,
) -> i32 {
    let id = read_cstr(plugin_id);
    let file_str = read_cstr(file_path);
    let lib_path = {
        let Ok(guard) = registry_slot().lock() else {
            return -1;
        };
        match guard.find_by_id(&id) {
            Some(meta) => meta.lib_path,
            None => {
                if !err_out.is_null() && err_cap > 0 {
                    let s = std::slice::from_raw_parts_mut(err_out, err_cap);
                    write_fixed_cstr("Plugin not found", s);
                }
                return -1;
            }
        }
    };

    let opt_file = if file_str.is_empty() {
        None
    } else {
        Some(file_str.as_str())
    };
    match execute_library_path(&lib_path, api, opt_file) {
        Ok(code) => code,
        Err(e) => {
            if !err_out.is_null() && err_cap > 0 {
                let s = std::slice::from_raw_parts_mut(err_out, err_cap);
                write_fixed_cstr(&e, s);
            }
            -2
        }
    }
}

#[no_mangle]
pub unsafe extern "C" fn uipod_rs_plugins_is_plugin_file(path: *const c_char) -> bool {
    let s = read_cstr(path);
    !s.is_empty() && is_plugin_binary_path(Path::new(&s))
}

#[no_mangle]
pub unsafe extern "C" fn uipod_rs_plugins_has_viewer(path: *const c_char) -> bool {
    let s = read_cstr(path);
    if s.is_empty() {
        return false;
    }
    let Ok(guard) = registry_slot().lock() else {
        return false;
    };
    guard.find_viewer_for_path(Path::new(&s)).is_some()
}

#[no_mangle]
pub unsafe extern "C" fn uipod_rs_plugins_open_viewer(
    path: *const c_char,
    api: *const UIPodPluginApi,
    err_out: *mut c_char,
    err_cap: usize,
) -> i32 {
    let s = read_cstr(path);
    if s.is_empty() {
        return -1;
    }
    let p = Path::new(&s);

    // Direct execution when tapping a `.uipod` plugin binary in Browse!
    if is_plugin_binary_path(p) {
        return match execute_library_path(p, api, None) {
            Ok(code) => code,
            Err(e) => {
                if !err_out.is_null() && err_cap > 0 {
                    let sl = std::slice::from_raw_parts_mut(err_out, err_cap);
                    write_fixed_cstr(&e, sl);
                }
                -2
            }
        };
    }

    let viewer_lib = {
        let Ok(guard) = registry_slot().lock() else {
            return -1;
        };
        guard.find_viewer_for_path(p).map(|m| m.lib_path)
    };
    let Some(lib_path) = viewer_lib else {
        return -1;
    };

    match execute_library_path(&lib_path, api, Some(&s)) {
        Ok(code) => code,
        Err(e) => {
            if !err_out.is_null() && err_cap > 0 {
                let sl = std::slice::from_raw_parts_mut(err_out, err_cap);
                write_fixed_cstr(&e, sl);
            }
            -2
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_viewer_extensions_cleanly() {
        let exts = parse_extensions_csv(" .TXT, lrc; Md , log ");
        assert_eq!(exts, vec!["txt", "lrc", "md", "log"]);
        assert!(is_plugin_binary_path(Path::new("/mnt/SDCARD/Plugins/UIPod/brick.UIPOD")));
        assert!(!is_plugin_binary_path(Path::new("/mnt/SDCARD/Music/song.flac")));
    }

    #[test]
    fn dlopen_discovers_and_executes_compiled_plugins() {
        let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let target_debug = manifest_dir.join("../../target/debug");
        let ext = if cfg!(target_os = "macos") {
            "dylib"
        } else {
            "so"
        };

        let candidates = [
            ("libbrick_breaker", "brick_breaker.uipod"),
            ("libuipod_2048", "uipod_2048.uipod"),
            ("libsnake", "snake.uipod"),
            ("libtetris", "tetris.uipod"),
            ("libsound_racer", "sound_racer.uipod"),
            ("libmusic_quiz", "music_quiz.uipod"),
            ("libstopwatch", "stopwatch.uipod"),
            ("libtext_viewer", "text_viewer.uipod"),
            ("liblcd_plasma", "lcd_plasma.uipod"),
        ];

        // If `cargo build --workspace` hasn't produced cdylibs in this profile yet, skip gracefully.
        if !target_debug.join(format!("libtext_viewer.{ext}")).exists()
            || !target_debug.join(format!("libsnake.{ext}")).exists()
            || !target_debug.join(format!("libtetris.{ext}")).exists()
            || !target_debug.join(format!("libsound_racer.{ext}")).exists()
            || !target_debug.join(format!("libmusic_quiz.{ext}")).exists()
        {
            return;
        }

        let tmp_dir = std::env::temp_dir().join(format!("uipod_plg_test_{}", std::process::id()));
        let _ = fs::remove_dir_all(&tmp_dir);
        fs::create_dir_all(&tmp_dir).unwrap();

        for (stem, uipod_name) in candidates {
            let src = target_debug.join(format!("{stem}.{ext}"));
            if src.exists() {
                fs::copy(&src, tmp_dir.join(uipod_name)).unwrap();
            }
        }

        let mut reg = PluginRegistry::new();
        reg.configure_dirs(&tmp_dir, Path::new(""));
        let count = reg.rescan();
        assert_eq!(count, 9);
        assert_eq!(reg.by_category(PluginCategory::Game).len(), 6);
        assert_eq!(reg.by_category(PluginCategory::App).len(), 1);
        assert_eq!(reg.by_category(PluginCategory::Demo).len(), 1);
        assert_eq!(reg.by_category(PluginCategory::Viewer).len(), 1);

        let mq = reg.find_by_id("music_quiz").unwrap();
        assert_eq!(mq.display_name(false), "Music Quiz");
        assert_eq!(mq.display_name(true), "Вгадай мелодію");
        assert_eq!(mq.kind, PluginKind::MenuDriven);

        let sr = reg.find_by_id("sound_racer").unwrap();
        assert_eq!(sr.display_name(false), "Sound Racer");
        assert_eq!(sr.display_name(true), "Sound Racer");
        assert_eq!(sr.kind, PluginKind::MenuDriven);

        let bb = reg.find_by_id("brick_breaker").unwrap();
        assert_eq!(bb.display_name(false), "Brick Breaker");
        assert_eq!(bb.display_name(true), "Цеглинки");
        assert_eq!(bb.kind, PluginKind::MenuDriven);

        let g2048 = reg.find_by_id("uipod_2048").unwrap();
        assert_eq!(g2048.kind, PluginKind::MenuDriven);

        let sn = reg.find_by_id("snake").unwrap();
        assert_eq!(sn.display_name(false), "Snake");
        assert_eq!(sn.display_name(true), "Змійка");
        assert_eq!(sn.kind, PluginKind::MenuDriven);

        let tt = reg.find_by_id("tetris").unwrap();
        assert_eq!(tt.display_name(false), "Tetris");
        assert_eq!(tt.display_name(true), "Тетріс");
        assert_eq!(tt.kind, PluginKind::MenuDriven);

        let sw = reg.find_by_id("stopwatch").unwrap();
        assert_eq!(sw.kind, PluginKind::Utility);

        let pl = reg.find_by_id("lcd_plasma").unwrap();
        assert_eq!(pl.kind, PluginKind::Utility);

        let viewer = reg
            .find_viewer_for_path(Path::new("/mnt/SDCARD/Music/lyrics.LRC"))
            .unwrap();
        assert_eq!(viewer.id, "text_viewer");
        assert_eq!(viewer.version, "0.2.0");
        assert_eq!(viewer.kind, PluginKind::Utility);

        // Invoke `text_viewer` end-to-end with a mock Host API that signals immediate exit
        extern "C" fn m_ticks() -> u64 { 100 }
        extern "C" fn m_sleep(_: u32) {}
        extern "C" fn m_lang() -> u32 { 1 }
        unsafe extern "C" fn m_data_dir(out: *mut c_char, cap: usize) -> usize {
            let p = b"/tmp\0";
            let n = p.len().min(cap);
            std::ptr::copy_nonoverlapping(p.as_ptr() as *const c_char, out, n);
            4
        }
        extern "C" fn m_theme() -> uipod_plugin_sdk::PluginTheme {
            uipod_plugin_sdk::PluginTheme {
                bg: 0xD6D8D5,
                fg: 0x3C3637,
                fg1: 0xA8A8A8,
                fg2: 0x595959,
            }
        }
        unsafe extern "C" fn m_begin(_: u32, _: bool, _: *const c_char) {}
        extern "C" fn m_frect(_: f32, _: f32, _: f32, _: f32, _: f32, _: u32) {}
        extern "C" fn m_srect(_: f32, _: f32, _: f32, _: f32, _: f32, _: f32, _: u32) {}
        extern "C" fn m_circle(_: f32, _: f32, _: f32, _: u32) {}
        extern "C" fn m_line(_: f32, _: f32, _: f32, _: f32, _: f32, _: u32) {}
        unsafe extern "C" fn m_text(
            _: f32, _: f32, _: *const c_char, _: f32, _: u8, _: u8, _: u32,
        ) -> f32 {
            42.0
        }
        unsafe extern "C" fn m_measure(s: *const c_char, fsz: f32, _: u8) -> f32 {
            let txt = CStr::from_ptr(s).to_string_lossy();
            (txt.chars().count() as f32) * fsz * 0.55
        }
        extern "C" fn m_prog(_: f32, _: f32, _: f32, _: f32, _: f32) {}
        extern "C" fn m_scroll(_: f32, _: f32, _: f32, _: f32, _: usize, _: usize, _: usize) {}
        extern "C" fn m_present() {}
        extern "C" fn m_fb() -> *mut u8 { std::ptr::null_mut() }
        extern "C" fn m_poll(_: u32) -> uipod_plugin_sdk::PluginInput {
            uipod_plugin_sdk::PluginInput {
                pressed: uipod_plugin_sdk::btn::MENU,
                held: 0,
                should_exit: false,
                menu_requested: true,
            }
        }
        unsafe extern "C" fn m_menu(
            _: *const c_char,
            _: *const uipod_plugin_sdk::UIPodPluginMenuItem,
            count: usize,
            _: usize,
        ) -> i32 {
            // Select the final menu item ("Exit" / "Вихід")
            if count > 0 {
                (count - 1) as i32
            } else {
                -1
            }
        }
        unsafe extern "C" fn m_kbd(_: *mut c_char, _: usize, _: bool) -> bool { false }
        unsafe extern "C" fn m_splash(_: *const c_char, _: u32) {}
        extern "C" fn m_astat() -> u32 { 0 }
        extern "C" fn m_anop() {}
        extern "C" fn m_aseek(_: u32) {}
        unsafe extern "C" fn m_atrack(
            _: *mut c_char,
            _: usize,
            _: *mut c_char,
            _: usize,
            _: *mut c_char,
            _: usize,
            _: *mut u32,
            _: *mut u32,
        ) -> bool {
            false
        }
        unsafe extern "C" fn m_apcm(_: u32, _: *mut i16, _: usize) -> usize {
            0
        }
        unsafe extern "C" fn m_aplay(_: *const c_char, _: u32) -> bool {
            false
        }
        extern "C" fn m_libcnt() -> usize {
            0
        }
        unsafe extern "C" fn m_libat(
            _: usize,
            _: *mut c_char,
            _: usize,
            _: *mut c_char,
            _: usize,
            _: *mut c_char,
            _: usize,
            _: *mut u32,
        ) -> bool {
            false
        }

        unsafe extern "C" fn m_begin_resp(
            _: u32,
            _: bool,
            _: *const c_char,
            out_w: *mut f32,
            out_h: *mut f32,
        ) {
            if !out_w.is_null() {
                *out_w = 1024.0;
            }
            if !out_h.is_null() {
                *out_h = 768.0;
            }
        }
        unsafe extern "C" fn m_resp_size(out_w: *mut f32, out_h: *mut f32) {
            if !out_w.is_null() {
                *out_w = 1024.0;
            }
            if !out_h.is_null() {
                *out_h = 768.0;
            }
        }

        let mock_api = UIPodPluginApi {
            api_version: UIPOD_PLUGIN_API_VERSION,
            get_ticks_ms: m_ticks,
            sleep_ms: m_sleep,
            get_language: m_lang,
            get_data_dir: m_data_dir,
            get_theme: m_theme,
            canvas_begin: m_begin,
            canvas_fill_rect: m_frect,
            canvas_stroke_rect: m_srect,
            canvas_fill_circle: m_circle,
            canvas_draw_line: m_line,
            canvas_draw_text: m_text,
            canvas_measure_text: m_measure,
            canvas_draw_progress_bar: m_prog,
            canvas_draw_scrollbar: m_scroll,
            canvas_present: m_present,
            fb_ptr: m_fb,
            fb_present: m_present,
            poll_input: m_poll,
            show_menu: m_menu,
            keyboard_input: m_kbd,
            show_splash: m_splash,
            audio_status: m_astat,
            audio_toggle_play_pause: m_anop,
            audio_next: m_anop,
            audio_prev: m_anop,
            audio_seek_ms: m_aseek,
            audio_current_track: m_atrack,
            audio_read_pcm: m_apcm,
            audio_play_track: m_aplay,
            audio_stop: m_anop,
            library_track_count: m_libcnt,
            library_track_at: m_libat,
            canvas_begin_responsive: m_begin_resp,
            get_responsive_size: m_resp_size,
        };

        let sample_txt = tmp_dir.join("hello.txt");
        fs::write(&sample_txt, "Привіт UIPod!\nДругий рядок для тесту.").unwrap();
        let rc = execute_library_path(
            &viewer.lib_path,
            &mock_api,
            Some(sample_txt.to_str().unwrap()),
        )
        .unwrap();
        assert_eq!(rc, 0);

        let _ = fs::remove_dir_all(&tmp_dir);
    }
}
