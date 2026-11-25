//! Unified Track Intelligence Engine (`uipod-track-intel`).
//!
//! Separates:
//! 1. **Tier 1 — Persistent Per-Track Analysis (`TrackIntelDb` / `TrackAnalysisRecord`, 512 bytes):**
//!    Verified BPM (`60..200 BPM` within `±0.5 BPM`), `first_beat_offset_ms`, 5-band tonal balance,
//!    RMS / `crest_factor`, percussive baselines, and a 256-bucket structural `energy_contour`.
//! 2. **Tier 2 — Lock-Free Real-Time Audio Telemetry (`TrackLiveFrame`, 592 bytes):**
//!    Wait-free SPSC audio tap + atomic `SeqLock` snapshots providing 64-bin log spectrum,
//!    256-sample scope waveform, stereo VU, latency-compensated `.nsf` APU channel telemetry,
//!    normalized `kick_impulse` / `snare_impulse`, and monotonic bar/beat phase.

pub mod analyzer;
pub mod db;
pub mod key;
pub mod live;
pub mod tags;
pub mod worker;

pub use analyzer::{
    analyze_mono_window, analyze_nsf_bytes, analyze_pcm_s16_buffer, compute_contour_256_from_mono,
    decimate_mono_f32, decimate_stereo_s16_to_mono, fold_bpm_60_200, AcousticMetrics,
    AnalyzeRequest, DEFAULT_BPM, MAX_BPM, MIN_BPM,
};
#[cfg(not(target_arch = "wasm32"))]
pub use analyzer::analyze_file_fast;
pub use db::{
    analysis_flag, decode_record_bytes, encode_record_bytes, read_fixed_utf8, write_fixed_utf8,
    TrackAnalysisRecord, TrackIntelDb, RECORD_STRIDE,
};
pub use key::{compute_track_key_hash, normalize_track_id, NormalizedTrackId, TrackKeyInput, SUBTRACK_NONE};
pub use live::{
    audio_tap_clear, audio_tap_push_apu, audio_tap_push_s16, interpolated_output_delay_us,
    publish_active_analysis, publish_live_frame, read_active_analysis, read_live_frame,
    read_synced_apu_telemetry, read_synced_s16_window, LiveTelemetryEngine, TrackLiveFrame,
};
pub use tags::{parse_bpm_str, read_tags_from_reader, ExtractedTags};
#[cfg(not(target_arch = "wasm32"))]
pub use worker::{StreamPcmFallbackFn, TrackIntelCoordinator};

#[cfg(test)]
mod tests {
    use super::*;
    use std::f32::consts::PI;

    /// Synthesize realistic stereo S16 drum tracks (`kick` + `snare` + off-beat `hi-hat`),
    /// supporting standard 4/4, half-time `70 -> 140 BPM` kicks, and 2-step `174 BPM` DnB!
    fn synth_test_track_s16(
        sr: u32,
        duration_sec: f32,
        bpm: f32,
        dnb_two_step: bool,
        halftime_kick_only: bool,
    ) -> Vec<i16> {
        let frames = (duration_sec * (sr as f32)).round() as usize;
        let mut out = vec![0i16; frames * 2];
        let beat_sec = 60.0 / bpm;

        for f in 0..frames {
            let t = (f as f32) / (sr as f32);
            let beat_pos = t / beat_sec;
            let bar_pos = beat_pos.rem_euclid(4.0);
            let beat_in_bar = bar_pos.floor() as usize;
            let in_beat_sec = (beat_pos - beat_pos.floor()) * beat_sec;

            // Kick pattern
            let kick_dt = if dnb_two_step {
                // Classic 174 BPM 2-step DnB: Kick on 0.0 and syncopated 2.5!
                if bar_pos < 0.5 {
                    Some(bar_pos * beat_sec)
                } else if (2.5..3.0).contains(&bar_pos) {
                    Some((bar_pos - 2.5) * beat_sec)
                } else {
                    None
                }
            } else if halftime_kick_only {
                // Half-time kick on Beat 0, Snare on Beat 2 (e.g. 140 BPM dubstep/trap)
                if beat_in_bar == 0 {
                    Some(in_beat_sec)
                } else {
                    None
                }
            } else if in_beat_sec < 0.075 {
                Some(in_beat_sec)
            } else {
                None
            };

            let kick = match kick_dt {
                Some(kd) if kd < 0.070 => {
                    let env = (1.0 - kd / 0.070).powi(2);
                    let freq = 58.0 + 55.0 * (-kd * 45.0).exp();
                    (2.0 * PI * freq * kd).sin() * env * 0.82
                }
                _ => 0.0,
            };

            // Snare body (420 Hz + crisp 1100 Hz snap)
            let snare_hit = if halftime_kick_only {
                beat_in_bar == 2 && in_beat_sec < 0.055
            } else {
                (beat_in_bar == 1 || beat_in_bar == 3) && in_beat_sec < 0.055
            };
            let snare = if snare_hit {
                let env = (1.0 - in_beat_sec / 0.055).powi(2);
                ((2.0 * PI * 420.0 * in_beat_sec).sin() * 0.65
                    + (2.0 * PI * 1120.0 * in_beat_sec).sin() * 0.35)
                    * env
                    * 0.58
            } else {
                0.0
            };

            // 8th-note hi-hats (3400 Hz)
            let eighth_sec = t.rem_euclid(beat_sec * 0.5);
            let hat = if eighth_sec < 0.020 {
                let env = 1.0 - eighth_sec / 0.020;
                (2.0 * PI * 3450.0 * t).sin() * env * 0.28
            } else {
                0.0
            };

            let s = ((kick + snare + hat).clamp(-0.98, 0.98) * 32767.0) as i16;
            out[f * 2] = s;
            out[f * 2 + 1] = s;
        }
        out
    }

