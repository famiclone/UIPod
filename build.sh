#!/usr/bin/env bash
# Build UIPod as an SDL application for the TrimUI Brick / NextUI (tg5040).
# Cross-compiles inside the UIPod toolchain image. Output lands in build-uipod/.
#
#   ./build.sh            # incremental build
#   ./build.sh clean      # wipe build dir and reconfigure from scratch
set -euo pipefail
export PATH="/opt/homebrew/bin:/usr/local/bin:$PATH"

ROOT="$(cd "$(dirname "$0")" && pwd)"
IMG="uipod-toolchain:latest"
BASE="ghcr.io/loveretro/tg5040-toolchain:latest"
BUILD_DIR="build-uipod"
TARGET_ID=210          # retro-handheld (TrimUI Brick SDL app), see tools/configure
JOBS="$(nproc 2>/dev/null || sysctl -n hw.ncpu || echo 4)"
UIPOD_VERSION="${UIPOD_VERSION:-$(tr -d '[:space:]' < "$ROOT/VERSION" 2>/dev/null || echo "v0.1.0")}"

# Build the derived toolchain image once (or rebuild if Rust/cargo was added).
if ! docker image inspect "$IMG" >/dev/null 2>&1 || \
   ! docker run --rm "$IMG" bash -lc "export PATH=/opt/cargo/bin:\$PATH; command -v cargo >/dev/null 2>&1" >/dev/null 2>&1; then
  echo ">> Building toolchain image $IMG ..."
  docker build -t "$IMG" -f "$ROOT/Dockerfile.uipod" "$ROOT"
fi

CLEAN=0
[ "${1:-}" = "clean" ] && CLEAN=1

# Run the container as the host user so files written under the bind-mounted
# /build are owned by you, not root (Docker bind mounts map container uid 0 ->
# host uid 0).  HOME=/tmp gives the non-root user a writable home for any tool
# that wants one.  This replaces the post-build sudo chown.
docker run --rm \
  --user "$(id -u):$(id -g)" -e HOME=/tmp -e VERSION="$UIPOD_VERSION" \
  -v "$ROOT":/build -w /build "$IMG" bash -lc "
  set -e
  if [ $CLEAN -eq 1 ]; then
    rm -rf $BUILD_DIR target/aarch64-unknown-linux-gnu
  fi
  mkdir -p $BUILD_DIR/.cargo-cache
  export PATH=/opt/cargo/bin:\$PATH
  export CARGO_HOME=/build/$BUILD_DIR/.cargo-cache
  export CARGO_TARGET_AARCH64_UNKNOWN_LINUX_GNU_LINKER=\${CROSS_COMPILE}gcc
  echo '>> Building pure-Rust crates (uipod-ui + uipod-navidrome + uipod-jellyfin -> libuipod_ffi.a) ...'
  cargo build --locked --release --target aarch64-unknown-linux-gnu -j $JOBS \
    --manifest-path /build/crates/uipod-ffi/Cargo.toml

  # /usr/local/bin (our sdl2-config wrapper, baked into the image) must win over
  # the sysroot's broken sdl2-config, so do NOT prepend \$SYSROOT/usr/bin here.
  if [ ! -f $BUILD_DIR/Makefile ] || ! grep -q libuipod_ffi $BUILD_DIR/Makefile; then
    rm -f $BUILD_DIR/Makefile $BUILD_DIR/make.dep
    cd $BUILD_DIR
    ../tools/configure --target=$TARGET_ID --type=N
  else
    cd $BUILD_DIR
  fi
  rm -f rbversion.h firmware/common/version.o
  if [ -f trimpod ] && [ /build/target/aarch64-unknown-linux-gnu/release/libuipod_ffi.a -nt trimpod ]; then
    rm -f trimpod
  fi
  make -j$JOBS VERSION=\"\$VERSION\"
  # Produce the runtime zip package.sh assembles the .pak from.
  make fullzip VERSION=\"\$VERSION\"
  # uMTP-Responder (USB file transfer), built out of tree from lib/umtprd.
  # cp -a keeps the sources' mtimes, so an unchanged tree doesn't rebuild.
  cd /build
  mkdir -p $BUILD_DIR/umtprd
  cp -a lib/umtprd/. $BUILD_DIR/umtprd/
  make -C $BUILD_DIR/umtprd -j$JOBS CC=\${CROSS_COMPILE}gcc umtprd
  # Plugins for TrimUI's stock video player, built from lib/tplayer_plugins.
  mkdir -p $BUILD_DIR/tplayer_plugins
  make -C lib/tplayer_plugins -j$JOBS CC=\${CROSS_COMPILE}gcc OUT=/build/$BUILD_DIR/tplayer_plugins
"

echo ">> Done. Binary: $BUILD_DIR/trimpod (runtime zip: $BUILD_DIR/trimpod-full.zip)"
