#!/usr/bin/env bash
# Build UIPod as a native arm64-v8a Android APK (`dist/UIPod-<version>-android-arm64.apk`)
# for Android retro handhelds (Retroid, Odin, Anbernic, Ayaneo, etc.).
#
# Runs natively on macOS (Apple Silicon / Intel) and Linux without requiring Docker,
# bootstrapping a local self-contained JDK 17 + Android SDK/NDK + SDL2 + OpenSSL
# toolchain under `build-android/.toolchain/` on first run.
#
#   ./build-android.sh          # incremental build
#   ./build-android.sh clean    # wipe build outputs (keeps cached .toolchain)
set -euo pipefail
export PATH="$HOME/.cargo/bin:/opt/homebrew/bin:/usr/local/bin:$PATH"

ROOT="$(cd "$(dirname "$0")" && pwd)"
BUILD_DIR="$ROOT/build-android"
TC_DIR="$BUILD_DIR/.toolchain"
ANDROID_DEPS_DIR="$TC_DIR/deps"
SDL2_JAVA_DIR="$TC_DIR/sdl2-java"
BIN_WRAPPERS="$TC_DIR/bin"
TARGET_ID=215          # android-handheld (see tools/configure)
JOBS="$(nproc 2>/dev/null || sysctl -n hw.ncpu || echo 4)"
UIPOD_VERSION="${UIPOD_VERSION:-$(tr -d '[:space:]' < "$ROOT/VERSION" 2>/dev/null || echo "v0.4.0")}"
SAFE_VER="$(printf '%s' "$UIPOD_VERSION" | sed 's/[^A-Za-z0-9._+-]/-/g')"
VER_CLEAN="${UIPOD_VERSION#v}"

VER_CODE="$(python3 - "$VER_CLEAN" <<'PY'
import re, sys
m = re.match(r"^(\d+)\.(\d+)\.(\d+)", sys.argv[1])
if m:
    print(int(m.group(1)) * 10000 + int(m.group(2)) * 100 + int(m.group(3)))
else:
    print(400)
PY
)"

if [ "${1:-}" = "clean" ]; then
  rm -rf "$BUILD_DIR/apps" "$BUILD_DIR/firmware" "$BUILD_DIR/lib" "$BUILD_DIR/lang" \
         "$BUILD_DIR/apk-stage" "$BUILD_DIR/Makefile" "$BUILD_DIR/autoconf.h" \
         "$BUILD_DIR/make.dep" "$BUILD_DIR/libmain.so" "$BUILD_DIR/uipod-full.zip" \
         "$ROOT/target/aarch64-linux-android"
fi

mkdir -p "$TC_DIR" "$ANDROID_DEPS_DIR/include" "$ANDROID_DEPS_DIR/lib" "$BIN_WRAPPERS" "$ROOT/dist"
HOST_OS="$(uname -s)"
HOST_ARCH="$(uname -m)"

# 1. Ensure working JDK 17+ (`javac`, `keytool`)
if ! javac -version >/dev/null 2>&1; then
  JDK_HOME_CAND="$(find "$TC_DIR/jdk" -maxdepth 5 -name javac 2>/dev/null | head -n 1 || true)"
  if [ -z "$JDK_HOME_CAND" ]; then
    echo ">> [Toolchain 1/4] Downloading portable Temurin OpenJDK 17 ..."
    mkdir -p "$TC_DIR/jdk"
    if [ "$HOST_OS" = "Darwin" ]; then
      J_OS="mac"
      J_ARCH="$([ "$HOST_ARCH" = "arm64" ] && echo "aarch64" || echo "x64")"
    else
      J_OS="linux"
      J_ARCH="$([ "$HOST_ARCH" = "aarch64" ] && echo "aarch64" || echo "x64")"
    fi
    curl -fL "https://api.adoptium.net/v3/binary/latest/17/ga/${J_OS}/${J_ARCH}/jdk/hotspot/normal/eclipse" \
      -o "$TC_DIR/jdk.tar.gz"
    tar -xzf "$TC_DIR/jdk.tar.gz" -C "$TC_DIR/jdk"
    rm -f "$TC_DIR/jdk.tar.gz"
    JDK_HOME_CAND="$(find "$TC_DIR/jdk" -maxdepth 4 -name javac | head -n 1)"
  fi
  export JAVA_HOME="$(cd "$(dirname "$JDK_HOME_CAND")/.." && pwd)"
  export PATH="$JAVA_HOME/bin:$PATH"
