---
name: ui-audit
description: Compare Figcraft screen designs and code LayoutSpec without modifying either side. Use when auditing synchronization status between Figcraft and UI source files.
---

# UI Audit (Read-Only Figcraft <-> Code Comparison)

Perform a read-only UI synchronization audit between Figcraft and code for the target screen specified by the user (if omitted, audit all screens in `ui-map.json`).

## Steps

1. Read `ui-map.json` to resolve the Figcraft screen/node, `code_files`, and `tokens_file`.
2. Inspect the current screen tree, layout properties, strokes, fills, and typography in Figcraft.
3. Read `code_files` and `tokens_file` (and check `git diff` for recent local UI edits).
4. Output a compact Markdown table listing **Element / Property**, **Figcraft Value**, **Code Value**, and **Status** (`in sync`, `needs pull`, `needs push`, or `conflict`).

Do NOT modify any code files or Figcraft data.
