//! Multi-format streaming audio decoder for Manual Dual-Deck DJ RAM window refills.
//!
//! Decodes `MP3`, `FLAC`, `OGG/Vorbis`, `WAV`, `AIFF`, `M4A/AAC/ALAC` (via `symphonia`),
//! `.nsf`/`.nsfe` (`uipod-nsf`), and `.gsf`/`.minigsf`/`.mid`/`.midi` (`uipod-codecs`)
//! into stereo `S16` frames resampled to the active mixer sample rate.

#[cfg(not(target_arch = "wasm32"))]
use std::f64::consts::PI as PI64;
#[cfg(not(target_arch = "wasm32"))]
use std::fs::File;
#[cfg(not(target_arch = "wasm32"))]
use std::path::Path;
#[cfg(not(target_arch = "wasm32"))]
use std::sync::OnceLock;

#[cfg(not(target_arch = "wasm32"))]
use symphonia::core::audio::SampleBuffer;
#[cfg(not(target_arch = "wasm32"))]
use symphonia::core::codecs::{Decoder, DecoderOptions, CODEC_TYPE_NULL, CODEC_TYPE_OPUS};
#[cfg(not(target_arch = "wasm32"))]
use symphonia::core::formats::{FormatOptions, FormatReader, SeekMode, SeekTo};
#[cfg(not(target_arch = "wasm32"))]
use symphonia::core::io::MediaSourceStream;
#[cfg(not(target_arch = "wasm32"))]
use symphonia::core::meta::MetadataOptions;
#[cfg(not(target_arch = "wasm32"))]
use symphonia::core::probe::Hint;
#[cfg(not(target_arch = "wasm32"))]
use symphonia::core::units::Time;

#[cfg(not(target_arch = "wasm32"))]
use uipod_codecs::RustAudioDecoder;
#[cfg(not(target_arch = "wasm32"))]
use uipod_nsf::{is_nsf_path, NsfEngine, SubtrackUri};

#[cfg(not(target_arch = "wasm32"))]
use crate::resampler::VinylResampler;

#[cfg(not(target_arch = "wasm32"))]
#[derive(Clone, Copy)]
pub struct OpusHostFns {
    pub get_size: unsafe extern "C" fn(channels: i32) -> i32,
    pub init: unsafe extern "C" fn(st: *mut u8, fs: i32, channels: i32) -> i32,
    pub decode: unsafe extern "C" fn(
        st: *mut u8,
        data: *const u8,
        len: i32,
        pcm: *mut i16,
        frame_size: i32,
        decode_fec: i32,
    ) -> i32,
}

#[cfg(not(target_arch = "wasm32"))]
static OPUS_HOST_FNS: OnceLock<OpusHostFns> = OnceLock::new();

#[cfg(not(target_arch = "wasm32"))]
pub fn register_opus_decoder_fns(fns: OpusHostFns) {
    let _ = OPUS_HOST_FNS.set(fns);
}

#[cfg(not(target_arch = "wasm32"))]
#[derive(Clone, Copy, Default)]
struct StereoAntiAliasBiquad {
    enabled: bool,
    b0: f32,
    b1: f32,
    b2: f32,
    a1: f32,
    a2: f32,
    z1_l: f32,
    z2_l: f32,
    z1_r: f32,
    z2_r: f32,
}

#[cfg(not(target_arch = "wasm32"))]
impl StereoAntiAliasBiquad {
    fn new(src_sr: u32, dst_sr: u32) -> Self {
        if src_sr <= dst_sr || dst_sr == 0 {
            return Self::default();
        }
        let fc_hz = 0.44 * (dst_sr as f64);
        let w0 = 2.0 * PI64 * fc_hz / (src_sr as f64);
        let cw = w0.cos();
        let alpha = w0.sin() / std::f64::consts::SQRT_2;
        let b0 = (1.0 - cw) * 0.5;
        let b1 = 1.0 - cw;
        let b2 = (1.0 - cw) * 0.5;
        let inv_a0 = 1.0 / (1.0 + alpha);
        Self {
            enabled: true,
            b0: (b0 * inv_a0) as f32,
            b1: (b1 * inv_a0) as f32,
            b2: (b2 * inv_a0) as f32,
            a1: ((-2.0 * cw) * inv_a0) as f32,
            a2: ((1.0 - alpha) * inv_a0) as f32,
            z1_l: 0.0,
            z2_l: 0.0,
            z1_r: 0.0,
            z2_r: 0.0,
        }
    }

