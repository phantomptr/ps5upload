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
   the PR template (`.github/PULL_REQUEST_TEMPLATE.md`).

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

## Quality gate — run before opening a PR

CI runs the same checks; running them locally first saves a round-trip:

```bash
npm run validate        # lints + unit/integration tests + typecheck + i18n + build
```

`npm run validate` covers, across all three layers:

- **Lints** — `cargo fmt`/`clippy` (engine + Tauri shell), ESLint, `tsc`,
  script syntax/audit, and the i18n coverage gate.
- **Unit + integration tests** — `cargo test --workspace` (protocol, core,
  engine, pkg, and the mock-FTX2 integration tests — no PS5 required),
  the Tauri shell tests, and the client Vitest suite.

Other useful targets:

```bash
make test               # script + engine + payload + client checks
npm run validate:full   # adds the payload ELF build (needs PS5_PAYLOAD_SDK)
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
