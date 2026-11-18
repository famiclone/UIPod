#!/usr/bin/env bash
# Assemble dist/UIPod.pak from the build (build-uipod), the bundled theme
# (assets/theme), and the static pak files (pak/ -- launch.sh, licenses,
# config.cfg and the .sys device files).
# Run ./build.sh first (it produces the runtime zip via 'make fullzip').
# The app's data dir is /tmp/uipod (NOT /tmp/rockbox) to avoid clashing with
# other Rockbox installs; inside the pak it lives in uipod/ and the binary is
# named "uipod".
set -euo pipefail

ROOT="$(cd "$(dirname "$0")" && pwd)"
ZIP="$ROOT/build-uipod/uipod-full.zip"
PAK="$ROOT/dist/UIPod.pak"
THEME="$ROOT/assets/theme/1ST_GEN_REMIX/.rockbox"
UIPOD_VERSION="${UIPOD_VERSION:-$(tr -d '[:space:]' < "$ROOT/VERSION" 2>/dev/null || echo "v0.1.0")}"
SAFE_VER="$(printf '%s' "$UIPOD_VERSION" | sed 's/[^A-Za-z0-9._+-]/-/g')"
BRICK_ZIP="$ROOT/dist/UIPod-${SAFE_VER}-brick.zip"

[ -n "$ZIP" ] && [ -f "$ZIP" ] || { echo "Missing build-uipod/uipod-full.zip — run ./build.sh first."; exit 1; }

echo ">> Staging runtime"
rm -rf "$ROOT/dist"
mkdir -p "$PAK"
tmp="$(mktemp -d)"
unzip -q "$ZIP" -d "$tmp"
# The zip stores the runtime under tmp/uipod/ (data dir = /tmp/uipod).
cp -a "$tmp/tmp/uipod" "$PAK/uipod"
rm -rf "$tmp"

echo ">> Pruning stock Rockbox themes (1ST_GEN_REMIX is the only theme)"
# The build zip bundles the stock themes (cabbiev2, classic_statusbar, ...).
# Drop them; keep only 1ST_GEN_REMIX + the failsafe the skin engine falls back to.
find "$PAK/uipod/themes" -type f \
     ! -name 'rockbox_failsafe.cfg' ! -name 'rockbox_default_icons.cfg' -delete 2>/dev/null
find "$PAK/uipod/wps" -mindepth 1 -maxdepth 1 \
     ! -name 'rockbox_failsafe*' -exec rm -rf {} + 2>/dev/null
rm -rf "$PAK/uipod/backdrops" 2>/dev/null

echo ">> Injecting 1ST_GEN_REMIX theme"
cp -a "$THEME/wps/." "$PAK/uipod/wps/"
cp -a "$THEME/themes/." "$PAK/uipod/themes/"

echo ">> Pruning stock Rockbox fonts (only Geologica ships; Font menu removed)"
# The build zip bundles the whole upstream font set (Terminus, Adobe-Helvetica, ...).
# Nothing references them (themes are hardcoded to Geologica), so drop them all.
rm -f "$PAK/uipod/fonts/"*.fnt "$PAK/uipod/fonts/COPYING-fonts.txt" 2>/dev/null || true

