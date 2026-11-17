#!/usr/bin/env bash
#
# Fast release: the owner's "just ship it" path for a small change. Bare checks only, no wait for
# CI, always a PRE-RELEASE (promote it by hand after trying it, as usual).
#
#   scripts/release/fast-release.sh <X.Y.Z>          # from a branch with the change committed
#   make fast-release V=<X.Y.Z>
#
# What it does, in order (stops at the first failure):
#   1. Checks the setup: the active gh account is phantomptr, you are on a branch (not main), the
#      tree is clean, the branch contains origin/main, and vX.Y.Z does not exist yet.
#   2. Catches the branch up with the last release (RELEASING.md §5) when nobody did: version files
#      to that release, CHANGELOG.md from its tag with this branch's "## Unreleased" text on top.
#   3. Bare checks, a few minutes at most, on what the branch changed: i18n coverage, the
#      old-protocol check, the embedded guest/pad pages (rebuilt and committed when stale), the C rename
#      audit when payload C changed, engine fmt when Rust changed, client typecheck + lint when
#      the client changed.
#   4. Pushes, opens a PR and squash-merges it with --admin at once (no CI wait).
#   5. Dispatches release-tag with prerelease=true and force=true (tags without CI on main), waits
#      for the tag, then for publish, then runs verify-release.sh.
#
# Full CI still runs on the PR and on main afterwards; it just does not hold the release. Use the
# normal path (RELEASING.md) for anything big: helper/payload changes, installs, transfers.

set -euo pipefail

V="${1:-}"
if [[ ! "$V" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]]; then
  echo "usage: scripts/release/fast-release.sh X.Y.Z" >&2
  exit 2
fi
ROOT="$(git rev-parse --show-toplevel)"
cd "$ROOT"

step() { printf '\n== %s\n' "$*"; }
fail() { printf 'fast-release: %s\n' "$*" >&2; exit 1; }
check() { # name, command...: prints ok/FAILED, keeps the log on failure
  local name="$1"; shift
  local log; log="$(mktemp -t "fast-release-${name}.XXXXXX")"
  if timeout 900 "$@" >"$log" 2>&1; then
    echo "ok      $name"; rm -f "$log"
  else
    echo "FAILED  $name (log: $log)"; tail -20 "$log" >&2; exit 1
  fi
}

step "1. Setup"
[ "$(gh api user --jq .login)" = "phantomptr" ] || fail "gh is not on phantomptr: gh auth switch --user phantomptr"
BRANCH="$(git rev-parse --abbrev-ref HEAD)"
[ "$BRANCH" != "main" ] && [ "$BRANCH" != "HEAD" ] || fail "commit the change on a branch, not main"
if [ -n "$(git status --porcelain --untracked-files=no | grep -v '^.. third_party/' || true)" ]; then
  fail "the tree has uncommitted changes: commit them first"
fi
git fetch -q origin main --tags
git merge-base --is-ancestor origin/main HEAD || fail "the branch is behind origin/main: git rebase origin/main"
git rev-parse -q --verify "refs/tags/v$V" >/dev/null && fail "v$V already exists"
LAST="$(git tag --list 'v[0-9]*' --sort=-v:refname | head -1)"; LAST="${LAST#v}"
echo "branch $BRANCH, last release $LAST, releasing $V"

step "2. Catch up with $LAST"
if [ "$(cat VERSION)" != "$LAST" ]; then
  node scripts/update-version.js "$LAST" >/dev/null
  echo "version files synced to $LAST"
fi
if ! grep -q "^## $LAST\$" CHANGELOG.md; then
  # This branch's notes on top of the released CHANGELOG (the release renamed "Unreleased").
  NOTES="$(awk '/^## Unreleased$/{f=1;next} /^## /{if(f)exit} f' CHANGELOG.md)"
  git show "v$LAST:CHANGELOG.md" > CHANGELOG.md
  if [ -n "$(printf '%s' "$NOTES" | tr -d '[:space:]')" ] && ! printf '%s' "$NOTES" | grep -q "^## $LAST"; then
    python3 - "$LAST" "$NOTES" <<'EOF'
import sys
last, notes = sys.argv[1], sys.argv[2].strip("\n")
p = "CHANGELOG.md"; s = open(p).read()
# main may still call the last release's notes "Unreleased": keep only what is new.
head = f"## {last}\n"
released = s.split(head, 1)[1].split("\n## ", 1)[0] if head in s else ""
seen = {l.strip() for l in released.splitlines() if l.strip()}
def heading(l):
    t = l.strip()
    return t.startswith("**") and t.endswith("**") and len(t) > 4
kept = [l for l in notes.splitlines() if not l.strip() or heading(l) or l.strip() not in seen]
# A heading stays only when something new is under it.
out = []
for i, l in enumerate(kept):
    if heading(l):
        rest = kept[i + 1:]
        nxt = next((j for j, r in enumerate(rest) if heading(r)), len(rest))
        if not any(r.strip() for r in rest[:nxt]):
            continue
    out.append(l)
new = "\n".join(out).strip("\n")
if new:
    s = s.replace(head, f"## Unreleased\n\n{new}\n\n{head}", 1)
open(p, "w").write(s)
EOF
  fi
  echo "CHANGELOG.md taken from v$LAST"
fi
awk '/^## /{print; exit}' CHANGELOG.md | grep -q '^## Unreleased$' || fail "CHANGELOG.md needs this release's notes under '## Unreleased' at the top"
if [ -n "$(git status --porcelain --untracked-files=no | grep -v '^.. third_party/' || true)" ]; then
  git add -A -- . ':!third_party'
  git commit -q -m "Catch up with v$LAST: version files and CHANGELOG"
  echo "committed the catch-up"