    #[inline]
    fn reset(&mut self) {
        self.z1_l = 0.0;
        self.z2_l = 0.0;
        self.z1_r = 0.0;
        self.z2_r = 0.0;
    }

    #[inline]
    fn filter_in_place(&mut self, interleaved: &mut [f32]) {
        if !self.enabled {
            return;
        }
        let frames = interleaved.len() / 2;
        for i in 0..frames {
            let xl = interleaved[i * 2];
            let yl = self.b0 * xl + self.z1_l;
            self.z1_l = self.b1 * xl - self.a1 * yl + self.z2_l;
            self.z2_l = self.b2 * xl - self.a2 * yl;
            interleaved[i * 2] = yl;

            let xr = interleaved[i * 2 + 1];
            let yr = self.b0 * xr + self.z1_r;
            self.z1_r = self.b1 * xr - self.a1 * yr + self.z2_r;
            self.z2_r = self.b2 * xr - self.a2 * yr;
            interleaved[i * 2 + 1] = yr;
        }
    }
}

#[cfg(not(target_arch = "wasm32"))]
enum InnerDecoder {
    Symphonia {
        format: Box<dyn FormatReader>,
        decoder: Box<dyn Decoder>,
        track_id: u32,
        src_sr: u32,
        duration_ms: u32,
        sample_buf: Option<SampleBuffer<f32>>,
        pkt_fifo: Vec<f32>,
        pkt_read: usize,
        eof: bool,
    },
    OggOpus {
        format: Box<dyn FormatReader>,
        track_id: u32,
        channels: usize,
        pre_skip: usize,
        skip_rem: usize,
        duration_ms: u32,
        state_aligned: Vec<u64>,
        fns: OpusHostFns,
        pkt_scratch: Vec<i16>,
        eof: bool,
    },
    Nsf {
        engine: Box<NsfEngine>,
        src_sr: u32,
        duration_ms: u32,
        scratch: Vec<i16>,
        eof: bool,
    },
    RustCodec {
        dec: Box<dyn RustAudioDecoder>,
        src_sr: u32,
        duration_ms: u32,
        scratch: Vec<i16>,
        eof: bool,
    },
}

#[cfg(not(target_arch = "wasm32"))]
pub struct DeckFileDecoder {
    inner: InnerDecoder,
    dst_sr: u32,
    aa_filter: StereoAntiAliasBiquad,
    rate_conv: VinylResampler,
    src_fifo: Vec<f32>,
    src_read: usize,
    resamp_scratch: Vec<f32>,
    decoded_dst_frame: u64,
}

