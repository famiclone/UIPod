#!/usr/bin/env bash
# Bump UIPod semantic version across VERSION and pak.json (and optionally commit + tag),
# generating a concise bullet changelog from git commits since the previous v* tag.
#
# Usage:
#   ./scripts/bump-version.sh patch            # v0.1.1 -> v0.1.2
#   ./scripts/bump-version.sh minor            # v0.1.1 -> v0.2.0
#   ./scripts/bump-version.sh major            # v0.1.1 -> v1.0.0
#   ./scripts/bump-version.sh v0.2.0           # explicit version
#   ./scripts/bump-version.sh patch --tag      # also git commit + git tag vX.Y.Z
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
VERSION_FILE="$ROOT/VERSION"
PAK_JSON="$ROOT/pak.json"

TARGET="${1:-patch}"
DO_TAG=0
if [ "${2:-}" = "--tag" ]; then
  DO_TAG=1
fi

CUR="$(tr -d '[:space:]' < "$VERSION_FILE" 2>/dev/null || echo "v0.1.0")"

NEW_VER="$(python3 - "$CUR" "$TARGET" <<'PY'
import re, sys

cur = sys.argv[1].strip()
target = sys.argv[2].strip()

m = re.match(r"^v?(\d+)\.(\d+)\.(\d+)$", cur)
major, minor, patch = (int(m.group(1)), int(m.group(2)), int(m.group(3))) if m else (0, 1, 0)

if target == "patch":
    patch += 1
    out = f"v{major}.{minor}.{patch}"
elif target == "minor":
    minor += 1
    patch = 0
    out = f"v{major}.{minor}.{patch}"
elif target == "major":
    major += 1
    minor = 0
    patch = 0
    out = f"v{major}.{minor}.{patch}"
else:
    if not re.match(r"^v?\d+\.\d+\.\d+([A-Za-z0-9._+-]*)?$", target):
        sys.stderr.write(f"Invalid version or bump target: {target}\n")
        sys.exit(1)
    out = target if target.startswith("v") else f"v{target}"

print(out)
PY
)"

printf '%s\n' "$NEW_VER" > "$VERSION_FILE"

python3 - "$ROOT" "$PAK_JSON" "$NEW_VER" <<'PY'
import json, re, subprocess, sys

root, path, ver = sys.argv[1], sys.argv[2], sys.argv[3]
with open(path, "r", encoding="utf-8") as f:
    data = json.load(f)

def short_git_changelog() -> str:
    try:
        prev_tag = subprocess.check_output(
            ["git", "-C", root, "describe", "--tags", "--abbrev=0", "--match", "v*"],
            stderr=subprocess.DEVNULL,
            text=True,
        ).strip()
        rev_range = f"{prev_tag}..HEAD"
    except Exception:
        rev_range = "-n 8"

    cmd = ["git", "-C", root, "log", "--no-merges", "--pretty=format:%s"]
    cmd.extend(rev_range.split())
    try:
        raw_lines = subprocess.check_output(cmd, stderr=subprocess.DEVNULL, text=True).splitlines()
    except Exception:
        raw_lines = []

    bullets = []
    seen = set()
    for line in raw_lines:
        s = line.strip()
        if not s or re.match(r"^chore\(release\):", s, re.I):
            continue
        s = re.sub(r"^[a-z]+(\([^)]+\))?!?:\s*", "", s, flags=re.I)
        s = re.sub(r"\s*\(v\d+\.\d+\.\d+\)\s*$", "", s)
        if not s:
            continue
        s = s[0].upper() + s[1:]
        key = s.lower()
        if key not in seen:
            seen.add(key)
            bullets.append(f"- {s}")
        if len(bullets) >= 6:
            break

    return "\n".join(bullets) if bullets else f"- Maintenance and stability updates ({ver})"

data["version"] = ver
changelog = data.get("changelog")
if not isinstance(changelog, dict):
    changelog = {}
if ver not in changelog or not str(changelog[ver]).strip() or str(changelog[ver]).startswith("Release "):
    merged = {ver: short_git_changelog()}
    for k, v in changelog.items():
        if k != ver:
            merged[k] = v
    data["changelog"] = merged

with open(path, "w", encoding="utf-8") as f:
    json.dump(data, f, indent=2, ensure_ascii=False)
    f.write("\n")
PY

echo ">> Updated VERSION and pak.json: $CUR -> $NEW_VER"

if [ "$DO_TAG" -eq 1 ]; then
  git -C "$ROOT" add VERSION pak.json
  git -C "$ROOT" commit -m "chore(release): $NEW_VER"
  git -C "$ROOT" tag -a "$NEW_VER" -m "UIPod $NEW_VER"
  echo ">> Created commit and tag $NEW_VER (push with: git push origin main --follow-tags)"
fi