    #[test]
    fn resolves_90_120_140_and_174_dnb_within_half_bpm_without_drift() {
        let cases = [
            (90.0f32, 44_100u32, false, false),
            (120.0f32, 48_000u32, false, false),
            (140.0f32, 44_100u32, false, false),
            (140.0f32, 48_000u32, false, true), // Half-time 70 BPM kick+snare + 8th hats -> 140 BPM!
            (174.0f32, 44_100u32, true, false), // 2-step syncopated DnB -> 174 BPM (NOT 87 or 116)!
        ];

        for (target_bpm, sr, dnb, halftime) in cases {
            let pcm = synth_test_track_s16(sr, 16.0, target_bpm, dnb, halftime);
            let req = AnalyzeRequest {
                key_hash: 0x1234_5678,
                fs_path: String::new(),
                subtrack_index: SUBTRACK_NONE,
                slice_start_ms: 0,
                duration_ms: 16_000,
                sample_rate_hint: sr,
                title: "Synthetic".to_string(),
                artist: "Test".to_string(),
                album: String::new(),
                genre: String::new(),
                tag_tbpm: None,
                high_priority: true,
            };
            let rec = analyze_pcm_s16_buffer(&pcm, sr, 0, &req);
            let err = (rec.bpm - target_bpm).abs();
            assert!(
                err <= 0.5,
                "BPM mismatch for target={target_bpm} (sr={sr}, dnb={dnb}, halftime={halftime}): got {:.3} BPM (err={:.3}, conf={:.2})",
                rec.bpm,
                err,
                rec.bpm_confidence
            );
            assert!(rec.bpm_confidence >= 0.35);
        }
    }

    #[test]
    fn subtrack_keys_isolate_nsf_and_cue_and_invalidate_on_mtime_or_size() {
        let nsf_0 = normalize_track_id("/mnt/SDCARD/Music/MegaMan2.nsf#0", None);
        let nsf_1 = normalize_track_id("/mnt/SDCARD/Music/MegaMan2.nsf#1", None);
        let android_same = normalize_track_id("/storage/emulated/0/Music/MegaMan2.nsf#1", None);
        assert_eq!(nsf_0.subtrack_index, 0);
        assert_eq!(nsf_1.subtrack_index, 1);
        assert_eq!(nsf_1.canonical_source_id, android_same.canonical_source_id);

        let h0 = compute_track_key_hash(&TrackKeyInput {
            canonical_source_id: &nsf_0.canonical_source_id,
            file_size: 32_768,
            mtime_sec: 1_700_000_000,
            subtrack_index: nsf_0.subtrack_index,
        });
        let h1 = compute_track_key_hash(&TrackKeyInput {
            canonical_source_id: &nsf_1.canonical_source_id,
            file_size: 32_768,
            mtime_sec: 1_700_000_000,
            subtrack_index: nsf_1.subtrack_index,
        });
        let h1_modified = compute_track_key_hash(&TrackKeyInput {
            canonical_source_id: &nsf_1.canonical_source_id,
            file_size: 32_769,
            mtime_sec: 1_700_000_000,
            subtrack_index: nsf_1.subtrack_index,
        });
        assert_ne!(h0, h1);
        assert_ne!(h1, h1_modified);

        // .cue subtracks on the same backing FLAC produce distinct keys
        let cue_a = normalize_track_id("/mnt/SDCARD/Music/CDImage.flac", Some(0));
        let cue_b = normalize_track_id("/mnt/SDCARD/Music/CDImage.flac", Some(1));
        let hc_a = compute_track_key_hash(&TrackKeyInput {
            canonical_source_id: &cue_a.canonical_source_id,
            file_size: 400_000_000,
            mtime_sec: 1_700_000_100,
            subtrack_index: cue_a.subtrack_index,
        });
        let hc_b = compute_track_key_hash(&TrackKeyInput {
            canonical_source_id: &cue_b.canonical_source_id,
            file_size: 400_000_000,
            mtime_sec: 1_700_000_100,
            subtrack_index: cue_b.subtrack_index,
        });
        assert_ne!(hc_a, hc_b);
    }

