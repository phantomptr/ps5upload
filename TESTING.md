# Testing

Four layers, cheapest first. Push a check down to the cheapest layer that
can hold it — a host self-test runs in milliseconds and cannot wedge a
console, which a hardware test very much can.

| Layer | Command | Needs |
|---|---|---|
| 1. Host gate | `npm run validate` | nothing |
| 2. Payload build + self-tests | `npm run validate:full` | `PS5_PAYLOAD_SDK` |
| 3. Live console | `npm run validate:hardware` | a PS5 with the payload loaded |
| 4. Fresh-install matrix | manual, per release | clean VMs |

## Layer 1 — the host gate

```sh
npm run validate      # or: make quality
```

Runs, and fails on any of: version drift against `VERSION`; script syntax
(Node, Bash, Python, and PowerShell when `pwsh` is present); the script
inventory audit; `git whitespace` (`git diff --check`); **engine fmt**,
**engine clippy** (`-D warnings`) and **engine tests**; **desktop cargo
check**, **desktop clippy** and **desktop tests**; **client typecheck**,
**client lint**, **client tests** and **client vite build**; and **i18n
coverage**.

Two of these catch things the others structurally cannot, and both have
caught real regressions:

- **`engine fmt`** fails on formatting no test exercises. Run
  `cargo fmt --all` in `engine/` before assuming a failure is real.
- **`client typecheck`** covers test files too. Vitest does not typecheck,
  so a test can pass while calling a function with the wrong argument
  types.

Running a narrower check and assuming the wider one follows is the most
common way to burn a cycle here.

## Layer 2 — payload build and self-tests

```sh
export PS5_PAYLOAD_SDK=/opt/ps5-payload-sdk
npm run validate:full          # or: make quality-full / make test-payload
```

Builds `payload/ps5upload.elf` and the DPI daemon, checks both are PS5
ELFs with valid gzip resources, then runs every host self-test with
`-Wall -Wextra -Werror`:

| Self-test | Pins |
|---|---|
| `hw_guard` | Recovering from a faulting Sony getter without losing the helper |
| `ptrace_recovery` | Timeout recovery never resuming injected registers |
| `timed_init` | Bounded one-time init never starting a second initializer |
| `appdb_scan` | Reading `app.db` as real SQLite records, not printable runs |
| `ftp_format` | PASV/EPSV/LIST reply shapes clients parse strictly |
| `ftp_lifecycle` | Session registration, generations, stop-to-kill |
| `sdk_param` | `param.json` rewrite, and reporting when nothing changed |
| `elf_param` | Finding SDK fields via program headers, not stray magic bytes |
| `sdk_changer_file` | Patching only tracked sources; durable backups |

The SDK version is pinned in `scripts/ps5-sdk.env` (currently **v0.42**)
and verified by checksum. Local installers and CI read the same file.

**Adding payload logic?** If it can be separated from the console, put it
header-only in `payload/include/` and give it a self-test here. That is
the established convention and the reason the list above exists.

## Layer 3 — live console

Nothing here is tied to one network. Every entry point takes an address:

```sh
export PS5_HOST=192.168.1.50            # console IP (loader + payload)
export PS5_ADDR=192.168.1.50:9113       # transfer port, for scripts/bench

make send-payload PS5_HOST=$PS5_HOST
npm run smoke:hardware                  # or: node tests/smoke-hardware.mjs
npm run validate:hardware               # host gate + make validate
```

Keep your own addresses in a gitignored local file rather than editing
committed defaults — those stay generic on purpose.

| Command | Does |
|---|---|
| `make validate` | Build, send, wait for `:9113`, smoke, sweep, write `bench/reports/<ts>-sweep.{json,md}` |
| `make validate-xl` | Adds the 200k-file stress profile |
| `node tests/smoke-hardware.mjs --no-spawn-engine` | Against an engine already running |

Clean up test uploads afterwards:

```sh
curl -X POST http://127.0.0.1:19113/api/ps5/cleanup \
  -H 'content-type: application/json' \
  -d '{"addr":"'"$PS5_HOST"':9114","path":"/data/ps5upload/tests/manual"}'
```

**Use a live console when touching:** the payload C runtime, FTX2
framing, transfer/reconcile/resume, storage, mount, cleanup, the file
browser, volume commands, or anything performance-sensitive.

### Hardware testing has teeth

A payload bug can leave a console needing a **power cycle** — ports stay
open but every connection resets, so payload takeover cannot recover it.
That has happened, from a stack overflow in a file-transfer handler.
Before testing a payload change:

- Prefer a read-only probe first; confirm the payload still answers
  `/api/ps5/status` between steps.
