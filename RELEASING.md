# Releasing

A release is one batch PR and one workflow run. There is no version-bump PR: the version comes
from the input to `release-tag`, which tags a bump commit that never goes to `main`.

## 1. The batch PR

- Put the release's changes in one PR.
- Write the CHANGELOG entry in the same PR under `## Unreleased`, at the top of
  `CHANGELOG.md` (above the last release's `## X.Y.Z`, with the usual `---` between sections).
  Do not edit `VERSION` or the version files.
- Wait for `PR gate`. It is the only check that matters for merging. CodeQL runs beside it
  and reports to the Security tab, but nothing waits for it.
- Merge with the admin bypass. Read the checks first and merge only when the read is non-empty,
  shows `pass` and has no `pending` or `fail`, because `--admin` does not wait:

  ```sh
  gh pr checks <n>
  gh pr merge <n> --squash --admin
  ```

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
   passed (`.github/scripts/release-ci-verdict.mjs`). After an up-to-date batch PR, that is
   immediate. In an emergency, `-f force=true` skips the wait.
5. Commits `12.1.0` as `github-actions[bot]`, tags it `v12.1.0`, and pushes only the tag. The
   main ruleset lets only an admin update `main`, so the bump commit lives on the tag.
6. Dispatches `publish` and `docker-engine` at the tag. Both build the tagged commit, so the
   app, the engine, `latest.json` and the images all say `12.1.0`.

## 3. Watch

```sh
gh run list --workflow publish.yml --limit 1
gh run list --workflow docker-engine.yml --limit 1
gh run watch <id>
```

`publish` takes about 25 minutes. Windows ARM64 is usually the last job to finish.
`publish-retry` re-runs failed jobs up to twice.

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
| ctest | payload C over AVA1, sanitized ctest | `payload/**`, core, ava1, ctest crates |
| payload | payload ELF, host self-tests | `payload/**`, `tailnet/**`, `client/**` (console page) |
| client | client (ts + vite) | `client/**` (src-tauri too), engine sources, `CHANGELOG.md`, `FAQ.md`, `site/join`, `site/pad` |
| desktop, android | rust desktop, android | `client/src-tauri/**`, engine, `tailnet/**` (android also `client/**`) |
| scripts | repo scripts on macOS and Windows | `scripts/**`, any `.js/.mjs/.sh/.py/.ps1` |

These always run: `changes`, `version sync`, `security scan`, `repo scripts` (Ubuntu) and
`PR gate`.

These run every lane: workflows, `.github/scripts`, lockfiles, `package.json`, `Makefile`,
`VERSION`, submodules, `protocol/**`, and any path without a rule.

A docs-only change (`*.md`, `site/**`, issue templates) runs only the jobs that always run.
The exceptions are Markdown that the app or engine reads. `CHANGELOG.md` and `FAQ.md` also run
the client job, because its tests read them. `API.md`, `AUTOMATION.md` and `MCP.md` are built
into the engine.

Pushes to `main` and manual runs always run every lane. `PR gate` fails if a job whose lane
was on did not succeed, if a job was skipped while its lane was on, or if it finds a job it
does not know.

## Nightly flake report

`flake-hunter.yml` runs nightly and repeats the timing-sensitive tests: the hub, `join_net` and
`server_out`, the tailnet node self-test, and `agent_mcp`. It runs each set 10 times on one Linux
runner and posts failures to the open issue "Nightly flake report". It never gates anything.