fi

# 2. Ensure Android SDK (Platform 34, Build-Tools 35.0.0) & NDK r26d
if [ -n "${ANDROID_HOME:-}" ] && [ ! -w "${ANDROID_HOME}" ]; then
  unset ANDROID_HOME
fi
export ANDROID_HOME="${ANDROID_HOME:-$TC_DIR/android-sdk}"
export ANDROID_NDK_HOME="$ANDROID_HOME/ndk/26.3.11579264"
if [ ! -d "$ANDROID_NDK_HOME" ] || [ ! -f "$ANDROID_HOME/platforms/android-34/android.jar" ] || [ ! -x "$ANDROID_HOME/build-tools/35.0.0/d8" ]; then
  echo ">> [Toolchain 2/4] Installing Android SDK 34, Build-Tools 35.0.0 & NDK r26d ..."
  mkdir -p "$ANDROID_HOME/cmdline-tools"
  if [ ! -x "$ANDROID_HOME/cmdline-tools/latest/bin/sdkmanager" ]; then
    CMD_OS="$([ "$HOST_OS" = "Darwin" ] && echo "mac" || echo "linux")"
    curl -fsSL "https://dl.google.com/android/repository/commandlinetools-${CMD_OS}-11076708_latest.zip" \
      -o "$TC_DIR/cmdtools.zip"
    unzip -q "$TC_DIR/cmdtools.zip" -d "$ANDROID_HOME/cmdline-tools"
    rm -rf "$ANDROID_HOME/cmdline-tools/latest" "$TC_DIR/cmdtools.zip"
    mv "$ANDROID_HOME/cmdline-tools/cmdline-tools" "$ANDROID_HOME/cmdline-tools/latest"
  fi
  yes | "$ANDROID_HOME/cmdline-tools/latest/bin/sdkmanager" --licenses >/dev/null || true
  "$ANDROID_HOME/cmdline-tools/latest/bin/sdkmanager" \
    "platforms;android-34" \
    "build-tools;35.0.0" \
    "ndk;26.3.11579264"
fi

