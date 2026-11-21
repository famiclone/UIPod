//! Phase 3 end-to-end integration tests verifying:
//! 1. `Now Playing -> Channels` state & interaction: exact 5 channels for `2A03` and 8 channels
//!    for `VRC6` / `MMC5`, live `channel_mute_mask` bit-toggling while audio synthesizes,
//!    Solo channel toggle + restore, `Enable All Channels`, and automatic hiding on non-NSF
//!    (`MP3` / `FLAC`) tracks.
//! 2. File Browser `.nsf` drill-down: `[N ♪]` music count badge, clean separation of
//!    `TrackKind::Music` vs collapsed `Sound Effects (M) ›` virtual subfolder, and accurate
//!    `file.nsf#idx` persistence when adding individual subtracks to `.m3u8` playlists.

use uipod_codecs::{
    get_nsf_active_chip_flags, get_nsf_apu_telemetry, get_nsf_channel_mute_mask,
    get_nsf_endless_loop, reset_nsf_runtime_session, set_nsf_channel_mute_mask,
    set_nsf_endless_loop, NsfDecoder, RustAudioDecoder,
};
use uipod_nsf::{format_subtrack_uri, scan_nsf_container, ChipFlags, SubtrackUri, TrackKind};

fn build_vrc6_and_sfx_test_nsf() -> Vec<u8> {
    let mut rom = vec![0u8; 0x80 + 0x0200];
    rom[0..5].copy_from_slice(b"NESM\x1A");
    rom[0x05] = 1;
    rom[0x06] = 3; // 2 music subtracks + 1 short SFX subtrack
    rom[0x07] = 1;
    rom[0x08..0x0A].copy_from_slice(&0x8000u16.to_le_bytes());
    rom[0x0A..0x0C].copy_from_slice(&0x8000u16.to_le_bytes()); // INIT = $8000
    rom[0x0C..0x0E].copy_from_slice(&0x8040u16.to_le_bytes()); // PLAY = $8040
    rom[0x0E..0x0E + 15].copy_from_slice(b"Castlevania III");
    rom[0x2E..0x2E + 6].copy_from_slice(b"Konami");
    rom[0x6E..0x70].copy_from_slice(&16666u16.to_le_bytes());
    rom[0x7B] = ChipFlags::VRC6;

    // INIT ($8000): store subtrack A into $00, reset frame counter $01 = 0
    // If A == 2 (SFX), play a brief 10-frame noise burst then silence!
    // Otherwise (Music 0 / 1), enable 2A03 Pulse 1 + VRC6 Sawtooth continuously.
    let init_code: &[u8] = &[
        0x85, 0x00, // STA $00
        0xA9, 0x00, // LDA #$00
        0x85, 0x01, // STA $01
        0xA9, 0x0F, // LDA #$0F
        0x8D, 0x15, 0x40, // STA $4015 (enable 2A03 channels)
        0xA5, 0x00, // LDA $00
        0xC9, 0x02, // CMP #$02 (subtrack 2 = SFX)
        0xF0, 0x19, // BEQ +25 ($802B)
        // Music setup: 2A03 Pulse 1 + VRC6 Sawtooth ($B000..$B002)
        0xA9, 0xBF, // LDA #$BF (constant vol 15)
        0x8D, 0x00, 0x40, // STA $4000
        0xA9, 0xFD, // LDA #$FD
        0x8D, 0x02, 0x40, // STA $4002
        0xA9, 0x08, // LDA #$08
        0x8D, 0x03, 0x40, // STA $4003
        0xA9, 0x20, // LDA #$20 (VRC6 Saw rate)
        0x8D, 0x00, 0xB0, // STA $B000
        0xA9, 0xE0, // LDA #$E0
        0x8D, 0x01, 0xB0, // STA $B001
        0xA9, 0x81, // LDA #$81 (enable VRC6 Sawtooth)
        0x8D, 0x02, 0xB0, // STA $B002
        0x60, // RTS
        // $802B: SFX setup (short burst on Pulse 1)
        0xA9, 0x9F, // LDA #$9F
        0x8D, 0x00, 0x40, // STA $4000
        0xA9, 0x50, // LDA #$50
        0x8D, 0x02, 0x40, // STA $4002
        0xA9, 0x08, // LDA #$08
        0x8D, 0x03, 0x40, // STA $4003
        0x60, // RTS
    ];
    rom[0x80..0x80 + init_code.len()].copy_from_slice(init_code);

    // PLAY ($8040): if subtrack == 2, silence after 8 frames; if music, alternate pitch every 32 frames
    let play_code: &[u8] = &[
        0xE6, 0x01, // INC $01
        0xA5, 0x00, // LDA $00
        0xC9, 0x02, // CMP #$02
        0xD0, 0x0D, // BNE +13 ($8055)
        0xA5, 0x01, // LDA $01
        0xC9, 0x08, // CMP #$08
        0x90, 0x07, // BCC +7
        0xA9, 0x00, // LDA #$00
        0x8D, 0x15, 0x40, // STA $4015 (silence SFX!)
        0x60, // RTS
        // $8055: Music pitch variation
        0xA5, 0x01, // LDA $01
        0x29, 0x20, // AND #$20
        0xF0, 0x06, // BEQ +6
        0xA9, 0xC8, // LDA #$C8
        0x8D, 0x02, 0x40, // STA $4002
        0x60, // RTS
        0xA9, 0xFD, // LDA #$FD
        0x8D, 0x02, 0x40, // STA $4002
        0x60, // RTS
    ];
    rom[0x80 + 0x40..0x80 + 0x40 + play_code.len()].copy_from_slice(play_code);
    rom
}

