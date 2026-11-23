---
name: release
description: Prepare, verify, bump version, update changelog, concise README.md, web/ landing copy & pak/uipod/about.txt, tag, and push a full UIPod release so GitHub Actions builds the TrimUI Brick .pak, Android arm64 APK, and WebAssembly web page (GitHub Pages + release bundle). Use when the user invokes /release or explicitly asks to create and push a new UIPod release.
---

# UIPod Full Release (`/release`)

End-to-end release workflow that syncs firmware <-> WebAssembly parity, updates the changelog, concise `README.md`, `web/` landing copy, and in-app manual (`pak/uipod/about.txt`), runs local pre-flight verification, tags `vX.Y.Z`, and pushes to GitHub so `.github/workflows/release.yml` builds and publishes **all targets** automatically:
- `UIPod.pak.zip` & `UIPod-<tag>-brick.zip` (TrimUI Brick / NextUI `tg5040`)
- `UIPod-<tag>-android-arm64.apk` (Android 8.0+ `arm64-v8a` handheld app)
- `UIPod-<tag>-web.zip` + live **GitHub Pages** deployment (WebAssembly simulator & landing page stamped with `<tag>`)

> **Authorization Note**: Invoking `/release` counts as explicit user approval to bump `VERSION`, commit, create a `v*` git tag, and push to `origin` under `.agents/rules/release-policy.md`.

## Arguments

`/release [patch | minor | major | vX.Y.Z]`
- Defaults to `minor` if new user-facing features were added since the previous `v*` tag, or `patch` for bug-fix/polish releases (unless the user specifies an explicit target like `/release patch` or `/release v0.6.0`).

## Execution Steps

### 1. Audit Changes & 1:1 Web Simulator Parity
1. Inspect `git status`, `git diff`, and `git log $(git describe --tags --abbrev=0 --match 'v*')..HEAD --oneline`.
2. Check if any UI screens, menus, settings, header indicators, controls, themes, `EN`/`UA` localization strings, or plugins changed in `apps/`, `crates/uipod-ui/`, or `plugins/` since the last release without corresponding updates in `web/uipod-wasm/`.
3. If parity gaps exist, sync `web/uipod-wasm/` first (following `.agents/rules/web-parity.md`: render strictly via `crates/uipod-ui`, never ad-hoc redrawing).

### 2. Bump Version & Update Changelog, `README.md`, `web/` Copy & `about.txt` (`.agents/rules/release-changelog.md`)
1. Run `./scripts/bump-version.sh <target>` to update:
   - `VERSION`
   - `pak.json` (`"version"`)
   - `web/package.json` (`"version"`)
   - `pak/uipod/about.txt` (`Version: vX.Y.Z`)
   - `README.md` (any stamped version references / badges)
2. **Refine `pak.json` Changelog**:
   Edit `"changelog"[new_ver]` in `pak.json` so it contains **1–5 concise `- ` bullet points** summarizing user-facing features, UI/source improvements, and fixes since the previous `v*` tag (no raw commit hashes or `chore` noise).