NDK_BIN="$(ls -d "$ANDROID_NDK_HOME"/toolchains/llvm/prebuilt/*/bin | head -n 1)"
rm -f "$BIN_WRAPPERS/aarch64-linux-android-gcc" "$BIN_WRAPPERS/aarch64-linux-android-g++" "$BIN_WRAPPERS/aarch64-linux-android-as"
cat > "$BIN_WRAPPERS/aarch64-linux-android-gcc" <<EOF
#!/bin/sh
exec "$NDK_BIN/aarch64-linux-android26-clang" "\$@"
EOF
cat > "$BIN_WRAPPERS/aarch64-linux-android-g++" <<EOF
#!/bin/sh
exec "$NDK_BIN/aarch64-linux-android26-clang++" "\$@"
EOF
cat > "$BIN_WRAPPERS/aarch64-linux-android-as" <<EOF
#!/bin/sh
exec "$NDK_BIN/aarch64-linux-android26-clang" "\$@"
EOF
chmod +x "$BIN_WRAPPERS/aarch64-linux-android-gcc" "$BIN_WRAPPERS/aarch64-linux-android-g++" "$BIN_WRAPPERS/aarch64-linux-android-as"
ln -sf "$NDK_BIN/llvm-ar"                         "$BIN_WRAPPERS/aarch64-linux-android-gcc-ar"
ln -sf "$NDK_BIN/llvm-ar"                         "$BIN_WRAPPERS/aarch64-linux-android-ar"
ln -sf "$NDK_BIN/llvm-ranlib"                     "$BIN_WRAPPERS/aarch64-linux-android-gcc-ranlib"
ln -sf "$NDK_BIN/llvm-ranlib"                     "$BIN_WRAPPERS/aarch64-linux-android-ranlib"
ln -sf "$NDK_BIN/ld.lld"                          "$BIN_WRAPPERS/aarch64-linux-android-ld"
ln -sf "$NDK_BIN/llvm-objcopy"                    "$BIN_WRAPPERS/aarch64-linux-android-objcopy"
ln -sf "$NDK_BIN/llvm-strip"                      "$BIN_WRAPPERS/aarch64-linux-android-strip"
# NDK r23+ replaced libgcc.a with compiler-rt builtins; provide an empty stub archive for Rockbox's `-lgcc`
"$NDK_BIN/llvm-ar" rcs "$ANDROID_DEPS_DIR/lib/libgcc.a"
cat > "$BIN_WRAPPERS/aarch64-linux-android-cpp" <<EOF
#!/bin/sh
if [ "\$1" = "--print-sysroot" ]; then
  exec "$NDK_BIN/aarch64-linux-android26-clang" --print-sysroot
fi
exec "$NDK_BIN/aarch64-linux-android26-clang" -E "\$@"
EOF
chmod +x "$BIN_WRAPPERS/aarch64-linux-android-cpp"

cat > "$BIN_WRAPPERS/sdl2-config" <<EOF
#!/bin/sh
case "\$1" in
  --cflags) echo "-I$ANDROID_DEPS_DIR/include/SDL2 -D_REENTRANT" ;;
  --libs|--static-libs) echo "-L$ANDROID_DEPS_DIR/lib -lSDL2" ;;
  --version) echo "2.30.9" ;;
esac
EOF
chmod +x "$BIN_WRAPPERS/sdl2-config"

export PATH="$BIN_WRAPPERS:$PATH"

# 3. Build static OpenSSL 3.0.15 (`libssl.a` + `libcrypto.a`, `-fPIC`) once
if [ ! -f "$ANDROID_DEPS_DIR/lib/libssl.a" ] || [ ! -f "$ANDROID_DEPS_DIR/lib/libcrypto.a" ]; then
  echo ">> [Toolchain 3/4] Building static OpenSSL 3.0.15 for android-arm64 ..."
  rm -rf "$TC_DIR/src-openssl"
  mkdir -p "$TC_DIR/src-openssl"
  curl -fsSL https://github.com/openssl/openssl/releases/download/openssl-3.0.15/openssl-3.0.15.tar.gz \
    | tar -xz -C "$TC_DIR/src-openssl"
  (
    cd "$TC_DIR/src-openssl/openssl-3.0.15"
    ANDROID_NDK_ROOT="$ANDROID_NDK_HOME" PATH="$NDK_BIN:$PATH" \
      ./Configure android-arm64 -D__ANDROID_API__=26 no-shared no-tests -fPIC \
      --prefix="$ANDROID_DEPS_DIR" --openssldir="$ANDROID_DEPS_DIR/ssl"
    ANDROID_NDK_ROOT="$ANDROID_NDK_HOME" PATH="$NDK_BIN:$PATH" \
      make -j"$JOBS" build_libs
    ANDROID_NDK_ROOT="$ANDROID_NDK_HOME" PATH="$NDK_BIN:$PATH" \
      make install_dev
  )
  rm -rf "$TC_DIR/src-openssl"
fi