echo ">> Injecting Geologica HiDPI & CJK fallback fonts (see assets/fonts)"
# Geologica-*.ttf are embedded directly into `uipod` via Rust `include_bytes!`;
# only Rockbox `.fnt` skin fonts + runtime CJK fallback `PixelMplus12` ship on disk.
cp -a "$ROOT"/assets/fonts/*.fnt "$PAK/uipod/fonts/"
cp -a "$ROOT/assets/fonts/PixelMplus12-Regular.ttf" "$PAK/uipod/fonts/"
cp "$ROOT/assets/fonts/COPYING" "$PAK/uipod/fonts/COPYING-fonts.txt"

echo ">> Injecting AutoEQ headphone database & EQ presets folder (see assets/autoeq)"
mkdir -p "$PAK/uipod/autoeq" "$PAK/uipod/eqs"
cp -a "$ROOT/assets/autoeq/." "$PAK/uipod/autoeq/"

echo ">> Overlaying the static pak files (pak/: launch.sh, licenses, config, .sys)"
# pak/ mirrors the deployed pak skeleton; its uipod/ merges onto the built one.
# pak.json lives at the repo root (the Pak Store reads it there); copy it in too.
cp -a "$ROOT/pak/." "$PAK/"
python3 - "$ROOT/pak.json" "$PAK/pak.json" "$UIPOD_VERSION" <<'PY'
import json, sys
src, dst, ver = sys.argv[1], sys.argv[2], sys.argv[3]
with open(src, "r", encoding="utf-8") as f:
    data = json.load(f)
data["version"] = ver
with open(dst, "w", encoding="utf-8") as f:
    json.dump(data, f, indent=2, ensure_ascii=False)
    f.write("\n")
PY

echo ">> Injecting umtprd (USB file transfer, GPL-3.0; source in lib/umtprd)"
cp "$ROOT/build-uipod/umtprd/umtprd" "$PAK/uipod/umtprd"
cp "$ROOT/lib/umtprd/LICENSE" "$PAK/licenses/umtprd_gplv3.txt"
chmod +x "$PAK/launch.sh" "$PAK/uipod/uipod" "$PAK/uipod/umtprd"

echo ">> Injecting stock video player plugins (AC-3, DivX, sound to UIPod; source in lib/tplayer_plugins)"
mkdir -p "$PAK/uipod/video"
cp "$ROOT"/build-uipod/tplayer_plugins/*.so "$PAK/uipod/video/"

echo ">> Injecting native UIPod plugins (.uipod; source in plugins/)"
mkdir -p "$PAK/uipod/plugins"
cp "$ROOT"/build-uipod/plugins/*.uipod "$PAK/uipod/plugins/"

# Ensure no macOS Finder metadata files leak into release archives.
find "$PAK" -name '.DS_Store' -delete 2>/dev/null || true

echo ">> Packaging release assets ($UIPOD_VERSION)"
# 1) NextUI Pak Store asset (`dist/UIPod.pak.zip`): archive root MUST be the
#    contents of UIPod.pak (`launch.sh`, `pak.json`, `uipod/`, ...).
# 2) Manual SD-card asset (`dist/UIPod-<version>-brick.zip`): archive root
#    contains the `UIPod.pak/` directory ready to extract into `/mnt/SDCARD/Tools/tg5040/`.
rm -f "$ROOT/dist/UIPod.pak.zip" "$BRICK_ZIP"
if command -v zip >/dev/null 2>&1; then
    ( cd "$PAK" && zip -qr "$ROOT/dist/UIPod.pak.zip" . )
    ( cd "$ROOT/dist" && zip -qr "$BRICK_ZIP" UIPod.pak )
else   # no 'zip' binary -- fall back to python3
    ( cd "$PAK" && python3 -c "import shutil,sys; shutil.make_archive(sys.argv[1],'zip','.')" \
          "$ROOT/dist/UIPod.pak" )
    python3 - "$ROOT/dist" "$BRICK_ZIP" <<'PY'
import os, sys, zipfile
dist_dir, out_zip = sys.argv[1], sys.argv[2]
pak_dir = os.path.join(dist_dir, "UIPod.pak")
with zipfile.ZipFile(out_zip, "w", zipfile.ZIP_DEFLATED) as zf:
    for r, _, files in os.walk(pak_dir):
        for fn in files:
            full = os.path.join(r, fn)
            zf.write(full, os.path.relpath(full, dist_dir))
PY
fi

echo ">> Done: $PAK ($UIPOD_VERSION)"
du -sh "$PAK"
du -h "$ROOT/dist/UIPod.pak.zip" "$BRICK_ZIP"
