#!/usr/bin/env bash
# Deploy UIPod.pak to a TrimUI Brick (tg5040 / NextUI) over USB via ADB.
#
# Usage:
#   ./deploy-brick.sh              # push dist/UIPod.pak (auto-builds if missing)
#   ./deploy-brick.sh -b|--build   # run ./build.sh && ./package.sh first, then push
#   ./deploy-brick.sh -f|--fast    # fast dev sync (~0.3s): binary + .uipod plugins + scripts only
#   ./deploy-brick.sh --reset-cfg  # also overwrite device uipod/config.cfg with defaults
#   ./deploy-brick.sh -s <serial>  # target a specific ADB device serial
set -euo pipefail
export PATH="/opt/homebrew/bin:/usr/local/bin:$HOME/Library/Android/sdk/platform-tools:$PATH"
export COPYFILE_DISABLE=1

ROOT="$(cd "$(dirname "$0")" && pwd)"
PAK_SRC="$ROOT/dist/UIPod.pak"
REMOTE_TOOLS="/mnt/SDCARD/Tools/tg5040"
REMOTE_PAK="$REMOTE_TOOLS/UIPod.pak"

DO_BUILD=0
FAST_MODE=0
RESET_CFG=0
SERIAL="${ADB_SERIAL:-}"

usage() {
  cat <<'EOF'
Usage: ./deploy-brick.sh [options]

Deploy UIPod to a TrimUI Brick (NextUI / tg5040) over USB via ADB.
Preserves user library.db, playlists, EQ presets, Navidrome/Jellyfin/Last.fm
configs, and existing config.cfg (unless --reset-cfg is passed).

Options:
  -b, --build       Rebuild (./build.sh && ./package.sh) before deploying
  -f, --fast        Fast sync: push only uipod binary, .uipod plugins & scripts
      --reset-cfg   Overwrite existing uipod/config.cfg on the Brick
  -s, --serial ID   Use specific ADB device serial
  -h, --help        Show this help message
EOF
}

while [ $# -gt 0 ]; do
  case "$1" in
    -b|--build)
      DO_BUILD=1
      shift
      ;;
    -f|--fast)
      FAST_MODE=1
      shift
      ;;
    --reset-cfg|--clean-config)
      RESET_CFG=1
      shift
      ;;
    -s|--serial)
      [ $# -ge 2 ] || { echo "Error: --serial requires a device ID" >&2; exit 1; }
      SERIAL="$2"
      shift 2
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    *)
      echo "Unknown option: $1" >&2
      usage >&2
      exit 1
      ;;
  esac
done

# 1. Locate `adb`
ADB_BIN="$(command -v adb 2>/dev/null || true)"
if [ -z "$ADB_BIN" ]; then
  for cand in \
    "${ANDROID_HOME:-}/platform-tools/adb" \
    "$HOME/Library/Android/sdk/platform-tools/adb" \
    "$ROOT/build-android/.toolchain/android-sdk/platform-tools/adb"; do
    if [ -x "$cand" ]; then
      ADB_BIN="$cand"
      break
    fi
  done
fi
if [ -z "$ADB_BIN" ]; then
  echo "Error: 'adb' not found. Install via: brew install android-platform-tools" >&2
  exit 1
fi

# 2. Build & package if requested, if dist/UIPod.pak is missing, or if build-uipod/uipod is newer
if [ "$DO_BUILD" -eq 1 ] || [ ! -x "$PAK_SRC/uipod/uipod" ]; then
  echo ">> Building & packaging UIPod.pak for TrimUI Brick ..."
  "$ROOT/build.sh"
  "$ROOT/package.sh"
elif [ -x "$ROOT/build-uipod/uipod" ] && [ "$ROOT/build-uipod/uipod" -nt "$PAK_SRC/uipod/uipod" ]; then
  echo ">> Repackaging updated build-uipod/uipod into dist/UIPod.pak ..."
  "$ROOT/package.sh"
fi

# 3. Resolve target TrimUI Brick ADB device
"$ADB_BIN" start-server >/dev/null 2>&1 || true

