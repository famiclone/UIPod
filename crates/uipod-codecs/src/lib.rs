//! Pure-Rust audio & game-music codec engine for UIPod (`uipod-codecs`).
//!
//! Currently implements:
//! - **GSF / MiniGSF** (`.gsf`, `.minigsf` + `.gsflib`, PSF version `0x22` — Game Boy Advance)
//! - **MIDI + SoundFont 2** (`.mid`, `.midi`, `.kar`, `.rmi` with embedded `Mobile_0100_base.sf2`
//!   and live hot-swappable custom `.sf2` SoundFonts)
//!
//! Designed with a generic PSF parser (`psf`) and trait-based codec registry so additional
//! game audio formats (`.2sf`/`.mini2sf`, `.psf`/`.minipsf`, `.usf`, `.ssf`, `.snsf`, etc.)
//! plug straight into the same pipeline on TrimUI Brick, Android, and WebAssembly.

pub mod gsf;
pub mod midi;
pub mod nsf_bridge;
pub mod psf;

pub use nsf_bridge::{
    expand_nsf_music_uris, get_nsf_active_chip_flags, get_nsf_apu_telemetry,
    get_nsf_channel_mute_mask, get_nsf_endless_loop, get_nsf_tempo_ratio,
    reset_nsf_runtime_session, set_nsf_channel_mute_mask, set_nsf_endless_loop,
    set_nsf_tempo_ratio, NsfDecoder,
};

use std::ffi::CStr;
use std::io::{Read, Seek, SeekFrom};
use std::os::raw::c_char;
use std::path::Path;
use std::sync::Mutex;

use gsf::apu::OUTPUT_SAMPLE_RATE;
use gsf::GsfDecoder;
use midi::{MidiDecoder, ParsedMidi, MIDI_SAMPLE_RATE};
use psf::{
    infer_track_and_title_from_path, resolve_sibling_path, PsfHeader, PsfTags, PSF_VERSION_GSF,
};

pub trait RustAudioDecoder: Send {
    fn sample_rate(&self) -> u32;
    fn duration_ms(&self) -> u32;
    fn elapsed_ms(&self) -> u32;
    fn seek_ms(&mut self, target_ms: u32);
    fn seek_step_ms(&mut self, target_ms: u32, _max_step_ms: u32) -> bool {
        self.seek_ms(target_ms);
        true
    }
    fn render_stereo_s16(&mut self, out_interleaved: &mut [i16], loop_forever: bool) -> usize;
}

impl RustAudioDecoder for GsfDecoder {
    fn sample_rate(&self) -> u32 {
        OUTPUT_SAMPLE_RATE
    }

    fn duration_ms(&self) -> u32 {
        GsfDecoder::duration_ms(self)
    }

    fn elapsed_ms(&self) -> u32 {
        GsfDecoder::elapsed_ms(self)
    }

    fn seek_ms(&mut self, target_ms: u32) {
        GsfDecoder::seek_ms(self, target_ms);
    }

    fn seek_step_ms(&mut self, target_ms: u32, _max_step_ms: u32) -> bool {
        GsfDecoder::seek_step_ms(self, target_ms, _max_step_ms)
    }

    fn render_stereo_s16(&mut self, out_interleaved: &mut [i16], loop_forever: bool) -> usize {
        GsfDecoder::render_stereo_s16(self, out_interleaved, loop_forever)
    }
}

impl RustAudioDecoder for MidiDecoder {
    fn sample_rate(&self) -> u32 {
        MIDI_SAMPLE_RATE
    }

    fn duration_ms(&self) -> u32 {
        MidiDecoder::duration_ms(self)
    }

    fn elapsed_ms(&self) -> u32 {
        MidiDecoder::elapsed_ms(self)
    }

    fn seek_ms(&mut self, target_ms: u32) {
        MidiDecoder::seek_ms(self, target_ms);
    }

    fn render_stereo_s16(&mut self, out_interleaved: &mut [i16], loop_forever: bool) -> usize {
        MidiDecoder::render_stereo_s16(self, out_interleaved, loop_forever)
    }
}

static ACTIVE_DECODER: Mutex<Option<Box<dyn RustAudioDecoder>>> = Mutex::new(None);
static LIB_TAG_CACHE: Mutex<Option<(String, PsfTags)>> = Mutex::new(None);

pub fn is_supported_codec_name(name: &str) -> bool {
    name.eq_ignore_ascii_case("gsf")
        || name.eq_ignore_ascii_case("midi")
        || name.eq_ignore_ascii_case("nsf")
}