#[test]
fn phase3_channels_submenu_2a03_vrc6_mmc5_live_toggles_solo_enable_all_and_visibility() {
    reset_nsf_runtime_session();

    // 1. When no NSF is playing (e.g. MP3/FLAC), `get_nsf_active_chip_flags()` MUST be `None`
    //    so `Now Playing -> Channels` and `Endless Loop` are hidden.
    assert_eq!(get_nsf_active_chip_flags(), None);

    // 2. Verify exact channel descriptor counts & names for 2A03 (5), VRC6 (8), and MMC5 (8)
    let d_2a03 = ChipFlags(0).channel_descriptors();
    assert_eq!(d_2a03.len(), 5);
    assert_eq!(
        d_2a03.iter().map(|d| d.display_name).collect::<Vec<_>>(),
        vec!["Pulse 1", "Pulse 2", "Triangle", "Noise", "DPCM"]
    );

    let d_vrc6 = ChipFlags(ChipFlags::VRC6).channel_descriptors();
    assert_eq!(d_vrc6.len(), 8);
    assert_eq!(d_vrc6[5].display_name, "VRC6 Pulse 1");
    assert_eq!(d_vrc6[6].display_name, "VRC6 Pulse 2");
    assert_eq!(d_vrc6[7].display_name, "VRC6 Sawtooth");

    let d_mmc5 = ChipFlags(ChipFlags::MMC5).channel_descriptors();
    assert_eq!(d_mmc5.len(), 8);
    assert_eq!(d_mmc5[5].display_name, "MMC5 Pulse 1");
    assert_eq!(d_mmc5[6].display_name, "MMC5 Pulse 2");
    assert_eq!(d_mmc5[7].display_name, "MMC5 PCM");

    // 3. Open VRC6 subtrack #0 and verify live audio channel toggling, Solo, Enable All, and Endless Loop
    let rom = build_vrc6_and_sfx_test_nsf();
    let mut dec =
        NsfDecoder::open_with_rate("/Music/Castlevania III.nsf#0", &rom, 44_100).unwrap();
    assert_eq!(get_nsf_active_chip_flags(), Some(ChipFlags(ChipFlags::VRC6)));
    assert_eq!(get_nsf_channel_mute_mask(), 0);
    assert!(!get_nsf_endless_loop());

    let mut pcm = vec![0i16; 4096 * 2];
    let n = dec.render_stereo_s16(&mut pcm, false);
    assert_eq!(n, 4096);
    let tel = get_nsf_apu_telemetry();
    assert_eq!(tel.channel_count, 8);
    assert!(tel.channels[0].volume > 0.1, "Pulse 1 should be active");
    assert!(
        tel.channels[7].volume > 0.1,
        "VRC6 Sawtooth should be active"
    );

    // Solo VRC6 Sawtooth (`bit 7`): mute bits `0..=6` (`0x007F`), leaving bit `7` audible!
    let valid_mask: u16 = d_vrc6.iter().fold(0u16, |acc, d| acc | (1u16 << d.bit_index));
    let solo_saw_mask = valid_mask & !(1u16 << d_vrc6[7].bit_index);
    assert_eq!(solo_saw_mask, 0x007F);
    set_nsf_channel_mute_mask(solo_saw_mask);
    let _ = dec.render_stereo_s16(&mut pcm, false);
    let peak_saw_solo = pcm.iter().map(|s| s.unsigned_abs()).max().unwrap_or(0);
    assert!(
        peak_saw_solo > 200,
        "Soloed VRC6 Sawtooth must remain clearly audible (peak={peak_saw_solo})"
    );
    let tel_solo = get_nsf_apu_telemetry();
    assert_eq!(tel_solo.channels[0].muted, 1);
    assert_eq!(tel_solo.channels[7].muted, 0);

    // Now mute ALL 8 channels (`0x00FF`) -> output decays to silence
    set_nsf_channel_mute_mask(valid_mask);
    let _ = dec.render_stereo_s16(&mut pcm, false); // drain BlipBuf AC coupling
    let _ = dec.render_stereo_s16(&mut pcm, false);
    let _ = dec.render_stereo_s16(&mut pcm, false);
    let peak_all_muted = pcm.iter().map(|s| s.unsigned_abs()).max().unwrap_or(0);
    assert!(
        peak_all_muted <= 2,
        "All 8 channels muted must produce silence (peak={peak_all_muted})"
    );

    // `Enable All Channels` (`mute_mask = 0`) + `Endless Loop` toggle
    set_nsf_channel_mute_mask(0);
    set_nsf_endless_loop(true);
    assert_eq!(get_nsf_channel_mute_mask(), 0);
    assert!(get_nsf_endless_loop());

    // Resetting session when switching back to MP3/FLAC hides `Channels` & resets mute_mask to 0,
    // while preserving the user's `Endless Loop` preference until toggled off.
    drop(dec);
    reset_nsf_runtime_session();
    assert_eq!(get_nsf_active_chip_flags(), None);
    assert_eq!(get_nsf_channel_mute_mask(), 0);
    assert!(get_nsf_endless_loop());
    set_nsf_endless_loop(false);
    assert!(!get_nsf_endless_loop());
}

