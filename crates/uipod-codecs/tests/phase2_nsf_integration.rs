use std::ffi::{CStr, CString};
use std::os::raw::{c_char, c_int, c_void};
use std::ptr;

use uipod_codecs::nsf_bridge::{
    UipodRsNsfContainerHeader, UipodRsNsfSubtrackInfo, uipod_rs_nsf_get_active_chip_flags,
    uipod_rs_nsf_get_channel_descriptors, uipod_rs_nsf_get_channel_mute_mask,
    uipod_rs_nsf_get_endless_loop, uipod_rs_nsf_parse_metadata, uipod_rs_nsf_reset_session,
    uipod_rs_nsf_scan_container, uipod_rs_nsf_set_channel_mute_mask, uipod_rs_nsf_set_endless_loop,
};
use uipod_codecs::{
    UipodRsTrackMeta, get_nsf_active_chip_flags, get_nsf_apu_telemetry, get_nsf_channel_mute_mask,
    reset_nsf_runtime_session, set_nsf_channel_mute_mask, uipod_rs_codec_close,
    uipod_rs_codec_elapsed_ms, uipod_rs_codec_open, uipod_rs_codec_render_s16,
    uipod_rs_codec_seek_ms,
};
use uipod_nsf::{ChipFlags, SubtrackUri, TrackKind};

// Minimal FFI bindings to system libsqlite3 for end-to-end SQLite v4 -> v5 migration & query testing
#[repr(C)]
struct Sqlite3(c_void);
#[repr(C)]
struct Sqlite3Stmt(c_void);

const SQLITE_OK: c_int = 0;
const SQLITE_ROW: c_int = 100;
const SQLITE_DONE: c_int = 101;
const SQLITE_TRANSIENT: isize = -1;

#[link(name = "sqlite3")]
unsafe extern "C" {
    fn sqlite3_open(filename: *const c_char, pp_db: *mut *mut Sqlite3) -> c_int;
    fn sqlite3_close(db: *mut Sqlite3) -> c_int;
    fn sqlite3_exec(
        db: *mut Sqlite3,
        sql: *const c_char,
        callback: Option<unsafe extern "C" fn(*mut c_void, c_int, *mut *mut c_char, *mut *mut c_char) -> c_int>,
        arg: *mut c_void,
        errmsg: *mut *mut c_char,
    ) -> c_int;
    fn sqlite3_prepare_v2(
        db: *mut Sqlite3,
        z_sql: *const c_char,
        n_byte: c_int,
        pp_stmt: *mut *mut Sqlite3Stmt,
        pz_tail: *mut *const c_char,
    ) -> c_int;
    fn sqlite3_bind_int(stmt: *mut Sqlite3Stmt, idx: c_int, val: c_int) -> c_int;
    fn sqlite3_bind_null(stmt: *mut Sqlite3Stmt, idx: c_int) -> c_int;
    fn sqlite3_bind_text(
        stmt: *mut Sqlite3Stmt,
        idx: c_int,
        val: *const c_char,
        n: c_int,
        destructor: isize,
    ) -> c_int;
    fn sqlite3_step(stmt: *mut Sqlite3Stmt) -> c_int;
    fn sqlite3_reset(stmt: *mut Sqlite3Stmt) -> c_int;
    fn sqlite3_finalize(stmt: *mut Sqlite3Stmt) -> c_int;
    fn sqlite3_column_int(stmt: *mut Sqlite3Stmt, i_col: c_int) -> c_int;
    fn sqlite3_column_text(stmt: *mut Sqlite3Stmt, i_col: c_int) -> *const u8;
}

unsafe fn sql_exec(db: *mut Sqlite3, sql: &str) -> Result<(), String> {
    let c_sql = CString::new(sql).unwrap();
    let mut err: *mut c_char = ptr::null_mut();
    let rc = unsafe { sqlite3_exec(db, c_sql.as_ptr(), None, ptr::null_mut(), &mut err) };
    if rc != SQLITE_OK {
        let msg = if !err.is_null() {
            unsafe { CStr::from_ptr(err) }.to_string_lossy().into_owned()
        } else {
            format!("sqlite rc={rc}")
        };
        return Err(msg);
    }
    Ok(())
}

