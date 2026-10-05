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
make build       # payload ELF + Rust engine + client UI
make run-client  # launch the Tauri dev app
```

See the [README](README.md) and [TESTING.md](TESTING.md) for the full
toolchain (PS5 Payload SDK, Rust, Node 22, Tauri prerequisites).

## Quality gate: run before opening a PR

CI runs the same checks; running them locally first saves a round-trip.
`npm run validate` runs most of it in one command (version sync, script checks, i18n,
engine and desktop fmt/clippy/tests, client typecheck/lint/tests/build). The complete gate is
these, and `cargo fmt --check` must pass in BOTH `engine/` and `client/src-tauri`:

```bash
( cd engine && cargo fmt --all -- --check )       # fmt in BOTH workspaces
( cd client/src-tauri && cargo fmt --all -- --check )
( cd engine && cargo clippy --workspace -- -D warnings )
( cd engine && cargo test --workspace )
( cd engine && cargo test -p ava1-ctest -- --test-threads=1 )
make test-payload                                 # needs PS5_PAYLOAD_SDK
make check-no-ftx2                                # the retired protocol must stay gone
( cd client && npm run lint && npm run typecheck && npm test && npm run build:vite )
npm run i18n:check                                # from the repo root
```

`ava1-ctest` builds the payload's AVA1 C on the host and tests it against the Rust side, so no
PS5 is needed. `check-no-ftx2` fails if the old protocol name or ports 9113/9114 reappear
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
- By contributing, you agree your work is licensed under the project's
  [GPLv3](LICENSE).

## Questions

Open a [GitHub issue](https://github.com/phantomptr/ps5upload/issues) or ask
in the [Discord](https://discord.gg/fzK3xddtrM).
