//! Standard MIDI File (`.mid`, `.midi`, `.kar`, `.rmi`) parser and sample-accurate timeline builder.

pub const MIDI_SAMPLE_RATE: u32 = 44_100;

#[derive(Clone, Copy, Debug)]
pub enum MidiEventKind {
    NoteOn { ch: u8, key: u8, vel: u8 },
    NoteOff { ch: u8, key: u8 },
    ControlChange { ch: u8, cc: u8, val: u8 },
    ProgramChange { ch: u8, program: u8 },
    PitchBend { ch: u8, value: i16 }, // -8192 .. +8191
    ChannelPressure { ch: u8, pressure: u8 },
    SystemReset,
}

#[derive(Clone, Copy, Debug)]
pub struct TimedMidiEvent {
    pub frame: u64,
    pub kind: MidiEventKind,
}

#[derive(Clone, Debug, Default)]
pub struct MidiMetadata {
    pub title: Option<String>,
    pub artist: Option<String>,
    pub album: Option<String>,
    pub comment: Option<String>,
    pub duration_ms: u32,
}

pub struct ParsedMidi {
    pub events: Vec<TimedMidiEvent>,
    pub total_frames: u64,
    pub meta: MidiMetadata,
}

#[derive(Clone, Copy)]
struct RawTrackEvent {
    tick: u64,
    track_idx: u16,
    order: u32,
    payload: RawPayload,
}

#[derive(Clone, Copy)]
enum RawPayload {
    Midi(MidiEventKind),
    Tempo(u32), // microseconds per quarter note
}

fn read_vlq(data: &[u8], pos: &mut usize) -> Option<u32> {
    let mut val: u32 = 0;
    for _ in 0..4 {
        let &b = data.get(*pos)?;
        *pos += 1;
        val = (val << 7) | ((b & 0x7F) as u32);
        if (b & 0x80) == 0 {
            return Some(val);
        }
    }
    Some(val)
}

fn clean_midi_text(bytes: &[u8]) -> Option<String> {
    let trimmed: Vec<u8> = bytes
        .iter()
        .copied()
        .take_while(|&b| b != 0)
        .filter(|&b| b >= 0x20 || b == b'\t')
        .collect();
    if trimmed.is_empty() {
        return None;
    }
    let s = match std::str::from_utf8(&trimmed) {
        Ok(utf8) => utf8.trim().to_string(),
        Err(_) => {
            // Latin-1 / CP1252 clean fallback common in classic 90s/2000s .mid/.kar files
            trimmed.iter().map(|&b| b as char).collect::<String>().trim().to_string()
        }
    };
    if s.is_empty() {
        None
    } else {
        Some(s)
    }
}

/// Unwrap RIFF `RMID` container or locate `MThd` within the first 4 KiB.
pub fn locate_smf_slice(data: &[u8]) -> Option<&[u8]> {
    if data.len() < 14 {
        return None;
    }
    if &data[0..4] == b"MThd" {
        return Some(data);
    }
    // RIFF .... RMID -> find `data` chunk containing `MThd`
    if data.len() >= 20 && &data[0..4] == b"RIFF" && &data[8..12] == b"RMID" {
        let riff_end = (u32::from_le_bytes([data[4], data[5], data[6], data[7]]) as usize)
            .saturating_add(8)
            .min(data.len());
        let mut pos = 12usize;
        while pos + 8 <= riff_end {
            let cid = &data[pos..pos + 4];
            let clen =
                u32::from_le_bytes([data[pos + 4], data[pos + 5], data[pos + 6], data[pos + 7]])
                    as usize;
            let body = pos + 8;
            let end = body.saturating_add(clen).min(riff_end);
            if cid == b"data" && end >= body + 14 && &data[body..body + 4] == b"MThd" {
                return Some(&data[body..end]);
            }
            pos = end + (clen & 1);
        }
    }
    // Scan first 4 KiB for MThd (in case of leading ID3v2 / MacBinary header)
    let limit = data.len().min(4096).saturating_sub(14);
    for off in 0..=limit {
        if &data[off..off + 4] == b"MThd" {
            let hdr_len = u32::from_be_bytes([
                data[off + 4],
                data[off + 5],
                data[off + 6],
                data[off + 7],
            ]);
            if hdr_len >= 6 {
                return Some(&data[off..]);
            }
        }
    }
    None
}

