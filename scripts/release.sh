#!/usr/bin/env bash
#
# The usual release path is release-tag.yml (no clone, no push to main): RELEASING.md. This
# script is the local alternative; it pushes the release commit to main, which the main ruleset
# allows only through the admin bypass.
#
# One-shot release helper. Cuts a `vX.Y.Z` tagged release without
# force-pushing main and without double-triggering the CI + publish
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
#      push the tag (triggers publish.yml once on the same commit).
#
# Why this matters
#   Pushing main and tag separately means two distinct workflow triggers
#   against the same SHA: ci validates the code (it ignores tags via
#   `tags-ignore`), and publish.yml builds + uploads the release
#   artifacts (it triggers on `push: tags`). No overlap, no force-push,
#   no dup runs. Force-pushing main right before tagging (what we did
#   manually before) is what caused the earlier double-fire.
#
#   If a tag-push publish run ever wedges (GitHub Actions outage), fall
#   back to the manual override: `gh workflow run publish.yml -f tag=vX.Y.Z`.
#
# Around it (scripts/release/)
#   before:  make release-check               quality, then e2e, then test-payload
#            release-check is local; for a PR, wait on the merge gate only:
#            scripts/release/wait-pr-gate.sh <pr>   ("PR gate"; CodeQL is not waited for,
#            its open alerts still block step 1 below)
#   after:   scripts/release/verify-release.sh vX.Y.Z            every asset there, none empty
#            scripts/release/verify-release.sh vX.Y.Z --promote  then mark it the latest
#            (promote only once it has been tried on a console)

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

# No release over an open code-scanning alert: fix it, or dismiss a by-design one with its
# reason on the repository's Security tab (scripts/security-scan.sh and the codeql workflow's
# gate check the same on every change). Needs gh signed in to an account that can read them.
repo="$(gh repo view --json nameWithOwner --jq .nameWithOwner)"
open_alerts="$(gh api --paginate "repos/$repo/code-scanning/alerts?ref=refs/heads/main&state=open&per_page=100" \
  --jq '.[] | "  \(.rule.id)  \(.most_recent_instance.location.path):\(.most_recent_instance.location.start_line)"')" || {
  echo "release: could not read the code-scanning alerts (gh signed in? security_events scope?)" >&2
  exit 1
}
if [[ -n "$open_alerts" ]]; then
  echo "release: open code-scanning alerts on main:" >&2
  echo "$open_alerts" >&2
  echo "fix them, or dismiss a by-design one with its reason, then release" >&2
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

# The sync above reports but never re-reads; prove every downstream file now
# agrees with VERSION before anything is committed or tagged.
node scripts/update-version.js --check

# On origin too: release-tag.yml creates tags there that this clone may not have fetched.
if git rev-parse -q --verify "refs/tags/${tag}" >/dev/null \
    || git ls-remote --exit-code --tags origin "refs/tags/${tag}" >/dev/null; then
  echo "release: tag ${tag} already exists — bump VERSION to release again" >&2
  exit 1
fi

# publish.yml refuses a release whose CHANGELOG has no section for the
# version, and a pushed tag cannot be moved — so refuse here, before the tag.
if ! grep -qx "## ${target}" CHANGELOG.md; then
  echo "release: CHANGELOG.md has no '## ${target}' section — write the entry (and commit it) first" >&2
  exit 1
fi

# ─── 3. Commit (only if update-version.js changed anything) ──────────────

if ! git diff --quiet || ! git diff --cached --quiet; then
  # Stage only the files update-version.js touches, not `git add -A`
  # which could sweep in editor swap files, .DS_Store, etc.
  #
  # This list must stay in step with the patchers in update-version.js.
  # It drifted once: the Cargo.lock files were added there but not here,
  # so v5.1.2 synced them and then left them unstaged — the release
  # commit was incomplete and the tree came back dirty.
  git add \
    VERSION \
    client/package.json \
    client/package-lock.json \
    client/src-tauri/tauri.conf.json \
    client/src-tauri/Cargo.toml \
    payload/include/config.h \
    engine/Cargo.toml \
    engine/Cargo.lock \
    client/src-tauri/Cargo.lock
  git commit -m "Release ${tag}"
fi

# ─── 4. Tag ───────────────────────────────────────────────────────────────

git tag -a "${tag}" -m "${tag}"

# ─── 5. Push branch and tag together (two separate workflow triggers) ─────

# Atomic: a refused tag must not leave the release commit on main, and a refused main must not
# start a publish. The local tag goes on failure so a rerun is not stopped by its own tag above.
if ! git push --atomic origin main "refs/tags/${tag}"; then
  git tag -d "${tag}" >/dev/null
  echo "release: push refused; nothing reached origin. Fix it (git pull --rebase?) and run this again" >&2
  exit 1
fi

echo
echo "Pushed ${tag} → origin. Watch the workflows at:"
echo "  https://github.com/phantomptr/ps5upload/actions"
echo "When publish finishes: scripts/release/verify-release.sh ${tag} [--promote]"