# 4. Build SDL2 2.30.9 (`libSDL2.so` + Java glue `org/libsdl/app/*`) once
if [ ! -f "$ANDROID_DEPS_DIR/lib/libSDL2.so" ] || [ ! -d "$SDL2_JAVA_DIR/org" ]; then
  echo ">> [Toolchain 4/4] Building SDL2 2.30.9 for arm64-v8a ..."
  rm -rf "$TC_DIR/src-sdl" "$SDL2_JAVA_DIR"
  mkdir -p "$TC_DIR/src-sdl" "$ANDROID_DEPS_DIR/include/SDL2" "$SDL2_JAVA_DIR"
  curl -fsSL https://github.com/libsdl-org/SDL/releases/download/release-2.30.9/SDL2-2.30.9.tar.gz \
    | tar -xz -C "$TC_DIR/src-sdl"
  "$ANDROID_NDK_HOME/ndk-build" \
    NDK_PROJECT_PATH="$TC_DIR/src-sdl/SDL2-2.30.9" \
    APP_BUILD_SCRIPT="$TC_DIR/src-sdl/SDL2-2.30.9/Android.mk" \
    NDK_LIBS_OUT="$TC_DIR/src-sdl/libs" \
    NDK_OUT="$TC_DIR/src-sdl/obj" \
    APP_ABI=arm64-v8a \
    APP_PLATFORM=android-26 \
    -j"$JOBS" SDL2
  cp -a "$TC_DIR/src-sdl/SDL2-2.30.9/include/"*.h "$ANDROID_DEPS_DIR/include/SDL2/"
  cp "$TC_DIR/src-sdl/obj/local/arm64-v8a/libSDL2.so" "$ANDROID_DEPS_DIR/lib/libSDL2.so"
  cp -a "$TC_DIR/src-sdl/SDL2-2.30.9/android-project/app/src/main/java/org" "$SDL2_JAVA_DIR/"
  rm -rf "$TC_DIR/src-sdl"
fi

# Ensure Rust `aarch64-linux-android` target is installed
if ! rustup target list --installed | grep -q '^aarch64-linux-android$'; then
  echo ">> Adding Rust target aarch64-linux-android ..."
  rustup target add aarch64-linux-android
fi

export ANDROID_DEPS_DIR
export CROSS_COMPILE="aarch64-linux-android-"
export CARGO_TARGET_AARCH64_LINUX_ANDROID_LINKER="$BIN_WRAPPERS/aarch64-linux-android-gcc"
export CARGO_TARGET_AARCH64_LINUX_ANDROID_AR="$BIN_WRAPPERS/aarch64-linux-android-ar"

echo ">> [1/4] Building pure-Rust crates & native .uipod plugins (aarch64-linux-android) ..."
(
  cd "$ROOT"
  cargo build --locked --release --workspace --target aarch64-linux-android -j "$JOBS"
)

# Clean host helper binaries if they were built inside a Linux container on macOS (or vice versa)
if [ -f "$ROOT/tools/bmp2rb" ]; then
  BMP_INFO="$(file "$ROOT/tools/bmp2rb" 2>/dev/null || true)"
  if { [ "$HOST_OS" = "Darwin" ] && ! printf '%s' "$BMP_INFO" | grep -q "Mach-O"; } || \
     { [ "$HOST_OS" = "Linux" ]  && ! printf '%s' "$BMP_INFO" | grep -q "ELF"; }; then
    rm -f "$ROOT/tools/bmp2rb" "$ROOT/tools/codepages" "$ROOT/tools/rdf2binary" \
          "$ROOT/tools/convbdf" "$ROOT/tools/scramble" "$ROOT/tools/mkboot" \
          "$ROOT/tools/"*.o
  fi
fi