#[test]
fn phase3_file_browser_drilldown_separates_music_vs_sfx_and_persists_playlist_uri() {
    let rom = build_vrc6_and_sfx_test_nsf();
    let scanned = scan_nsf_container(&rom).expect("scan VRC6 + SFX container");
    assert_eq!(scanned.game_title, "Castlevania III");
    assert_eq!(scanned.chip_flags.primary_badge_label(), "VRC6");
    assert_eq!(scanned.total_subtracks, 3);

    let music: Vec<_> = scanned
        .subtracks
        .iter()
        .filter(|s| s.kind == TrackKind::Music)
        .collect();
    let sfx: Vec<_> = scanned
        .subtracks
        .iter()
        .filter(|s| s.kind == TrackKind::Sfx)
        .collect();

    // `[2 ♪]` Music count vs `Sound Effects (1) ›` virtual subfolder
    assert_eq!(music.len(), 2);
    assert_eq!(sfx.len(), 1);
    assert_eq!(sfx[0].index, 2);

    // Per-subtrack `Add to Playlist...` serializes & parses exact `/path/Game.nsf#idx`
    let m3u_entry =
        format_subtrack_uri("/mnt/SDCARD/Music/Castlevania III.nsf", music[1].index);
    assert_eq!(m3u_entry, "/mnt/SDCARD/Music/Castlevania III.nsf#1");
    let roundtrip = SubtrackUri::parse(&m3u_entry);
    assert_eq!(roundtrip.file_path, "/mnt/SDCARD/Music/Castlevania III.nsf");
    assert_eq!(roundtrip.subtrack_index, Some(1));
}