fn is_gm_gs_xg_reset_sysex(payload: &[u8]) -> bool {
    // Strip trailing F7 if present
    let body = if payload.last() == Some(&0xF7) {
        &payload[..payload.len() - 1]
    } else {
        payload
    };
    // GM System On: 7E <dev> 09 01 (or 02/03)
    if body.len() >= 4 && body[0] == 0x7E && body[2] == 0x09 {
        return true;
    }
    // Roland GS Reset: 41 <dev> 42 12 40 00 7F 00 41
    if body.len() >= 8
        && body[0] == 0x41
        && body[2] == 0x42
        && body[3] == 0x12
        && body[4] == 0x40
        && body[5] == 0x00
        && body[6] == 0x7F
    {
        return true;
    }
    // Yamaha XG System On: 43 <dev> 4C 00 00 7E 00
    if body.len() >= 7
        && body[0] == 0x43
        && body[2] == 0x4C
        && body[3] == 0x00
        && body[4] == 0x00
        && body[5] == 0x7E
    {
        return true;
    }
    false
}

impl ParsedMidi {
    pub fn parse(raw_file: &[u8]) -> Result<Self, &'static str> {
        let data = locate_smf_slice(raw_file).ok_or("Not a valid Standard MIDI file")?;
        if data.len() < 14 || &data[0..4] != b"MThd" {
            return Err("Missing MThd header");
        }

        let hdr_len = u32::from_be_bytes([data[4], data[5], data[6], data[7]]) as usize;
        if hdr_len < 6 || 8 + hdr_len > data.len() {
            return Err("Truncated MThd header");
        }
        let format = u16::from_be_bytes([data[8], data[9]]);
        let num_tracks = u16::from_be_bytes([data[10], data[11]]);
        let division = u16::from_be_bytes([data[12], data[13]]);
        if format > 2 || division == 0 {
            return Err("Unsupported MIDI format or zero division");
        }

        let mut pos = 8 + hdr_len;
        let mut raw_events: Vec<RawTrackEvent> = Vec::new();
        let mut order_counter: u32 = 0;

        let mut title: Option<String> = None;
        let mut kar_title: Option<String> = None;
        let mut kar_artist: Option<String> = None;
        let mut copyright: Option<String> = None;
        let mut comment: Option<String> = None;

        let mut seq_tick_offset: u64 = 0;
        let mut tracks_seen: u16 = 0;

