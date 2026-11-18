# Releasing

A release is one batch PR and one workflow run. There is no version-bump PR: the version comes
from the input to `release-tag`, which tags a bump commit that never goes to `main`.

## Fast release (a small change, now)

For a small, low-risk change (wording, docs, a UI tweak) that should go out without waiting for
CI: commit it on a branch with its notes under `## Unreleased` in CHANGELOG.md, then

```sh
make fast-release V=12.6.2      # or scripts/release/fast-release.sh 12.6.2
```

It catches the branch up with the last release (§5) if nobody did, runs bare checks on what the
branch changed (i18n coverage, no old-protocol names or ports (make check-no-ftx2), the embedded guest/pad pages, the C rename audit,
engine fmt, client typecheck and lint), merges the PR with `--admin` at once, dispatches
`release-tag` with `prerelease=true force=true`, waits for publish and runs `verify-release.sh`.
It is always a pre-release; promote it after a console test as usual. CI still runs on the PR and
on `main`; it just doesn't hold the release. Anything touching the helper, installs or transfers
takes the normal path above.

`scripts/release/fast-release.sh 12.6.2 --gate` (`make fast-release V=12.6.2 GATE=1`) is the
same run with the PR gate in it: the merge goes through `merge-when-green.sh` (§1), so nothing is
merged or tagged unless the gate passed, and `release-tag` runs without `force`.

## 1. The batch PR

