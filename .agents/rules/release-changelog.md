---
trigger: always_on
---

# Release Changelog & Manual Rule

Whenever preparing, bumping, tagging, or publishing a release of **UIPod** (`VERSION`, `pak.json`, `scripts/bump-version.sh`, or `v*` git tags):

1. **Always generate a short changelog** (`1–5` concise `- ` bullet points) summarizing user-facing features, fixes, and UI/source improvements since the previous `v*` tag.
2. **Keep entries clean and scannable**:
   - Omit raw commit hashes, merge noise, and internal `chore(release)` entries.
   - Focus on what changed for TrimUI Brick users (UI, themes, controls, Local / Navidrome / Jellyfin playback, plugins/games, performance, bug fixes).
3. **Always update `README.md` and the in-app manual (`pak/uipod/about.txt`)**:
   - Bump the `Version: vX.Y.Z` header in `pak/uipod/about.txt` (and any stamped version references in `README.md`) to match the new release version.
   - Update any affected sections in **both** `README.md` (Features, Controls, Sources, Plugins, Installation, Building) and `pak/uipod/about.txt` (controls, menus, sound/EQ, sources, plugins/games) so the GitHub front page, `Settings -> About` (`Text Viewer`), and the WebAssembly Simulator manual always reflect the exact current functionality of the release.
4. **Sync across all release surfaces**:
   - Write the concise changelog entry under `"changelog"[version]` in `pak.json` before packaging/tagging so NextUI Pak Store metadata and `.github/workflows/release.yml` GitHub Release notes pick it up automatically.
   - Include the short changelog in your response to the user when reporting a new build or release.
