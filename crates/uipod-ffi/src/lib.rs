#![allow(clippy::missing_safety_doc)]

pub use uipod_codecs as codecs;
pub use uipod_dj as dj;
pub use uipod_jellyfin as jellyfin;
pub use uipod_lastfm as lastfm;
pub use uipod_navidrome as navidrome;
pub use uipod_nsf as nsf;
pub use uipod_plugins as plugins;
pub use uipod_ui as ui;

use std::ffi::CStr;
use std::os::raw::c_char;
use std::sync::atomic::{AtomicU32, AtomicU8, Ordering};
use std::sync::Mutex;

use uipod_dj::{
    should_respect_album_gapless, CrossfadeMode, DjConfig, DjTelemetry, DualDeckMixer,
    MaxTempoStretch, MixerPhase, TransitionLength,
};

static DJ_MIXER: Mutex<Option<DualDeckMixer>> = Mutex::new(None);
static DJ_CFG_MODE: AtomicU8 = AtomicU8::new(0);
static DJ_CFG_KEEP_GAPLESS: AtomicU8 = AtomicU8::new(1);

// Lock-free 60 FPS DJ Telemetry SeqLock for Now Playing UI
static DJ_TEL_SEQ: AtomicU32 = AtomicU32::new(0);
static DJ_TEL_WORDS: [AtomicU32; 5] = [
    AtomicU32::new(0),
    AtomicU32::new(0),
    AtomicU32::new(0),
    AtomicU32::new(0),
    AtomicU32::new(0),
];

#[inline]
unsafe fn ffi_cstr<'a>(ptr: *const c_char) -> &'a str {
    if ptr.is_null() {
        ""
    } else {
        unsafe { CStr::from_ptr(ptr) }.to_str().unwrap_or("")
    }
}

fn publish_dj_telemetry(tel: DjTelemetry) {
    let seq = DJ_TEL_SEQ.load(Ordering::Relaxed);
    let odd = (seq | 1).wrapping_add(if (seq & 1) != 0 { 2 } else { 0 });
    DJ_TEL_SEQ.store(odd, Ordering::Release);

    let w0 = (tel.active as u32)
        | ((tel.mix_kind as u32) << 8)
        | ((tel.bass_swap_flash as u32) << 16);
    DJ_TEL_WORDS[0].store(w0, Ordering::Relaxed);
    DJ_TEL_WORDS[1].store(tel.progress.to_bits(), Ordering::Relaxed);
    DJ_TEL_WORDS[2].store(tel.bpm_a.to_bits(), Ordering::Relaxed);
    DJ_TEL_WORDS[3].store(tel.bpm_b.to_bits(), Ordering::Relaxed);
    DJ_TEL_WORDS[4].store(tel.current_bpm.to_bits(), Ordering::Relaxed);

    DJ_TEL_SEQ.store(odd.wrapping_add(1), Ordering::Release);
}

fn read_dj_telemetry() -> DjTelemetry {
    for _ in 0..8 {
        let s1 = DJ_TEL_SEQ.load(Ordering::Acquire);
        if (s1 & 1) != 0 {
            std::hint::spin_loop();
            continue;
        }
        let w0 = DJ_TEL_WORDS[0].load(Ordering::Relaxed);
        let w1 = DJ_TEL_WORDS[1].load(Ordering::Relaxed);
        let w2 = DJ_TEL_WORDS[2].load(Ordering::Relaxed);
        let w3 = DJ_TEL_WORDS[3].load(Ordering::Relaxed);
        let w4 = DJ_TEL_WORDS[4].load(Ordering::Relaxed);
        std::sync::atomic::fence(Ordering::Acquire);
        if s1 == DJ_TEL_SEQ.load(Ordering::Relaxed) {
            return DjTelemetry {
                active: (w0 & 0xFF) as u8,
                mix_kind: ((w0 >> 8) & 0xFF) as u8,
                bass_swap_flash: ((w0 >> 16) & 0xFF) as u8,
                progress: f32::from_bits(w1),
                bpm_a: f32::from_bits(w2),
                bpm_b: f32::from_bits(w3),
                current_bpm: f32::from_bits(w4),
            };
        }
    }
    DjTelemetry::default()
}

fn with_mixer<R>(f: impl FnOnce(&mut DualDeckMixer) -> R, default_val: R) -> R {
    if let Ok(mut guard) = DJ_MIXER.lock() {
        let mixer = guard.get_or_insert_with(|| DualDeckMixer::new(44_100));
        f(mixer)
    } else {
        default_val
    }
}

#[no_mangle]
pub extern "C" fn uipod_rs_dj_set_config(
    mode: u8,
    length: u8,
    max_stretch: u8,
    keep_album_gapless: i32,
    quick_cut_on_next: i32,
) {
    let cfg = DjConfig {
        mode: CrossfadeMode::from_u8(mode),
        length: TransitionLength::from_u8(length),
        max_stretch: MaxTempoStretch::from_u8(max_stretch),
        keep_album_gapless: keep_album_gapless != 0,
        quick_cut_on_next: quick_cut_on_next != 0,
    };
    DJ_CFG_MODE.store(cfg.mode as u8, Ordering::Relaxed);
    DJ_CFG_KEEP_GAPLESS.store(u8::from(cfg.keep_album_gapless), Ordering::Relaxed);
    with_mixer(
        |m| {
            m.set_config(cfg);
            if cfg.mode == CrossfadeMode::Off {
                m.reset();
                uipod_codecs::set_nsf_tempo_ratio(1.0);
                publish_dj_telemetry(DjTelemetry::default());
            }
        },
        (),
    );
}

