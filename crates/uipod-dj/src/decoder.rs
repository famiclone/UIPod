//! Multi-format streaming audio decoder for Manual Dual-Deck DJ RAM window refills.
//!
//! Decodes `MP3`, `FLAC`, `OGG/Vorbis`, `WAV`, `AIFF`, `M4A/AAC/ALAC` (via `symphonia`),
//! `.nsf`/`.nsfe` (`uipod-nsf`), and `.gsf`/`.minigsf`/`.mid`/`.midi` (`uipod-codecs`)
//! into stereo `S16` frames resampled to the active mixer sample rate.

#[cfg(not(target_arch = "wasm32"))]
use std::fs::File;
#[cfg(not(target_arch = "wasm32"))]
use std::path::Path;

#[cfg(not(target_arch = "wasm32"))]
use symphonia::core::audio::SampleBuffer;
#[cfg(not(target_arch = "wasm32"))]
use symphonia::core::codecs::{Decoder, DecoderOptions, CODEC_TYPE_NULL};
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
    rate_conv: VinylResampler,
    src_fifo: Vec<f32>,
    src_read: usize,
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
                rate_conv: VinylResampler::new(),
                src_fifo: Vec::new(),
                src_read: 0,
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
                    rate_conv: VinylResampler::new(),
                    src_fifo: Vec::new(),
                    src_read: 0,
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
                    rate_conv: VinylResampler::new(),
                    src_fifo: Vec::new(),
                    src_read: 0,
                    decoded_dst_frame: 0,
                });
            }
        }

        // 3. Standard lossless & lossy audio via `symphonia`
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
        let src_sr = track.codec_params.sample_rate.unwrap_or(dst_sr).max(8_000);
        let est_ms = track
            .codec_params
            .n_frames
            .map(|nf| ((nf * 1000) / (src_sr as u64)) as u32)
            .unwrap_or(duration_hint_ms);
        let codec_params = track.codec_params.clone();
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
            rate_conv: VinylResampler::new(),
            src_fifo: Vec::with_capacity(8192),
            src_read: 0,
            decoded_dst_frame: 0,
        })
    }

    #[inline]
    pub fn duration_ms(&self) -> u32 {
        match &self.inner {
            InnerDecoder::Symphonia { duration_ms, .. }
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
            InnerDecoder::Nsf { eof, .. } | InnerDecoder::RustCodec { eof, .. } => *eof,
        };
        inner_eof && self.src_read >= self.src_fifo.len()
    }

    pub fn seek_to_dst_frame(&mut self, target_dst_frame: u64) {
        let target_ms = ((target_dst_frame * 1000) / (self.dst_sr as u64)) as u32;
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
                let sec = (target_ms / 1000) as u64;
                let frac = ((target_ms % 1000) as f64) * 0.001;
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
    }

    /// Decode up to `out_stereo_s16.len() / 2` stereo frames at `dst_sr`.
    pub fn decode_dst_s16(&mut self, out_stereo_s16: &mut [i16]) -> usize {
        let want_frames = out_stereo_s16.len() / 2;
        if want_frames == 0 {
            return 0;
        }
        let src_sr = match &self.inner {
            InnerDecoder::Symphonia { src_sr, .. }
            | InnerDecoder::Nsf { src_sr, .. }
            | InnerDecoder::RustCodec { src_sr, .. } => *src_sr,
        };

        if src_sr == self.dst_sr {
            self.pull_raw_src_f32(want_frames);
            let avail = (self.src_fifo.len().saturating_sub(self.src_read)) / 2;
            let take = avail.min(want_frames);
            for i in 0..take * 2 {
                let s = (self.src_fifo[self.src_read + i] * 32767.0)
                    .round()
                    .clamp(-32768.0, 32767.0) as i16;
                out_stereo_s16[i] = s;
            }
            self.src_read += take * 2;
            self.decoded_dst_frame += take as u64;
            return take;
        }

        let ratio = (src_sr as f32) / (self.dst_sr as f32);
        let est_src = ((want_frames as f32) * ratio).ceil() as usize + 16;
        self.pull_raw_src_f32(est_src);
        let avail_slice = &self.src_fifo[self.src_read..];
        let inner_eof = match &self.inner {
            InnerDecoder::Symphonia { eof, .. }
            | InnerDecoder::Nsf { eof, .. }
            | InnerDecoder::RustCodec { eof, .. } => *eof,
        };
        let mut tmp_out = vec![0.0f32; want_frames * 2];
        let (cons_in, prod_out) =
            self.rate_conv
                .process_with_eof(avail_slice, ratio, inner_eof, &mut tmp_out);
        self.src_read += cons_in * 2;
        for i in 0..prod_out * 2 {
            out_stereo_s16[i] = (tmp_out[i] * 32767.0)
                .round()
                .clamp(-32768.0, 32767.0) as i16;
        }
        self.decoded_dst_frame += prod_out as u64;
        prod_out
    }
}
