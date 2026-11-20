# WebAssembly Simulator 1:1 Parity Rule (`Firmware <-> uipod-wasm`)

Whenever modifying UI screens, menus, input controls, audio visualizers, plugins/games, or `web/uipod-wasm`:

1. **Zero Ad-Hoc Redrawing**:
   - Every UI screen, header state, popup (`VolumeBarPopup`), keyboard, EQ, and visualizer in `uipod-wasm` MUST render through shared `crates/uipod-ui` (`UiEngine`, `VizEngine`, `LayoutSpec`). Never duplicate rendering geometry or hardcode pixel coordinates in `web/`.
2. **Shared Plugin Logic (`plugins/*` & `uipod-plugin-sdk`)**:
   - Games and utilities in WASM must preserve 1:1 visual layout, gameplay rules, `PluginKind::MenuDriven` / `Utility` lifecycles, and `START`/`MENU` system menus with the native `.uipod` crates in `plugins/*`. Prefer sharing pure-Rust plugin state/rendering code directly rather than writing simplified clones.
3. **1:1 Input & Playback Physics**:
   - Button gestures in `uipod-wasm` must match `apps/action.c` and `apps/keymaps/keymap-retro-handheld.c`: short tap on release vs `650ms` hold (`Hold B` -> Home, `Hold A`/`MENU` -> Context Menu across all context-aware screens, `Hold Left/Right` on `Now Playing` -> smooth accelerated rewind/fast-forward with `<<`/`>>` header icons, `START` -> global Play/Pause or in-game menu, `Volume +/-` -> global `VolumeBarPopup`).
