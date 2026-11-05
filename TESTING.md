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
(Node, Bash, Python, and PowerShell when `pwsh` is present); the Linux launcher,
Sony API lock and per-console isolation checks (skipped on Windows); the script
inventory audit (`--strict`, as CI); `git whitespace` (`git diff --check`);
**engine fmt**, **engine clippy** (`--locked -D warnings`) and **engine tests**;
**desktop fmt**, **desktop clippy** (`--all-targets --locked`, which type-checks
everything a `cargo check` would) and **desktop tests**; **client typecheck**,
**client lint**, **client tests**, **client vite build** and the **embedded guest
and pad pages** (rebuilt and compared with the committed files); and **i18n coverage**.

Two of these catch things the others structurally cannot, and both have
caught real regressions:

- **`engine fmt`** fails on formatting no test exercises. Run
  `cargo fmt --all` in `engine/` before assuming a failure is real.
- **`client typecheck`** covers test files too. Vitest does not typecheck,
  so a test can pass while calling a function with the wrong argument
  types.

Running a narrower check and assuming the wider one follows is the most
common way to burn a cycle here.

`npm run validate` is not the whole gate. Before a PR also run:

```sh
( cd client/src-tauri && cargo fmt --all -- --check )   # validate only checks fmt in engine/
( cd engine && cargo test -p ps5upload-ctest -- --test-threads=1 )
make test-payload                                       # needs PS5_PAYLOAD_SDK
make check-no-ftx2                                      # the retired protocol must stay gone
```

## Layer 2 — payload build and self-tests

```sh
export PS5_PAYLOAD_SDK=/opt/ps5-payload-sdk
npm run validate:full          # or: make quality-full / make test-payload
```

Builds `payload/ps5upload.elf` and the installer, checks both are PS5
ELFs with valid gzip resources, then runs every host self-test with
`-Wall -Wextra -Werror`.

Each self-test lives in `payload/tests/*_selftest.c` (hardware guards, ptrace recovery,
`app.db` reading, SDK param rewriting, installer, cheats, wake watchdog and more).
The payload's AVA1 code (the management methods, the glue) is tested by `ps5upload-ctest`, which
compiles it with the AVA1 C library on the host and checks it against the Rust side (see the full
gate below). The AVA1 library itself is tested in its own repository (`third_party/ava1`:
`make -C third_party/ava1 test`).

The SDK version is pinned in `scripts/ps5-sdk.env` (currently **v0.45**)
and verified by checksum. Local installers and CI read the same file.

To move to a newer SDK release, run `make update-sdk` (or
`scripts/update-ps5-sdk.sh [--tag vX.Y] [--check]`). It downloads the release,
rewrites the pin and its checksum, refreshes the vendored `prospero-nid` and
`payload/third_party/sdk-crt` if upstream changed them at that tag, and installs
the SDK into `$PS5_PAYLOAD_SDK` (default `/opt/ps5-payload-sdk`), keeping the old
one as a `.backup-<old tag>-<time>` directory. If `/opt` needs root it uses
`sudo` from a terminal; without it, the backup goes to your home directory.
Then `rm -rf payload/build payload/installer/build payload/third_party/sdk-crt/build`,
run `make test-payload`, and commit the changed pin.

**Adding payload logic?** If it can be separated from the console, put it
header-only in `payload/include/` and give it a self-test here. That is
the established convention.

## Layer 3 — live console

Nothing here is tied to one network. Every entry point takes an address:

```sh
export PS5_HOST=192.168.1.50            # console IP (loader + payload)
export PS5_ADDR=192.168.1.50            # console IP; the engine owns the port (9120)

make send-payload PS5_HOST=$PS5_HOST
npm run smoke:hardware                  # or: node tests/smoke-hardware.mjs
npm run validate:hardware               # host gate + make validate
```

Keep your own addresses in a gitignored local file rather than editing
committed defaults — those stay generic on purpose.

