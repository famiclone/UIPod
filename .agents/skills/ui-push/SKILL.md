---
name: ui-push
description: Surgically push UI tweaks and transfer ALL missing/code_only screens from code into Figcraft in a single pass (Protocol B in AGENTS.md). Use when syncing code layout/style changes or missing code screens into Figcraft.
---

# UI Push (Code -> Figcraft)

Execute **Protocol B: Code -> Figcraft (`push`)** from `AGENTS.md` for the target screen specified by the user. When invoked **without a specific screen target**, discover and transfer **ALL** missing/`code_only` screens across the entire codebase in **one single batch** (never split across multiple runs).

## Steps

1. **Auto-Refresh Full Screen, Overlay & Component State Inventory First**:
   - When pushing all screens (no target arg), first run an exhaustive codebase scan (`grep -rn "MAKE_MENU(\|uipod_page_vtable" apps/` + custom GPU screens, popups/overlays like `VolumeBarPopup` / video OSD, and `Header` / `Battery` / transport state variants in `crates/uipod-ui/src/renderer.rs`, `crates/uipod-ui/src/icons.rs`, and `apps/uipod_*.c`) and register any unrecorded screens or component variants into `ui-map.json` before pushing — so **zero** submenus, dialogs, popups, or header badge states (`Hold`, `Downloads`, `FX`, `Source`, `Charging`, `Play/Pause/FF/Rew`, `%Tl Legend`) are missed.
   - Check `assets/icons/ui.figcraft` timestamp/tree first so recent user edits in Figcraft are preserved.
2. **For existing `"mapped"` screens — Surgical Node Patching ONLY**:
   - Patch ONLY the modified properties (`gap`, `padding`, dimensions, radii, strokes, fills, font sizes, text bindings, or newly added child nodes).
   - **Strictly preserve** the existing screen frame, layout containers, component links, and untouched sibling nodes — NEVER delete and recreate an existing screen from scratch.
3. **For ALL `"code_only"` / new screens — Batch Transfer into Figcraft in One Pass**:
   - Build a new top-level screen frame on the `Screens` page in Figcraft for **every** `"code_only"` screen in a single atomic update, matching exact C/Rust source rows and geometry, linked component instances (`Header`, `MenuItem`, `Scrollbar`, `toggle`), color token variables (`bg-color`, `fg-color`, `fg-color-1`, `fg-color-2`), and `en`/`ua` localization variables.
   - **Important Figcraft child positioning rule**: Figcraft renders stored `transform` `[1, 0, 0, 1, tx, ty]` directly until re-laid out in the editor, so ALWAYS compute explicit `transform` offsets for every child node (e.g. centered text inside a container must set `auto_resize: "height"`, `align_h: "center"`, `child_layout.sizing_h: "fill"`, `width: container_w`, `height: font_size * 1.25`, and `ty = (container_h - height) * 0.5`), and verify exact character/row sets from C/Rust source files rather than Web demo stubs.
   - Arrange new frames cleanly on the non-overlapping canvas grid (`dx = 1184` step).
   - Update `ui-map.json` with all newly allocated `node_id`s, `figcraft_screen` names, and `"status": "mapped"`.
4. Validate all parent/child/ID invariants across `document.json` and `ui-map.json` to confirm zero layout regressions or broken references.
5. Output a concise changelog of patched nodes (`Node: old -> new`) and newly transferred screens (`Screen -> #node_id`).
