# The converter's engine API and screen (Plan 6) — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** a user can point the app at a game folder or a mount image, see what the engine makes of it, convert it into an installable FPKG, and hand the result to the stream installer — on desktop, Docker and Android, with the console untouched until the install.

**Status 2026-09-13: implemented and exercised** (branch `feat/fpkg-builder`, commits `1993df3c` … `774032ed`). The engine serves `POST /api/fpkg/inspect` and `POST /api/fpkg/build`; the client has a **Convert to FPKG** screen behind `/convert`.

## What was built

| Piece | Where |
|---|---|
| `build::inspect(source, output_dir)` — source kind, readiness checks, content id, title, firmware, estimated size, free space | `engine/crates/ps5upload-fpkg/src/build.rs` |
| `POST /api/fpkg/inspect` — the same, as JSON | `engine/crates/ps5upload-engine/src/fpkg_api.rs` |
| `POST /api/fpkg/build` — a job on the transfer infrastructure: 200 ms byte-progress ticker, `/api/jobs/{id}/cancel` stops it per block, `done` carries the package path; output defaults to `~/Downloads/fpkgs` | same |
| `fpkg_inspect` / `fpkg_build` Tauri commands + browser shims | `client/src-tauri/src/commands/ps5_engine.rs`, `client/src/lib/browserInvoke.ts` |
| `client/src/api/fpkg.ts` — the typed wrappers | client |
| The screen: source + output fields, `pickPath` for desktop/Android, an inspect panel (title, sizes, free space, warnings, the passing checks), a progress bar with cancel, then **Install on the console** via `installStream` | `client/src/screens/FpkgConvert/index.tsx` |
| Nav entry + route + 30 English keys | `client/src/layout/navItems.ts`, `App.tsx`, `i18n/locales/en.ts` |

## Evidence

- **Over HTTP**: `inspect` on the committed exFAT fixture returned the full report; `build` reported progress and finished in 2 s, writing the same 2,319,261 bytes the CLI produces.
- **A real 27.8 GiB mount** (`PPSA09519.exfat`): inspected as *"WUCHANG: Fallen Feathers"*, firmware 11.60, 95 files, 27.84 GiB package, 36.94 GiB free, **0 warnings** — every readiness check passes on a real mount.
- **In a browser** (Playwright against the built UI served with `/api` proxied to the engine): the check panel rendered the title, 6 files, a 4.2 MiB package and "everything the package needs is here"; the conversion finished; the package landed in `~/Downloads/fpkgs` at the same size as the CLI's.
- Client gates: `tsc` clean, `eslint` clean, `vitest` 1,425 tests / 129 files pass (including the browser-invoke coverage, nav and i18n gates). `cargo check` clean for the engine and the Tauri crate.

## Deliberately not in this plan

- **A phase label in the job state** ("writing the image" vs "verifying"): `JobState::Running` is constructed in 17 places, so the phase lines go to the engine log instead; the screen shows a percentage.
- **The web build browsing the engine's disk**: the browser has no native dialog, so the screen takes a typed path there. Wiring the existing `LocalPathPicker` into this screen is the follow-up.
- **Key-material import, firmware override, output-folder picking**: the keys are built in (v1 decision) and the firmware override has no verified use yet.