fi

step "3. Bare checks"
CHANGED="$(git diff --name-only origin/main...HEAD)"
has() { printf '%s\n' "$CHANGED" | grep -qE "$1"; }
check i18n node scripts/i18n-coverage.mjs
check no-ftx2 make check-no-ftx2
if has '^client/src/i18n/locales/en\.ts$'; then
  # English this branch removed or changed, still expected by a test (12.6.2 shipped a red e2e
  # spec this way: the fast path skips e2e). Seconds, not a test run.
  check old-strings-in-tests python3 - <<'EOF'
import re, subprocess, sys
diff = subprocess.run(["git", "diff", "origin/main...HEAD", "--", "client/src/i18n/locales/en.ts"],
                      capture_output=True, text=True).stdout
pat = re.compile(r'^\s*"?[\w.\-]+"?:\s*"(.*)",?\s*$')
removed = {m.group(1) for l in diff.splitlines() if l.startswith("-") and not l.startswith("---")
           for m in [pat.match(l[1:])] if m}
added = {m.group(1) for l in diff.splitlines() if l.startswith("+") and not l.startswith("+++")
         for m in [pat.match(l[1:])] if m}
gone = [v for v in removed - added if len(v) >= 16]
files = subprocess.run(["git", "ls-files", "client/e2e", "client/src"], capture_output=True, text=True).stdout.split()
tests = [f for f in files if f.startswith("client/e2e/") or re.search(r"\.test\.(ts|tsx)$", f)]
# Pieces of the old text no current string still has: a test quoting one expects the old text.
current = open("client/src/i18n/locales/en.ts", encoding="utf-8").read()
pieces = set()
for v in gone:
    for part in re.split(r"\{\w+\}", v):
        for i in range(0, max(1, len(part) - 27), 6):
            w = part[i:i + 28]
            if len(w) == 28 and w not in current:
                pieces.add(w)
bad = []
for f in tests:
    text = open(f, encoding="utf-8", errors="replace").read().replace("\\'", "'").replace("&#x27;", "'")
    hit = next((w for w in pieces if w in text), None)
    if hit:
        bad.append(f"{f}: still expects \"{hit}\"")
if bad:
    print("\n".join(bad)); sys.exit(1)
print(f"{len(gone)} changed string(s), none expected by a test")
EOF
fi
if has '^client/'; then
  # Rebuilds the pages; when they were stale the rebuilt ones are committed below and checked again.
  timeout 900 node scripts/check-embedded-pages.mjs >/dev/null 2>&1 || true
fi
if [ -n "$(git status --porcelain -- engine/crates/ps5upload-engine/src/join_page.html engine/crates/ps5upload-engine/src/pad_page.html site/join site/pad)" ]; then
  git add engine/crates/ps5upload-engine/src/join_page.html engine/crates/ps5upload-engine/src/pad_page.html site/join site/pad
  git commit -q -m "Rebuild the embedded guest and pad pages"
  check embedded-pages-again node scripts/check-embedded-pages.mjs
fi
if has '^payload/.*\.[ch]$'; then
  check rename-audit bash -c "cd engine && cargo test -q -p ps5upload-ctest --test payload_cutover"
fi
if has '\.rs$'; then
  check engine-fmt bash -c "cd engine && cargo fmt --all --check"
fi
if has '^client/'; then
  check client-typecheck bash -c "cd client && npm run typecheck"
  check client-lint bash -c "cd client && npm run lint"
fi

step "4. PR and merge"
git push -q -u origin "$BRANCH"
PR="$(gh pr list --head "$BRANCH" --state open --json number --jq '.[0].number')"
if [ -z "$PR" ]; then
  BODY="$(mktemp -t fast-release-body.XXXXXX)"
  {
    echo "Fast release v$V (scripts/release/fast-release.sh): bare checks only, merged without waiting for CI."
    echo
    awk '/^## Unreleased$/{f=1;next} /^## /{if(f)exit} f' CHANGELOG.md
  } > "$BODY"
  gh pr create --base main --head "$BRANCH" --title "Release v$V" --body-file "$BODY" >/dev/null
  rm -f "$BODY"
  PR="$(gh pr list --head "$BRANCH" --state open --json number --jq '.[0].number')"
fi
gh pr merge "$PR" --squash --admin >/dev/null
echo "PR #$PR merged"

step "5. Tag v$V (pre-release) and publish"
gh workflow run release-tag.yml --ref main -f version="$V" -f prerelease=true -f force=true
sleep 8
TAG_RUN="$(gh run list --workflow release-tag.yml --limit 1 --json databaseId --jq '.[0].databaseId')"
gh run watch "$TAG_RUN" --exit-status --interval 15 >/dev/null || fail "release-tag failed: gh run view $TAG_RUN --log-failed"
echo "tagged v$V"
PUB=""
for _ in $(seq 1 30); do
  PUB="$(gh run list --workflow publish.yml --limit 5 --json databaseId,headBranch \
    --jq "[.[] | select(.headBranch==\"v$V\")][0].databaseId // empty")"
  [ -n "$PUB" ] && break
  sleep 10
done
[ -n "$PUB" ] || fail "no publish run for v$V appeared"
echo "publish run $PUB (about 25 minutes)"
gh run watch "$PUB" --exit-status --interval 60 >/dev/null || fail "publish failed: gh run view $PUB --log-failed"
scripts/release/verify-release.sh "v$V"
git switch -q main && git pull -q --ff-only || true
echo
echo "v$V is out as a pre-release: https://github.com/phantomptr/ps5upload/releases/tag/v$V"
echo "Promote it after a console test: scripts/release/verify-release.sh v$V --promote"