/// Builds a valid multi-subtrack 6502 NSF ROM containing:
/// - Subtracks `0` & `1`: continuous arpeggiating Pulse 1 + Triangle music (`TrackKind::Music`)
/// - Subtrack `2`: short 12-frame coin SFX (`TrackKind::Sfx`)
fn build_synthetic_nsf(game: &str, artist: &str, copyright: &str, chip_flags: u8) -> Vec<u8> {
    let mut rom = vec![0u8; 0x80 + 0x0200];
    rom[0..5].copy_from_slice(b"NESM\x1A");
    rom[0x05] = 1; // version
    rom[0x06] = 3; // 3 total songs (2 Music, 1 SFX)
    rom[0x07] = 1; // starting song (1-based)
    rom[0x08..0x0A].copy_from_slice(&0x8000u16.to_le_bytes()); // load
    rom[0x0A..0x0C].copy_from_slice(&0x8000u16.to_le_bytes()); // init
    rom[0x0C..0x0E].copy_from_slice(&0x8020u16.to_le_bytes()); // play

    let write_padded = |dst: &mut [u8], text: &str| {
        let bytes = text.as_bytes();
        let n = bytes.len().min(dst.len() - 1);
        dst[..n].copy_from_slice(&bytes[..n]);
    };
    write_padded(&mut rom[0x0E..0x2E], game);
    write_padded(&mut rom[0x2E..0x4E], artist);
    write_padded(&mut rom[0x4E..0x6E], copyright);
    rom[0x6E..0x70].copy_from_slice(&16639u16.to_le_bytes()); // NTSC 60 Hz
    rom[0x7B] = chip_flags;

    let prg = &mut rom[0x80..];
    // INIT ($8000): A = subtrack (0, 1, 2)
    // Store subtrack in $00, reset frame counter $01 = 0, enable Pulse1 + Triangle ($4015 = $05)
    let init_code: &[u8] = &[
        0x85, 0x00, // STA $00 (subtrack)
        0xA9, 0x00, // LDA #$00
        0x85, 0x01, // STA $01 (tick)
        0xA9, 0x05, // LDA #$05
        0x8D, 0x15, 0x40, // STA $4015
        0x60, // RTS
    ];
    prg[0x00..init_code.len()].copy_from_slice(init_code);

    // PLAY ($8020):
    // Increment $01; if subtrack ($00) == 2 (SFX), silence after 12 frames;
    // otherwise step through 4 arpeggio notes every 8 frames so heuristic sees continuous Music!
    let play_code: &[u8] = &[
        0xE6, 0x01, // 8020: INC $01
        0xA5, 0x00, // 8022: LDA $00
        0xC9, 0x02, // 8024: CMP #$02
        0xF0, 0x22, // 8026: BEQ $804A (SFX handler)
        // Music handler: index = (($01 >> 3) & 3)
        0xA5, 0x01, // 8028: LDA $01
        0x4A, // 802A: LSR A
        0x4A, // 802B: LSR A
        0x4A, // 802C: LSR A
        0x29, 0x03, // 802D: AND #$03
        0x18, // 802F: CLC
        0x65, 0x00, // 8030: ADC $00 (offset pitch table slightly between song 0 & song 1)
        0xAA, // 8032: TAX
        0xA9, 0x9F, // 8033: LDA #$9F (50% duty, constant vol 15)
        0x8D, 0x00, 0x40, // 8035: STA $4000
        0xBD, 0x80, 0x80, // 8038: LDA $8080,X (period lo)
        0x8D, 0x02, 0x40, // 803B: STA $4002
        0xA9, 0x08, // 803E: LDA #$08
        0x8D, 0x03, 0x40, // 8040: STA $4003
        0xA9, 0xFF, // 8043: LDA #$FF (Triangle linear counter)
        0x8D, 0x08, 0x40, // 8045: STA $4008
        0x60, // 8048: RTS
        0xEA, // 8049: NOP
        // SFX handler ($804A): play quick 2-tone blip for 12 frames, then mute $4015 = 0
        0xA5, 0x01, // 804A: LDA $01
        0xC9, 0x0C, // 804C: CMP #$0C
        0xB0, 0x10, // 804E: BCS $8060 (mute)
        0xA9, 0x9E, // 8050: LDA #$9E
        0x8D, 0x00, 0x40, // 8052: STA $4000
        0xA9, 0x70, // 8055: LDA #$70
        0x8D, 0x02, 0x40, // 8057: STA $4002
        0xA9, 0x08, // 805A: LDA #$08
        0x8D, 0x03, 0x40, // 805C: STA $4003
        0x60, // 805F: RTS
        // Mute ($8060):
        0xA9, 0x00, // 8060: LDA #$00
        0x8D, 0x00, 0x40, // 8062: STA $4000
        0x8D, 0x15, 0x40, // 8065: STA $4015
        0x60, // 8068: RTS
    ];
    prg[0x20..0x20 + play_code.len()].copy_from_slice(play_code);

    // Note period table at $8080 (5 entries)
    prg[0x80..0x85].copy_from_slice(&[0xFD, 0xC9, 0xA9, 0x7F, 0x64]);
    rom
}

