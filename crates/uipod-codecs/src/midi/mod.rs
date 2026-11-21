//! MIDI (`.mid`, `.midi`, `.kar`, `.rmi`) + SoundFont 2 (`.sf2`) playback engine.
//!
//! Ships `Mobile_0100_base.sf2` embedded as the built-in default SoundFont and supports
//! live hot-swapping to any local `.sf2` SoundFont selected from the UIPod context menu.

pub mod sf2;
pub mod smf;
pub mod synth;

use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use sf2::SoundFont2;
pub use smf::{MidiMetadata, ParsedMidi, MIDI_SAMPLE_RATE};
pub use synth::MidiDecoder;

/// Bundled Beatnik `Mobile_0100_base.sf2` General MIDI + Drum SoundFont (`122,148` bytes).
pub const DEFAULT_SOUNDFONT_NAME: &str = "Mobile_0100_base";
pub static DEFAULT_SOUNDFONT_BYTES: &[u8] =
    include_bytes!("../../../../assets/soundfonts/Mobile_0100_base.sf2");

static DEFAULT_SF2: OnceLock<Arc<SoundFont2>> = OnceLock::new();
static CUSTOM_SF2: Mutex<Option<(String, Arc<SoundFont2>)>> = Mutex::new(None);
static SF2_GENERATION: AtomicU32 = AtomicU32::new(1);

pub fn default_soundfont() -> Arc<SoundFont2> {
    DEFAULT_SF2
        .get_or_init(|| {
            let parsed = SoundFont2::parse(DEFAULT_SOUNDFONT_BYTES)
                .expect("Bundled Mobile_0100_base.sf2 must parse cleanly");
            Arc::new(parsed)
        })
        .clone()
}

pub fn current_soundfont_generation() -> u32 {
    SF2_GENERATION.load(Ordering::Relaxed)
}

pub fn current_soundfont() -> Arc<SoundFont2> {
    if let Ok(guard) = CUSTOM_SF2.lock() {
        if let Some((_, ref sf)) = *guard {
            return sf.clone();
        }
    }
    default_soundfont()
}

/// Returns the active custom `.sf2` path (or `None` when using the built-in `Mobile_0100_base.sf2`).
pub fn current_custom_soundfont_path() -> Option<String> {
    if let Ok(guard) = CUSTOM_SF2.lock() {
        if let Some((ref path, _)) = *guard {
            return Some(path.clone());
        }
    }
    None
}

/// Activate a custom SoundFont from in-memory `.sf2` bytes (used by disk loader & WebAssembly).
pub fn set_custom_soundfont_bytes(label_or_path: &str, bytes: &[u8]) -> Result<(), &'static str> {
    let parsed = SoundFont2::parse(bytes)?;
    let arc = Arc::new(parsed);
    if let Ok(mut guard) = CUSTOM_SF2.lock() {
        *guard = Some((label_or_path.to_string(), arc));
        SF2_GENERATION.fetch_add(1, Ordering::Relaxed);
        Ok(())
    } else {
        Err("Failed to lock SoundFont state")
    }
}

/// Reset to the bundled `Mobile_0100_base.sf2` SoundFont.
pub fn clear_custom_soundfont() {
    if let Ok(mut guard) = CUSTOM_SF2.lock() {
        if guard.is_some() {
            *guard = None;
            SF2_GENERATION.fetch_add(1, Ordering::Relaxed);
        }
    }
}

