# Contributing to ps5upload

Thanks for your interest in improving ps5upload! Contributions come in via
the standard **fork + pull request** flow — the repository doesn't grant
direct push access, so everyone (including regulars) proposes changes the
same way.

## How to contribute

1. **Fork** the repo to your own account.
2. **Branch** off `main` in your fork: `git checkout -b my-change`.
3. Make your change, keeping it focused — one logical change per PR.
4. **Run the quality gate locally** (see below) until it's green.
5. Open a **pull request** against `phantomptr/ps5upload:main` and fill out
   the PR template.

Every PR is gated by CI (the **`PR gate`** check) and reviewed by the code
owner before it can merge. PRs are squash-merged, so your branch becomes a
single clean commit on `main`.

## Dev setup

```bash
make install     # bootstrap the dev env (auto-detects macOS / Linux / Windows)
export PS5_PAYLOAD_SDK="$HOME/ps5-payload-sdk"   # where it put the SDK; `make build` needs this
make build       # payload ELF + Rust engine + client UI
make run-client  # launch the Tauri dev app
```

See the [README](README.md) and [TESTING.md](TESTING.md) for the full
toolchain (PS5 Payload SDK, Rust, Node 22, Tauri prerequisites).

The PS5 Payload SDK release is pinned in `scripts/ps5-sdk.env`. `make update-sdk`
moves the pin to the latest upstream release and installs it (`SDK_TAG=vX.Y` for a
specific one, `SDK_CHECK=1` to only check); see TESTING.md, Layer 2.

## Quality gate: run before opening a PR

CI runs the same checks, including the host-side payload self-tests (nothing in CI runs on a
console); running them locally first saves a round-trip. [TESTING.md](TESTING.md) lists what
`npm run validate` does.

```bash
npm run validate      # version sync, script checks, i18n, engine and desktop checks, client checks
( cd client/src-tauri && cargo fmt --all -- --check )
make test-payload     # needs PS5_PAYLOAD_SDK
make check-no-ftx2    # the retired protocol must stay gone
```

To run one engine lane by hand, leave `ps5upload-ctest` out of the workspace run and run it on its
own: its C server is one per process, so it needs a single test thread.

```bash
( cd engine && cargo test --workspace --exclude ps5upload-ctest )
( cd engine && cargo test -p ps5upload-ctest -- --test-threads=1 )
```

`ps5upload-ctest` builds the payload's AVA1 C (the `third_party/ava1` library plus the payload's
management methods) on the host and tests it against the Rust side, so no PS5 is needed. A change
to the protocol itself goes to [github.com/phantomptr/ava1](https://github.com/phantomptr/ava1)
first and is released there; then the submodule is moved to the new tag here and the exact `ava1`
version in `engine/Cargo.toml` and `client/src-tauri/Cargo.toml` with it (`make ava1-version-check`
fails until both match). `check-no-ftx2` fails if the old protocol name or ports 9113/9114 reappear
outside the changelog and a short list of exceptions in the Makefile.

Other useful targets:

```bash
make test               # script + engine + payload + client checks
npm run validate:full   # adds the payload ELF build and self-tests
npm run coverage        # frontend + Rust coverage reports
```

If your change touches user-facing strings, add the key to
`client/src/i18n/locales/en.ts`; `npm run i18n:check` will tell you if a
locale needs an allowlist entry.

## Guidelines

- Match the surrounding code's style, naming, and comment density.
- Keep PRs small and reviewable; unrelated cleanups belong in their own PR.
- Hardware-specific behavior (anything that talks to a real PS5) can't be
  verified in CI — call out in your PR how you tested it on hardware.
- A new or changed engine route or request struct changes the API's description: run
  `( cd engine && cargo run -p ps5upload-apidoc )` and commit `api/openapi.json`. CI fails otherwise.
- By contributing, you agree your work is licensed under the project's
  [GPLv3](LICENSE).

## Questions

Open a [GitHub issue](https://github.com/phantomptr/ps5upload/issues) or ask
in the [Discord](https://discord.gg/fzK3xddtrM).