#[cfg(not(target_arch = "wasm32"))]
impl DeckFileDecoder {
    pub fn open(track_path: &str, dst_sr: u32, duration_hint_ms: u32) -> Option<Self> {
        let dst_sr = dst_sr.clamp(8_000, 192_000);
        let uri = SubtrackUri::parse(track_path.trim());
        let phys_path = Path::new(uri.file_path);
        if !phys_path.is_file() {
            return None;
        }

        // 1. Native `.nsf` / `.nsfe` chiptunes (renders directly at `dst_sr`)
        if is_nsf_path(track_path) {
            let bytes = std::fs::read(phys_path).ok()?;
            let mut engine = NsfEngine::load_with_meta(&bytes, dst_sr, None).ok()?;
            let sub_idx = uri
                .subtrack_index
                .filter(|&i| i < engine.container_meta().total_subtracks)
                .unwrap_or_else(|| engine.first_music_subtrack());
            engine.set_endless_loop(false);
            engine.select_subtrack(sub_idx).ok()?;
            let dur_ms = if duration_hint_ms > 0 {
                duration_hint_ms
            } else {
                engine.total_duration_ms().max(5_000)
            };
            return Some(Self {
                inner: InnerDecoder::Nsf {
                    engine: Box::new(engine),
                    src_sr: dst_sr,
                    duration_ms: dur_ms,
                    scratch: vec![0i16; 2048 * 2],
                    eof: false,
                },
                dst_sr,
                aa_filter: StereoAntiAliasBiquad::default(),
                rate_conv: VinylResampler::new(),
                src_fifo: Vec::new(),
                src_read: 0,
                resamp_scratch: Vec::new(),
                decoded_dst_frame: 0,
            });
        }

        // 2. Native `.gsf` / `.minigsf` / `.mid` / `.midi`
        if let Some(ext) = phys_path.extension().and_then(|e| e.to_str()) {
            if ext.eq_ignore_ascii_case("gsf") || ext.eq_ignore_ascii_case("minigsf") {
                let bytes = std::fs::read(phys_path).ok()?;
                let img = uipod_codecs::gsf::load_gsf_from_disk(&bytes, uri.file_path).ok()?;
                let dec = uipod_codecs::gsf::GsfDecoder::from_image(img);
                let src_sr = dec.sample_rate().max(8_000);
                let dur_ms = if duration_hint_ms > 0 {
                    duration_hint_ms
                } else {
                    dec.duration_ms().max(30_000)
                };
                return Some(Self {
                    inner: InnerDecoder::RustCodec {
                        dec: Box::new(dec),
                        src_sr,
                        duration_ms: dur_ms,
                        scratch: vec![0i16; 2048 * 2],
                        eof: false,
                    },
                    dst_sr,
                    aa_filter: StereoAntiAliasBiquad::new(src_sr, dst_sr),
                    rate_conv: VinylResampler::new(),
                    src_fifo: Vec::new(),
                    src_read: 0,
                    resamp_scratch: vec![0.0; 8192 * 2],
                    decoded_dst_frame: 0,
                });
            }
            if ext.eq_ignore_ascii_case("mid")
                || ext.eq_ignore_ascii_case("midi")
                || ext.eq_ignore_ascii_case("kar")
                || ext.eq_ignore_ascii_case("rmi")
            {
                let bytes = std::fs::read(phys_path).ok()?;
                let parsed = uipod_codecs::midi::ParsedMidi::parse(&bytes).ok()?;
                let dec = uipod_codecs::midi::MidiDecoder::new(parsed);
                let src_sr = dec.sample_rate().max(8_000);
                let dur_ms = if duration_hint_ms > 0 {
                    duration_hint_ms
                } else {
                    dec.duration_ms().max(30_000)
                };
                return Some(Self {
                    inner: InnerDecoder::RustCodec {
                        dec: Box::new(dec),
                        src_sr,
                        duration_ms: dur_ms,
                        scratch: vec![0i16; 2048 * 2],
                        eof: false,
                    },
                    dst_sr,
                    aa_filter: StereoAntiAliasBiquad::new(src_sr, dst_sr),
                    rate_conv: VinylResampler::new(),
                    src_fifo: Vec::new(),
                    src_read: 0,
                    resamp_scratch: vec![0.0; 8192 * 2],
                    decoded_dst_frame: 0,
                });
            }
        }

        // 3. Standard lossless & lossy audio via `symphonia` (plus Ogg `.opus` via `OpusHostFns`)
        let file = File::open(phys_path).ok()?;
        let mss = MediaSourceStream::new(Box::new(file), Default::default());
        let mut hint = Hint::new();
        if let Some(ext) = phys_path.extension().and_then(|e| e.to_str()) {
            hint.with_extension(ext);
        }
        let probed = symphonia::default::get_probe()
            .format(
                &hint,
                mss,
                &FormatOptions::default(),
                &MetadataOptions::default(),
            )
            .ok()?;
        let format = probed.format;
        let track = format
            .tracks()
            .iter()
            .find(|t| t.codec_params.codec != CODEC_TYPE_NULL)?;
        let track_id = track.id;
        let codec_params = track.codec_params.clone();

        if codec_params.codec == CODEC_TYPE_OPUS {
            let fns = *OPUS_HOST_FNS.get()?;
            let ch = codec_params
                .channels
                .map(|c| c.count())
                .unwrap_or(2)
                .clamp(1, 2);
            let st_bytes = unsafe { (fns.get_size)(ch as i32) };
            if st_bytes <= 0 || st_bytes > 262_144 {
                return None;
            }
            let words = ((st_bytes as usize) + 7) / 8;
            let mut state_aligned = vec![0u64; words];
            let rc = unsafe { (fns.init)(state_aligned.as_mut_ptr() as *mut u8, 48_000, ch as i32) };
            if rc != 0 {
                return None;
            }
            let pre_skip = codec_params.delay.unwrap_or(0) as usize;
            let est_ms = codec_params
                .n_frames
                .map(|nf| ((nf * 1000) / 48_000) as u32)
                .unwrap_or(duration_hint_ms);
            return Some(Self {
                inner: InnerDecoder::OggOpus {
                    format,
                    track_id,
                    channels: ch,
                    pre_skip,
                    skip_rem: pre_skip,
                    duration_ms: if duration_hint_ms > 0 {
                        duration_hint_ms
                    } else {
                        est_ms
                    },
                    state_aligned,
                    fns,
                    pkt_scratch: vec![0i16; 5760 * 2],
                    eof: false,
                },
                dst_sr,
                aa_filter: StereoAntiAliasBiquad::new(48_000, dst_sr),
                rate_conv: VinylResampler::new(),
                src_fifo: Vec::with_capacity(8192 * 2),
                src_read: 0,
                resamp_scratch: vec![0.0; 8192 * 2],
                decoded_dst_frame: 0,
            });
        }

        let src_sr = codec_params.sample_rate.unwrap_or(dst_sr).max(8_000);
        let est_ms = codec_params
            .n_frames
            .map(|nf| ((nf * 1000) / (src_sr as u64)) as u32)
            .unwrap_or(duration_hint_ms);
        let decoder = symphonia::default::get_codecs()
            .make(&codec_params, &DecoderOptions::default())
            .ok()?;

        Some(Self {
            inner: InnerDecoder::Symphonia {
                format,
                decoder,
                track_id,
                src_sr,
                duration_ms: if duration_hint_ms > 0 {
                    duration_hint_ms
                } else {
                    est_ms
                },
                sample_buf: None,
                pkt_fifo: Vec::with_capacity(8192),
                pkt_read: 0,
                eof: false,
            },
            dst_sr,
            aa_filter: StereoAntiAliasBiquad::new(src_sr, dst_sr),
            rate_conv: VinylResampler::new(),
            src_fifo: Vec::with_capacity(8192 * 2),
            src_read: 0,
            resamp_scratch: vec![0.0; 8192 * 2],
            decoded_dst_frame: 0,
        })
    }

