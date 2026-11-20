---
name: ui-pull
description: Sync UI design from Figcraft into code with pixel-level visual verification (Protocol A in AGENTS.md). Use when pulling screen designs or layout updates from Figcraft into code.
---

# UI Pull (Figcraft -> Code)

Execute **Protocol A: Figcraft -> Code (`pull`)** from `AGENTS.md` for the target screen specified by the user.

## Steps

1. Check `git status` on target UI files from `ui-map.json`. Warn before overwriting if uncommitted layout tweaks exist in code.
2. Read the full node hierarchy, layout rules, and style tokens for the target screen from Figcraft.
3. Update `tokens_file`, the declarative `LayoutSpec` block, and rendering code in `code_files`. Preserve dynamic layout calculations (`gap`, `padding`, centering) instead of per-item hardcoded coordinates.
4. Build the project to confirm zero compile errors or warnings.
5. Run the Visual Verification Loop: export `/tmp/figcraft_<screen>.png` from Figcraft, generate `/tmp/code_<screen>.png` via `screenshot_cmd` (if configured), visually compare both images, and adjust `LayoutSpec` until they match 1:1.
6. Summarize updated constants and verification status.
