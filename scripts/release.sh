#!/usr/bin/env bash
#
# One-shot release helper. Cuts a `vX.Y.Z` tagged release without
# force-pushing main and without double-triggering the CI + release
# workflows.
#
# Usage
#   ./scripts/release.sh 2.2.0        # bump VERSION, sync, commit, tag, push
#   ./scripts/release.sh              # release whatever VERSION currently says
#   npm run release 2.2.0             # same thing via package.json
#
# Ritual
#   1. Guard: working tree must be clean, on main, up-to-date with origin.
#   2. If a version arg was given, write it to VERSION. Either way, run
#      scripts/update-version.js to propagate VERSION into the 6 downstream
#      files (package.json, tauri.conf.json, etc.).
#   3. If scripts/update-version.js produced changes, create one commit
#      `Release vX.Y.Z` containing them. Otherwise skip the commit
#      (everything was already synced).
#   4. Create annotated tag `vX.Y.Z` at the (possibly new) HEAD.
#   5. Push main first (triggers ci once on the release commit), then
#      push the tag (triggers release once on the same commit).
#
# Why this matters
#   Pushing main and tag separately means two distinct workflow triggers
#   against the same SHA: ci validates the code, release builds artifacts.
#   No overlap, no force-push, no dup runs. Force-pushing main right
#   before tagging (what we did manually before) is what caused the
#   earlier double-fire.

set -euo pipefail

cd "$(dirname "$0")/.."

# ─── 1. Guards ────────────────────────────────────────────────────────────

if ! git diff --quiet || ! git diff --cached --quiet; then
  echo "release: working tree has uncommitted changes — commit or stash first" >&2
  exit 1
fi

branch="$(git rev-parse --abbrev-ref HEAD)"
if [[ "$branch" != "main" ]]; then
  echo "release: must run from main (current: $branch)" >&2
  exit 1
fi

git fetch --quiet origin main
local_sha="$(git rev-parse HEAD)"
remote_sha="$(git rev-parse origin/main)"
if [[ "$local_sha" != "$remote_sha" ]]; then
  echo "release: local main differs from origin/main — pull or push before releasing" >&2
  exit 1
fi

# ─── 2. Sync version ──────────────────────────────────────────────────────

version_arg="${1:-}"
if [[ -n "$version_arg" ]]; then
  node scripts/update-version.js "$version_arg"
else
  node scripts/update-version.js
fi
target="$(cat VERSION)"
tag="v${target}"

if git rev-parse -q --verify "refs/tags/${tag}" >/dev/null; then
  echo "release: tag ${tag} already exists — bump VERSION to release again" >&2
  exit 1
fi

# ─── 3. Commit (only if update-version.js changed anything) ──────────────

if ! git diff --quiet || ! git diff --cached --quiet; then
  git add -A
  git commit -m "Release ${tag}"
fi

# ─── 4. Tag ───────────────────────────────────────────────────────────────

git tag -a "${tag}" -m "${tag}"

# ─── 5. Push branch then tag (two separate workflow triggers) ─────────────

git push origin main
git push origin "refs/tags/${tag}"

echo
echo "Pushed ${tag} → origin. Watch the workflows at:"
echo "  https://github.com/phantomptr/ps5upload/actions"
