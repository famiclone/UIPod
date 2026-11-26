# UIPod

An iPod Mini-inspired offline, **Navidrome**, and **Jellyfin** media player for **TrimUI Brick / NextUI**, **Android handhelds**, and the **Web**.
[**Live Web Simulator**](https://famiclone.dev/UIPod) · [**Latest Release**](https://github.com/famiclone/UIPod/releases/latest)

## Highlights

### UI & Customization
- Crisp vector **Geologica** & **Noto Serif** typography with responsive handheld layouts (`4:3`, `16:9`, `16:10`, `3:2`, `1:1`, portrait).
- **38 four-shade iPod Mini themes**, live HEX palette editor, monochrome dithered album art, and **6 screen transitions**.
- Stacked floating HUD overlays for volume, multi-line toasts, live progress bars, and confirmation cards.
- Full **English** and **Ukrainian** UI localization with **Japanese**, **Chinese**, and **Korean** metadata support.

### Music, Chiptunes & Video
- Unified library merging local folders, **Navidrome**, and **Jellyfin** with background offline downloads.
- Built-in SD file browser with delete, rename, copy, move, and file inspection.
- Lossless & compressed audio, `.m4b` audiobooks with chapters, `.nsf`/`.nsfe` NES chiptunes, `.gsf` GBA audio, and MIDI with custom SoundFonts.
- Hardware video playback for local files and Jellyfin Movies/TV with aspect toggle, audio track switching, and `.srt` subtitles.
- Concurrent **Last.fm** (QR sign-in) and **ListenBrainz** scrobbling with persistent offline queues.

### Sound & DJ Engine
- **10-band Graphic & Parametric EQ** with quick bypass and searchable offline **AutoEQ** headphone profiles.
- **Dual-Deck Smart DJ Crossfade** with Vinyl & Master Tempo sync, bass swap, filter wash, and album gapless protection.
- Real-time stereo effects: **Speed** time-stretch, **Walkman Tape Mode**, **Reverb**, **Ping-Pong Delay**, **Chorus**, and **Flanger**.
- Direct hardware DAC output and 3.5mm headphone bass shelf tuning on TrimUI Brick.

### Extras & Handheld Utilities
- **10 four-shade audio visualizers** including Cassette Deck, Analog VU Meter, Sonic Vortex, and Phosphor Scope, with optional palette inversion.
- **4-Shade E-Book Reader** for `.epub`, `.fb2`, `.mobi`, `.md`, and `.txt` with chapters, footnotes, dithered illustrations, and Sans/Serif fonts.
- Built-in games & tools: **DJ Mixer** (live dual-deck mixing with beat sync, loops, and DJ filter), **Beat Stack**, **Music Quiz**, **Sound Racer**, **Pulsar Vanguard**, **Tetris**, **Snake**, **Brick Breaker**, **2048**, **Stopwatch**, and **LCD Plasma**.
- Live USB-C MTP file transfer during playback, Android screen-off background audio, Sleep Timer, and TrimUI Brick charge limit.

## Installation

- **TrimUI Brick (NextUI):** Install via **NextUI Pak Store** (`UIPod.pak.zip`) or extract `UIPod-v0.10.2-brick.zip` into `/mnt/SDCARD/Tools/tg5040/UIPod.pak`.
- **Android Handhelds:** Install `UIPod-v0.10.2-android-arm64.apk` (Android 8.0+ `arm64-v8a`) and grant media storage access.
- **Browser:** Open [**famiclone.dev/UIPod**](https://famiclone.dev/UIPod) (or self-host `UIPod-v0.10.2-web.zip`).

## Controls

| Button | Action |
| --- | --- |
| **D-Pad Up / Down** | Navigate lists (hold to fast-scroll); adjust volume or EQ faders |
| **D-Pad Left / Right** | Page Up / Down in lists; Prev / Next track on Now Playing (hold to seek) |
| **A / Hold A** | Select, Play / Pause, or open footnote in Reader; **Hold A** opens context menu or Reader Options |
| **B / Hold B** | Back one screen; **Hold B** returns to Main Menu from anywhere |
| **X** | Toggle Large Album Art; Bypass in Graphic EQ; aspect mode in Video; font size in Reader |
| **Y** | Toggle Audio Visualizer on Now Playing (`Y` or `B` returns to Now Playing) |
| **START** | Global Play / Pause (or Pause Menu inside games) |
| **MENU** | Context menu; opens Settings on Main Menu; Reader Options in Text Viewer |
| **L1 / R1** | Prev / Next track (or Quick Cut when DJ Crossfade is active); Page Up / Down in lists |
| **L2 / R2** | Prev / Next chapter; switch Visualizer preset; skip track in Reader, Beat Stack, Sound Racer & Pulsar Vanguard |
| **SELECT / SELECT + VOL ±** | Toggle Paginated ↔ Scroll mode in Reader (solo tap); **SELECT + VOL ±** adjusts screen brightness anywhere |
| **Side Switch** | Input Hold / Lock (volume buttons stay active) |

<details>
<summary><strong>Building from Source</strong></summary>

```sh
# TrimUI Brick (tg5040 via Docker)
./build.sh && ./package.sh
./deploy-brick.sh          # optional: deploy over USB ADB (-b rebuild, -f fast binary sync)

# Android Handhelds (arm64-v8a; self-bootstraps JDK 17 + SDK/NDK)
./build-android.sh

# Web Landing Page & WebAssembly Simulator (Rust wasm-pack + Node 22+)
(cd web && npm ci) && ./web/build.sh
```

</details>

## License & Credits

Licensed under **GPL-2.0**. Built on [Rockbox](https://www.rockbox.org/) (`GPL-2.0`) and Trimpod Classic (`GPL-2.0`), with **Geologica** (`SIL OFL 1.1`), **Noto Sans CJK & Noto Serif** (`SIL OFL 1.1`), **uMTP-Responder** (`GPL-3.0`), and **NextUI**.