- Some reads (temperatures, power telemetry) ptrace-pause the system UI.
  They are safe on demand and unsafe on a loop.
- Expect blanks that are not bugs: SoC clock, SoC power, CPU usage, fan
  duty, per-drive sensors and the console date/time are not exposed by
  retail firmware.

## Layer 4 — fresh-install matrix, per release

CI proves the binaries build. It does not prove they launch on a machine
that has never been a dev box. Every artifact has a historical
"ships green, fails on fresh install" failure:

| Platform | Failure caught before | Guard now in tree |
|---|---|---|
| Windows x64 / arm64 | `VCRUNTIME140.dll not found` | `.cargo/config.toml` sets `+crt-static` |
| Windows x64 / arm64 | Explorer "Extract All" rejects the zip | `publish.yml` packs via `pwsh Compress-Archive` |
| Linux x64 / arm64 | AppImage needs FUSE | ships `PS5Upload.sh` with `APPIMAGE_EXTRACT_AND_RUN=1` |
| Linux (any) | glibc too new | built on `ubuntu-24.04` (glibc 2.39) |
| macOS x64 / arm64 | Gatekeeper on first run | documented; right-click → Open |

Before tagging: unzip with the OS's own tool and launch on a clean
Windows x64 VM, Windows arm64, Ubuntu 24.04, arm64 Linux, macOS arm64 and
macOS x64. Sideload the APK on a real Android device.

See `v2.5.1` and `v2.5.2` for the "noticed after release, patched same
day" playbook.

## Coverage reports

```sh
npm run coverage            # or: make coverage
make coverage-client        # one side only
make coverage-engine
```

Outputs `client/coverage/index.html`, `coverage/engine/html/index.html`
and `coverage/engine/lcov.info`. Rust coverage needs `cargo-llvm-cov`
(`npm run coverage -- --install-tools`).

## Cross-platform

CI checks the core crate for every shipped target
(`x86_64`/`aarch64` × linux-gnu, apple-darwin, pc-windows-msvc) and runs
the desktop Rust suite natively on `ubuntu-24.04`, `macos-14` and
`windows-2022` — that native matrix is what exercises the
`#[cfg(target_os = ...)]` paths (keep-awake, update/download handling,
launcher, sidecar extraction). A macOS dev box generally cannot build the
Windows/Linux targets, so CI is the source of truth there.

## Android

```sh
make android-deps      # verify SDK / JDK 17 / NDK / rustup target
make android-build     # debug APK, no device needed
make android-deploy    # build + install on connected devices
make run-android       # run on a device with live reload
```

Android links the engine **in-process** rather than spawning a sidecar,
so engine startup failures surface differently — check the in-app log.
`.rar` is desktop-only (the UnRAR C dependency is excluded).

## Self-hosted engine

```sh
make docker-engine        # build the image
make docker-engine-run    # run it; exposes :19113
```

CI publishes the same image to GHCR on every release tag. The API is
**unauthenticated and can read, write and delete files on the console** —
LAN only, never the internet.

## i18n

```sh
npm run i18n:check        # gate: fails on any non-allowlisted miss
npm run i18n:report       # same, always prints the per-language summary
```

`client/src/i18n/locales/en.ts` is the source of truth. A new `tr()` key
must exist there or the gate fails. Then either translate it into the 18
locales or add it to `scripts/i18n-known-missing.json` as a deliberate,
scoped deferral.

**Do not run `i18n:bootstrap`.** It rewrites the allowlist to the current
state, silencing every gap at once — including whatever you were about to
forget.

## Script hygiene

```sh
npm run scripts:check     # syntax
npm run scripts:audit     # inventory
```

A lab or debug script with no caller is intentional, not dead code. Only
remove generated artifacts, or scripts whose replacement is documented.

## Multi-volume `.rar`

Multi-part archives work in both debug and release builds.

This needed a fix: upstream `unrar 0.5.8` reads ~8 KB out of a much
smaller buffer every time it crosses a volume boundary, which aborted any
debug build that touched a multi-part `.rar` and was a latent
out-of-bounds read in release. `0.5.8` is the latest release, so the crate
is vendored at `third_party/unrar` with the fix and both workspaces
redirect to it via `[patch.crates-io]`.

See `third_party/unrar/README.md` for the patch and
`docs/unrar-upstream-bug.md` for the report to send upstream. If you bump
the crate, re-apply the hunk or drop the vendoring once upstream ships the
fix.

The shipped `.rar` fixtures are all single-volume, so they never exercise
this — verifying it needs a real multi-part archive.
