# UIPod for TrimUI Brick & Android Handhelds

An iPod Mini-inspired offline, **Navidrome**, and **Jellyfin** media player built for the **TrimUI Brick (`tg5040`)** running [NextUI](https://github.com/LoveRetro/NextUI), **Android retro handhelds (`arm64-v8a`)**, and the browser via **WebAssembly + WebGL2**.

Built on a heavily customized [Rockbox](https://github.com/Rockbox/rockbox) audio core with dynamic native-HiDPI pure-Rust OpenGL ES 3.0 UI rendering (`uipod-ui` across `16:9`, `16:10`, `3:2`, `4:3`, `1:1`, and portrait displays), pure-Rust **Navidrome** (`uipod-navidrome`), **Jellyfin** (`uipod-jellyfin`), and **Last.fm** (`uipod-lastfm`) modules, hardware video playback with `.srt` subtitles, studio DSP effects, AutoEQ headphone profiles, native `.uipod` plugins/games, and Bayer-dithered 4-shade GPU audio visualizers.

## Features

- **Dynamic Native-HiDPI Rust UI (`uipod-ui`)**: Vector typography (**Geologica**), **38 four-shade iPod Mini themes** + live HEX palette editor, **6 screen transitions**, Bayer-dithered monochrome album art, and **English + Ukrainian** localization.
- **Unified Multi-Source Library & Browse File Manager**:
  - **Local**: Browse sits first on the Main Menu with last-folder memory and full file management (`Delete`, `Rename`, `Copy`, `Move`, `Info` via Text Viewer), plus watched folders for FLAC, MP3, Opus, Ogg, AAC/M4A, ALAC, WAV, `.m4b` audiobooks, `.cue` sheets, and H.264/MP4 video.
  - **Navidrome / Subsonic (`Settings → Sources → Navidrome`)**: Streaming, sliding-window queue prefetching, Favourites playlist, and persistent background `Download` queue into `/mnt/SDCARD/Media/Music/navidrome`.
  - **Jellyfin (`Settings → Sources → Jellyfin`)**: Movies, TV Shows (`Seasons → Episodes`), and Music libraries with zero-CPU Direct Stream (`Static=true`), automatic server-side H.264/AAC transcoding, resume sync, and offline downloads.
  - **Video Player (`Hold A / MENU`)**: Hardware video playback with selectable **Aspect Mode** (`Fit` / `Crop` / `Stretch`), multi-track **Audio Track** switching, and external sidecar **`.srt` Subtitles**.
  - **Last.fm Scrobbling 2.0 (`Settings → Accounts → Last.fm`)**: Pure-Rust client with on-screen **QR Code Sign-In**, real-time Now Playing updates, local & streaming scrobbling, and a persistent offline scrobble cache.
- **Studio DSP, Graphic EQ & AutoEQ (`Settings → Sound`)**:
  - **10-Band Parametric & GPU Graphic EQ** (Preamp + 10 vertical band sliders with `X` quick Bypass) and a searchable offline **AutoEQ** headphone database.
  - **Stereo Effects Suite (`Settings → Sound → Effects`)**: Master `FX` toggle + **Speed (Texture Warp)** granular time-stretching (`1/16×–1.5×`), **Walkman Tape Mode** (Wow & Flutter, Hiss, Crinkles, Saturation, HF Roll-Off, Pause spin-down/up, Seek motor sound), **Reverb**, **Ping-Pong Delay**, **Chorus**, and **Flanger**.
- **10 Bayer-Dithered 4-Shade GPU Visualizers**: Cassette Deck, Analog Stereo VU Meter, Sonic Vortex, Phosphor Scope, Spectrum Matrix, Cyber Horizon, Liquid Metal, Chladni Plate, Star Warp, and LCD Plasma — synced to speaker & Bluetooth A2DP latency.
- **Native Rust Plugins & Games (`Extras → Plugins`)**: Versioned `.uipod` SDK powering **Music Quiz**, **Sound Racer** (PCM & `.nsf` chiptune rhythm highway), **Tetris**, **Snake**, **Brick Breaker**, **2048**, **Stopwatch**, **LCD Plasma**, and **Text Viewer**.
- **USB MTP & Handheld Power Care**: Live USB-C MTP `/mnt/SDCARD` file transfer without stopping music, Android background foreground-service playback with screen off, Sleep Timer, and TrimUI Brick AXP2202 Battery Charge Limit (`75%–95%`).

## Controls

| Input | Action |
| --- | --- |
| **D-Pad Up / Down** | Navigate lists & menus (hold to fast-scroll); volume or fader adjust in EQ |
| **D-Pad Left / Right** | Page Up / Down in lists; Prev / Next track on Now Playing (hold to smooth-seek) |
| **A / Hold A** | Select / Play-Pause; **Hold A** opens context menu (`Play`, `Download`, `Effects`, file actions, `Video Settings`) |
| **B / Hold B** | Back one screen; **Hold B** returns directly to Main Menu from anywhere |
| **X** | Toggle Large Album Art on Now Playing; Bypass in Graphic EQ; 4×4 / 2×2 grain in Visualizers |
| **START** | Global Play / Pause (or in-game Pause Menu inside games) |
| **MENU** | Context menu; opens `Settings` from Main Menu; Reader Options in Text Viewer |
| **L1 / R1** | Prev / Next track; Page Up / Page Down in lists |
| **L2 / R2** | Prev / Next chapter in `.m4b`/`.cue`; switch Visualizer preset; skip track in Sound Racer |
| **SELECT + VOL ±** | Adjust screen brightness anywhere |
| **Side Switch** | Input Hold / Lock (hardware volume rocker stays active) |

## Installation & Web Simulator

### Try in Browser (WebAssembly + WebGL2)
Open the live interactive TrimUI Brick simulator & Web Studio on **GitHub Pages**: [**famiclone.github.io/UIPod**](https://famiclone.github.io/UIPod/) (or download `UIPod-v0.6.0-web.zip` from Releases).

### TrimUI Brick / Hammer (`tg5040` / NextUI)
1. Download `UIPod-v0.6.0-brick.zip` (or install `UIPod.pak.zip` directly via **NextUI Pak Store**) from [Releases](https://github.com/famiclone/UIPod/releases/latest).
2. Extract `UIPod.pak` into `/mnt/SDCARD/Tools/tg5040/UIPod.pak` (so `launch.sh` sits at `/mnt/SDCARD/Tools/tg5040/UIPod.pak/launch.sh`).
3. On your TrimUI Brick, launch **Tools → UIPod**.

### Android Handhelds (`arm64-v8a`)
1. Download `UIPod-v0.6.0-android-arm64.apk` from [Releases](https://github.com/famiclone/UIPod/releases/latest) (Android 8.0+ / API 26+).
2. Install on your Retroid, Odin, Anbernic, Ayaneo, or gamepad-equipped Android device.
3. Grant media storage access to scan local folders, or configure Navidrome / Jellyfin / Last.fm in **Settings**.

## Building Locally

### TrimUI Brick (`tg5040`)
Requires Docker (`tg5040` cross-toolchain + Rust `aarch64-unknown-linux-gnu` are built automatically from `Dockerfile.uipod`):

```sh
./build.sh      # cross-compile Rust crates + firmware -> build-uipod/
./package.sh    # assemble dist/UIPod.pak, dist/UIPod.pak.zip & dist/UIPod-<ver>-brick.zip
```

### Android (`arm64-v8a`)
Self-bootstraps JDK 17, Android SDK/NDK, SDL2, and OpenSSL under `build-android/.toolchain/` on macOS or Linux:

```sh
./build-android.sh   # assemble & sign dist/UIPod-<ver>-android-arm64.apk
```

### Web Landing Page & WebAssembly Simulator (`web/`)
Requires Rust (`wasm32-unknown-unknown` + `wasm-pack`) and Node.js 22+:

```sh
(cd web && npm ci)
./web/build.sh       # build uipod-wasm + Vite static bundle -> web/dist/
```

## Versioning & Automated Releases

Version metadata lives in `VERSION`, `pak.json`, `web/package.json`, and `pak/uipod/about.txt` (and is automatically stamped into firmware, Android APK, and the WebAssembly site via `UIPOD_VERSION`).

- **CI Auto-Build (`.github/workflows/build.yml`)**: Every push and PR to `main` runs Rust unit tests and builds all three targets in parallel (`UIPod.pak.zip` for TrimUI Brick, `UIPod-*-android-arm64.apk` for Android, and `web/dist` for the WebAssembly simulator).
- **Tagged Release & GitHub Pages (`.github/workflows/release.yml`)**: Every `v*` tag builds the TrimUI Brick `.pak`, signed Android `arm64-v8a` `.apk`, and WebAssembly bundle (`UIPod-<ver>-web.zip`) in parallel, publishes the GitHub Release, and deploys the updated web page live to **GitHub Pages**:
  - From CLI:
    ```sh
    ./scripts/bump-version.sh minor --tag   # or patch / major / v0.6.0
    git push origin main --follow-tags
    ```
  - Or directly from GitHub UI: **Actions → Release → Run workflow** (`patch` / `minor` / `major`).

## License & Credits

Licensed under the **GNU General Public License v2.0** (`GPL-2.0`).

- **Rockbox** — core firmware (`GPL-2.0`)
- **Trimpod Classic** by Werewolf Camp / `tyrannotorus` — original TrimUI Brick adaptation (`GPL-2.0`)
- **1ST_GEN_REMIX** theme by Monica G.
- **Geologica** (`SIL OFL 1.1`) & **PixelMplus** (`M+ Font License`)
- **uMTP-Responder** (`GPL-3.0`, standalone binary in `lib/umtprd`)
- **NextUI** (`tg5040` toolchain & platform environment)