    #[inline]
    pub fn duration_ms(&self) -> u32 {
        match &self.inner {
            InnerDecoder::Symphonia { duration_ms, .. }
            | InnerDecoder::OggOpus { duration_ms, .. }
            | InnerDecoder::Nsf { duration_ms, .. }
            | InnerDecoder::RustCodec { duration_ms, .. } => *duration_ms,
        }
    }

    #[inline]
    pub fn decoded_dst_frame(&self) -> u64 {
        self.decoded_dst_frame
    }

    #[inline]
    pub fn is_eof(&self) -> bool {
        let inner_eof = match &self.inner {
            InnerDecoder::Symphonia {
                eof,
                pkt_fifo,
                pkt_read,
                ..
            } => *eof && *pkt_read >= pkt_fifo.len(),
            InnerDecoder::OggOpus { eof, .. }
            | InnerDecoder::Nsf { eof, .. }
            | InnerDecoder::RustCodec { eof, .. } => *eof,
        };
        inner_eof && self.src_read >= self.src_fifo.len()
    }

    pub fn seek_to_dst_frame(&mut self, target_dst_frame: u64) {
        let dst_sr_f = self.dst_sr.max(1) as f64;
        let target_sec = (target_dst_frame as f64) / dst_sr_f;
        let sec = target_sec.floor().max(0.0) as u64;
        let frac = (target_sec - (sec as f64)).clamp(0.0, 0.999_999_999);
        let target_ms = (target_sec * 1000.0).round().max(0.0) as u32;

        self.aa_filter.reset();
        self.rate_conv.reset();
        self.src_fifo.clear();
        self.src_read = 0;
        self.decoded_dst_frame = target_dst_frame;

        match &mut self.inner {
            InnerDecoder::Symphonia {
                format,
                decoder,
                track_id,
                pkt_fifo,
                pkt_read,
                eof,
                ..
            } => {
                let _ = format.seek(
                    SeekMode::Accurate,
                    SeekTo::Time {
                        time: Time::new(sec, frac),
                        track_id: Some(*track_id),
                    },
                );
                decoder.reset();
                pkt_fifo.clear();
                *pkt_read = 0;
                *eof = false;
            }
            InnerDecoder::OggOpus {
                format,
                track_id,
                channels,
                pre_skip,
                skip_rem,
                state_aligned,
                fns,
                eof,
                ..
            } => {
                let _ = format.seek(
                    SeekMode::Accurate,
                    SeekTo::Time {
                        time: Time::new(sec, frac),
                        track_id: Some(*track_id),
                    },
                );
                let _ = unsafe {
                    (fns.init)(
                        state_aligned.as_mut_ptr() as *mut u8,
                        48_000,
                        *channels as i32,
                    )
                };
                *skip_rem = if target_dst_frame == 0 { *pre_skip } else { 0 };
                *eof = false;
            }
            InnerDecoder::Nsf { engine, eof, .. } => {
                engine.seek_ms(target_ms);
                *eof = false;
            }
            InnerDecoder::RustCodec { dec, eof, .. } => {
                dec.seek_ms(target_ms);
                *eof = false;
            }
        }
    }