fn build_minimal_midi() -> Vec<u8> {
    let mut midi = Vec::new();
    midi.extend_from_slice(b"MThd");
    midi.extend_from_slice(&6u32.to_be_bytes());
    midi.extend_from_slice(&0u16.to_be_bytes()); // format 0
    midi.extend_from_slice(&1u16.to_be_bytes()); // 1 track
    midi.extend_from_slice(&480u16.to_be_bytes()); // 480 tpqn

    let trk: &[u8] = &[
        0x00, 0x90, 60, 100, // Note On C4
        0x83, 0x60, 0x80, 60, 0, // +480 ticks: Note Off C4
        0x00, 0xFF, 0x2F, 0x00, // End of Track
    ];
    midi.extend_from_slice(b"MTrk");
    midi.extend_from_slice(&(trk.len() as u32).to_be_bytes());
    midi.extend_from_slice(trk);
    midi
}

#[test]
fn sqlite_migration_batch_scanner_true_shuffle_cascade_and_m3u8_roundtrip() {
    let tmp_dir = std::env::temp_dir().join(format!("uipod_phase2_test_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp_dir);
    std::fs::create_dir_all(&tmp_dir).unwrap();

    let castlevania_path = tmp_dir.join("Castlevania.nsf");
    let megaman_path = tmp_dir.join("MegaMan2.nsf");
    let contra_path = tmp_dir.join("Contra.nsf");

    std::fs::write(
        &castlevania_path,
        build_synthetic_nsf("Castlevania", "Kinuyo Yamashita", "1986 Konami", ChipFlags::VRC6),
    )
    .unwrap();
    std::fs::write(
        &megaman_path,
        build_synthetic_nsf("Mega Man 2", "Takashi Tateishi", "1988 Capcom", 0),
    )
    .unwrap();
    std::fs::write(
        &contra_path,
        build_synthetic_nsf("Contra", "Kazuki Muraoka", "1988 Konami", 0),
    )
    .unwrap();

    unsafe {
        let mut db: *mut Sqlite3 = ptr::null_mut();
        let mem = CString::new(":memory:").unwrap();
        assert_eq!(sqlite3_open(mem.as_ptr(), &mut db), SQLITE_OK);

        // 1. Seed legacy v4 schema with a standard FLAC track and an unexpanded legacy .nsf row
        sql_exec(
            db,
            "CREATE TABLE roots(id INTEGER PRIMARY KEY, path TEXT NOT NULL UNIQUE, category INTEGER NOT NULL);
             CREATE TABLE dirs(path TEXT PRIMARY KEY, parent TEXT, root_id INTEGER NOT NULL, mtime INTEGER NOT NULL);
             CREATE TABLE tracks(
               id INTEGER PRIMARY KEY, path TEXT NOT NULL UNIQUE, dir TEXT NOT NULL,
               root_id INTEGER NOT NULL, category INTEGER NOT NULL, mtime INTEGER NOT NULL, size INTEGER NOT NULL,
               title TEXT, artist TEXT, albumartist TEXT, album TEXT, genre TEXT,
               composer TEXT, grouping TEXT, year INTEGER, tracknum INTEGER, discnum INTEGER,
               duration_ms INTEGER, codectype INTEGER, bitrate INTEGER, frequency INTEGER,
               browse_artist TEXT, sort_artist TEXT);
             INSERT INTO roots(id, path, category) VALUES(1, '/Music', 0);
             INSERT INTO dirs(path, parent, root_id, mtime) VALUES('/Music', NULL, 1, 1700000000);
             INSERT INTO tracks(path, dir, root_id, category, mtime, size, title, artist, album, duration_ms)
               VALUES('/Music/Prelude.flac', '/Music', 1, 0, 1700000000, 123456, 'Prelude', 'Artist', 'Album', 180000);
             INSERT INTO tracks(path, dir, root_id, category, mtime, size, title, artist, album, duration_ms)
               VALUES('/Music/Castlevania.nsf', '/Music', 1, 0, 1700000000, 1024, 'Castlevania', 'Konami', 'Castlevania', 120000);",
        )
        .unwrap();

        // 2. Apply v4 -> v5 migration (`apps/uipod_library.c:db_migrate`)
        sql_exec(
            db,
            "ALTER TABLE tracks ADD COLUMN source INTEGER NOT NULL DEFAULT 0;
             ALTER TABLE tracks ADD COLUMN file_path TEXT NOT NULL DEFAULT '';
             ALTER TABLE tracks ADD COLUMN subtrack_index INTEGER NULL;
             ALTER TABLE tracks ADD COLUMN track_kind INTEGER NOT NULL DEFAULT 0;
             ALTER TABLE tracks ADD COLUMN chip_flags INTEGER NOT NULL DEFAULT 0;
             ALTER TABLE tracks ADD COLUMN loop_point_ms INTEGER NULL;
             UPDATE tracks SET file_path = path WHERE file_path = '' OR file_path IS NULL;
             UPDATE dirs SET mtime = 0 WHERE path IN (
               SELECT DISTINCT dir FROM tracks
               WHERE subtrack_index IS NULL AND (lower(path) LIKE '%.nsf' OR lower(path) LIKE '%.nsfe'));
             DELETE FROM tracks WHERE subtrack_index IS NULL
               AND (lower(path) LIKE '%.nsf' OR lower(path) LIKE '%.nsfe');
             CREATE UNIQUE INDEX idx_tracks_src_file_sub
               ON tracks(source, file_path, COALESCE(subtrack_index, -1));
             CREATE INDEX idx_tracks_file_path ON tracks(file_path);
             CREATE INDEX idx_tracks_kind ON tracks(category, track_kind);",
        )
        .unwrap();

        // Verify standard FLAC row survived migration with `file_path == path` and `subtrack_index IS NULL`
        let mut chk: *mut Sqlite3Stmt = ptr::null_mut();
        let chk_sql = CString::new(
            "SELECT file_path, COALESCE(subtrack_index, -1), track_kind FROM tracks WHERE path='/Music/Prelude.flac';",
        )
        .unwrap();
        assert_eq!(sqlite3_prepare_v2(db, chk_sql.as_ptr(), -1, &mut chk, ptr::null_mut()), SQLITE_OK);
        assert_eq!(sqlite3_step(chk), SQLITE_ROW);
        let fp = CStr::from_ptr(sqlite3_column_text(chk, 0) as *const c_char).to_string_lossy();
        assert_eq!(fp, "/Music/Prelude.flac");
        assert_eq!(sqlite3_column_int(chk, 1), -1);
        assert_eq!(sqlite3_column_int(chk, 2), 0);
        sqlite3_finalize(chk);

        // 3. Batch-scan all 3 `.nsf` containers via `uipod_rs_nsf_scan_container` in one transaction
        sql_exec(db, "BEGIN;").unwrap();
        let ins_sql = CString::new(
            "INSERT INTO tracks(path, dir, root_id, category, mtime, size, title, artist, album,
                                tracknum, duration_ms, source, file_path, subtrack_index, track_kind, chip_flags, loop_point_ms)
             VALUES(?1, '/Music', 1, 0, 1700000100, 640, ?2, ?3, ?4, ?5, ?6, 0, ?7, ?8, ?9, ?10, ?11);",
        )
        .unwrap();
        let mut ins: *mut Sqlite3Stmt = ptr::null_mut();
        assert_eq!(sqlite3_prepare_v2(db, ins_sql.as_ptr(), -1, &mut ins, ptr::null_mut()), SQLITE_OK);

        for (real_path, logical_file) in [
            (&castlevania_path, "/Music/Castlevania.nsf"),
            (&megaman_path, "/Music/MegaMan2.nsf"),
            (&contra_path, "/Music/Contra.nsf"),
        ] {
            let c_real = CString::new(real_path.to_str().unwrap()).unwrap();
            let mut hdr = UipodRsNsfContainerHeader {
                game_title: [0; 256],
                artist: [0; 256],
                copyright: [0; 256],
                total_subtracks: 0,
                starting_subtrack: 0,
                chip_flags: 0,
                is_pal: 0,
                payload_crc32: 0,
            };
            let mut subs = [UipodRsNsfSubtrackInfo {
                subtrack_index: 0,
                track_kind: 0,
                chip_flags: 0,
                duration_ms: 0,
                fade_ms: 0,
                loop_point_ms: -1,
                title: [0; 256],
            }; 16];

            let count = uipod_rs_nsf_scan_container(
                c_real.as_ptr(),
                ptr::null(),
                0,
                &mut hdr,
                subs.as_mut_ptr(),
                subs.len(),
            );
            assert_eq!(count, 3, "Each synthetic NSF must yield 3 subtracks");

            // Verify 2 Music + 1 Sfx classification
            assert_eq!(subs[0].track_kind, TrackKind::Music as u8);
            assert_eq!(subs[1].track_kind, TrackKind::Music as u8);
            assert_eq!(subs[2].track_kind, TrackKind::Sfx as u8);

            let c_artist = CStr::from_ptr(hdr.artist.as_ptr().cast::<c_char>());
            let c_album = CStr::from_ptr(hdr.game_title.as_ptr().cast::<c_char>());
            let c_logical = CString::new(logical_file).unwrap();

            for sub in subs.iter().take(count) {
                let uri = CString::new(format!("{logical_file}#{}", sub.subtrack_index)).unwrap();
                sqlite3_bind_text(ins, 1, uri.as_ptr(), -1, SQLITE_TRANSIENT);
                sqlite3_bind_text(
                    ins,
                    2,
                    sub.title.as_ptr().cast::<c_char>(),
                    -1,
                    SQLITE_TRANSIENT,
                );
                sqlite3_bind_text(ins, 3, c_artist.as_ptr(), -1, SQLITE_TRANSIENT);
                sqlite3_bind_text(ins, 4, c_album.as_ptr(), -1, SQLITE_TRANSIENT);
                sqlite3_bind_int(ins, 5, (sub.subtrack_index as c_int) + 1);
                sqlite3_bind_int(ins, 6, sub.duration_ms as c_int);
                sqlite3_bind_text(ins, 7, c_logical.as_ptr(), -1, SQLITE_TRANSIENT);
                sqlite3_bind_int(ins, 8, sub.subtrack_index as c_int);
                sqlite3_bind_int(ins, 9, sub.track_kind as c_int);
                sqlite3_bind_int(ins, 10, sub.chip_flags as c_int);
                if sub.loop_point_ms >= 0 {
                    sqlite3_bind_int(ins, 11, sub.loop_point_ms);
                } else {
                    sqlite3_bind_null(ins, 11);
                }
                assert_eq!(sqlite3_step(ins), SQLITE_DONE);
                sqlite3_reset(ins);
            }
        }
        sqlite3_finalize(ins);
        sql_exec(db, "COMMIT;").unwrap();

        // 4. Verify uniqueness index rejects duplicate `(source, file_path, subtrack_index)`
        let dup_res = sql_exec(
            db,
            "INSERT INTO tracks(path, dir, root_id, category, mtime, size, source, file_path, subtrack_index)
             VALUES('/Music/Castlevania_dup_uri', '/Music', 1, 0, 1, 1, 0, '/Music/Castlevania.nsf', 1);",
        );
        assert!(dup_res.is_err(), "idx_tracks_src_file_sub must reject duplicate subtrack_index");

        // 5. Verify True Subtrack Shuffle (`WHERE COALESCE(track_kind, 0) = 0`) across all files:
        //    1 FLAC + (3 NSF files * 2 Music subtracks) = 7 Music tracks, and ZERO SFX (`#2`) leakage!
        let shuf_sql = CString::new(
            "SELECT path, track_kind FROM tracks WHERE COALESCE(track_kind, 0) = 0 ORDER BY RANDOM();",
        )
        .unwrap();
        let mut shuf: *mut Sqlite3Stmt = ptr::null_mut();
        assert_eq!(sqlite3_prepare_v2(db, shuf_sql.as_ptr(), -1, &mut shuf, ptr::null_mut()), SQLITE_OK);
        let mut shuffled_paths = Vec::new();
        while sqlite3_step(shuf) == SQLITE_ROW {
            let p = CStr::from_ptr(sqlite3_column_text(shuf, 0) as *const c_char)
                .to_string_lossy()
                .into_owned();
            let kind = sqlite3_column_int(shuf, 1);
            assert_eq!(kind, 0, "SFX subtrack leaked into Shuffle: {p}");
            assert!(!p.ends_with("#2"), "SFX subtrack #2 leaked into Shuffle: {p}");
            shuffled_paths.push(p);
        }
        sqlite3_finalize(shuf);
        assert_eq!(shuffled_paths.len(), 7);
        assert!(shuffled_paths.iter().any(|p| p == "/Music/Castlevania.nsf#0"));
        assert!(shuffled_paths.iter().any(|p| p == "/Music/Castlevania.nsf#1"));
        assert!(shuffled_paths.iter().any(|p| p == "/Music/MegaMan2.nsf#0"));
        assert!(shuffled_paths.iter().any(|p| p == "/Music/MegaMan2.nsf#1"));
        assert!(shuffled_paths.iter().any(|p| p == "/Music/Contra.nsf#0"));
        assert!(shuffled_paths.iter().any(|p| p == "/Music/Contra.nsf#1"));

        // 6. Verify File Manager rename cascade (`Contra.nsf` -> `Probotector.nsf`) & delete cascade
        sql_exec(
            db,
            "UPDATE tracks SET
               path = CASE WHEN subtrack_index IS NOT NULL
                      THEN ('/Music/Probotector.nsf#' || subtrack_index)
                      ELSE '/Music/Probotector.nsf' END,
               file_path = '/Music/Probotector.nsf'
             WHERE file_path = '/Music/Contra.nsf';",
        )
        .unwrap();

        let mut ren_st: *mut Sqlite3Stmt = ptr::null_mut();
        let ren_sql = CString::new(
            "SELECT COUNT(*) FROM tracks WHERE path IN ('/Music/Probotector.nsf#0', '/Music/Probotector.nsf#1', '/Music/Probotector.nsf#2');",
        )
        .unwrap();
        assert_eq!(sqlite3_prepare_v2(db, ren_sql.as_ptr(), -1, &mut ren_st, ptr::null_mut()), SQLITE_OK);
        assert_eq!(sqlite3_step(ren_st), SQLITE_ROW);
        assert_eq!(sqlite3_column_int(ren_st, 0), 3);
        sqlite3_finalize(ren_st);

        sql_exec(db, "DELETE FROM tracks WHERE file_path = '/Music/Probotector.nsf';").unwrap();

        // 7. Verify `.m3u8` round-trip serialization + parsing + metadata lookup of `/Music/Castlevania.nsf#1`
        let m3u8_path = tmp_dir.join("VampireHunters.m3u8");
        let m3u8_body = "#EXTM3U\n#EXTINF:124,Kinuyo Yamashita - Castlevania - Track 02\n/Music/Castlevania.nsf#1\n/Music/MegaMan2.nsf#0\n";
        std::fs::write(&m3u8_path, m3u8_body).unwrap();

        let read_back = std::fs::read_to_string(&m3u8_path).unwrap();
        let parsed_entries: Vec<&str> = read_back
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty() && !l.starts_with('#'))
            .collect();
        assert_eq!(
            parsed_entries,
            vec!["/Music/Castlevania.nsf#1", "/Music/MegaMan2.nsf#0"]
        );

        let uri = SubtrackUri::parse(parsed_entries[0]);
        assert_eq!(uri.file_path, "/Music/Castlevania.nsf");
        assert_eq!(uri.subtrack_index, Some(1));
        assert_eq!(
            uipod_nsf::format_subtrack_uri(uri.file_path, uri.subtrack_index.unwrap()),
            "/Music/Castlevania.nsf#1"
        );

        // Resolve subtrack metadata directly from container bytes + `/Music/Castlevania.nsf#1` URI
        let cv_bytes = std::fs::read(&castlevania_path).unwrap();
        let c_uri = CString::new(parsed_entries[0]).unwrap();
        let mut meta = UipodRsTrackMeta {
            title: [0; 256],
            artist: [0; 256],
            album: [0; 256],
            genre: [0; 128],
            composer: [0; 256],
            comment: [0; 256],
            year: 0,
            track_num: 0,
            duration_ms: 0,
            sample_rate: 0,
            bitrate_kbps: 0,
        };
        assert_eq!(
            uipod_rs_nsf_parse_metadata(cv_bytes.as_ptr(), cv_bytes.len(), c_uri.as_ptr(), &mut meta),
            1
        );
        assert_eq!(meta.track_num, 2); // subtrack #1 -> track 2
        assert_eq!(meta.year, 1986);
        assert_eq!(
            CStr::from_ptr(meta.album.as_ptr().cast::<c_char>()).to_string_lossy(),
            "Castlevania"
        );
        assert_eq!(
            CStr::from_ptr(meta.comment.as_ptr().cast::<c_char>()).to_string_lossy(),
            "VRC6"
        );

        sqlite3_close(db);
    }

    let _ = std::fs::remove_dir_all(&tmp_dir);
}

