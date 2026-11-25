#!/usr/bin/env bash
# Builds the UIPod WebAssembly runtime (`uipod-wasm`) and production web landing bundle (`web/dist/`).
set -euo pipefail
export PATH="/opt/homebrew/bin:/usr/local/bin:$HOME/.cargo/bin:$PATH"

ROOT="$(cd "$(dirname "$0")" && pwd)"
mkdir -p "$ROOT/public/fonts"
cp "$ROOT/../assets/fonts/NotoSansJP-Regular.ttf" "$ROOT/public/fonts/NotoSansJP-Regular.ttf"

echo ">> 1/2 Building uipod-wasm (Rust -> WebAssembly / WebGL2)..."
(cd "$ROOT/uipod-wasm" && wasm-pack build --target web --out-dir ../src/wasm-pkg)

echo ">> 2/2 Building UIPod web landing page (TypeScript + Vite)..."
(cd "$ROOT" && npm run build)

echo ">> Done! Static site ready in web/dist/ (preview with: cd web && npm run preview)"