fn open_decoder_by_name(
    codec_name: &str,
    track_path: &str,
    bytes: &[u8],
) -> Result<Box<dyn RustAudioDecoder>, &'static str> {
    if codec_name.eq_ignore_ascii_case("nsf") {
        let dec = NsfDecoder::open(track_path, bytes)?;
        return Ok(Box::new(dec));
    }
    // Switching to a non-NSF Rust codec resets `channel_mute_mask` to 0 and clears active flags
    nsf_bridge::reset_nsf_runtime_session();
    if codec_name.eq_ignore_ascii_case("gsf") {
        let img = gsf::load_gsf_from_disk(bytes, track_path)?;
        return Ok(Box::new(GsfDecoder::from_image(img)));
    }
    if codec_name.eq_ignore_ascii_case("midi") {
        let parsed = ParsedMidi::parse(bytes)?;
        return Ok(Box::new(MidiDecoder::new(parsed)));
    }
    Err("Unsupported Rust codec")
}

/// Read only the `[TAG]` trailer of a `.gsflib` on disk without reading/decompressing its ROM payload.
fn read_gsflib_tags_fast(track_path: &str, lib_rel: &str) -> Option<PsfTags> {
    let base_dir = Path::new(track_path).parent()?;
    let lib_path = resolve_sibling_path(base_dir, lib_rel)?;
    let key = lib_path.to_string_lossy().into_owned();

    if let Ok(guard) = LIB_TAG_CACHE.lock() {
        if let Some((ref cached_key, ref cached_tags)) = *guard {
            if *cached_key == key {
                return Some(cached_tags.clone());
            }
        }
    }

    let mut f = std::fs::File::open(&lib_path).ok()?;
    let file_len = f.metadata().ok()?.len();
    if file_len < 16 {
        return None;
    }
    let mut hdr_buf = [0u8; 16];
    f.read_exact(&mut hdr_buf).ok()?;
    let hdr = PsfHeader::parse(&hdr_buf).ok()?;
    let tag_off = hdr.tag_offset()?;
    let tags = if tag_off < file_len {
        let tag_len = (file_len - tag_off).min(32_768) as usize;
        f.seek(SeekFrom::Start(tag_off)).ok()?;
        let mut tag_buf = vec![0u8; tag_len];
        f.read_exact(&mut tag_buf).ok()?;
        PsfTags::parse_tag_section(&tag_buf)
    } else {
        PsfTags::default()
    };

    if let Ok(mut guard) = LIB_TAG_CACHE.lock() {
        *guard = Some((key, tags.clone()));
    }
    Some(tags)
}

// ============================================================================
// C FFI for Rockbox / UIPod (`apps/codecs.c` & `lib/rbcodec/metadata/gsf.c`)
// ============================================================================

#[repr(C)]
pub struct UipodRsTrackMeta {
    pub title: [u8; 256],
    pub artist: [u8; 256],
    pub album: [u8; 256],
    pub genre: [u8; 128],
    pub composer: [u8; 256],
    pub comment: [u8; 256],
    pub year: i32,
    pub track_num: i32,
    pub duration_ms: u32,
    pub sample_rate: u32,
    pub bitrate_kbps: u32,
}

pub(crate) fn copy_str_to_c_buf(src: Option<&str>, dst: &mut [u8]) {
    dst.fill(0);
    let Some(s) = src else { return };
    let s = s.trim();
    if s.is_empty() || dst.len() < 2 {
        return;
    }
    let max = dst.len() - 1;
    let bytes = s.as_bytes();
    let mut len = bytes.len().min(max);
    // Do not split a multi-byte UTF-8 codepoint at the boundary
    while len > 0 && !s.is_char_boundary(len) {
        len -= 1;
    }
    dst[..len].copy_from_slice(&bytes[..len]);
    dst[len] = 0;
}

pub(crate) unsafe fn cstr_to_str<'a>(ptr: *const c_char) -> &'a str {
    if ptr.is_null() {
        ""
    } else {
        unsafe { CStr::from_ptr(ptr) }.to_str().unwrap_or("")
    }
}

#[no_mangle]
pub unsafe extern "C" fn uipod_rs_codec_is_builtin(codec_name: *const c_char) -> i32 {
    let name = unsafe { cstr_to_str(codec_name) };
    if is_supported_codec_name(name) {
        1
    } else {
        0
    }
}