    fn pull_raw_src_f32(&mut self, min_frames: usize) {
        if self.src_read > 0 {
            let fifo_len = self.src_fifo.len();
            let rem = fifo_len.saturating_sub(self.src_read);
            if rem > 0 {
                self.src_fifo.copy_within(self.src_read..fifo_len, 0);
            }
            self.src_fifo.truncate(rem);
            self.src_read = 0;
        }

        let prev_len = self.src_fifo.len();

        while self.src_fifo.len() / 2 < min_frames {
            match &mut self.inner {
                InnerDecoder::Symphonia {
                    format,
                    decoder,
                    track_id,
                    sample_buf,
                    pkt_fifo,
                    pkt_read,
                    eof,
                    ..
                } => {
                    if *pkt_read < pkt_fifo.len() {
                        self.src_fifo.extend_from_slice(&pkt_fifo[*pkt_read..]);
                        pkt_fifo.clear();
                        *pkt_read = 0;
                        continue;
                    }
                    if *eof {
                        break;
                    }
                    let Ok(packet) = format.next_packet() else {
                        *eof = true;
                        break;
                    };
                    if packet.track_id() != *track_id {
                        continue;
                    }
                    let Ok(decoded) = decoder.decode(&packet) else {
                        continue;
                    };
                    let spec = *decoded.spec();
                    let ch_n = spec.channels.count().max(1);
                    let frames = decoded.frames();
                    if frames == 0 {
                        continue;
                    }
                    let need_cap = (frames * ch_n).max(4096);
                    if sample_buf
                        .as_ref()
                        .map(|b| b.capacity() < frames * ch_n)
                        .unwrap_or(true)
                    {
                        *sample_buf = Some(SampleBuffer::<f32>::new(need_cap as u64, spec));
                    }
                    let sbuf = sample_buf.as_mut().unwrap();
                    sbuf.copy_interleaved_ref(decoded);
                    let samples = sbuf.samples();
                    self.src_fifo.reserve(frames * 2);
                    if ch_n == 1 {
                        for &m in &samples[..frames] {
                            let v = m.clamp(-1.0, 1.0);
                            self.src_fifo.push(v);
                            self.src_fifo.push(v);
                        }
                    } else {
                        for f in 0..frames {
                            let base = f * ch_n;
                            self.src_fifo.push(samples[base].clamp(-1.0, 1.0));
                            self.src_fifo.push(samples[base + 1].clamp(-1.0, 1.0));
                        }
                    }
                }
                InnerDecoder::OggOpus {
                    format,
                    track_id,
                    channels,
                    skip_rem,
                    state_aligned,
                    fns,
                    pkt_scratch,
                    eof,
                    ..
                } => {
                    if *eof {
                        break;
                    }
                    let Ok(packet) = format.next_packet() else {
                        *eof = true;
                        break;
                    };
                    if packet.track_id() != *track_id {
                        continue;
                    }
                    let buf = packet.buf();
                    if buf.is_empty() {
                        continue;
                    }
                    let max_frame_sz = (pkt_scratch.len() / (*channels).max(1)) as i32;
                    let decoded_frames = unsafe {
                        (fns.decode)(
                            state_aligned.as_mut_ptr() as *mut u8,
                            buf.as_ptr(),
                            buf.len() as i32,
                            pkt_scratch.as_mut_ptr(),
                            max_frame_sz,
                            0,
                        )
                    };
                    if decoded_frames <= 0 {
                        continue;
                    }
                    let dec_n = decoded_frames as usize;
                    let skip = (*skip_rem).min(dec_n);
                    *skip_rem -= skip;
                    if skip >= dec_n {
                        continue;
                    }
                    let keep = dec_n - skip;
                    let scale = 1.0 / 32768.0;
                    self.src_fifo.reserve(keep * 2);
                    if *channels == 1 {
                        for &m in &pkt_scratch[skip..dec_n] {
                            let v = (m as f32) * scale;
                            self.src_fifo.push(v);
                            self.src_fifo.push(v);
                        }
                    } else {
                        for f in skip..dec_n {
                            self.src_fifo.push((pkt_scratch[f * 2] as f32) * scale);
                            self.src_fifo.push((pkt_scratch[f * 2 + 1] as f32) * scale);
                        }
                    }
                }
                InnerDecoder::Nsf {
                    engine,
                    scratch,
                    eof,
                    ..
                } => {
                    if *eof {
                        break;
                    }
                    let got = engine.render_stereo_s16(scratch);
                    if got == 0 {
                        *eof = true;
                        break;
                    }
                    let scale = 1.0 / 32768.0;
                    self.src_fifo.reserve(got * 2);
                    for &s in &scratch[..got * 2] {
                        self.src_fifo.push((s as f32) * scale);
                    }
                }
                InnerDecoder::RustCodec {
                    dec, scratch, eof, ..
                } => {
                    if *eof {
                        break;
                    }
                    let got = dec.render_stereo_s16(scratch, false);
                    if got == 0 {
                        *eof = true;
                        break;
                    }
                    let scale = 1.0 / 32768.0;
                    self.src_fifo.reserve(got * 2);
                    for &s in &scratch[..got * 2] {
                        self.src_fifo.push((s as f32) * scale);
                    }
                }
            }
        }

        if self.src_fifo.len() > prev_len {
            self.aa_filter.filter_in_place(&mut self.src_fifo[prev_len..]);
        }
    }