- Put the release's changes in one PR.
- Write the CHANGELOG entry in the same PR under `## Unreleased`, at the top of
  `CHANGELOG.md` (above the last release's `## X.Y.Z`, with the usual `---` between sections).
  Do not edit `VERSION` or the version files.
- Check that `release-tag` will accept the CHANGELOG, from the PR's branch. `--check` reads
  only; without it the same command rewrites `CHANGELOG.md` in place (it is what `release-tag`
  runs to rename the heading), so never run it bare in a checkout:

  ```sh
  node scripts/release/changelog-release.mjs 12.1.0 --previous 12.0.0 --check
  ```

- Wait for `PR gate` and merge, in one command. `PR gate` is the only check that matters for
  merging. CodeQL runs beside it and reports to the Security tab, but nothing waits for it.

  ```sh
  scripts/release/merge-when-green.sh <n>
  ```

  It waits for the gate and squash-merges with the admin bypass only when the gate passed, for
  the commit it checked. When the gate fails on something outside the change (no runner, apt
  mirrors, the payload wait, a simulator crash, a Playwright flaky-only result) it re-runs the
  failed jobs once and waits again; a real failure, or a second failure, ends it with
  `NOT MERGED` and a non-zero exit. `--no-merge` stops after the verdict.

  Do not put the wait and the merge together by hand. `gh pr merge --admin` does not look at
  any check, and a pipe hides the wait's exit status: `wait-pr-gate.sh <n> | tail -3 && gh pr
  merge <n> --squash --admin` merged a release PR whose gate had failed, because `&&` saw
  `tail` succeed. `wait-pr-gate.sh` alone is for watching a PR you are not about to merge.

## 2. Tag it

```sh
gh workflow run release-tag.yml --ref main -f version=12.1.0
```

Add `-f prerelease=false` only when this release should become the latest right away. By
default it is a pre-release, and you promote it by hand after testing it on a console.

`release-tag`:

1. Checks that `12.1.0` is X.Y.Z, newer than every tag, and not already tagged.
2. Runs `node scripts/update-version.js 12.1.0` and renames `## Unreleased` to `## 12.1.0`
   (`scripts/release/changelog-release.mjs`). It refuses if `main` has not caught up with the
   last release (see step 5).
3. Checks that only version files and `CHANGELOG.md` changed and that both Cargo lockfiles
   still resolve with `--locked`.
4. Waits for `ci` on the `main` commit it builds on. It does not wait when that commit's tree is
   the tree of the merged PR's head, the PR was up to date with `main`, and the PR's `ci`
   passed (`.github/scripts/release-ci-verdict.mjs`). If the PR's change turned every lane on
   (a batch PR always does: it syncs `VERSION` and the lockfiles), that is enough on its own;
   otherwise the previous `main` commit must have passed too. After an up-to-date batch PR, the
   wait is immediate. In an emergency, `-f force=true` skips the wait.
5. Commits `12.1.0` as `github-actions[bot]`, tags it `v12.1.0`, and pushes only the tag. The
   main ruleset lets only an admin update `main`, so the bump commit lives on the tag.
6. Dispatches `publish` and `docker-engine` at the tag. Both build the tagged commit, so the
   app, the engine, `latest.json` and the images all say `12.1.0`.
7. Cancels a `release-cache` run that is still going on `main`. The merge started it, and it
   would hold two macOS and two Linux runners that `publish` needs.

## 3. Watch

```sh
gh run list --workflow publish.yml --limit 1
gh run list --workflow docker-engine.yml --limit 1
gh run watch <id>
```

`publish` took 31 minutes for v12.6.3 when it had the runners to itself, and 37 to 45 minutes
when it started while `ci`, CodeQL and `release-cache` were still running for the merge. The
changes of 2026-11-17 should bring the first case to about 25 minutes. A failed `publish` run
re-runs its failed jobs up to twice (its `retry-failed` job, through `rerun-failed`).

If it still ends failed, read why before anything else:
`node scripts/release/gate-failures.mjs <run id>` says, job by job, whether the failure came from
outside the release (runner, apt mirror, payload wait, simulator) or from the build. A tag cannot
be moved, but an unpublished one is not lost:

- The apt steps run `main`'s `scripts/ci-apt.sh`, not the tag's (`scripts/ci-apt-latest.sh`),
  so a mirror fix merged to `main` reaches the tag's next attempt: `gh run rerun <id> --failed`.
- For a fix in the workflow itself, dispatch `publish` from `main` for the existing tag. The
  workflow file is then `main`'s, and every job still checks out and builds the tag:
  `gh workflow run publish.yml --ref main -f tag=v12.1.0`.

What runs where, in `publish`:

| Job | Runner | Time (v12.6.3) | Notes |
| --- | --- | --- | --- |
| build-payload | ubuntu-24.04 | 4 min | The PS5 payload and the converter. Every other build starts at the same time and waits for its ELFs only where it needs them. |
| build-client (6 legs) | macOS 15 (x2), Ubuntu x64 and arm64, Windows x64 and arm64 | 17-25 min | The engine sidecar, then the Tauri app. The desktop app's dependencies start compiling before the payload wait. Windows is last: it has no compile cache, because sccache breaks on Windows with Rust 1.100. |
| build-android | ubuntu-24.04 | 20 min | Not a gate. |
| build-ios | xcode-27 (macOS) | 19 min | Not a gate. |
| virus-scan | ubuntu-24.04 | Ends about 1 min after the last build | Scans each build's files with ClamAV as they are uploaded, and gives create-release the hashes of the files that scanned clean. |
| create-release | ubuntu-24.04 | about 2 min after the last build | Scans any file that virus-scan did not cover, then makes the manifests, the attestations and the release. Before the change this was 6 min, with 4.5 min of it scanning. |

The repository has free runners: 20 Linux or Windows jobs and 5 macOS jobs can run at once.
`publish` uses three of the macOS slots, and the `ci` run for the merge uses four, so a release
cut right after a merge can wait for macOS runners.

`release-cache` runs on `main` and fills the compile cache that `publish` reads. When it
finished before the release, `publish` had 45-68 % Rust cache hits. When the two ran at the same
time, it had 0-20 %. It has no Windows legs, because sccache is off on Windows.

## 4. Verify

```sh
scripts/release/verify-release.sh v12.1.0      # every asset there, none empty
```

The Mac `.dmg` must have no licence agreement, because the updater mounts it with nobody there
to accept one:

```sh
gh release download v12.1.0 -p '*.dmg' -D /tmp/rel
hdiutil imageinfo /tmp/rel/*.dmg | grep 'Software License Agreement'   # -> false
dev="$(hdiutil attach -nobrowse -readonly /tmp/rel/*.dmg | awk 'NR==1 {print $1}')"   # no prompt
hdiutil detach "$dev"
```

`latest.json` should have the right version and 15 platforms, each with a `sha256`:

```sh
gh release download v12.1.0 -p latest.json -O - \
  | jq '{version, assets: (.assets | length), sha256: (.sha256 | length)}'
# -> {"version": "12.1.0", "assets": 15, "sha256": 15}
```

After testing it on a console, promote it. This also moves the images' `:latest`:

```sh
scripts/release/verify-release.sh v12.1.0 --promote
```

## 5. After a release: catch `main` up

`main` still has the old version files and calls the released notes `## Unreleased`. The next
batch PR fixes that before it adds anything:

```sh
node scripts/update-version.js 12.1.0
git checkout v12.1.0 -- CHANGELOG.md   # if nothing was added under Unreleased since the release
```

If something was added since the release, rename the released part to `## 12.1.0` by hand
instead. New entries then go under a new `## Unreleased`. Until this is done, `release-tag`
refuses the next version, so the old notes cannot ship twice.

`scripts/release.sh` (tag from a local clone) still works for the owner. It pushes to `main`
through the admin bypass.

## Merge queue (not on yet)

`ci` already runs on `merge_group`, so turning the queue on needs no workflow change. Edit the
`main` ruleset and add:

- **Require merge queue**: squash, and
- **Require status checks to pass** with the single check `PR gate`.

Do not make any CodeQL check required. CodeQL does not run for queue entries or for PRs without
code, so a required CodeQL check would wait forever. `gh pr merge --admin` still merges outside
the queue.

## How CI picks jobs

On a PR or queue entry, the `changes` job maps the changed paths to lanes
(`.github/scripts/ci-changes.mjs`):

| Lane | Jobs | Changed paths |
| --- | --- | --- |
| engine | rust (engine workspace), rust tests, go live, engine targets, ava1 | `engine/**`, `tailnet/**`, `payload/include/**`, API/AUTOMATION/MCP.md |
| hub | rust hub, only when the engine lane is off | `engine/crates/ps5upload-hub/**` |
| ctest | payload C over AVA1, sanitized ctest (2 shards each) | `payload/**`, core, ava1, ctest crates |
| payload | payload ELF, host self-tests | `payload/**`, `tailnet/**`, `client/**` (console page) |
| client | client (ts + vite), client browser tests (Playwright, 3 shards) | `client/**` (src-tauri too), engine sources, `site/join`, `site/pad` |
| docs | docs: the vitest files that read the FAQ and changelog, and `e2e/bundled-docs.spec.ts`; only when the client lane is off | `CHANGELOG.md`, `FAQ.md` |
| desktop, android | rust desktop, android | `client/src-tauri/**`, engine, `tailnet/**` (android also `client/**`) |
| scripts | repo scripts on macOS and Windows | `scripts/**`, any `.js/.mjs/.sh/.py/.ps1` |

These always run: `changes`, `version sync`, `security scan`, `repo scripts` (Ubuntu) and
`PR gate`.

These run every lane: workflows, `.github/scripts`, lockfiles, `package.json`, `Makefile`,
`VERSION`, submodules, `protocol/**`, and any path without a rule.

A docs-only change (`*.md`, `site/**`, issue templates) runs only the jobs that always run.
The exceptions are Markdown that the app or engine reads. `CHANGELOG.md` and `FAQ.md` run the
small `docs` job: only the tests that read them. A PR that also changes client code runs the
full client lane instead, which includes those tests. `API.md`, `AUTOMATION.md` and `MCP.md`
are built into the engine.

Pushes to `main` and manual runs always run every lane. `PR gate` fails if a job whose lane
was on did not succeed, if a job was skipped while its lane was on, or if it finds a job it
does not know.

Outside the lanes:

- Coverage reports run nightly (the `ci` workflow's schedule) and on a manual run, not on every
  push to `main`. The nightly run makes the reports and runs nothing else; the release check
  (`release-ci-verdict.mjs`) ignores it.
- CodeQL analyses Rust on a PR only when the PR changes `*.rs`, a `Cargo.toml`/`Cargo.lock`,
  the CodeQL config or its workflow. Pushes to `main`, the weekly run and manual runs analyse
  all four languages.
- The Docker image smoke-check (`docker-engine.yml`: verify, verify-webui) runs on every push to
  `main`, nightly, and on a manual run with no tag. On a PR it runs only when the Dockerfiles,
  their `.dockerignore`, `scripts/docker-smoke.sh`, the workflow or the engine's Cargo
  manifests change. Tag pushes and tagged dispatches publish exactly as before.

## CI caches

GitHub keeps 10 GiB of Actions cache for the repository and drops the least recently used
entries above that. Until 12.6.4 every push to `main` saved a new set of `ci` caches (the key
held the run id: Android ~1.8 GB, desktop 3 x ~1 GB, the test legs 1.3-3.3 GB), which pushed
out everything else within hours, so `release-cache`'s compile cache rarely reached `publish`.

The rules now:

- A cache key names what the cache holds, never the run. Cargo caches:
  `<OS>-<job>-rust<version>-<commit>-<deps>`, where `<deps>` hashes only the registry and git
  crates in `Cargo.lock` (`.github/scripts/cargo-cache.mjs key`), so a release's version bump
  keeps the key. Gradle: the `@tauri-apps/cli` version. The converter image: its Dockerfile and
  SDK pin. `restore-keys` fall back to the same compiler with other dependencies, never to an
  older compiler (those objects are useless and would only grow the cache).
- Only a push to `main` saves, and only when the exact key missed. PRs, the merge queue and tag
  runs restore and save nothing (a tag's or PR's cache is visible to that ref alone).
  `release-cache` saves the registry and, through sccache, the release build's objects, from
  `main` only. BuildKit's `type=gha` cache in `docker-engine` is written from `main` only.
- Before a save, `cargo-cache.mjs prune` removes our own crates' build output (test binaries
  included) and `target/tmp`. Cargo rebuilds path crates on every CI run anyway, so the cache
  holds dependencies only.
- `rust-ctest` and the sanitizer job save from shard 1 only.

See the pool with:

```sh
gh api repos/phantomptr/ps5upload/actions/cache/usage
gh cache list --limit 200 --sort size_in_bytes
```

One-time clean-up after this change is on `main`: delete the old run-keyed and lockfile-hash
entries (they can no longer be hit by an exact key and only hold space). The filter keeps
sccache, Node, BuildKit and the converter image:

```sh
gh cache list --limit 5000 --json id,key \
  --jq '.[] | select((.key | test("^(Linux|macOS|Windows)-.*-[0-9a-f]{64}(-[0-9]+)?$"))
                     and (.key | test("convert-toolchain") | not)) | .id' \
  | xargs -n1 gh cache delete
```

Dropping every entry (`gh cache delete --all`) also works; the next push to `main` and the next
`release-cache` run fill it again.

## Nightly flake report

`flake-hunter.yml` runs nightly and repeats the timing-sensitive tests: the hub, `join_net` and
`server_out`, the tailnet node self-test, and `agent_mcp`. It runs each set 10 times on one Linux
runner and posts failures to the open issue "Nightly flake report". It never gates anything.