#[no_mangle]
pub unsafe extern "C" fn uipod_rs_codec_open(
    codec_name: *const c_char,
    track_path: *const c_char,
    data: *const u8,
    data_len: usize,
) -> i32 {
    let res = std::panic::catch_unwind(|| {
        let name = unsafe { cstr_to_str(codec_name) };
        let path = unsafe { cstr_to_str(track_path) };
        let bytes: &[u8] = if !data.is_null() && data_len > 0 {
            unsafe { std::slice::from_raw_parts(data, data_len) }
        } else if !path.is_empty() {
            // Fallback: read directly from physical file path (stripping any `.nsf#<idx>` suffix)
            let phys_path = uipod_nsf::SubtrackUri::parse(path).file_path;
            return match std::fs::read(phys_path) {
                Ok(vec) => match open_decoder_by_name(name, path, &vec) {
                    Ok(dec) => {
                        if let Ok(mut guard) = ACTIVE_DECODER.lock() {
                            *guard = Some(dec);
                            0
                        } else {
                            -1
                        }
                    }
                    Err(_) => -1,
                },
                Err(_) => -1,
            };
        } else {
            return -1;
        };

        match open_decoder_by_name(name, path, bytes) {
            Ok(dec) => {
                if let Ok(mut guard) = ACTIVE_DECODER.lock() {
                    *guard = Some(dec);
                    0
                } else {
                    -1
                }
            }
            Err(_) => -1,
        }
    });
    res.unwrap_or(-1)
}

#[no_mangle]
pub extern "C" fn uipod_rs_codec_sample_rate() -> u32 {
    if let Ok(guard) = ACTIVE_DECODER.lock() {
        if let Some(ref dec) = *guard {
            return dec.sample_rate();
        }
    }
    OUTPUT_SAMPLE_RATE
}

#[no_mangle]
pub extern "C" fn uipod_rs_codec_duration_ms() -> u32 {
    if let Ok(guard) = ACTIVE_DECODER.lock() {
        if let Some(ref dec) = *guard {
            return dec.duration_ms();
        }
    }
    0
}

#[no_mangle]
pub extern "C" fn uipod_rs_codec_elapsed_ms() -> u32 {
    if let Ok(guard) = ACTIVE_DECODER.lock() {
        if let Some(ref dec) = *guard {
            return dec.elapsed_ms();
        }
    }
    0
}

#[no_mangle]
pub extern "C" fn uipod_rs_codec_seek_ms(target_ms: u32) -> i32 {
    let res = std::panic::catch_unwind(|| {
        if let Ok(mut guard) = ACTIVE_DECODER.lock() {
            if let Some(ref mut dec) = *guard {
                dec.seek_ms(target_ms);
                return 0;
            }
        }
        -1
    });
    res.unwrap_or(-1)
}

#[no_mangle]
pub extern "C" fn uipod_rs_codec_seek_step_ms(target_ms: u32, max_step_ms: u32) -> i32 {
    let res = std::panic::catch_unwind(|| {
        if let Ok(mut guard) = ACTIVE_DECODER.lock() {
            if let Some(ref mut dec) = *guard {
                return if dec.seek_step_ms(target_ms, max_step_ms) {
                    1
                } else {
                    0
                };
            }
        }
        -1
    });
    res.unwrap_or(-1)
}

#[no_mangle]
pub unsafe extern "C" fn uipod_rs_codec_render_s16(
    out_stereo_s16: *mut i16,
    max_frames: usize,
    loop_forever: i32,
) -> i32 {
    if out_stereo_s16.is_null() || max_frames == 0 {
        return 0;
    }
    let res = std::panic::catch_unwind(|| {
        let out_slice = unsafe { std::slice::from_raw_parts_mut(out_stereo_s16, max_frames * 2) };
        if let Ok(mut guard) = ACTIVE_DECODER.lock() {
            if let Some(ref mut dec) = *guard {
                let n = dec.render_stereo_s16(out_slice, loop_forever != 0);
                return n as i32;
            }
        }
        -1
    });
    res.unwrap_or(-1)
}

#[no_mangle]
pub extern "C" fn uipod_rs_codec_close() {
    if let Ok(mut guard) = ACTIVE_DECODER.lock() {
        *guard = None;
    }
}