        while pos + 8 <= data.len() && tracks_seen < num_tracks.max(1) {
            let chunk_id = &data[pos..pos + 4];
            let chunk_len =
                u32::from_be_bytes([data[pos + 4], data[pos + 5], data[pos + 6], data[pos + 7]])
                    as usize;
            let body_start = pos + 8;
            let body_end = body_start.saturating_add(chunk_len).min(data.len());
            pos = body_end;

            if chunk_id != b"MTrk" {
                continue;
            }
            let trk_idx = tracks_seen;
            tracks_seen += 1;

            let mut tpos = body_start;
            let mut tick: u64 = if format == 2 { seq_tick_offset } else { 0 };
            let mut running_status: u8 = 0;
            let mut had_notes_in_track = false;

            while tpos < body_end {
                let Some(delta) = read_vlq(data, &mut tpos) else {
                    break;
                };
                tick = tick.saturating_add(delta as u64);
                if tpos >= body_end {
                    break;
                }

                let first = data[tpos];
                if first == 0xFF {
                    // Meta event
                    tpos += 1;
                    if tpos >= body_end {
                        break;
                    }
                    let meta_type = data[tpos];
                    tpos += 1;
                    let Some(mlen) = read_vlq(data, &mut tpos) else {
                        break;
                    };
                    let mstart = tpos;
                    let mend = mstart.saturating_add(mlen as usize).min(body_end);
                    tpos = mend;
                    let mslice = &data[mstart..mend];

                    match meta_type {
                        0x2F => {
                            // End of Track
                            break;
                        }
                        0x51 if mslice.len() == 3 => {
                            let us_per_qn = ((mslice[0] as u32) << 16)
                                | ((mslice[1] as u32) << 8)
                                | (mslice[2] as u32);
                            if us_per_qn > 0 {
                                raw_events.push(RawTrackEvent {
                                    tick,
                                    track_idx: trk_idx,
                                    order: order_counter,
                                    payload: RawPayload::Tempo(us_per_qn),
                                });
                                order_counter = order_counter.wrapping_add(1);
                            }
                        }
                        0x03 => {
                            // Sequence / Track Name
                            if let Some(txt) = clean_midi_text(mslice) {
                                if let Some(rest) = txt.strip_prefix("@T") {
                                    let t = rest.trim();
                                    if !t.is_empty() {
                                        if kar_title.is_none() {
                                            kar_title = Some(t.to_string());
                                        } else if kar_artist.is_none() {
                                            kar_artist = Some(t.to_string());
                                        }
                                    }
                                } else if (trk_idx == 0 || (title.is_none() && !had_notes_in_track))
                                    && !txt.eq_ignore_ascii_case("untitled")
                                    && !txt.eq_ignore_ascii_case("midi")
                                {
                                    if title.is_none() || trk_idx == 0 {
                                        title = Some(txt);
                                    }
                                }
                            }
                        }
                        0x02 => {
                            if copyright.is_none() {
                                copyright = clean_midi_text(mslice);
                            }
                        }
                        0x01 => {
                            // Text event (check Karaoke @T metadata first)
                            if let Some(txt) = clean_midi_text(mslice) {
                                if let Some(rest) = txt.strip_prefix("@T") {
                                    let t = rest.trim();
                                    if !t.is_empty() {
                                        if kar_title.is_none() {
                                            kar_title = Some(t.to_string());
                                        } else if kar_artist.is_none() {
                                            kar_artist = Some(t.to_string());
                                        }
                                    }
                                } else if comment.is_none() && tick == 0 {
                                    comment = Some(txt);
                                }
                            }
                        }
                        _ => {}
                    }
                } else if first == 0xF0 || first == 0xF7 {
                    // SysEx event (resets running status)
                    running_status = 0;
                    tpos += 1;
                    let Some(slen) = read_vlq(data, &mut tpos) else {
                        break;
                    };
                    let sstart = tpos;
                    let send = sstart.saturating_add(slen as usize).min(body_end);
                    tpos = send;
                    if first == 0xF0 && is_gm_gs_xg_reset_sysex(&data[sstart..send]) {
                        raw_events.push(RawTrackEvent {
                            tick,
                            track_idx: trk_idx,
                            order: order_counter,
                            payload: RawPayload::Midi(MidiEventKind::SystemReset),
                        });
                        order_counter = order_counter.wrapping_add(1);
                    }
                } else {
                    let status = if (first & 0x80) != 0 {
                        tpos += 1;
                        if first < 0xF8 {
                            running_status = first;
                        }
                        first
                    } else {
                        if running_status == 0 {
                            // Malformed byte without running status: skip
                            tpos += 1;
                            continue;
                        }
                        running_status
                    };

                    if status >= 0xF0 {
                        // System Common / Real-Time messages inside SMF
                        match status {
                            0xF1 | 0xF3 => tpos = (tpos + 1).min(body_end),
                            0xF2 => tpos = (tpos + 2).min(body_end),
                            _ => {}
                        }
                        continue;
                    }

                    let cmd = status & 0xF0;
                    let ch = status & 0x0F;
                    match cmd {
                        0x80 => {
                            if tpos + 2 > body_end {
                                break;
                            }
                            let key = data[tpos] & 0x7F;
                            tpos += 2;
                            raw_events.push(RawTrackEvent {
                                tick,
                                track_idx: trk_idx,
                                order: order_counter,
                                payload: RawPayload::Midi(MidiEventKind::NoteOff { ch, key }),
                            });
                            order_counter = order_counter.wrapping_add(1);
                        }
                        0x90 => {
                            if tpos + 2 > body_end {
                                break;
                            }
                            let key = data[tpos] & 0x7F;
                            let vel = data[tpos + 1] & 0x7F;
                            tpos += 2;
                            had_notes_in_track = true;
                            let kind = if vel == 0 {
                                MidiEventKind::NoteOff { ch, key }
                            } else {
                                MidiEventKind::NoteOn { ch, key, vel }
                            };
                            raw_events.push(RawTrackEvent {
                                tick,
                                track_idx: trk_idx,
                                order: order_counter,
                                payload: RawPayload::Midi(kind),
                            });
                            order_counter = order_counter.wrapping_add(1);
                        }
                        0xA0 => {
                            // Polyphonic key pressure (2 data bytes)
                            if tpos + 2 > body_end {
                                break;
                            }
                            tpos += 2;
                        }
                        0xB0 => {
                            if tpos + 2 > body_end {
                                break;
                            }
                            let cc = data[tpos] & 0x7F;
                            let val = data[tpos + 1] & 0x7F;
                            tpos += 2;
                            raw_events.push(RawTrackEvent {
                                tick,
                                track_idx: trk_idx,
                                order: order_counter,
                                payload: RawPayload::Midi(MidiEventKind::ControlChange {
                                    ch,
                                    cc,
                                    val,
                                }),
                            });
                            order_counter = order_counter.wrapping_add(1);
                        }
                        0xC0 => {
                            if tpos + 1 > body_end {
                                break;
                            }
                            let program = data[tpos] & 0x7F;
                            tpos += 1;
                            raw_events.push(RawTrackEvent {
                                tick,
                                track_idx: trk_idx,
                                order: order_counter,
                                payload: RawPayload::Midi(MidiEventKind::ProgramChange {
                                    ch,
                                    program,
                                }),
                            });
                            order_counter = order_counter.wrapping_add(1);
                        }
                        0xD0 => {
                            if tpos + 1 > body_end {
                                break;
                            }
                            let pressure = data[tpos] & 0x7F;
                            tpos += 1;
                            raw_events.push(RawTrackEvent {
                                tick,
                                track_idx: trk_idx,
                                order: order_counter,
                                payload: RawPayload::Midi(MidiEventKind::ChannelPressure {
                                    ch,
                                    pressure,
                                }),
                            });
                            order_counter = order_counter.wrapping_add(1);
                        }
                        0xE0 => {
                            if tpos + 2 > body_end {
                                break;
                            }
                            let lsb = (data[tpos] & 0x7F) as i16;
                            let msb = (data[tpos + 1] & 0x7F) as i16;
                            tpos += 2;
                            let value = ((msb << 7) | lsb) - 8192;
                            raw_events.push(RawTrackEvent {
                                tick,
                                track_idx: trk_idx,
                                order: order_counter,
                                payload: RawPayload::Midi(MidiEventKind::PitchBend { ch, value }),
                            });
                            order_counter = order_counter.wrapping_add(1);
                        }
                        _ => {}
                    }
                }
            }

            if format == 2 {
                seq_tick_offset = tick;
            }
        }