echo ">> [2/4] Building Rockbox C core, DSP, OpenGL ES presenter & audio codecs (libmain.so) ..."
if [ ! -f "$BUILD_DIR/Makefile" ] || \
   ! grep -q "aarch64-linux-android/release/libuipod_ffi.a" "$BUILD_DIR/Makefile" || \
   ! grep -q "$ANDROID_DEPS_DIR" "$BUILD_DIR/Makefile"; then
  rm -rf "$BUILD_DIR/apps" "$BUILD_DIR/firmware" "$BUILD_DIR/Makefile" "$BUILD_DIR/autoconf.h" "$BUILD_DIR/make.dep"
  (
    cd "$BUILD_DIR"
    "$ROOT/tools/configure" --target="$TARGET_ID" --type=N
  )
fi

rm -f "$BUILD_DIR/rbversion.h" "$BUILD_DIR/firmware/common/version.o"
if [ -f "$BUILD_DIR/libmain.so" ] && [ "$ROOT/target/aarch64-linux-android/release/libuipod_ffi.a" -nt "$BUILD_DIR/libmain.so" ]; then
  rm -f "$BUILD_DIR/libmain.so"
fi

make -C "$BUILD_DIR" -j"$JOBS" VERSION="$UIPOD_VERSION"
make -C "$BUILD_DIR" fullzip VERSION="$UIPOD_VERSION"

echo ">> [3/4] Staging Android runtime assets & arm64-v8a native libraries ..."
APK_WORK="$BUILD_DIR/apk-stage"
rm -rf "$APK_WORK"
mkdir -p "$APK_WORK/assets/uipod" "$APK_WORK/lib/arm64-v8a" "$APK_WORK/res/values" "$APK_WORK/res/mipmap-xxxhdpi"

TMP_UNZIP="$(mktemp -d)"
unzip -q "$BUILD_DIR/uipod-full.zip" -d "$TMP_UNZIP"
cp -a "$TMP_UNZIP/data/data/dev.famiclone.uipod/files/uipod/." "$APK_WORK/assets/uipod/"
rm -rf "$TMP_UNZIP"

find "$APK_WORK/assets/uipod/themes" -type f \
     ! -name 'rockbox_failsafe.cfg' ! -name 'rockbox_default_icons.cfg' -delete 2>/dev/null || true
find "$APK_WORK/assets/uipod/wps" -mindepth 1 -maxdepth 1 \
     ! -name 'rockbox_failsafe*' -exec rm -rf {} + 2>/dev/null || true
rm -rf "$APK_WORK/assets/uipod/backdrops" 2>/dev/null || true

cp -a "$ROOT/assets/theme/1ST_GEN_REMIX/.rockbox/wps/." "$APK_WORK/assets/uipod/wps/"
cp -a "$ROOT/assets/theme/1ST_GEN_REMIX/.rockbox/themes/." "$APK_WORK/assets/uipod/themes/"