#[no_mangle]
pub unsafe extern "C" fn uipod_rs_gsf_parse_metadata(
    header_16: *const u8,
    tag_bytes: *const u8,
    tag_len: usize,
    track_path: *const c_char,
    out_meta: *mut UipodRsTrackMeta,
) -> i32 {
    if header_16.is_null() || out_meta.is_null() {
        return 0;
    }
    let res = std::panic::catch_unwind(|| {
        let hdr_slice = unsafe { std::slice::from_raw_parts(header_16, 16) };
        let Ok(hdr) = PsfHeader::parse(hdr_slice) else {
            return 0;
        };
        if hdr.version != PSF_VERSION_GSF {
            return 0;
        }

        let mut tags = if !tag_bytes.is_null() && tag_len >= 5 {
            let tag_slice = unsafe { std::slice::from_raw_parts(tag_bytes, tag_len) };
            PsfTags::parse_tag_section(tag_slice)
        } else {
            PsfTags::default()
        };

        let path_str = unsafe { cstr_to_str(track_path) };
        if !path_str.is_empty() {
            if let Some(ref lib_rel) = tags.lib {
                if tags.game.is_none() || tags.artist.is_none() || tags.year.is_none() {
                    if let Some(parent_tags) = read_gsflib_tags_fast(path_str, lib_rel) {
                        tags.merge_fallback_from(&parent_tags);
                    }
                }
            }
            let (inf_track, inf_title) = infer_track_and_title_from_path(path_str);
            if tags.track_num.is_none() {
                tags.track_num = inf_track;
            }
            if tags.title.is_none() {
                tags.title = inf_title;
            }
        }

        let meta = unsafe { &mut *out_meta };
        copy_str_to_c_buf(tags.title.as_deref(), &mut meta.title);
        copy_str_to_c_buf(tags.artist.as_deref(), &mut meta.artist);
        copy_str_to_c_buf(tags.game.as_deref(), &mut meta.album);
        copy_str_to_c_buf(
            tags.genre.as_deref().or(Some("Game")),
            &mut meta.genre,
        );
        copy_str_to_c_buf(
            tags.copyright.as_deref().or(tags.dumper.as_deref()),
            &mut meta.composer,
        );
        copy_str_to_c_buf(tags.comment.as_deref(), &mut meta.comment);
        meta.year = tags.year.unwrap_or(0);
        meta.track_num = tags.track_num.unwrap_or(-1);
        meta.duration_ms = tags.total_duration_ms();
        meta.sample_rate = OUTPUT_SAMPLE_RATE;
        meta.bitrate_kbps = 1411;
        1
    });
    res.unwrap_or(0)
}

#[no_mangle]
pub unsafe extern "C" fn uipod_rs_midi_parse_metadata(
    data: *const u8,
    data_len: usize,
    track_path: *const c_char,
    out_meta: *mut UipodRsTrackMeta,
) -> i32 {
    if data.is_null() || data_len < 14 || out_meta.is_null() {
        return 0;
    }
    let res = std::panic::catch_unwind(|| {
        let slice = unsafe { std::slice::from_raw_parts(data, data_len) };
        let Ok(parsed) = ParsedMidi::parse(slice) else {
            return 0;
        };

        let path_str = unsafe { cstr_to_str(track_path) };
        let (inf_track, inf_title) = if !path_str.is_empty() {
            infer_track_and_title_from_path(path_str)
        } else {
            (None, None)
        };

        let meta = unsafe { &mut *out_meta };
        let title = parsed.meta.title.as_deref().or(inf_title.as_deref());
        copy_str_to_c_buf(title, &mut meta.title);
        copy_str_to_c_buf(parsed.meta.artist.as_deref(), &mut meta.artist);
        copy_str_to_c_buf(parsed.meta.album.as_deref(), &mut meta.album);
        copy_str_to_c_buf(Some("MIDI"), &mut meta.genre);
        copy_str_to_c_buf(None, &mut meta.composer);
        copy_str_to_c_buf(parsed.meta.comment.as_deref(), &mut meta.comment);
        meta.year = 0;
        meta.track_num = inf_track.unwrap_or(-1);
        meta.duration_ms = parsed.meta.duration_ms;
        meta.sample_rate = MIDI_SAMPLE_RATE;
        meta.bitrate_kbps = 1411;
        1
    });
    res.unwrap_or(0)
}

/// Set active SoundFont (`.sf2`) file path, or pass NULL / `""` to restore bundled `Mobile_0100_base.sf2`.
/// Returns `1` on success, `0` if the `.sf2` file could not be read or parsed.
#[no_mangle]
pub unsafe extern "C" fn uipod_rs_midi_set_soundfont(sf2_path: *const c_char) -> i32 {
    let res = std::panic::catch_unwind(|| {
        let path_str = unsafe { cstr_to_str(sf2_path) };
        match midi::set_custom_soundfont_from_disk(path_str) {
            Ok(()) => 1,
            Err(_) => 0,
        }
    });
    res.unwrap_or(0)
}

/// Copy active custom `.sf2` path into `out_buf` (returns `1` if custom `.sf2` is active,
/// or `0` if bundled `Mobile_0100_base.sf2` is active).
#[no_mangle]
pub unsafe extern "C" fn uipod_rs_midi_get_soundfont(
    out_buf: *mut c_char,
    buf_len: usize,
) -> i32 {
    if !out_buf.is_null() && buf_len > 0 {
        let dst = unsafe { std::slice::from_raw_parts_mut(out_buf as *mut u8, buf_len) };
        dst[0] = 0;
        if let Some(custom) = midi::current_custom_soundfont_path() {
            copy_str_to_c_buf(Some(&custom), dst);
            return 1;
        }
        return 0;
    }
    if midi::current_custom_soundfont_path().is_some() {
        1
    } else {
        0
    }
}
