---
name: brick-deploy
description: Build (if needed) and deploy UIPod.pak to a connected TrimUI Brick (tg5040 / NextUI) over USB via ADB using ./deploy-brick.sh, preserving user settings, library.db, playlists, and server configs. Use when the user invokes /brick-deploy or asks to push/upload/install UIPod onto the TrimUI Brick over USB / ADB.
---

# Deploy UIPod to TrimUI Brick via USB ADB (`/brick-deploy`)

Push `dist/UIPod.pak` directly to `/mnt/SDCARD/Tools/tg5040/UIPod.pak` on a connected **TrimUI Brick (`tg5040` / NextUI)** over USB using `./deploy-brick.sh`.

## Rules & Behavior

1. **Preserve User Data**:
   - `./deploy-brick.sh` preserves existing on-device `uipod/config.cfg` (unless `--reset-cfg` is explicitly requested), as well as `library.db`, `Playlists`, `eqs`, `navidrome.cfg`/`navidrome.db`, `jellyfin.cfg`/`jellyfin.db`, and `lastfm.cfg`.
   - Never bump `VERSION`, create git tags, or push to GitHub during `/brick-deploy` (`.agents/rules/release-policy.md`).

2. **When to Rebuild First**:
   - Check if any Rust (`crates/`, `plugins/`) or C/pak (`apps/`, `firmware/`, `lib/`, `pak/`, `assets/`) files were modified more recently than `dist/UIPod.pak/uipod/uipod`, or if `dist/UIPod.pak` does not exist yet.
   - If sources changed since the last package, run `./deploy-brick.sh --build` (or `./deploy-brick.sh --build --fast` when only binaries/plugins/scripts changed and `UIPod.pak` is already installed on the Brick).
   - If `dist/UIPod.pak` is already up to date, run `./deploy-brick.sh` (or `./deploy-brick.sh --fast` for rapid binary/plugin iteration).

## Execution Steps

1. Check connected ADB devices (`/opt/homebrew/bin/adb devices` or `adb devices`). If no device is listed, ask the user to connect the TrimUI Brick via USB-C and wake the screen, or let `./deploy-brick.sh` wait briefly.
2. Run `./deploy-brick.sh` (with `--build` if local sources changed since `dist/UIPod.pak` was packaged, and `--fast` when doing an incremental binary/plugin update onto an existing install).
3. Report the connected device serial, deploy mode (`full` vs `fast`), and target path (`/mnt/SDCARD/Tools/tg5040/UIPod.pak`).