    /// Decode up to `out_stereo_s16.len() / 2` stereo frames at `dst_sr`.
    /// Always fills the full `want_frames` unless the underlying stream reaches true EOF,
    /// preserving continuous resampler phase & anti-alias state across every chunk boundary.
    pub fn decode_dst_s16(&mut self, out_stereo_s16: &mut [i16]) -> usize {
        let want_frames = out_stereo_s16.len() / 2;
        if want_frames == 0 {
            return 0;
        }
        let src_sr = match &self.inner {
            InnerDecoder::Symphonia { src_sr, .. }
            | InnerDecoder::Nsf { src_sr, .. }
            | InnerDecoder::RustCodec { src_sr, .. } => *src_sr,
            InnerDecoder::OggOpus { .. } => 48_000,
        };

        if src_sr == self.dst_sr {
            self.pull_raw_src_f32(want_frames);
            let avail = (self.src_fifo.len().saturating_sub(self.src_read)) / 2;
            let take = avail.min(want_frames);
            for i in 0..take * 2 {
                let s = (self.src_fifo[self.src_read + i] * 32768.0)
                    .round()
                    .clamp(-32768.0, 32767.0) as i16;
                out_stereo_s16[i] = s;
            }
            self.src_read += take * 2;
            self.decoded_dst_frame += take as u64;
            return take;
        }

        let ratio = (src_sr as f32) / (self.dst_sr as f32);
        if self.resamp_scratch.len() < want_frames * 2 {
            self.resamp_scratch.resize(want_frames * 2, 0.0);
        }

        let mut total_prod = 0usize;
        while total_prod < want_frames {
            let rem_out = want_frames - total_prod;
            let need_src = self.rate_conv.needed_input_frames(rem_out, ratio);
            let cur_avail = (self.src_fifo.len().saturating_sub(self.src_read)) / 2;
            if cur_avail < need_src {
                self.pull_raw_src_f32(need_src);
            }
            let avail_slice = &self.src_fifo[self.src_read..];
            let inner_eof = match &self.inner {
                InnerDecoder::Symphonia {
                    eof,
                    pkt_fifo,
                    pkt_read,
                    ..
                } => *eof && *pkt_read >= pkt_fifo.len(),
                InnerDecoder::OggOpus { eof, .. }
                | InnerDecoder::Nsf { eof, .. }
                | InnerDecoder::RustCodec { eof, .. } => *eof,
            };
            let dst_slice = &mut self.resamp_scratch[total_prod * 2..want_frames * 2];
            let (cons_in, prod_out) =
                self.rate_conv
                    .process_with_eof(avail_slice, ratio, inner_eof, dst_slice);
            self.src_read += cons_in * 2;
            total_prod += prod_out;
            if prod_out == 0 && (cons_in == 0 || inner_eof) {
                break;
            }
        }

        for i in 0..total_prod * 2 {
            out_stereo_s16[i] = (self.resamp_scratch[i] * 32768.0)
                .round()
                .clamp(-32768.0, 32767.0) as i16;
        }
        self.decoded_dst_frame += total_prod as u64;
        total_prod
    }
}