| Command | Does |
|---|---|
| `make validate` | Build, send, wait for `:9120`, smoke, sweep, write `bench/reports/<ts>-sweep.{json,md}` |
| `make validate-xl` | Adds the 200k-file stress profile |
| `node tests/smoke-hardware.mjs --no-spawn-engine` | Against an engine already running (started with `PS5UPLOAD_ENGINE_DEBUG=1`, or its cleanup step answers 404) |

Clean up test uploads afterwards. This route answers only on an engine started with `PS5UPLOAD_ENGINE_DEBUG=1`, which the test scripts do:

```sh
curl -X POST http://127.0.0.1:19113/api/ps5/cleanup \
  -H 'content-type: application/json' \
  -d '{"addr":"'"$PS5_HOST"'","path":"/data/ps5upload/tests/manual"}'
```

**Use a live console when touching:** the payload C runtime, AVA1
(`third_party/ava1/c/`), transfer/reconcile/resume, storage, mount, cleanup, the file
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
- Expect blanks that are not bugs: not every firmware answers every sensor
  read (SoC power, clocks, per-drive sensors), and the helper has to be new enough to send each one.

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

## Browser tests

```sh
cd client && npm run build:vite && npx playwright test   # or name a spec file
```

The suite starts its own servers on ports 14470-14472 (`PS5UPLOAD_VITE_PORT` moves them), so a dev
server on 1420 is left alone. In CI a test that passes only on its retry fails the run; the job
uploads the traces of what failed as `playwright-results`.

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
the desktop Rust suite natively on `ubuntu-24.04`, `macos-15` and
`windows-2022` — that native matrix is what exercises the
`#[cfg(target_os = ...)]` paths (keep-awake, update/download handling,
launcher, sidecar extraction). A macOS dev box generally cannot build the
Windows/Linux targets, so CI is the source of truth there.

## Android

```sh
make android-deps      # verify SDK / JDK 17 / NDK / rustup target
make android-build     # debug APK, no device needed
make android-deploy    # build + install on connected devices
make run-android       # run on the emulator (or a plugged-in phone) with live reload
```

`make run-android` boots a Pixel 10 Pro XL emulator with its window when no device is attached
(`PS5UPLOAD_AVD_DEVICE=pixel_9_pro` picks another profile; each gets its own AVD) and shuts it
down when the run ends, if it booted it.

Android links the engine **in-process** rather than spawning a sidecar,
so engine startup failures surface differently — check the in-app log.
`.rar` is desktop-only (the UnRAR C dependency is excluded).

## iPhone

```sh
make run-ios           # run in the iPhone Simulator with live reload (macOS + Xcode)
make ios-build         # unsigned .ipa for a real iPhone (sideload it, see the FAQ)
```

`make run-ios` uses a booted iPhone simulator, or boots the newest Pro Max the installed iOS
runtimes support (`PS5UPLOAD_IOS_SIM="iPhone 17 Pro"` picks another), and shuts it down at the
end if it booted it. Xcode needs its iOS platform once: `xcodebuild -downloadPlatform iOS`.
The simulator shows how the app looks and works and can reach a PS5 on this Mac's network; the
limits of a real iPhone (background suspension, the local-network prompt) only show on one. As on
Android the engine runs in-process and `.rar` isn't available: the RAR helper is a separate program,
which a phone app can't start.

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
must exist there or the gate fails. Then either translate it into the 20
other locales (21 locale files in all) or add it to `scripts/i18n-known-missing.json` as a deliberate,
scoped deferral.

When you change the English of an existing key, retranslate it too. `i18n:check` compares against
`scripts/i18n-source-hashes.json` and fails when English moved but a translation did not. After
retranslating run `npm run i18n:hashes`; if a translation is still right (only the case changed, say)
run `npm run i18n:ack -- <key>`. The site's strings are covered the same way.

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
is vendored at `third_party/unrar` with the fix and `engine/Cargo.toml`
redirects to it via `[patch.crates-io]` (the desktop app gets it through the engine's helper).

See `third_party/unrar/README.md` for the patch. If you bump
the crate, re-apply the hunk or drop the vendoring once upstream ships the
fix.

The shipped `.rar` fixtures are all single-volume, so they never exercise
this — verifying it needs a real multi-part archive.
