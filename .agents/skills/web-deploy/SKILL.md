---
name: web-deploy
description: Audit 1:1 firmware parity, build the UIPod WebAssembly player (uipod-wasm) and Vite web landing page, and deploy/push to GitHub Pages. Use when the user invokes /web-deploy or asks to build and push the web page / WASM simulator to GitHub Pages.
---

# Web & WASM Deploy to GitHub Pages (`/web-deploy`)

Build the UIPod WebAssembly simulator (`web/uipod-wasm` + `crates/uipod-ui`) and Vite web landing page (`web/`), enforce **1:1 parity** with the native firmware & `ui.figcraft`, and deploy to GitHub Pages via `./web/deploy.sh`.

## Mandatory Rules

1. **1:1 UI, Layout & Menu Parity (`ui.figcraft` / Firmware <-> `uipod-wasm`)**:
   - Before building/deploying, check recent changes in `ui-map.json`, `crates/uipod-ui/`, `apps/menus/*.c`, and `apps/uipod_*.c` against `web/uipod-wasm/src/`.
   - Sync any newly added or updated menus, submenus, dialogs, header badge states (`Hold`, `Downloads`, `FX`, `Source`, `Charging`, `Play/Pause/FF/Rew`, `%Tl Legend`), popups (`VolumeBarPopup`), themes, and `EN`/`UA` strings into `uipod-wasm` before deploying.
   - **No ad-hoc redrawing**: all screens, components, and visualizers MUST render through shared `crates/uipod-ui` (`UiEngine`, `VizEngine`, `LayoutSpec`). Never hardcode magic pixel coordinates or duplicate UI drawing in `web/`.

2. **Shared Plugin & Audio Architecture (Zero Divergent Clones)**:
   - Plugins/games (`Tetris`, `Snake`, `2048`, `Brick Breaker`, `Sound Racer`, `Music Quiz`, `Stopwatch`, `Text Viewer`) must match the exact visuals, rules, `PluginKind::MenuDriven` / `Utility` lifecycle, and `START`/`MENU` system menus of `plugins/*` + `crates/uipod-plugin-sdk`. Prefer sharing pure-Rust logic/crates directly over rewriting simplified clones in `web/uipod-wasm`.
   - Browser-only limitations (no raw `/mnt/SDCARD` FS, no Mixed-Content LAN HTTP from HTTPS GitHub Pages) should degrade gracefully with realistic demo/uploaded media, while keeping every UI screen, DSP control, and animation identical.

3. **1:1 Controls, Gestures & Seek Physics (`apps/action.c` & `keymap-retro-handheld.c`)**:
   - Short tap vs long hold (`650ms`) semantics MUST match the firmware:
     - `Left` / `Right` on `Now Playing`: short tap (`release`) skips track; hold engages smooth accelerated seek (`PlayMode::Rewind` / `PlayMode::FastForward` `<<`/`>>` in `Header`) until release.
     - `Hold B`: returns to `Main` from any screen (except Keyboard where `B` cancels).
     - `Hold A` / `MENU`: opens context menu on all context-aware screens (`Now Playing`, `Browse`, `Playlists`, `Library`, `Video Player`, etc.).
     - `START`: toggles Play/Pause globally, or opens/closes the in-game System Menu inside `MenuDriven` plugins.
     - `Volume +/-`: adjusts volume globally and triggers `VolumeBarPopup`.

4. **Strict Git Staging & Release Isolation (`.agents/rules/release-policy.md`)**:
   - Stage ONLY web and shared Rust/asset paths (`web/`, `crates/uipod-ui/`, `crates/uipod-plugin-sdk/`, `plugins/`, `assets/fonts/`, `pak/uipod/about.txt`, `.github/workflows/pages.yml`).
   - **NEVER** stage unrelated C firmware WIP, **NEVER** bump `VERSION` or `pak.json`, and **NEVER** create `v*` git release tags during `/web-deploy`.

## Execution Steps

1. **Parity Check & Sync**:
   - Inspect `git status` / `git diff` across UI, menus, controls, and plugins; if firmware/Figcraft UI or input behavior changed since the last `uipod-wasm` update, sync `web/uipod-wasm` first.
2. **Local Build & Verification**:
   - Run `cargo test --offline --workspace --lib` and `./web/build.sh` (sandboxed) to verify `uipod-wasm` (`wasm-pack`) and TypeScript/Vite (`npm run build`) compile with zero warnings or errors.
3. **Deploy to GitHub Pages**:
   - Run `./web/deploy.sh -m "<concise commit message>"` (with `BypassSandbox: true` for GitHub SSH/HTTPS push; pass `--gh-pages` only if explicitly requested).
4. **Report**:
   - Summarize synced parity updates, built bundle sizes in `web/dist/`, and the pushed commit / GitHub Pages workflow status.
