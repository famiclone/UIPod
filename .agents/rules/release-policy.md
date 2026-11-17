---
trigger: always_on
---

# Release & GitHub Push Policy

1. **Explicit User Approval Required**:
   - **NEVER** bump release versions (`VERSION`, `pak.json`, `scripts/bump-version.sh`), package a new versioned release, create `v*` git tags, or `git push` commits/tags to GitHub unless the user **explicitly** instructs you to build a new release and/or push to GitHub.
2. **Local Development Builds vs. Releases**:
   - During development, bug fixes, or feature implementation (even if the user says `"збери"` / `"build"`), only run local compilation/testing (`cargo test`, `./build.sh`, or local `dist/UIPod.pak` staging) **without** bumping `VERSION`, creating release tags, or pushing to remote.
   - Wait for the user's explicit command (e.g., `"збери реліз і запуш"`, `"пуш на гітхаб"`, `"зроби новий реліз"`) before bumping version numbers, generating release archives, tagging, or pushing to `origin`.
