---
name: ui-init
description: Auto-discover Figcraft screens and ALL UI source screens/menus to generate or update ui-map.json in the repository root. Use when initializing or refreshing the Figcraft <-> code screen registry.
---

# UI Init (`ui-map.json` Exhaustive Discovery)

Scan both Figcraft (`assets/icons/ui.figcraft` or Figcraft MCP tools) and the **entire codebase** to create or update `ui-map.json` in the repository root in a single exhaustive pass.

## Exhaustive Codebase Discovery (MANDATORY)

Before writing `ui-map.json`, you MUST enumerate **every** distinct UI screen, menu, and dialog in code — never scan only top-level filenames:

1. Grep all declarative menus: `grep -rn "MAKE_MENU(" apps/`
2. Grep all custom interactive pages/dialogs: `grep -rn "uipod_page_vtable" apps/`
3. Grep all custom GPU full-screen renderers in `crates/uipod-ui/src/renderer.rs` (`draw_*_screen*`) and custom `uipod_rs_ui_plugin_begin*` screens in `apps/` (e.g. QR auth, plugins).
4. Deduplicate platform-conditional variants of the same screen (e.g. `uipod_power_menu` / `uipod_power_menu_nobatt` / `uipod_power_menu_android` -> single `settings_power` entry).

## Rules

1. If `ui-map.json` already exists, preserve manual `meta` settings (such as `screenshot_cmd` or custom file overrides) and only add newly discovered screens, update renamed paths/IDs, or refresh `status`.
2. Read exact root `node_id` and `figcraft_screen` names directly from Figcraft — never invent placeholder IDs.
3. Match each Figcraft screen to its corresponding source/header files, and include **every** unmatched code screen/submenu from the exhaustive grep above as `"status": "code_only"` (`"node_id": null`).
4. Set `status` for each entry:
   - `"mapped"`: exists in both Figcraft and code
   - `"design_only"`: exists in Figcraft, `code_files` is `[]`
   - `"code_only"`: exists in code, `node_id` is `null`
5. Print a concise summary of total `mapped`, `design_only`, and `code_only` entries after saving `ui-map.json`.
