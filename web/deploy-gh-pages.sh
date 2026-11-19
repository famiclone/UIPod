#!/usr/bin/env bash
# Builds the UIPod WebAssembly site and publishes `web/dist/` to the `gh-pages` branch
# without touching the working tree or `main` branch.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
"$ROOT/web/build.sh"

REMOTE_URL="$(git -C "$ROOT" remote get-url origin)"
STAGE="$(mktemp -d /tmp/uipod-gh-pages.XXXXXX)"
trap 'rm -rf "$STAGE"' EXIT

cp -R "$ROOT/web/dist/." "$STAGE/"
touch "$STAGE/.nojekyll"

cd "$STAGE"
git init -q -b gh-pages
git add -A
git commit -q -m "Deploy UIPod WebAssembly simulator to GitHub Pages"
git remote add origin "$REMOTE_URL"
git push --force origin gh-pages

echo ">> Deployed to gh-pages on $REMOTE_URL"
echo ">> Site URL: https://famiclone.github.io/UIPod/"