        if raw_events.is_empty() {
            return Err("MIDI file contains no events");
        }

        // Sort chronologically by tick, keeping Tempo events ahead of MIDI events at identical ticks
        raw_events.sort_by(|a, b| {
            a.tick
                .cmp(&b.tick)
                .then_with(|| {
                    let pa = matches!(a.payload, RawPayload::Tempo(_));
                    let pb = matches!(b.payload, RawPayload::Tempo(_));
                    pb.cmp(&pa)
                })
                .then_with(|| a.track_idx.cmp(&b.track_idx))
                .then_with(|| a.order.cmp(&b.order))
        });

        let smpte_mode = (division & 0x8000) != 0;
        let smpte_ticks_per_sec: f64 = if smpte_mode {
            let raw_fps = -((division >> 8) as i8) as f64;
            let fps = if (raw_fps - 29.0).abs() < 0.1 {
                29.97
            } else {
                raw_fps.max(1.0)
            };
            let tpf = ((division & 0xFF) as f64).max(1.0);
            fps * tpf
        } else {
            0.0
        };
        let tpqn = (division & 0x7FFF).max(1) as f64;

        let mut us_per_qn: f64 = 500_000.0; // Default 120 BPM
        let mut cur_tick: u64 = 0;
        let mut cur_seconds: f64 = 0.0;
        let mut last_musical_frame: u64 = 0;

        let mut events: Vec<TimedMidiEvent> = Vec::with_capacity(raw_events.len());

        for ev in raw_events {
            let dt = ev.tick.saturating_sub(cur_tick) as f64;
            if dt > 0.0 {
                let dsec = if smpte_mode {
                    dt / smpte_ticks_per_sec
                } else {
                    (dt * us_per_qn) / (tpqn * 1_000_000.0)
                };
                cur_seconds += dsec;
                cur_tick = ev.tick;
            }
            let frame = (cur_seconds * (MIDI_SAMPLE_RATE as f64)).round() as u64;
            match ev.payload {
                RawPayload::Tempo(new_tempo) => {
                    if !smpte_mode && new_tempo > 0 {
                        us_per_qn = new_tempo as f64;
                    }
                }
                RawPayload::Midi(kind) => {
                    if matches!(
                        kind,
                        MidiEventKind::NoteOn { .. } | MidiEventKind::NoteOff { .. }
                    ) {
                        last_musical_frame = last_musical_frame.max(frame);
                    }
                    events.push(TimedMidiEvent { frame, kind });
                }
            }
        }

        // Release tail: 750 ms after the last note event so natural reverb/release rings out cleanly
        let tail_frames = (MIDI_SAMPLE_RATE as u64 * 3) / 4;
        let fallback_end = events.last().map(|e| e.frame).unwrap_or(0);
        let raw_end = if last_musical_frame > 0 {
            last_musical_frame.saturating_add(tail_frames)
        } else {
            fallback_end.saturating_add(tail_frames)
        };
        let min_frames = MIDI_SAMPLE_RATE as u64; // 1.0s minimum
        let max_frames = (MIDI_SAMPLE_RATE as u64) * 3600; // 1 hour cap
        let total_frames = raw_end.clamp(min_frames, max_frames);
        let duration_ms = ((total_frames * 1000) / (MIDI_SAMPLE_RATE as u64)).max(1000) as u32;

        let final_title = kar_title.or(title);
        let final_artist = kar_artist.or(copyright);

        Ok(Self {
            events,
            total_frames,
            meta: MidiMetadata {
                title: final_title,
                artist: final_artist,
                album: None,
                comment,
                duration_ms,
            },
        })
    }
}