3. **Update `README.md` (Strict Concise & Jargon-Free Style)**:
   Keep `README.md` crisp and scannable in under 15 seconds — **never** turn it into a wall of technical prose:
   - **Banned bloat**: Never mention internal Rust crate names (`uipod-ui`, `uipod-lastfm`), C/FFI files, OpenGL/GLES versions, shader/DSP algorithm jargon, internal flags (`Static=true`, `UIPOD_VERSION`), `.cfg`/`.sf2` filenames, SD card media folder paths inside feature bullets, or deep menu paths / button combos inside feature lists (`Settings -> Sound -> ...`, `Hold A -> ...` — keep all controls strictly in the `Controls` table). Always use `famiclone.dev/UIPod` (never legacy `github.io` URLs).
   - **Required layout**:
     1. *Header (max 3 lines)*: Title, one-sentence pitch (TrimUI Brick / NextUI, Android, Web), and direct links to [**Live Web Simulator**](https://famiclone.dev/UIPod) + [**Latest Release**](https://github.com/famiclone/UIPod/releases/latest).
     2. *Highlights*: 4 grouped subsections (`UI & Customization`, `Music, Chiptunes & Video`, `Sound & DJ Engine`, `Extras & Handheld Utilities`) — **1 short line per bullet**. Add new user-facing features in **5–12 words** (or merge into an existing bullet) without adding paragraphs. Skip `README.md` feature edits for purely internal refactors, bugfixes, or CI tweaks.
     3. *Installation*: 3 single-line bullets (TrimUI Brick / NextUI Pak Store, Android APK, Browser). Preserve any version pattern expected by `bump-version.sh` or use clean wildcards (`UIPod-*-brick.zip`, `UIPod-*-android-arm64.apk`).
     4. *Controls*: Clean 2-column Markdown table (`Button` | `Action`).
     5. *Building from Source*: Always collapsed inside `<details><summary><strong>Building from Source</strong></summary>...</details>` so CLI build commands stay hidden by default.
     6. *License & Credits*: Compact 2–3 line footer (`GPL-2.0` + key upstream credits).
4. **Sync Web Landing Page Copy (`web/`)**:
   Whenever user-facing features or controls change in `README.md`, inspect the landing page copy in `web/` (`index.html` / `src/`) and sync feature/control descriptions using the exact same concise, jargon-free tone. Never alter page layout, CSS styling, or simulator components unless explicitly asked.
5. **Update `pak/uipod/about.txt`**:
   Verify `Version: <vX.Y.Z>` at the top of `pak/uipod/about.txt` and update any affected manual sections (`1. Global Controls`, `2. Media Library & Sources`, `3. Appearance`, `4. Sound, Equalizer & AutoEQ`, `5. Extras & Native Plugins`, `6. Visualizers, USB MTP & Device Power`) so both `Settings -> About` (via `Text Viewer`) and the embedded Web Simulator manual (`include_str!` in `web/uipod-wasm`) reflect the exact release behavior. *(Detailed menu paths and button combos belong here in the in-app manual, NOT in `README.md`.)*

### 3. Local Pre-Flight Verification
*(Always run **after** updating `pak/uipod/about.txt` and `web/` landing copy so `uipod-wasm` and Vite bundle the updated text!)*
1. **Rust workspace unit tests**:
   Run `cargo test --offline --workspace --lib` (fall back to `cargo test --workspace --lib` if needed).
2. **Web page & WASM simulator build**:
   Run `./web/build.sh` to verify `uipod-wasm` (`wasm-pack`) and Vite/TypeScript (`tsc --noEmit && vite build`) compile with zero errors and stamp the new `vX.Y.Z` version.
3. **TrimUI Brick firmware check** (when C/Rust firmware files changed):
   Run `./build.sh && ./package.sh` locally to verify `tg5040` cross-compilation succeeds before tagging.
   *(Do **not** run `./build-android.sh` locally — the Android `arm64-v8a` APK is built in GitHub Actions.)*

### 4. Stage, Commit, Tag & Push
1. Stage all intentional release changes (`VERSION`, `pak.json`, `README.md`, `pak/uipod/about.txt`, `web/`, `crates/`, `apps/`, `firmware/`, `plugins/`, `.github/workflows/`, etc.).
   - **Never** stage `.gemini/`, `.claude/`, `*.figcraft`, credentials (`*.cfg`, `.env*`), or unrelated scratch files.
2. Commit with a clean release message:
   `git commit -m "feat: UIPod <vX.Y.Z> — <concise summary>"` (or `chore(release): <vX.Y.Z>` if code was already committed).
3. Create the annotated release tag and push `main` + tag to GitHub:
   ```bash
   git tag -a <vX.Y.Z> -m "UIPod <vX.Y.Z>"
   git push origin HEAD:main
   git push origin <vX.Y.Z>
### 5. Verify GitHub Actions Pipeline & Report

1. Check that .github/workflows/release.yml triggered on GitHub:gh run list --workflow=release.yml --limit 1
2. Report to the user:
   - Released version <vX.Y.Z>
   - The concise -  bullet changelog
   - Summary of updates made to README.md, web/ landing copy, and pak/uipod/about.txt
   - Triggered GitHub Actions jobs (build-tg5040, build-android, build-web -> publish-release -> deploy-pages) and links to GitHub Releases & GitHub Pages (https://famiclone.dev/UIPod).
