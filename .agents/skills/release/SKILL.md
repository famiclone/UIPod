---
name: release
description: Prepare, verify, bump version, update changelog, README.md & pak/uipod/about.txt, tag, and push a full UIPod release so GitHub Actions builds the TrimUI Brick .pak, Android arm64 APK, and WebAssembly web page (GitHub Pages + release bundle). Use when the user invokes /release or explicitly asks to create and push a new UIPod release.
---

# UIPod Full Release (`/release`)

End-to-end release workflow that syncs firmware <-> WebAssembly parity, updates the changelog, `README.md`, and in-app manual (`pak/uipod/about.txt`), runs local pre-flight verification, tags `vX.Y.Z`, and pushes to GitHub so `.github/workflows/release.yml` builds and publishes **all targets** automatically:
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

### 2. Bump Version & Update Changelog, `README.md` & `about.txt` (`.agents/rules/release-changelog.md`)
1. Run `./scripts/bump-version.sh <target>` to update:
   - `VERSION`
   - `pak.json` (`"version"`)
   - `web/package.json` (`"version"`)
   - `pak/uipod/about.txt` (`Version: vX.Y.Z`)
   - `README.md` (any stamped version references / badges)
2. **Refine `pak.json` Changelog**:
   Edit `"changelog"[new_ver]` in `pak.json` so it contains **1–5 concise `- ` bullet points** summarizing user-facing features, UI/source improvements, and fixes since the previous `v*` tag (no raw commit hashes or `chore` noise).
3. **Update `README.md`**:
   Review `README.md` against all changes in this release and update any affected sections (`Features`, `Controls`, `Media Sources`, `Plugins`, `Installation` for TrimUI Brick / Android / Web Simulator, or `Building`) so the GitHub repository front page stays 100% accurate.
4. **Update `pak/uipod/about.txt`**:
   Verify `Version: <vX.Y.Z>` at the top of `pak/uipod/about.txt` and update any affected manual sections (`1. Global Controls`, `2. Media Library & Sources`, `3. Appearance`, `4. Sound, Equalizer & AutoEQ`, `5. Extras & Native Plugins`, `6. Visualizers, USB MTP & Device Power`) so both `Settings -> About` (via `Text Viewer`) and the embedded Web Simulator manual (`include_str!` in `web/uipod-wasm`) reflect the exact release behavior.

### 3. Local Pre-Flight Verification
*(Always run **after** updating `pak/uipod/about.txt` so `uipod-wasm` embeds the updated manual!)*
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
   ```

### 5. Verify GitHub Actions Pipeline & Report
1. Check that `.github/workflows/release.yml` triggered on GitHub:
   `gh run list --workflow=release.yml --limit 1`
2. Report to the user:
   - Released version `<vX.Y.Z>`
   - The concise `- ` bullet changelog
   - Summary of updates made to `README.md` and `pak/uipod/about.txt`
   - Triggered GitHub Actions jobs (`build-tg5040`, `build-android`, `build-web` -> `publish-release` -> `deploy-pages`) and links to GitHub Releases & GitHub Pages.
