#!/usr/bin/env bash
# Builds the UIPod WebAssembly player (`uipod-wasm`) + Vite web page (`web/dist/`)
# and deploys to GitHub Pages (supports both GitHub Actions Pages workflow and `gh-pages` branch).
#
# Usage:
#   ./web/deploy.sh                        # Auto-detect Pages mode (Actions `main` or `gh-pages` branch)
#   ./web/deploy.sh -m "Update web player" # Custom commit message
#   ./web/deploy.sh --gh-pages             # Push built `web/dist/` directly to `origin/gh-pages`
#   ./web/deploy.sh --actions              # Commit web/UI paths to `main`, push & trigger `pages.yml`
set -euo pipefail
export PATH="/opt/homebrew/bin:/usr/local/bin:$HOME/.cargo/bin:$PATH"

WEB_DIR="$(cd "$(dirname "$0")" && pwd)"
REPO_ROOT="$(cd "$WEB_DIR/.." && pwd)"

MODE="auto"
COMMIT_MSG="chore(web): update web page and WASM player"

while [[ $# -gt 0 ]]; do
  case "$1" in
    --gh-pages)
      MODE="gh-pages"
      shift
      ;;
    --actions)
      MODE="actions"
      shift
      ;;
    -m|--message)
      COMMIT_MSG="${2:?Missing commit message after $1}"
      shift 2
      ;;
    -h|--help)
      sed -n '2,9p' "$0" | sed 's/^# \{0,1\}//'
      exit 0
      ;;
    *)
      echo "Unknown option: $1" >&2
      exit 1
      ;;
  esac
done

# 1. Build WASM player + Vite production bundle locally first (fail fast on any Rust/TS error)
"$WEB_DIR/build.sh"
touch "$WEB_DIR/dist/.nojekyll"

# 2. Auto-detect GitHub Pages build_type (`workflow` vs `legacy` branch) when MODE == "auto"
if [[ "$MODE" == "auto" ]]; then
  if command -v gh >/dev/null 2>&1; then
    BUILD_TYPE="$(gh api repos/:owner/:repo/pages --jq '.build_type' 2>/dev/null || true)"
    if [[ "$BUILD_TYPE" == "legacy" ]]; then
      MODE="gh-pages"
    else
      MODE="actions"
    fi
  else
    MODE="actions"
  fi
fi

if [[ "$MODE" == "gh-pages" ]]; then
  echo ">> Deploying web/dist/ directly to origin/gh-pages..."
  TMP_GIT="$(mktemp -d "${TMPDIR:-/tmp}/uipod-gh-pages.XXXXXX")"
  trap 'rm -rf "$TMP_GIT"' EXIT

  REMOTE_URL="$(git -C "$REPO_ROOT" remote get-url origin)"
  cp -R "$WEB_DIR/dist/." "$TMP_GIT/"
  touch "$TMP_GIT/.nojekyll"

  (
    cd "$TMP_GIT"
    git init -q -b gh-pages
    git add -A
    git commit -q -m "$COMMIT_MSG"
    git push -f "$REMOTE_URL" gh-pages:gh-pages
  )
  echo ">> Published web/dist/ to origin/gh-pages!"
else
  echo ">> Deploying via GitHub Actions Pages workflow (origin/main -> .github/workflows/pages.yml)..."
  cd "$REPO_ROOT"

  # Stage ONLY web & shared UI crate paths (never stage unrelated C firmware WIP or bump VERSION)
  WEB_PATHS=(
    "web"
    "crates/uipod-ui"
    "crates/uipod-plugin-sdk"
    "plugins"
    "assets/fonts"
    "pak/uipod/about.txt"
    ".github/workflows/pages.yml"
  )
  git add -- "${WEB_PATHS[@]}"

  if ! git diff --cached --quiet; then
    git commit -m "$COMMIT_MSG"
    git push origin HEAD:main
    echo ">> Pushed web & WASM changes to origin/main — GitHub Pages workflow triggered!"
  else
    # Check if local HEAD is ahead of origin/main on web commits
    LOCAL_SHA="$(git rev-parse HEAD)"
    REMOTE_SHA="$(git rev-parse origin/main 2>/dev/null || echo "")"
    if [[ -n "$REMOTE_SHA" && "$LOCAL_SHA" != "$REMOTE_SHA" ]]; then
      git push origin HEAD:main
      echo ">> Pushed commits to origin/main — GitHub Pages workflow triggered!"
    elif command -v gh >/dev/null 2>&1; then
      echo ">> No new local web diffs; dispatching pages.yml workflow on GitHub..."
      gh workflow run pages.yml
      echo ">> Triggered GitHub Pages workflow (pages.yml)!"
    else
      echo ">> Web sources are already up to date with origin/main."
    fi
  fi
fi