if [ -z "$SERIAL" ]; then
  DEVICES=()
  while IFS= read -r line; do
    [ -n "$line" ] && DEVICES+=("$line")
  done < <("$ADB_BIN" devices | awk 'NR>1 && $2=="device" {print $1}')

  if [ "${#DEVICES[@]}" -eq 0 ]; then
    echo ">> Waiting for TrimUI Brick over USB ADB (connect USB-C cable & ensure Brick is awake) ..."
    "$ADB_BIN" wait-for-device
    while IFS= read -r line; do
      [ -n "$line" ] && DEVICES+=("$line")
    done < <("$ADB_BIN" devices | awk 'NR>1 && $2=="device" {print $1}')
  fi

  if [ "${#DEVICES[@]}" -eq 0 ]; then
    echo "Error: No authorized ADB device found." >&2
    exit 1
  elif [ "${#DEVICES[@]}" -eq 1 ]; then
    SERIAL="${DEVICES[0]}"
  else
    # Multiple ADB devices connected: auto-pick the TrimUI Brick (`tg5040` / `/mnt/SDCARD`)
    for dev in "${DEVICES[@]}"; do
      if "$ADB_BIN" -s "$dev" shell "[ -d /mnt/SDCARD/Tools/tg5040 ] || [ -d /mnt/SDCARD/.system/tg5040 ]" >/dev/null 2>&1; then
        SERIAL="$dev"
        break
      fi
    done
    if [ -z "$SERIAL" ]; then
      echo "Error: Multiple ADB devices connected (${DEVICES[*]}), none recognized as tg5040. Pass -s <serial>." >&2
      exit 1
    fi
  fi
fi

adb_cmd() {
  "$ADB_BIN" -s "$SERIAL" "$@"
}

echo ">> Connected to TrimUI Brick ($SERIAL)"

# Verify SD card is mounted on the Brick & preserve existing pak folder casing (e.g. UiPod.pak vs UIPod.pak)
if ! adb_cmd shell "[ -d /mnt/SDCARD ]" >/dev/null 2>&1; then
  echo "Error: /mnt/SDCARD is not mounted on device $SERIAL." >&2
  exit 1