    #[test]
    fn binary_db_roundtrips_512_byte_records_and_rejects_crc_corruption() {
        let tmp_dir = std::env::temp_dir().join(format!(
            "uipod_intel_test_{}",
            std::process::id()
        ));
        let _ = std::fs::create_dir_all(&tmp_dir);
        let db_file = tmp_dir.join("track_intel.db");
        let _ = std::fs::remove_file(&db_file);

        let mut rec = TrackAnalysisRecord {
            key_hash: 0xCAFE_BABE_1234_5678,
            subtrack_index: 3,
            channels: 2,
            analysis_flags: analysis_flag::HAS_TBPM_TAG,
            duration_ms: 214_500,
            sample_rate: 44_100,
            bpm: 140.0,
            bpm_confidence: 0.94,
            first_beat_offset_ms: 428,
            tag_tbpm: 140.0,
            tonal_balance: [0.28, 0.24, 0.22, 0.16, 0.10],
            avg_rms: 0.19,
            crest_factor: 5.4,
            kick_baseline: 0.22,
            snare_baseline: 0.17,
            ..Default::default()
        };
        write_fixed_utf8(&mut rec.title, "Hyperdrive — Біт");
        write_fixed_utf8(&mut rec.artist, "UIPod Synth");
        write_fixed_utf8(&mut rec.album, "Brick Sessions");
        write_fixed_utf8(&mut rec.genre, "Electro");
        for i in 0..256 {
            rec.energy_contour[i] = i as u8;
        }

        let mut raw = encode_record_bytes(&rec);
        assert_eq!(raw.len(), 512);
        let decoded = decode_record_bytes(&raw).expect("valid CRC");
        assert_eq!(read_fixed_utf8(&decoded.title), "Hyperdrive — Біт");
        assert!((decoded.bpm - 140.0).abs() < 1e-6);
        assert_eq!(decoded.energy_contour[200], 200);

        // Flip 1 bit inside payload -> CRC32 must reject!
        raw[120] ^= 0x01;
        assert!(decode_record_bytes(&raw).is_none());

        // Persist across reopen
        {
            let mut db = TrackIntelDb::open(&db_file);
            assert!(db.upsert(rec));
            assert_eq!(db.len(), 1);
        }
        {
            let mut db2 = TrackIntelDb::open(&db_file);
            assert_eq!(db2.len(), 1);
            let hit = db2.get(0xCAFE_BABE_1234_5678).expect("persisted hit");
            assert!((hit.bpm - 140.0).abs() < 1e-6);
            assert_eq!(hit.subtrack_index, 3);
        }

        let _ = std::fs::remove_dir_all(&tmp_dir);
    }

    #[test]
    fn lock_free_spsc_and_seqlock_snapshots_roundtrip_cleanly() {
        let mut pcm_in = vec![0i16; 2048 * 2];
        for i in 0..2048 {
            let v = (((i as f32) * 0.1).sin() * 18000.0) as i16;
            pcm_in[i * 2] = v;
            pcm_in[i * 2 + 1] = -v;
        }
        audio_tap_push_s16(&pcm_in, 48_000, 24_000, 1_000);

        let mut pcm_out = [0i16; 512 * 2];
        let (got, sr, _) = read_synced_s16_window(&mut pcm_out, 1_005);
        assert_eq!(got, 512);
        assert_eq!(sr, 48_000);

        let mut rec = TrackAnalysisRecord {
            key_hash: 0xABCD_EF01_2345_6789,
            bpm: 140.0,
            bpm_confidence: 0.92,
            first_beat_offset_ms: 100,
            duration_ms: 180_000,
            ..Default::default()
        };
        write_fixed_utf8(&mut rec.title, "LockFree Test");
        publish_active_analysis(Some(&rec));

        let read_rec = read_active_analysis().expect("active analysis present");
        assert_eq!(read_rec.key_hash, 0xABCD_EF01_2345_6789);
        assert!((read_rec.bpm - 140.0).abs() < 1e-6);

        let mut eng = LiveTelemetryEngine::new();
        let frame = eng.step(
            1,
            rec.key_hash,
            42_857,
            180_000,
            24.0,
            1.0 / 60.0,
            &pcm_out,
            48_000,
            None,
            Some(&read_rec),
        );
        publish_live_frame(&frame);

        let snap = read_live_frame();
        assert_eq!(snap.track_key_hash, rec.key_hash);
        assert_eq!(snap.has_analysis, 1);
        assert!((snap.bpm - 140.0).abs() < 1e-6);
        assert!(snap.vu_l > 0.01);

        publish_active_analysis(None);
        assert!(read_active_analysis().is_none());
    }
}