#[test]
fn playlist_and_1000_file_header_inspection_are_instant_zero_emulation() {
    use std::ffi::CString;
    use std::time::Instant;
    use uipod_codecs::nsf_bridge::{
        uipod_rs_nsf_inspect_header_fast, uipod_rs_nsf_parse_metadata, UipodRsNsfContainerHeader,
        UipodRsNsfSubtrackInfo,
    };
    use uipod_codecs::UipodRsTrackMeta;

    let mut rom = build_vrc6_and_sfx_test_nsf();
    // Pretend this container has 32 subtracks so a naive full scan would emulate 32 * 480 frames!
    rom[0x06] = 32;

    // 1. Parsing 200 distinct `.nsf#idx` playlist entries from 128-byte NESM headers ONLY
    let header_128 = &rom[0..128];
    let t0 = Instant::now();
    for i in 0..200u16 {
        let uri = CString::new(format!("/mnt/SDCARD/Music/Game_{i:04}.nsf#{}", i % 32)).unwrap();
        let mut meta: UipodRsTrackMeta = unsafe { std::mem::zeroed() };
        let ok = unsafe {
            uipod_rs_nsf_parse_metadata(header_128.as_ptr(), header_128.len(), uri.as_ptr(), &mut meta)
        };
        assert_eq!(ok, 1);
        assert_eq!(meta.duration_ms, 124_000);
    }
    let elapsed_200_meta = t0.elapsed();
    assert!(
        elapsed_200_meta.as_millis() < 50,
        "200 playlist metadata parses took {:?} (expected < 50 ms)",
        elapsed_200_meta
    );

    // 2. Inspecting 1,000 unindexed `.nsf` headers for File Browser `[N ♪]` badges & shuffle
    let t1 = Instant::now();
    for i in 0..1000u32 {
        let path = CString::new(format!("/mnt/SDCARD/NES/Unindexed_{i:04}.nsf")).unwrap();
        let mut hdr: UipodRsNsfContainerHeader = unsafe { std::mem::zeroed() };
        let mut subs = vec![
            UipodRsNsfSubtrackInfo {
                subtrack_index: 0,
                track_kind: 0,
                chip_flags: 0,
                duration_ms: 0,
                fade_ms: 0,
                loop_point_ms: -1,
                title: [0u8; 256],
            };
            32
        ];
        let count = unsafe {
            uipod_rs_nsf_inspect_header_fast(
                path.as_ptr(),
                header_128.as_ptr(),
                header_128.len(),
                &mut hdr,
                subs.as_mut_ptr(),
                subs.len(),
            )
        };
        assert_eq!(count, 32);
    }
    let elapsed_1000_badges = t1.elapsed();
    assert!(
        elapsed_1000_badges.as_millis() < 150,
        "1,000 header badge inspections took {:?} (expected < 150 ms)",
        elapsed_1000_badges
    );

    // 3. Opening a specific subtrack (`#1`) in `NsfDecoder` skips scanning the other 31 subtracks
    let t2 = Instant::now();
    let dec = NsfDecoder::open_with_rate("/mnt/SDCARD/NES/FreshUncached.nsf#1", &rom, 44_100)
        .expect("open #1 directly without full container scan");
    let elapsed_open = t2.elapsed();
    assert_eq!(dec.engine().current_subtrack(), 1);
    assert!(
        elapsed_open.as_millis() < 25,
        "Direct NsfDecoder::open took {:?} (expected < 25 ms)",
        elapsed_open
    );
}