#[no_mangle]
pub extern "C" fn uipod_rs_dj_get_phase() -> i32 {
    if DJ_CFG_MODE.load(Ordering::Relaxed) == 0 {
        return 0;
    }
    with_mixer(|m| m.phase() as i32, 0)
}

#[no_mangle]
pub extern "C" fn uipod_rs_dj_reset() {
    uipod_codecs::set_nsf_tempo_ratio(1.0);
    publish_dj_telemetry(DjTelemetry::default());
    with_mixer(|m| m.reset(), ());
}

#[no_mangle]
pub unsafe extern "C" fn uipod_rs_dj_should_gapless_album(
    shuffle_active: i32,
    album_a: *const c_char,
    track_a: i32,
    album_b: *const c_char,
    track_b: i32,
) -> i32 {
    let keep = DJ_CFG_KEEP_GAPLESS.load(Ordering::Relaxed) != 0;
    let aa = unsafe { ffi_cstr(album_a) };
    let ab = unsafe { ffi_cstr(album_b) };
    i32::from(should_respect_album_gapless(
        keep,
        shuffle_active != 0,
        aa,
        track_a,
        ab,
        track_b,
    ))
}

#[no_mangle]
pub unsafe extern "C" fn uipod_rs_dj_begin_deck_a(
    sample_rate: u32,
    album_a: *const c_char,
    track_a: i32,
    shuffle_active: i32,
    manual_quick_cut: i32,
) {
    let aa = unsafe { ffi_cstr(album_a) };
    uipod_codecs::set_nsf_tempo_ratio(1.0);
    publish_dj_telemetry(DjTelemetry::default());
    with_mixer(
        |m| {
            m.begin_deck_a_capture(
                sample_rate,
                aa,
                track_a,
                shuffle_active != 0,
                manual_quick_cut != 0,
            );
        },
        (),
    );
}

#[no_mangle]
pub unsafe extern "C" fn uipod_rs_dj_push_deck_a(
    stereo_s16: *const i16,
    frames: usize,
    spill_out: *mut i16,
    spill_cap_frames: usize,
) -> usize {
    if stereo_s16.is_null() || frames == 0 {
        return 0;
    }
    let src = unsafe { std::slice::from_raw_parts(stereo_s16, frames * 2) };
    let mut spilled_frames = 0usize;
    with_mixer(
        |m| {
            m.push_deck_a_tail(src, |spilled_slice| {
                if !spill_out.is_null() && spill_cap_frames > spilled_frames {
                    let avail_cap = (spill_cap_frames - spilled_frames) * 2;
                    let copy_len = spilled_slice.len().min(avail_cap) & !1;
                    if copy_len > 0 {
                        let dst = unsafe {
                            std::slice::from_raw_parts_mut(
                                spill_out.add(spilled_frames * 2),
                                copy_len,
                            )
                        };
                        dst.copy_from_slice(&spilled_slice[..copy_len]);
                        spilled_frames += copy_len / 2;
                    }
                }
            });
        },
        (),
    );
    spilled_frames
}

#[no_mangle]
pub unsafe extern "C" fn uipod_rs_dj_finish_deck_a_begin_deck_b(
    album_b: *const c_char,
    track_b: i32,
    deck_b_is_nsf: i32,
) {
    let ab = unsafe { ffi_cstr(album_b) };
    with_mixer(
        |m| m.finish_deck_a_and_begin_deck_b(ab, track_b, deck_b_is_nsf != 0),
        (),
    );
}

#[no_mangle]
pub unsafe extern "C" fn uipod_rs_dj_push_deck_b(
    stereo_s16: *const i16,
    frames: usize,
    is_eof: i32,
) -> i32 {
    let src: &[i16] = if stereo_s16.is_null() || frames == 0 {
        &[]
    } else {
        unsafe { std::slice::from_raw_parts(stereo_s16, frames * 2) }
    };
    i32::from(with_mixer(|m| m.push_deck_b_frames(src, is_eof != 0), false))
}

#[no_mangle]
pub unsafe extern "C" fn uipod_rs_dj_render_s16(
    out_stereo_s16: *mut i16,
    max_frames: usize,
    out_midpoint_crossed: *mut i32,
) -> usize {
    if out_stereo_s16.is_null() || max_frames == 0 {
        return 0;
    }
    let dst = unsafe { std::slice::from_raw_parts_mut(out_stereo_s16, max_frames * 2) };
    let (written, mid, tel, nsf_tempo) = with_mixer(
        |m| {
            let (w, crossed) = m.render_s16(dst);
            let t = m.telemetry();
            let nsf_r = if m.phase() == MixerPhase::Mixing {
                m.recommended_nsf_tempo_b()
            } else {
                1.0
            };
            (w, crossed, t, nsf_r)
        },
        (0, false, DjTelemetry::default(), 1.0),
    );

    uipod_codecs::set_nsf_tempo_ratio(nsf_tempo);
    publish_dj_telemetry(tel);

    if !out_midpoint_crossed.is_null() {
        unsafe {
            *out_midpoint_crossed = i32::from(mid);
        }
    }
    written
}

#[no_mangle]
pub unsafe extern "C" fn uipod_rs_dj_get_telemetry(out_tel: *mut DjTelemetry) -> i32 {
    if out_tel.is_null() {
        return 0;
    }
    let tel = read_dj_telemetry();
    unsafe {
        *out_tel = tel;
    }
    i32::from(tel.active != 0)
}