rm -f "$APK_WORK/assets/uipod/fonts/"*.fnt "$APK_WORK/assets/uipod/fonts/COPYING-fonts.txt" 2>/dev/null || true
cp -a "$ROOT"/assets/fonts/*.fnt "$APK_WORK/assets/uipod/fonts/"
cp -a "$ROOT/assets/fonts/PixelMplus12-Regular.ttf" "$APK_WORK/assets/uipod/fonts/"
cp "$ROOT/assets/fonts/COPYING" "$APK_WORK/assets/uipod/fonts/COPYING-fonts.txt"

mkdir -p "$APK_WORK/assets/uipod/autoeq" "$APK_WORK/assets/uipod/eqs"
cp -a "$ROOT/assets/autoeq/." "$APK_WORK/assets/uipod/autoeq/"
cp -a "$ROOT/pak/uipod/." "$APK_WORK/assets/uipod/"
rm -rf "$APK_WORK/assets/uipod/systems" "$APK_WORK/assets/uipod/libmain.so" 2>/dev/null || true
find "$APK_WORK" -name '.DS_Store' -delete 2>/dev/null || true

for codec_file in "$APK_WORK/assets/uipod/codecs/"*.codec; do
  [ -f "$codec_file" ] || continue
  stem="$(basename "$codec_file" .codec)"
  cp "$codec_file" "$APK_WORK/lib/arm64-v8a/libcodec_${stem}.so"
  aarch64-linux-android-strip --strip-unneeded "$APK_WORK/lib/arm64-v8a/libcodec_${stem}.so"
  printf '1' > "$codec_file"
done

cp "$ANDROID_DEPS_DIR/lib/libSDL2.so" "$APK_WORK/lib/arm64-v8a/libSDL2.so"
aarch64-linux-android-strip --strip-unneeded "$APK_WORK/lib/arm64-v8a/libSDL2.so"
cp "$BUILD_DIR/libmain.so" "$APK_WORK/lib/arm64-v8a/libmain.so"
aarch64-linux-android-strip --strip-unneeded "$APK_WORK/lib/arm64-v8a/libmain.so"

for pl_so in "$ROOT/target/aarch64-linux-android/release/libbrick_breaker.so" \
             "$ROOT/target/aarch64-linux-android/release/libuipod_2048.so" \
             "$ROOT/target/aarch64-linux-android/release/libsnake.so" \
             "$ROOT/target/aarch64-linux-android/release/libtetris.so" \
             "$ROOT/target/aarch64-linux-android/release/libsound_racer.so" \
             "$ROOT/target/aarch64-linux-android/release/libmusic_quiz.so" \
             "$ROOT/target/aarch64-linux-android/release/libstopwatch.so" \
             "$ROOT/target/aarch64-linux-android/release/libtext_viewer.so" \
             "$ROOT/target/aarch64-linux-android/release/liblcd_plasma.so"; do
  pl_base="$(basename "$pl_so" .so)"
  pl_name="${pl_base#lib}"
  cp "$pl_so" "$APK_WORK/lib/arm64-v8a/libuipod_plugin_${pl_name}.so"
  aarch64-linux-android-strip --strip-unneeded "$APK_WORK/lib/arm64-v8a/libuipod_plugin_${pl_name}.so"
done

cp "$ROOT/android/res/values/strings.xml" "$APK_WORK/res/values/strings.xml"
python3 - "$APK_WORK/res/mipmap-xxxhdpi/ic_launcher.png" <<'PY'
import math, struct, sys, zlib

W, H = 192, 192
BG_BODY = (0x2A, 0x26, 0x27)
BORDER  = (0x55, 0x52, 0x53)
LCD_BG  = (0xD6, 0xD8, 0xD5)
LCD_FG  = (0x3C, 0x36, 0x37)
WHEEL   = (0xEE, 0xEF, 0xED)
CENTER  = (0x59, 0x59, 0x59)

rows = []
for y in range(H):
    row = bytearray([0])
    for x in range(W):
        dx = max(0.0, abs(x - 95.5) - 62.0)
        dy = max(0.0, abs(y - 95.5) - 76.0)
        if math.hypot(dx, dy) > 18.0:
            row.extend((0, 0, 0, 0))
            continue
        if math.hypot(dx, dy) > 14.5:
            row.extend((*BORDER, 255))
            continue
        if 34 <= x <= 157 and 22 <= y <= 92:
            if x in (34, 157) or y in (22, 92) or (34 <= y <= 37):
                row.extend((*LCD_FG, 255))
            else:
                row.extend((*LCD_BG, 255))
            continue
        wd = math.hypot(x - 95.5, y - 138.0)
        if wd <= 13.5:
            row.extend((*CENTER, 255))
        elif wd <= 34.0:
            row.extend((*WHEEL, 255))
        else:
            row.extend((*BG_BODY, 255))
    rows.append(bytes(row))

def chunk(tag, data):
    return struct.pack('>I', len(data)) + tag + data + struct.pack('>I', zlib.crc32(tag + data) & 0xffffffff)

raw = b''.join(rows)
png = (
    b'\x89PNG\r\n\x1a\n'
    + chunk(b'IHDR', struct.pack('>IIBBBBB', W, H, 8, 6, 0, 0, 0))
    + chunk(b'IDAT', zlib.compress(raw, 9))
    + chunk(b'IEND', b'')
)
with open(sys.argv[1], 'wb') as f:
    f.write(png)
PY

echo ">> [4/4] Compiling Android Java glue, DEXing, aligning & signing APK ..."
BT="$ANDROID_HOME/build-tools/35.0.0"
ANDROID_JAR="$ANDROID_HOME/platforms/android-34/android.jar"
mkdir -p "$APK_WORK/compiled_res" "$APK_WORK/gen" "$APK_WORK/classes" "$APK_WORK/dex"

"$BT/aapt2" compile --dir "$APK_WORK/res" -o "$APK_WORK/compiled_res/res.zip"
"$BT/aapt2" link \
  -o "$APK_WORK/unaligned.apk" \
  -I "$ANDROID_JAR" \
  --manifest "$ROOT/android/AndroidManifest.xml" \
  --java "$APK_WORK/gen" \
  -A "$APK_WORK/assets" \
  --version-code "$VER_CODE" \
  --version-name "$VER_CLEAN" \
  "$APK_WORK/compiled_res/res.zip"

find "$SDL2_JAVA_DIR" "$ROOT/android/src" "$APK_WORK/gen" -name '*.java' > "$APK_WORK/sources.txt"
javac --release 11 -g:none -encoding UTF-8 -cp "$ANDROID_JAR" -d "$APK_WORK/classes" @"$APK_WORK/sources.txt"

jar cf "$APK_WORK/classes.jar" -C "$APK_WORK/classes" .
"$BT/d8" --min-api 26 --lib "$ANDROID_JAR" --output "$APK_WORK/dex" "$APK_WORK/classes.jar"

cp "$APK_WORK/dex/classes.dex" "$APK_WORK/classes.dex"
(
  cd "$APK_WORK"
  zip -qr unaligned.apk classes.dex lib/arm64-v8a
)

"$BT/zipalign" -f -p 4 "$APK_WORK/unaligned.apk" "$APK_WORK/aligned.apk"

KS_ALIAS="${UIPOD_KEY_ALIAS:-uipod}"
KS_PASS="${UIPOD_KEYSTORE_PASS:-uipodandroid}"
KEY_PASS="${UIPOD_KEY_PASS:-$KS_PASS}"
if [ -n "${UIPOD_KEYSTORE_BASE64:-}" ]; then
  KEYSTORE="$APK_WORK/release.jks"
  printf '%s' "$UIPOD_KEYSTORE_BASE64" | base64 -d > "$KEYSTORE"
else
  KEYSTORE="${UIPOD_KEYSTORE_PATH:-$TC_DIR/uipod-debug.jks}"
fi
if [ ! -f "$KEYSTORE" ]; then
  keytool -genkeypair -v \
    -keystore "$KEYSTORE" \
    -storepass "$KS_PASS" \
    -keypass "$KEY_PASS" \
    -alias "$KS_ALIAS" \
    -keyalg RSA -keysize 2048 -validity 10000 \
    -dname 'CN=UIPod, OU=Handheld, O=famiclone, L=Kyiv, S=Kyiv, C=UA' >/dev/null 2>&1
fi

OUT_APK="$ROOT/dist/UIPod-${SAFE_VER}-android-arm64.apk"
"$BT/apksigner" sign \
  --ks "$KEYSTORE" \
  --ks-key-alias "$KS_ALIAS" \
  --ks-pass "pass:${KS_PASS}" \
  --key-pass "pass:${KEY_PASS}" \
  --out "$OUT_APK" \
  "$APK_WORK/aligned.apk"

"$BT/apksigner" verify "$OUT_APK"
rm -rf "$APK_WORK"
du -h "$OUT_APK"
echo ">> Done! Android APK: $OUT_APK"
