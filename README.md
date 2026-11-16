# UIPod for TrimUI Brick

An iPod-inspired offline, **Navidrome**, and **Jellyfin** media player built for the **TrimUI Brick (`tg5040`)** running [NextUI](https://github.com/LoveRetro/NextUI).

Built on a heavily customized [Rockbox](https://github.com/Rockbox/rockbox) core with native `1024×768` HiDPI pure-Rust UI rendering (`uipod-ui`), pure-Rust Navidrome (`uipod-navidrome`) & Jellyfin (`uipod-jellyfin`) streaming/caching modules, hardware CedarX video playback, AutoEQ headphone profiles, and 100 Milkdrop GPU visualizations (`projectM`).

## Features

- **Native `1024×768` HiDPI UI** rendered in Rust (`uipod-ui`) with **Geologica** vector typography and **English + Ukrainian** localization.
- **Three Media Sources (`Settings → Source type`)**:
  - **Local**: Offline music, podcasts, audiobooks (`.m4b` chapter skip + resume), and videos on SD card.
  - **Navidrome**: Subsonic/Navidrome streaming, automatic caching, scrobbling, and `MENU → Download` offline pinning into `/mnt/SDCARD/Media/Music/navidrome`.
  - **Jellyfin**: Movies, TV Series (`Seasons → Episodes`), and Music libraries with zero-CPU Direct Stream (`Static=true`) for compatible 8-bit H.264/MPEG4, automatic server-side H.264 + AAC transcoding for HEVC/AV1/VP9/10-bit/4K/DTS, playback resume sync, and `MENU → Download` offline saving into `/mnt/SDCARD/Media/Videos/jellyfin` & `/mnt/SDCARD/Media/Music/jellyfin`.
- **Audio & Visuals**: Direct ALSA hardware/software volume split, USB-C & Bluetooth A2DP audio, built-in AutoEQ headphone database + 10-band parametric EQ, and 100 Milkdrop presets.
- **USB MTP File Transfer**: Plug in USB while UIPod is running to mount `/mnt/SDCARD` over MTP alongside ADB.

## Controls (TrimUI Brick)

| Input | Action |
| --- | --- |
| **A** | Select / Open |
| **B** | Back |
| **START** | Play / Pause |
| **MENU** | Context menu (`Play`, `Download`, queue/playlist actions); on Main Menu opens `Settings` |
| **SELECT + VOL UP / DOWN** | Screen brightness |
| **L1 / R1** | Prev / Next track (Hold to seek; `±10 s` in video) |
| **L2 / R2** | Prev / Next chapter |
| **Side Switch** | Lock controls (volume still works) |

## Installation (TrimUI Brick / NextUI)

### Manual SD Install
1. Download `UIPod-<version>-brick.zip` (or `UIPod.pak.zip`) from [Releases](https://github.com/famiclone/UIPod/releases).
2. Extract `UIPod.pak` into `/mnt/SDCARD/Tools/tg5040/UIPod.pak` (so `launch.sh` sits directly at `/mnt/SDCARD/Tools/tg5040/UIPod.pak/launch.sh`).
3. On your TrimUI Brick, open **Tools → UIPod**.

### NextUI Pak Store Format
Each GitHub Release also attaches `UIPod.pak.zip` matching `pak.json` (`release_filename` + `version` tag) for direct NextUI Pak Store compatibility.

## Building Locally

Requires Docker (`tg5040` cross-toolchain + Rust `aarch64-unknown-linux-gnu` are built automatically from `Dockerfile.uipod`):

```sh
./build.sh      # cross-compile Rust crates + firmware -> build-uipod/
./package.sh    # assemble dist/UIPod.pak, dist/UIPod.pak.zip & dist/UIPod-<ver>-brick.zip
```

## Versioning & Automated Releases

Version metadata lives in `VERSION` and `pak.json` (and is compiled into `Settings → About` via `UIPOD_VERSION`).

- **CI Auto-Build (`.github/workflows/build.yml`)**: Every push and PR to `main` runs Rust unit tests, cross-compiles `UIPod.pak` for TrimUI Brick (`tg5040`), and uploads `UIPod.pak.zip` as a build artifact.
- **Tagged Release (`.github/workflows/release.yml`)**:
  - From CLI:
    ```sh
    ./scripts/bump-version.sh patch --tag   # or minor / major / v0.2.0
    git push origin main --follow-tags
    ```
  - Or directly from GitHub UI: **Actions → Release → Run workflow** (`patch` / `minor` / `major`).

## License & Credits

Licensed under the **GNU General Public License v2.0** (`GPL-2.0`).

- **Rockbox** — core firmware (`GPL-2.0`)
- **Trimpod Classic** by Werewolf Camp / `tyrannotorus` — original TrimUI Brick adaptation (`GPL-2.0`)
- **projectM** (`LGPL-2.1`) & **Cream of the Crop** Milkdrop presets
- **1ST_GEN_REMIX** theme by Monica G.
- **Geologica** (`SIL OFL 1.1`) & **PixelMplus** (`M+ Font License`)
- **uMTP-Responder** (`GPL-3.0`, standalone binary in `lib/umtprd`)
- **NextUI** (`tg5040` toolchain & platform environment)