/// Load and activate a `.sf2` file from disk, or reset to `Mobile_0100_base.sf2` if `path` is empty.
pub fn set_custom_soundfont_from_disk(path: &str) -> Result<(), &'static str> {
    let trimmed = path.trim();
    if trimmed.is_empty() {
        clear_custom_soundfont();
        return Ok(());
    }

    // If the identical path is already active in memory, keep it without re-reading disk.
    if let Ok(guard) = CUSTOM_SF2.lock() {
        if let Some((ref cur_path, _)) = *guard {
            if cur_path == trimmed {
                return Ok(());
            }
        }
    }

    let bytes = std::fs::read(trimmed).map_err(|_| "Cannot read .sf2 file")?;
    set_custom_soundfont_bytes(trimmed, &bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    static TEST_SF2_LOCK: Mutex<()> = Mutex::new(());

    fn build_minimal_smf() -> Vec<u8> {
        let mut smf = Vec::new();
        // MThd: format 0, 1 track, 480 ticks/quarter
        smf.extend_from_slice(b"MThd");
        smf.extend_from_slice(&6u32.to_be_bytes());
        smf.extend_from_slice(&0u16.to_be_bytes());
        smf.extend_from_slice(&1u16.to_be_bytes());
        smf.extend_from_slice(&480u16.to_be_bytes());

        let mut trk = Vec::new();
        // Track Name "Test Melody"
        trk.extend_from_slice(&[0x00, 0xFF, 0x03, 11]);
        trk.extend_from_slice(b"Test Melody");
        // Tempo 120 BPM (500_000 us/qn)
        trk.extend_from_slice(&[0x00, 0xFF, 0x51, 0x03, 0x07, 0xA1, 0x20]);
        // Program 0 (Acoustic Grand Piano) on ch 0
        trk.extend_from_slice(&[0x00, 0xC0, 0x00]);
        // Note On C4 (60) vel 100 + E4 (64) vel 96 + Kick drum (ch 9, key 36)
        trk.extend_from_slice(&[0x00, 0x90, 60, 100]);
        trk.extend_from_slice(&[0x00, 0x90, 64, 96]);
        trk.extend_from_slice(&[0x00, 0x99, 36, 110]);
        // Delta 480 ticks (0.5s): Note Off
        trk.extend_from_slice(&[0x83, 0x60, 0x80, 60, 0]);
        trk.extend_from_slice(&[0x00, 0x80, 64, 0]);
        trk.extend_from_slice(&[0x00, 0x89, 36, 0]);
        // End of Track
        trk.extend_from_slice(&[0x00, 0xFF, 0x2F, 0x00]);

        smf.extend_from_slice(b"MTrk");
        smf.extend_from_slice(&(trk.len() as u32).to_be_bytes());
        smf.extend_from_slice(&trk);
        smf
    }

    #[test]
    fn bundled_mobile_0100_base_sf2_parses_melodic_and_drum_presets() {
        let sf = default_soundfont();
        assert!(!sf.presets.is_empty());
        assert!(sf.samples.len() > 40_000);
        // Must resolve Piano (bank 0, preset 0) and GM Drum kit (bank 128, preset 0)
        let piano = sf.find_preset(0, 0, false).expect("Piano preset");
        assert!(!piano.regions.is_empty());
        let drums = sf.find_preset(128, 0, true).expect("Drum kit preset");
        assert!(!drums.regions.is_empty());
    }

    #[test]
    fn synthesizes_midi_with_default_soundfont_and_supports_seek() {
        let _guard = TEST_SF2_LOCK.lock().unwrap();
        clear_custom_soundfont();
        let raw = build_minimal_smf();
        let parsed = ParsedMidi::parse(&raw).expect("Valid SMF");
        assert_eq!(parsed.meta.title.as_deref(), Some("Test Melody"));
        assert!(parsed.meta.duration_ms >= 1200);

        let mut dec = MidiDecoder::new(parsed);
        let mut pcm = vec![0i16; 4096 * 2];
        let frames = dec.render_stereo_s16(&mut pcm, false);
        assert_eq!(frames, 4096);

        let peak = pcm.iter().map(|&s| (s as i32).abs()).max().unwrap_or(0);
        assert!(peak > 200, "Expected audible synthesized waveform, got peak={peak}");

        dec.seek_ms(250);
        assert!(dec.elapsed_ms() >= 249 && dec.elapsed_ms() <= 251);
        let frames_after = dec.render_stereo_s16(&mut pcm[..1024 * 2], false);
        assert_eq!(frames_after, 1024);
    }

    #[test]
    fn live_soundfont_hot_swap_updates_decoder_without_restarting_playback() {
        let _guard = TEST_SF2_LOCK.lock().unwrap();
        clear_custom_soundfont();
        let raw = build_minimal_smf();
        let parsed = ParsedMidi::parse(&raw).expect("Valid SMF");
        let mut dec = MidiDecoder::new(parsed);

        let mut pcm = vec![0i16; 1024 * 2];
        assert_eq!(dec.render_stereo_s16(&mut pcm, false), 1024);
        let gen_before = current_soundfont_generation();

        // Hot-swap custom SF2 bytes (re-using Mobile_0100_base bytes under a custom path)
        set_custom_soundfont_bytes("/mnt/SDCARD/Music/CustomTest.sf2", DEFAULT_SOUNDFONT_BYTES)
            .expect("Custom SF2 load");
        assert_eq!(
            current_custom_soundfont_path().as_deref(),
            Some("/mnt/SDCARD/Music/CustomTest.sf2")
        );
        assert!(current_soundfont_generation() > gen_before);
        assert_eq!(dec.render_stereo_s16(&mut pcm, false), 1024);

        // Reset back to default Mobile_0100_base.sf2
        clear_custom_soundfont();
        assert!(current_custom_soundfont_path().is_none());
        assert_eq!(dec.render_stereo_s16(&mut pcm, false), 1024);

        // Optional local smoke test against user's sample MIDIs & SF2 banks if present
        let dl_dir = std::path::Path::new("/Users/famiclone/Downloads/midi");
        if dl_dir.is_dir() {
            for sf_rel in ["MOBILE60.SF2", "Android.sf2", "RolandGM.sf2"] {
                let sf_path = dl_dir.join(sf_rel);
                if let Ok(sf_bytes) = std::fs::read(&sf_path) {
                    let sf = SoundFont2::parse(&sf_bytes)
                        .unwrap_or_else(|e| panic!("Failed to parse {sf_rel}: {e}"));
                    assert!(!sf.presets.is_empty());
                }
            }
            let poly_dir = dl_dir.join("C65/Poly");
            if let Ok(entries) = std::fs::read_dir(&poly_dir) {
                for entry in entries.flatten().take(5) {
                    let p = entry.path();
                    if p.extension()
                        .and_then(|s| s.to_str())
                        .is_some_and(|e| e.eq_ignore_ascii_case("mid"))
                    {
                        let bytes = std::fs::read(&p).unwrap();
                        let pm = ParsedMidi::parse(&bytes)
                            .unwrap_or_else(|e| panic!("Failed to parse {:?}: {e}", p));
                        let mut mdec = MidiDecoder::new(pm);
                        let mut buf = vec![0i16; 2048 * 2];
                        let n = mdec.render_stereo_s16(&mut buf, false);
                        assert!(n > 0);
                    }
                }
            }
        }
    }
}