#[test]
fn nsf_channel_mute_mask_live_audio_same_container_persistence_and_reset_hygiene() {
    let game_a = build_synthetic_nsf("Castlevania III", "Konami Kukeiha Club", "1989 Konami", ChipFlags::VRC6);
    let game_b = build_synthetic_nsf("Mega Man 2", "Takashi Tateishi", "1988 Capcom", 0);
    let midi_bytes = build_minimal_midi();

    unsafe {
        reset_nsf_runtime_session();
        assert_eq!(get_nsf_channel_mute_mask(), 0);
        assert_eq!(get_nsf_active_chip_flags(), None);
        assert_eq!(uipod_rs_nsf_get_active_chip_flags(), -1);

        let c_nsf = CString::new("nsf").unwrap();
        let c_midi = CString::new("midi").unwrap();
        let uri_a0 = CString::new("/Music/Castlevania3.nsf#0").unwrap();
        let uri_a1 = CString::new("/Music/Castlevania3.nsf#1").unwrap();
        let uri_b0 = CString::new("/Music/MegaMan2.nsf#0").unwrap();
        let uri_mid = CString::new("/Music/Stage.mid").unwrap();

        // 1. Open Game A Subtrack #0 -> verify active VRC6 chip flags & 8 channel descriptors (5 2A03 + 3 VRC6)
        assert_eq!(
            uipod_rs_codec_open(c_nsf.as_ptr(), uri_a0.as_ptr(), game_a.as_ptr(), game_a.len()),
            0
        );
        assert_eq!(get_nsf_active_chip_flags(), Some(ChipFlags(ChipFlags::VRC6)));
        assert_eq!(uipod_rs_nsf_get_active_chip_flags(), ChipFlags::VRC6 as c_int);
        assert_eq!(uipod_rs_nsf_get_channel_descriptors(ChipFlags::VRC6, ptr::null_mut(), 0), 8);

        // Render unmuted frames -> verify audible PCM output
        let mut pcm = vec![0i16; 2048 * 2];
        let frames = uipod_rs_codec_render_s16(pcm.as_mut_ptr(), 2048, 0);
        assert_eq!(frames, 2048);
        let peak_unmuted = pcm.iter().map(|s| s.unsigned_abs()).max().unwrap_or(0);
        assert!(peak_unmuted > 200, "Expected audible PCM before muting, got peak={peak_unmuted}");

        // 2. Mute all channels live (`0x00FF`) -> verify immediate silence & SeqLock telemetry sync
        uipod_rs_nsf_set_channel_mute_mask(0x00FF);
        assert_eq!(uipod_rs_nsf_get_channel_mute_mask(), 0x00FF);
        // Drain the ~15 Hz AC-coupling capacitor decay (~90 ms = 4096 samples), then verify crisp zero output
        let _ = uipod_rs_codec_render_s16(pcm.as_mut_ptr(), 2048, 0);
        let _ = uipod_rs_codec_render_s16(pcm.as_mut_ptr(), 2048, 0);
        let frames_muted = uipod_rs_codec_render_s16(pcm.as_mut_ptr(), 2048, 0);
        assert_eq!(frames_muted, 2048);
        let peak_muted = pcm[..(frames_muted as usize) * 2]
            .iter()
            .map(|s| s.unsigned_abs())
            .max()
            .unwrap_or(0);
        assert!(
            peak_muted <= 2,
            "All-channel mute mask 0x00FF must silence synthesis immediately, got peak={peak_muted}"
        );
        let tel = get_nsf_apu_telemetry();
        assert_eq!(tel.mute_mask, 0x00FF);
        assert_eq!(tel.channel_count, 8);

        // Verify in-track seek (`L2`/`R2`) and `endless_loop` runtime toggle
        assert_eq!(uipod_rs_codec_seek_ms(3_000), 0);
        assert!(uipod_rs_codec_elapsed_ms() >= 2_980);

        assert_eq!(uipod_rs_nsf_get_endless_loop(), 0);
        uipod_rs_nsf_set_endless_loop(1);
        assert_eq!(uipod_rs_nsf_get_endless_loop(), 1);
        // Sync live `endless_loop` into active engine and verify `Seek +15s` past 2-loop boundary
        let _ = uipod_rs_codec_render_s16(pcm.as_mut_ptr(), 64, 0);
        assert_eq!(uipod_rs_codec_seek_ms(15_000), 0);
        assert!(uipod_rs_codec_elapsed_ms() >= 14_980);
        uipod_rs_nsf_set_endless_loop(0);

        // 3. Set custom bassline mask (`0b00011` = mute Pulse 1 & Pulse 2) and step to Subtrack #1
        //    within the SAME `.nsf` container -> mask MUST persist!
        set_nsf_channel_mute_mask(0b00011);
        uipod_rs_codec_close();
        assert_eq!(
            uipod_rs_codec_open(c_nsf.as_ptr(), uri_a1.as_ptr(), game_a.as_ptr(), game_a.len()),
            0
        );
        assert_eq!(
            get_nsf_channel_mute_mask(),
            0b00011,
            "Channel mute mask must persist across subtracks within the same .nsf container"
        );

        // 4. Switch to a DIFFERENT `.nsf` container (`MegaMan2.nsf#0`) -> mask MUST reset to 0!
        uipod_rs_codec_close();
        assert_eq!(
            uipod_rs_codec_open(c_nsf.as_ptr(), uri_b0.as_ptr(), game_b.as_ptr(), game_b.len()),
            0
        );
        assert_eq!(
            get_nsf_channel_mute_mask(),
            0,
            "Channel mute mask must reset to 0 when switching to a different .nsf container"
        );
        assert_eq!(get_nsf_active_chip_flags(), Some(ChipFlags(0)));

        // 5. Mute a channel, then switch to a standard/non-NSF file (`Stage.mid`) -> mask MUST reset to 0
        //    and `get_nsf_active_chip_flags()` MUST become `None`!
        set_nsf_channel_mute_mask(0b00101);
        uipod_rs_codec_close();
        assert_eq!(
            uipod_rs_codec_open(
                c_midi.as_ptr(),
                uri_mid.as_ptr(),
                midi_bytes.as_ptr(),
                midi_bytes.len()
            ),
            0
        );
        assert_eq!(
            get_nsf_channel_mute_mask(),
            0,
            "Channel mute mask must reset to 0 when loading a non-NSF audio file"
        );
        assert_eq!(get_nsf_active_chip_flags(), None);
        uipod_rs_codec_close();

        // 6. Open NSF again, set mask, then stop playback cleanly (`uipod_rs_nsf_reset_session`) -> mask MUST reset to 0!
        assert_eq!(
            uipod_rs_codec_open(c_nsf.as_ptr(), uri_b0.as_ptr(), game_b.as_ptr(), game_b.len()),
            0
        );
        set_nsf_channel_mute_mask(0b01111);
        uipod_rs_codec_close();
        uipod_rs_nsf_reset_session();
        assert_eq!(get_nsf_channel_mute_mask(), 0);
        assert_eq!(get_nsf_active_chip_flags(), None);
    }
}