fi
EXISTING_PAK="$(adb_cmd shell "for d in '$REMOTE_TOOLS'/*.pak; do base=\$(basename \"\$d\"); if [ \"\$(printf '%s' \"\$base\" | tr 'A-Z' 'a-z')\" = 'uipod.pak' ]; then printf '%s' \"\$d\"; break; fi; done" 2>/dev/null | tr -d '\r' || true)"
if [ -n "$EXISTING_PAK" ]; then
  REMOTE_PAK="$EXISTING_PAK"
fi

# 4. Stop UIPod (and any video/MTP helper processes) if currently running on the Brick.
#    Note: stopping UIPod while USB is plugged in triggers `mtp.sh stop`
#    (`/bin/setusbconfig adb`), which briefly re-enumerates USB (~1s).
if adb_cmd shell "pidof uipod tplayerslave umtprd >/dev/null 2>&1"; then
  echo ">> Stopping running UIPod instance on Brick ..."
  adb_cmd shell "killall -TERM uipod 2>/dev/null || true" || true
  sleep 0.9
  adb_cmd wait-for-device
  # Escalate to SIGKILL if uipod, tplayerslave, or any pak child is hung/orphaned,
  # then wait for launch.sh cleanup() to restore keymon and unmount /tmp/uipod.
  adb_cmd shell '
    i=0
    while pidof uipod >/dev/null 2>&1 && [ $i -lt 6 ]; do
      sleep 0.15
      i=$((i + 1))
    done
    if pidof uipod tplayerslave ffprobe ffmpeg umtprd >/dev/null 2>&1; then
      killall -STOP tplayerslave 2>/dev/null || true
      killall -9 tplayerslave ffprobe ffmpeg uipod umtprd 2>/dev/null || true
      pkill -9 -f "UIPod\.pak/uipod/" 2>/dev/null || true
      sleep 0.3
    fi
    while mount 2>/dev/null | grep -q "/tmp/uipod"; do
      umount -l /tmp/uipod 2>/dev/null || break
    done
  ' >/dev/null 2>&1 || true
  sleep 0.5
  adb_cmd wait-for-device
fi

# 5. Deploy files
if [ "$FAST_MODE" -eq 1 ] && adb_cmd shell "[ -d '$REMOTE_PAK/uipod' ]" >/dev/null 2>&1; then
  echo ">> Fast-syncing binary, .uipod plugins & scripts to $REMOTE_PAK ..."
  TMP_TGZ="$(mktemp -t uipod-fast-XXXXXX).tgz"
  trap 'rm -f "$TMP_TGZ"' EXIT

  TAR_ARGS=(--exclude='.DS_Store' --exclude='._*')
  tar --version 2>/dev/null | grep -qi bsdtar && TAR_ARGS+=(--no-xattrs --no-mac-metadata)

  tar "${TAR_ARGS[@]}" -C "$PAK_SRC" -czf "$TMP_TGZ" \
    launch.sh mtp.sh shutdown.sh pak.json \
    uipod/uipod uipod/umtprd uipod/umtprd.conf uipod/about.txt uipod/langs uipod/systems uipod/plugins

  adb_cmd push "$TMP_TGZ" /tmp/uipod-deploy.tgz >/dev/null
  adb_cmd shell "
    set -e
    mkdir -p '$REMOTE_PAK'
    tar -xzf /tmp/uipod-deploy.tgz -C '$REMOTE_PAK'
    rm -f /tmp/uipod-deploy.tgz
    chmod +x '$REMOTE_PAK/launch.sh' '$REMOTE_PAK/shutdown.sh' '$REMOTE_PAK/mtp.sh' \
             '$REMOTE_PAK/uipod/uipod' '$REMOTE_PAK/uipod/umtprd' 2>/dev/null || true
    sync
  "
else
  echo ">> Packaging & pushing full UIPod.pak bundle to $REMOTE_PAK ..."
  TMP_TGZ="$(mktemp -t uipod-full-XXXXXX).tgz"
  trap 'rm -f "$TMP_TGZ"' EXIT

  TAR_ARGS=(--exclude='.DS_Store' --exclude='._*')
  tar --version 2>/dev/null | grep -qi bsdtar && TAR_ARGS+=(--no-xattrs --no-mac-metadata)

  tar "${TAR_ARGS[@]}" -C "$PAK_SRC" -czf "$TMP_TGZ" .

  adb_cmd push "$TMP_TGZ" /tmp/uipod-deploy.tgz
  adb_cmd shell "
    set -e
    mkdir -p '$REMOTE_PAK'
    KEEP_CFG=0
    if [ '$RESET_CFG' -eq 0 ] && [ -f '$REMOTE_PAK/uipod/config.cfg' ]; then
      cp '$REMOTE_PAK/uipod/config.cfg' /tmp/uipod-keep-config.cfg
      KEEP_CFG=1
    fi
    tar -xzf /tmp/uipod-deploy.tgz -C '$REMOTE_PAK'
    if [ \"\$KEEP_CFG\" -eq 1 ] && [ -f /tmp/uipod-keep-config.cfg ]; then
      mv /tmp/uipod-keep-config.cfg '$REMOTE_PAK/uipod/config.cfg'
    fi
    rm -f /tmp/uipod-deploy.tgz
    chmod +x '$REMOTE_PAK/launch.sh' '$REMOTE_PAK/shutdown.sh' '$REMOTE_PAK/mtp.sh' \
             '$REMOTE_PAK/uipod/uipod' '$REMOTE_PAK/uipod/umtprd' 2>/dev/null || true
    sync
  "
fi

VER="$(tr -d '[:space:]' < "$ROOT/VERSION" 2>/dev/null || echo "dev")"
echo ">> Done! UIPod ($VER) is installed on TrimUI Brick at $REMOTE_PAK"
