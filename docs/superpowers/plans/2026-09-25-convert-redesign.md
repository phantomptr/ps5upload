# Convert to FPKG Redesign Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Rebuild the Convert to FPKG screen as three guided cards (Game, Options, Build & install) with drag-and-drop, remembered choices, automatic minimum firmware, per-game compression estimates, stage-by-stage progress for convert and install, and a result card whose actions always target the right package.

**Architecture:** The fpkg crate reports a typed build `Stage` and computes per-level estimates; the engine publishes the stage on the job, derives the minimum firmware from module SDK pairs (it links `ps5upload-core`, which already reads them), and adds a guarded delete endpoint. The client replaces `state/fpkgConversion.ts` with a pipeline state machine that chains the build job into the existing `installStream`, and the screen is split into focused card components driven by a pure stage-row model.

**Tech Stack:** Rust (axum engine, `ps5upload-fpkg`, `ps5upload-core`), React + TypeScript + zustand (client), Vitest, Tauri 2 commands, cargo test.

**Spec:** `docs/superpowers/specs/2026-09-25-convert-redesign-design.md`

## Global Constraints

- Layout: one column of three numbered cards — ① Game, ② Options, ③ Build & install (spec "Layout").
- Compression tiles: ⚡ Fast, ⚖️ Balanced (recommended, default), 🗜️ Smallest, each showing estimated size and time for this game.
- Build stages in order: `check`, `plan`, `compress`, `write`, `verify`; install stages: `send`, `install`.
- The converted `.pkg` is kept after install; **Delete package** only removes a `.pkg` this engine process built.
- Minimum firmware is automatic; never higher than the game's declared `requiredSystemSoftwareVersion`; unknown/unreadable module pairs keep the declared value. The UI firmware field is removed; the API `firmware` stays as an override.
- Persisted per-viewer preferences: last output folder, last compression level (`safeGetItem`/`safeSetItem`, never throwing).
- No PS5 connected: **Convert only** enabled; **Convert & install** shows "Connect to a PS5 to install".
- i18n: every new string goes into `client/src/i18n/locales/en.ts` (lines at column 0, never prettier-formatted) and its key into every locale's `missing` list in `scripts/i18n-known-missing.json`; `npm run i18n:check` (repo root) must pass.
- Rust: `cargo fmt` after every edit; `cargo clippy -p <crate> --all-targets` clean.
- Do NOT edit `client/src-tauri/` while a console install is running through the dev app (it restarts the engine and kills the install).

## Review Focus

1. **A drop or path change while a job runs** → ignored (cards ① ② locked); the running job and its result must not be reset. Test in Task 9.
2. **The engine restarts or the job vanishes mid-convert** → the stage list must end in *failed* with "The engine stopped responding; the conversion did not finish", not spin forever. Test in Task 9.
3. **The console changes between convert and install** (user switches host) → the install goes to the host current at the moment install starts, and the result names that host. Test in Task 9.
4. **A remembered output folder that no longer exists** (unplugged drive) → the screen still loads, the folder is shown, and the check reports the problem instead of crashing; a missing-but-creatable folder is still accepted. Test in Task 7 (prefs load) and Task 3 (engine inspect with a missing output dir).
5. **A module whose SDK pair is unreadable** (encrypted retail eboot, stripped header) → minimum firmware falls back to the declared value, never lower. Test in Task 3.

---

## File Structure

Engine / Rust:
- Modify `engine/crates/ps5upload-fpkg/src/build.rs` — `Stage` enum, `BuildControl.stage`, stage calls, `estimate()`.
- Modify `engine/crates/ps5upload-fpkg/src/stream.rs` — `Progress.stage`, stage calls.
- Create `engine/crates/ps5upload-engine/src/fpkg_firmware.rs` — minimum firmware from module SDK pairs.
- Modify `engine/crates/ps5upload-engine/src/fpkg_api.rs` — stage on the job, inspect response (min firmware + estimates), default firmware, delete endpoint, built-package registry.
- Modify `engine/crates/ps5upload-engine/src/lib.rs` — `JobStage`, `JobState::Running.stage`, route `/api/fpkg/delete`, `mod fpkg_firmware`.

Client:
- Modify `client/src/api/fpkg.ts`, `client/src/api/ps5.ts` (JobSnapshot.stage), `client/src/lib/browserInvoke.ts`, `client/src-tauri/src/commands/ps5_engine.rs`, `client/src-tauri/src/lib.rs`.
- Create `client/src/state/convertPrefs.ts` (+ test).
- Modify `client/src/state/pkgLibrary.ts` — `installStream` `opts.onTask`.
- Replace `client/src/state/fpkgConversion.ts` (+ create test) — pipeline store.
- Create `client/src/screens/FpkgConvert/stages.ts` (+ test) — pure stage-row model.
- Create `client/src/lib/useWebviewDrop.ts` — desktop drag-and-drop hook.
- Create `client/src/screens/FpkgConvert/GameCard.tsx`, `OptionsCard.tsx`, `CompressionTiles.tsx`, `RunCard.tsx`; rewrite `client/src/screens/FpkgConvert/index.tsx`.
- Modify `client/src/i18n/locales/en.ts`, `scripts/i18n-known-missing.json`.

---

### Task 1: Build stages in the fpkg crate

**Files:**
- Modify: `engine/crates/ps5upload-fpkg/src/build.rs` (BuildControl ~line 162, `build_mode` ~200–730)
- Modify: `engine/crates/ps5upload-fpkg/src/stream.rs` (`Progress` line 57, phases at 188 and 263)
- Test: `engine/crates/ps5upload-fpkg/tests/writer.rs`

**Interfaces:**
- Produces: `pub enum build::Stage { Check, Plan, Compress, Write, Verify }` with `pub fn id(self) -> &'static str` (`"check"`, …) and `pub fn index(self) -> u32` (0–4), `pub const build::STAGE_COUNT: u32 = 5`; `BuildControl { bytes, cancel, stage: Option<&'a mut dyn FnMut(Stage)> }`; `stream::Progress { phase, bytes, stage: &'a mut dyn FnMut(Stage) }`.

- [ ] **Step 1: Write the failing test** (append to `tests/writer.rs`)

```rust
/// A build reports its stages in order, each once: what the Convert screen's stage list shows.
#[test]
fn a_build_reports_its_stages_in_order() {
    use ps5upload_fpkg::build::{BuildControl, Stage};
    let source_dir = TempDir::new("stages-src");
    let out = TempDir::new("stages-out");
    write_tree(source_dir.path());
    let mut request = BuildRequest::new(source_dir.path(), out.path());
    request.time = Some((1_700_000_000, 0));
    let mut seen = Vec::new();
    let mut on_stage = |s: Stage| seen.push(s);
    let mut control = BuildControl {
        stage: Some(&mut on_stage),
        ..BuildControl::default()
    };
    let report = build::build_controlled(&request, &mut |_| {}, &mut control).unwrap();
    assert!(report.verify.ok());
    assert_eq!(
        seen,
        vec![Stage::Check, Stage::Plan, Stage::Compress, Stage::Write, Stage::Verify]
    );
    assert_eq!(Stage::Compress.id(), "compress");
    assert_eq!(Stage::Verify.index(), 4);
}
```

- [ ] **Step 2: Run it to verify it fails**

Run: `cd engine && cargo test -p ps5upload-fpkg --test writer a_build_reports_its_stages_in_order`
Expected: compile error — no `Stage`, no field `stage`.

- [ ] **Step 3: Implement**

In `build.rs` above `pub struct BuildControl`:

```rust
/// The stages a build goes through, in order: what a caller shows as a stage list.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stage {
    /// Reading the source and its readiness checks.
    Check,
    /// Laying out the package.
    Plan,
    /// Compressing the image (a flat or stored build passes straight through).
    Compress,
    /// Writing the package file.
    Write,
    /// Reading the package back and checking it.
    Verify,
}

/// How many [`Stage`]s a build has.
pub const STAGE_COUNT: u32 = 5;

impl Stage {
    pub fn id(self) -> &'static str {
        match self {
            Stage::Check => "check",
            Stage::Plan => "plan",
            Stage::Compress => "compress",
            Stage::Write => "write",
            Stage::Verify => "verify",
        }
    }

    pub fn index(self) -> u32 {
        self as u32
    }
}
```

Add to `BuildControl`:

```rust
    /// Called as each [`Stage`] begins.
    pub stage: Option<&'a mut dyn FnMut(Stage)>,
```

In `build_mode`, add a local helper at the top and call it at the boundaries:

```rust
    let mut enter = |control: &mut BuildControl, s: Stage| {
        if let Some(f) = control.stage.as_deref_mut() {
            f(s);
        }
    };
    enter(control, Stage::Check);
```

Call `enter(control, Stage::Plan)` right before `progress(&format!("planning {}", tree.describe()));`. In the `Mode::Streaming` arm, pass the stage through `stream::Progress`:

```rust
            let mut stage = |s: Stage| {
                if let Some(f) = control.stage.as_deref_mut() {
                    f(s);
                }
            };
            let mut p = stream::Progress {
                phase: progress,
                bytes: &mut bytes,
                stage: &mut stage,
            };
```

(`bytes` and `stage` both borrow `control` mutably: take `let mut bytes_cb = control.bytes.take();` and `let mut stage_cb = control.stage.take();` before the arm and build the closures from those locals, restoring nothing afterwards — the control is not used again in this arm.) In the `Mode::InMemory` arm call `enter(control, Stage::Compress)` then `enter(control, Stage::Write)` before `progress("writing the inner image")`. Before `progress("verifying")` call `enter(control, Stage::Verify)`.

In `stream.rs` add `pub stage: &'a mut dyn FnMut(crate::build::Stage),` to `Progress`; call `(progress.stage)(crate::build::Stage::Compress)` before the `kraken` match (always — a stored build passes through it immediately), and `(progress.stage)(crate::build::Stage::Write)` before `(progress.phase)("writing the image")`.

- [ ] **Step 4: Run the tests**

Run: `cd engine && cargo fmt -p ps5upload-fpkg && cargo test -p ps5upload-fpkg`
Expected: all pass, including the new test.

- [ ] **Step 5: Commit**

```bash
git add engine/crates/ps5upload-fpkg/src/build.rs engine/crates/ps5upload-fpkg/src/stream.rs engine/crates/ps5upload-fpkg/tests/writer.rs
git commit -m "feat(fpkg): report typed build stages"
```

### Task 2: Stage on the engine job

**Files:**
- Modify: `engine/crates/ps5upload-engine/src/lib.rs` (`JobState` ~line 198 and every `JobState::Running {` literal)
- Modify: `engine/crates/ps5upload-engine/src/fpkg_api.rs` (build handler ticker ~150–215)

**Interfaces:**
- Consumes: `build::Stage`, `build::STAGE_COUNT`, `BuildControl.stage` (Task 1).
- Produces: `pub(crate) struct JobStage { pub id: String, pub index: u32, pub count: u32, pub done: u64, pub total: u64 }` (serde Serialize/Deserialize/Clone/Debug); `JobState::Running { …, #[serde(default, skip_serializing_if = "Option::is_none")] stage: Option<JobStage> }`. Wire JSON: `"stage": {"id":"compress","index":2,"count":5,"done":123,"total":456}`.

- [ ] **Step 1: Write the failing test** (in `lib.rs`'s test module that already serializes `JobState`; if none, add `mod job_stage_tests` at the end of `lib.rs`)

```rust
#[cfg(test)]
mod job_stage_tests {
    use super::*;

    #[test]
    fn a_running_job_carries_its_stage() {
        let state = JobState::Running {
            started_at_ms: 1,
            bytes_sent: 10,
            total_bytes: 100,
            files: Vec::new(),
            skipped_files: 0,
            skipped_bytes: 0,
            files_processing: 0,
            files_finalized: 0,
            files_finalizing_total: 0,
            bytes_finalized: 0,
            stage: Some(JobStage { id: "compress".into(), index: 2, count: 5, done: 7, total: 9 }),
        };
        let v = serde_json::to_value(&state).unwrap();
        assert_eq!(v["stage"]["id"], "compress");
        assert_eq!(v["stage"]["index"], 2);
        let none = JobState::Running {
            started_at_ms: 1, bytes_sent: 0, total_bytes: 0, files: Vec::new(),
            skipped_files: 0, skipped_bytes: 0, files_processing: 0, files_finalized: 0,
            files_finalizing_total: 0, bytes_finalized: 0, stage: None,
        };
        assert!(serde_json::to_value(&none).unwrap().get("stage").is_none());
    }
}
```

(Match the literal to the real field list of `Running`; the compiler lists missing fields.)

- [ ] **Step 2: Run to verify it fails**

Run: `cd engine && cargo test -p ps5upload-engine a_running_job_carries_its_stage`
Expected: compile error — no `JobStage`, no field `stage`.

- [ ] **Step 3: Implement**

Add `JobStage` next to `JobState` and the `stage` field (last in `Running`). Add `stage: None,` to every other `JobState::Running { … }` literal (`cargo build -p ps5upload-engine` lists each). In `fpkg_api.rs`'s build handler: add `let stage = Arc::new(AtomicU64::new(u64::MAX));` (stage index, `MAX` = none yet) and `let stage_base = Arc::new(AtomicU64::new(0));` is not needed — per-stage bytes are the build's own `bytes` callback values, which each stage restarts. Pass to the build:

```rust
        let stage_for_build = stage.clone();
        let mut on_stage = move |s: build::Stage| {
            stage_for_build.store(u64::from(s.index()), Ordering::Relaxed);
        };
        let mut control = BuildControl {
            bytes: Some(&mut |done, total_now| {
                bytes.store(done, Ordering::Relaxed);
                total.store(total_now, Ordering::Relaxed);
            }),
            cancel: Some(&cancel),
            stage: Some(&mut on_stage),
        };
```

In the ticker, next to `*bytes_sent = …`:

```rust
                    let s = tick_stage.load(Ordering::Relaxed);
                    *stage = (s != u64::MAX).then(|| {
                        let st = STAGES[s as usize];
                        JobStage {
                            id: st.id().to_string(),
                            index: st.index(),
                            count: build::STAGE_COUNT,
                            done: tick_bytes.load(Ordering::Relaxed),
                            total: tick_total.load(Ordering::Relaxed),
                        }
                    });
```

with `const STAGES: [build::Stage; 5] = [Check, Plan, Compress, Write, Verify];` at module scope and `stage,` added to the ticker's `Some(JobState::Running { bytes_sent, total_bytes, stage, .. })` pattern.

- [ ] **Step 4: Run tests**

Run: `cd engine && cargo fmt --all && cargo test -p ps5upload-engine && cargo clippy -p ps5upload-engine --all-targets`
Expected: pass, no warnings.

- [ ] **Step 5: Commit**

```bash
git add engine/crates/ps5upload-engine/src/lib.rs engine/crates/ps5upload-engine/src/fpkg_api.rs
git commit -m "feat(engine): a build job publishes its stage"
```

### Task 3: Minimum firmware from module SDK pairs

**Files:**
- Create: `engine/crates/ps5upload-engine/src/fpkg_firmware.rs`
- Modify: `engine/crates/ps5upload-engine/src/lib.rs` (`mod fpkg_firmware;`)
- Modify: `engine/crates/ps5upload-engine/src/fpkg_api.rs` (inspect + build handlers)

**Interfaces:**
- Consumes: `ps5upload_core::fakelibs::{param_site_from_header, sdk_pair_at, is_known_sdk_pair}`, `ps5upload_fpkg::source::{open, SourceTree}`.
- Produces: `pub(crate) fn min_firmware(tree: &mut dyn SourceTree, declared: Option<&str>) -> Option<String>` returning `"M.mm"` (e.g. `"4.00"`) only when every executable (`eboot.bin`, `sce_module/*.prx`) has a known pair and the highest is below `declared` (`"10.20"`-style, as `Inspection.required_firmware`); else `None`. Inspect JSON gains `"min_firmware": "4.00" | null`. The build handler sets `request.firmware = Some(min)` when the caller gave none.

- [ ] **Step 1: Write the failing tests** (in `fpkg_firmware.rs`)

```rust
#[cfg(test)]
mod tests {
    use super::*;

    /// A raw ELF with one PT_SCE_PROCPARAM segment whose param block carries `pair`.
    fn module(pair: Option<(u32, u32)>) -> Vec<u8> {
        let mut e = vec![0u8; 0x1000];
        e[0..4].copy_from_slice(b"\x7fELF");
        e[0x20..0x28].copy_from_slice(&0x40u64.to_le_bytes()); // e_phoff
        e[0x36..0x38].copy_from_slice(&56u16.to_le_bytes()); // e_phentsize
        e[0x38..0x3A].copy_from_slice(&1u16.to_le_bytes()); // e_phnum
        let ph = 0x40;
        e[ph..ph + 4].copy_from_slice(&0x6100_0001u32.to_le_bytes()); // PT_SCE_PROCPARAM
        e[ph + 8..ph + 16].copy_from_slice(&0x800u64.to_le_bytes()); // p_offset
        e[ph + 0x20..ph + 0x28].copy_from_slice(&0x40u64.to_le_bytes()); // p_filesz
        if let Some((ps4, ps5)) = pair {
            e[0x808..0x80C].copy_from_slice(&0x4942_524Fu32.to_le_bytes()); // "ORBI" magic
            e[0x810..0x814].copy_from_slice(&ps4.to_le_bytes());
            e[0x814..0x818].copy_from_slice(&ps5.to_le_bytes());
        }
        e
    }

    struct Tree(Vec<(String, Vec<u8>)>);
    // implement ps5upload_fpkg::source::SourceTree for Tree: files() from the vec with sizes,
    // read()/read_range() slicing the bytes, empty_dirs() empty, describe() "test".

    const FW4: (u32, u32) = (0x0904_0001, 0x0400_0031);
    const FW9: (u32, u32) = (0x1159_0001, 0x0900_0040);

    #[test]
    fn a_backported_title_needs_its_highest_module_pair() {
        let mut t = Tree(vec![
            ("eboot.bin".into(), module(Some(FW4))),
            ("sce_module/libc.prx".into(), module(Some(FW4))),
        ]);
        assert_eq!(min_firmware(&mut t, Some("10.20")).as_deref(), Some("4.00"));
        let mut t = Tree(vec![
            ("eboot.bin".into(), module(Some(FW4))),
            ("sce_module/libc.prx".into(), module(Some(FW9))),
        ]);
        assert_eq!(min_firmware(&mut t, Some("10.20")).as_deref(), Some("9.00"));
    }

    #[test]
    fn never_above_what_the_game_declares() {
        let mut t = Tree(vec![("eboot.bin".into(), module(Some(FW9)))]);
        assert_eq!(min_firmware(&mut t, Some("5.10")), None);
    }

    /// Review focus 5: an unreadable pair keeps the declared value, never lowers it.
    #[test]
    fn an_unreadable_module_keeps_the_declared_value() {
        let mut t = Tree(vec![
            ("eboot.bin".into(), module(Some(FW4))),
            ("sce_module/libc.prx".into(), module(None)),
        ]);
        assert_eq!(min_firmware(&mut t, Some("10.20")), None);
        let mut t = Tree(vec![("eboot.bin".into(), vec![0x53, 0x43, 0x45, 0x00, 1, 2, 3])]);
        assert_eq!(min_firmware(&mut t, Some("10.20")), None);
    }
}
```

(Fill in the `SourceTree` impl for `Tree` — about 20 lines mirroring `MemTree` in `ps5upload-fpkg/src/source.rs` tests.)

- [ ] **Step 2: Run to verify they fail**

Run: `cd engine && cargo test -p ps5upload-engine fpkg_firmware`
Expected: compile error — `min_firmware` missing.

- [ ] **Step 3: Implement**

```rust
//! The lowest firmware a converted game can run on, from its executables' SDK stamps.
//!
//! A backport rewrites each module's SDK pair down (to the FW 4 pair, usually) and leaves
//! `param.json` declaring the firmware it was built for, so a package that copies the
//! declared value refuses to install on the very consoles the backport targets (Spider-Man 2:
//! declares 10.20, every module carries the FW 4 pair).

use ps5upload_core::fakelibs::{is_known_sdk_pair, param_site_from_header, sdk_pair_at};
use ps5upload_fpkg::source::SourceTree;

/// Headers are small; the param segment is placed from the first 64 KiB.
const HEADER: usize = 0x1_0000;

fn is_executable(path: &str) -> bool {
    path == "eboot.bin"
        || (path.starts_with("sce_module/") && path.to_ascii_lowercase().ends_with(".prx"))
}

/// A module's `(ps4, ps5)` pair, read from its header and 0x18 bytes at the param site.
fn module_pair(tree: &mut dyn SourceTree, path: &str) -> Option<(u32, u32)> {
    let head = tree.read_range(path, 0, HEADER).ok()?;
    let site = param_site_from_header(&head)?;
    let chunk = tree.read_range(path, site as u64, 0x18).ok()?;
    let pair = sdk_pair_at(&chunk)?;
    is_known_sdk_pair(pair).then_some(pair)
}

/// "M.mm" for a PS5 SDK word: its top byte is the firmware major in BCD, the next the minor.
fn firmware_of(ps5: u32) -> String {
    let major = (ps5 >> 24) & 0xFF;
    let minor = (ps5 >> 16) & 0xFF;
    format!("{}.{:02x}", format!("{major:x}").parse::<u32>().unwrap_or(0), minor)
}

fn parts(v: &str) -> Option<(u32, u32)> {
    let (a, b) = v.trim().split_once('.')?;
    Some((a.parse().ok()?, format!("{b:0<2}").parse().ok()?))
}

pub(crate) fn min_firmware(tree: &mut dyn SourceTree, declared: Option<&str>) -> Option<String> {
    let executables: Vec<String> = tree
        .files()
        .iter()
        .filter(|f| is_executable(&f.path))
        .map(|f| f.path.clone())
        .collect();
    if !executables.iter().any(|p| p == "eboot.bin") {
        return None;
    }
    let mut highest = 0u32;
    for path in &executables {
        let (_, ps5) = module_pair(tree, path)?;
        highest = highest.max(ps5);
    }
    let needed = firmware_of(highest);
    match declared.and_then(parts) {
        Some(d) if parts(&needed)? < d => Some(needed),
        _ => None,
    }
}
```

In `fpkg_api.rs`: define

```rust
#[derive(serde::Serialize)]
struct InspectResponse {
    #[serde(flatten)]
    inspection: build::Inspection,
    min_firmware: Option<String>,
}
```

and in `fpkg_inspect_handler`'s blocking closure compute `let min = source::open(&path).ok().and_then(|mut t| crate::fpkg_firmware::min_firmware(t.as_mut(), insp.required_firmware.as_deref()));` then return `InspectResponse { inspection: insp, min_firmware: min }`. In the build handler after `request.firmware = req.firmware…`, add:

```rust
        if request.firmware.is_none() {
            request.firmware = ps5upload_fpkg::source::open(&request_source).ok().and_then(|mut t| {
                crate::fpkg_firmware::min_firmware(t.as_mut(), inspection.required_firmware.as_deref())
            });
        }
```

Add a Review-focus-4 test to `fpkg_api.rs` tests (or `fpkg_firmware.rs`): `build::inspect(src, &missing_dir)` with a non-existent output folder returns `Ok` and `output_free` from its nearest existing parent (already implemented by `free_bytes`) — assert `is_ok()`.

- [ ] **Step 4: Run tests**

Run: `cd engine && cargo fmt --all && cargo test -p ps5upload-engine fpkg && cargo clippy -p ps5upload-engine --all-targets`
Expected: pass.

- [ ] **Step 5: Commit**

```bash
git add engine/crates/ps5upload-engine/src/fpkg_firmware.rs engine/crates/ps5upload-engine/src/lib.rs engine/crates/ps5upload-engine/src/fpkg_api.rs
git commit -m "feat(engine): converted packages declare the firmware the game runs on"
```

### Task 4: Per-game compression estimates

**Files:**
- Modify: `engine/crates/ps5upload-fpkg/src/build.rs` (new `estimate`)
- Modify: `engine/crates/ps5upload-engine/src/fpkg_api.rs` (`InspectResponse.estimates`)
- Test: `engine/crates/ps5upload-fpkg/tests/writer.rs`

**Interfaces:**
- Consumes: `kraken::{encode_block_at, Level, BLOCK}`.
- Produces: `#[derive(Serialize, Clone, Copy, Debug)] pub struct build::Estimate { pub bytes: u64, pub seconds: u64 }`; `#[derive(Serialize, Clone, Copy, Debug)] pub struct build::Estimates { pub fast: Estimate, pub balanced: Estimate, pub smallest: Estimate }`; `pub fn build::estimate(source_path: &Path) -> Result<Estimates>`. Inspect JSON gains `"estimates": {"fast":{"bytes":…,"seconds":…},…} | null`.

- [ ] **Step 1: Failing test** (append to `tests/writer.rs`)

```rust
#[test]
fn estimates_rank_the_levels_and_cover_the_game() {
    let dir = TempDir::new("estimate-src");
    write_tree(dir.path());
    let e = build::estimate(dir.path()).unwrap();
    assert!(e.smallest.bytes <= e.balanced.bytes && e.balanced.bytes <= e.fast.bytes);
    // The tree is mostly a 600 KiB run of one byte: well under its size once compressed.
    assert!(e.balanced.bytes < 600 * 1024);
    assert!(e.fast.seconds <= e.smallest.seconds);
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `cd engine && cargo test -p ps5upload-fpkg --test writer estimates_rank`
Expected: compile error — no `build::estimate`.

- [ ] **Step 3: Implement** (in `build.rs`)

```rust
/// Blocks sampled for an estimate.
const ESTIMATE_BLOCKS: usize = 60;

/// Estimated package size and build time at each level, from compressing a sample of the
/// game's blocks spread across its files; time assumes every core of this machine.
pub fn estimate(source_path: &Path) -> Result<Estimates> {
    use crate::kraken::{encode_block_at, Level, BLOCK};
    let mut tree = source::open(source_path)?;
    let files: Vec<SourceFile> = tree.files().iter().filter(|f| f.size > 0).cloned().collect();
    let total: u64 = files.iter().map(|f| f.size).sum();
    if total == 0 {
        return format_err("the source has no data");
    }
    // Evenly spaced points through the concatenated files, one block each.
    let step = (total / ESTIMATE_BLOCKS as u64).max(1);
    let mut blocks = Vec::new();
    let (mut at, mut base) = (0u64, 0u64);
    for f in &files {
        while at < base + f.size && blocks.len() < ESTIMATE_BLOCKS {
            let off = at - base;
            let len = (f.size - off).min(BLOCK as u64) as usize;
            blocks.push(tree.read_range(&f.path, off, len)?);
            at += step;
        }
        base += f.size;
    }
    let raw: usize = blocks.iter().map(Vec::len).sum();
    let threads = std::thread::available_parallelism().map_or(4, |n| n.get()) as f64;
    let measure = |level: Level| {
        let t = std::time::Instant::now();
        let stored: usize = blocks
            .iter()
            .map(|b| encode_block_at(b, level).iter().map(|h| h.bytes().len()).sum::<usize>())
            .sum();
        let secs = t.elapsed().as_secs_f64().max(1e-6);
        let rate = raw as f64 / secs * threads; // bytes per second, all cores
        Estimate {
            bytes: (total as f64 * stored as f64 / raw as f64) as u64,
            seconds: (total as f64 / rate).ceil() as u64,
        }
    };
    Ok(Estimates {
        fast: measure(Level::Fast),
        balanced: measure(Level::Balanced),
        smallest: measure(Level::Smallest),
    })
}
```

In `fpkg_inspect_handler`, add `estimates: Option<build::Estimates>` to `InspectResponse`, computed as `build::estimate(Path::new(&source)).ok()` in the same blocking closure (never fails the inspection). Note: `stored ≤ raw` per block because raw halves are kept, so ordering holds for the test data; the Fast/Smallest relation on real data is by construction (Smallest's parse keeps the smallest of its passes and the lazy seed).

- [ ] **Step 4: Run tests**

Run: `cd engine && cargo fmt --all && cargo test -p ps5upload-fpkg --test writer estimates && cargo test -p ps5upload-engine fpkg && cargo clippy -p ps5upload-fpkg -p ps5upload-engine --all-targets`
Expected: pass.

- [ ] **Step 5: Commit**

```bash
git add engine/crates/ps5upload-fpkg/src/build.rs engine/crates/ps5upload-fpkg/tests/writer.rs engine/crates/ps5upload-engine/src/fpkg_api.rs
git commit -m "feat(fpkg): estimate package size and time at each level"
```

### Task 5: Delete a built package

**Files:**
- Modify: `engine/crates/ps5upload-engine/src/fpkg_api.rs`
- Modify: `engine/crates/ps5upload-engine/src/lib.rs` (route)

**Interfaces:**
- Produces: `POST /api/fpkg/delete` body `{ "path": string }` → `200 {"ok":true}` or `400 {"error": "…"}`; `fn built_packages() -> &'static Mutex<HashSet<PathBuf>>`; `fn delete_built(path: &Path) -> Result<(), String>`.

- [ ] **Step 1: Failing tests** (in `fpkg_api.rs` test module)

```rust
#[cfg(test)]
mod delete_tests {
    use super::*;

    #[test]
    fn only_a_package_this_engine_built_can_be_deleted() {
        let dir = std::env::temp_dir().join(format!("fpkg-del-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let ours = dir.join("ours.pkg");
        let theirs = dir.join("theirs.pkg");
        std::fs::write(&ours, b"x").unwrap();
        std::fs::write(&theirs, b"x").unwrap();
        built_packages().lock().unwrap().insert(ours.clone());
        assert!(delete_built(&theirs).is_err());
        assert!(theirs.exists());
        assert!(delete_built(&ours).is_ok());
        assert!(!ours.exists());
        // Deleted once, forgotten: a second delete is refused rather than hitting a new file.
        std::fs::write(&ours, b"x").unwrap();
        assert!(delete_built(&ours).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `cd engine && cargo test -p ps5upload-engine only_a_package_this_engine_built`
Expected: compile error.

- [ ] **Step 3: Implement**

```rust
/// Packages this engine process built, the only files `/api/fpkg/delete` will remove.
fn built_packages() -> &'static std::sync::Mutex<std::collections::HashSet<PathBuf>> {
    static SET: std::sync::OnceLock<std::sync::Mutex<std::collections::HashSet<PathBuf>>> =
        std::sync::OnceLock::new();
    SET.get_or_init(Default::default)
}

fn delete_built(path: &Path) -> Result<(), String> {
    let mut set = built_packages().lock().unwrap_or_else(|e| e.into_inner());
    if !set.contains(path) {
        return Err("not a package this app built in this session".into());
    }
    std::fs::remove_file(path).map_err(|e| e.to_string())?;
    set.remove(path);
    Ok(())
}

#[derive(Deserialize)]
pub(crate) struct DeleteReq {
    path: String,
}

/// POST /api/fpkg/delete — remove a package this engine built.
pub(crate) async fn fpkg_delete_handler(
    State(_): State<AppState>,
    Json(req): Json<DeleteReq>,
) -> impl IntoResponse {
    match delete_built(&resolve_engine_path(&req.path)) {
        Ok(()) => (StatusCode::OK, Json(serde_json::json!({ "ok": true }))).into_response(),
        Err(e) => json_err(StatusCode::BAD_REQUEST, e).into_response(),
    }
}
```

Record on success in the build job's `Ok(report)` arm: `built_packages().lock().unwrap_or_else(|e| e.into_inner()).insert(report.path.clone());`. Route in `lib.rs` next to `/api/fpkg/build`: `.route("/api/fpkg/delete", post(fpkg_api::fpkg_delete_handler))`.

- [ ] **Step 4: Run tests**

Run: `cd engine && cargo fmt --all && cargo test -p ps5upload-engine && cargo clippy -p ps5upload-engine --all-targets`
Expected: pass.

- [ ] **Step 5: Commit**

```bash
git add engine/crates/ps5upload-engine/src/fpkg_api.rs engine/crates/ps5upload-engine/src/lib.rs
git commit -m "feat(engine): delete a package the converter built"
```

### Task 6: Client API surface

**Files:**
- Modify: `client/src/api/fpkg.ts`, `client/src/api/ps5.ts` (`JobSnapshot` ~line 4074), `client/src/lib/browserInvoke.ts`, `client/src-tauri/src/commands/ps5_engine.rs`, `client/src-tauri/src/lib.rs`

**Interfaces:**
- Produces (TypeScript):
  - `export interface FpkgEstimate { bytes: number; seconds: number }`
  - `FpkgInspection.min_firmware?: string | null; FpkgInspection.estimates?: { fast: FpkgEstimate; balanced: FpkgEstimate; smallest: FpkgEstimate } | null`
  - `fpkg.deletePackage(path: string): Promise<{ ok: boolean }>` (Tauri command `fpkg_delete { path }`; browser `POST /api/fpkg/delete`)
  - `JobSnapshot.stage?: { id: string; index: number; count: number; done: number; total: number }`

- [ ] **Step 1: Write the failing test** (`client/src/api/fpkg.test.ts`)

```ts
import { describe, expect, it, vi } from "vitest";

vi.mock("../lib/invoke", () => ({ invoke: vi.fn(async () => ({ ok: true })) }));
import { invoke } from "../lib/invoke";
import { fpkg } from "./fpkg";

describe("fpkg.deletePackage", () => {
  it("calls the delete command with the path", async () => {
    await fpkg.deletePackage("/out/PPSA01234.pkg");
    expect(invoke).toHaveBeenCalledWith("fpkg_delete", { path: "/out/PPSA01234.pkg" });
  });
});
```

(Use whatever module `api/fpkg.ts` imports `invoke` from; adjust the mock path to match.)

- [ ] **Step 2: Run to verify it fails**

Run: `cd client && npx vitest run src/api/fpkg.test.ts`
Expected: FAIL — `deletePackage` is not a function.

- [ ] **Step 3: Implement** the types above; `deletePackage: (path: string) => invoke<{ ok: boolean }>("fpkg_delete", { path })`; in `browserInvoke.ts` add `case "fpkg_delete": return postJson<T>("/api/fpkg/delete", { path: args["path"] });`; in `ps5_engine.rs` add

```rust
/// Delete a package the converter built. POST /api/fpkg/delete.
#[tauri::command]
pub async fn fpkg_delete(path: String) -> Result<JsonValue, String> {
    let base = engine::url();
    post_json(&format!("{base}/api/fpkg/delete"), &serde_json::json!({ "path": path })).await
}
```

and register `commands::fpkg_delete,` beside `commands::fpkg_build,` in `client/src-tauri/src/lib.rs`. (Check: no console install is running — this edit restarts `tauri dev`.)

- [ ] **Step 4: Run tests**

Run: `cd client && npx vitest run src/api/fpkg.test.ts && npx tsc --noEmit -p .`
Expected: pass.

- [ ] **Step 5: Commit**

```bash
git add client/src/api/fpkg.ts client/src/api/fpkg.test.ts client/src/api/ps5.ts client/src/lib/browserInvoke.ts client/src-tauri/src/commands/ps5_engine.rs client/src-tauri/src/lib.rs
git commit -m "feat(client): fpkg delete, stage, estimates and min firmware in the API"
```

### Task 7: Remembered convert preferences

**Files:**
- Create: `client/src/state/convertPrefs.ts`, `client/src/state/convertPrefs.test.ts`

**Interfaces:**
- Consumes: `safeGetItem`, `safeSetItem` from `../lib/safeStorage`; `FpkgCompression` from `../api/fpkg`.
- Produces: `useConvertPrefs` zustand store `{ outputDir: string; compression: FpkgCompression; setOutputDir(dir: string): void; setCompression(c: FpkgCompression): void }`; storage keys `ps5upload.convert.output_dir`, `ps5upload.convert.compression`.

- [ ] **Step 1: Failing test**

```ts
import { beforeEach, describe, expect, it, vi } from "vitest";

describe("convertPrefs", () => {
  beforeEach(() => {
    localStorage.clear();
    vi.resetModules();
  });

  it("defaults to no folder and Balanced", async () => {
    const { useConvertPrefs } = await import("./convertPrefs");
    expect(useConvertPrefs.getState().outputDir).toBe("");
    expect(useConvertPrefs.getState().compression).toBe("balanced");
  });

  it("remembers the folder and level across loads", async () => {
    const first = await import("./convertPrefs");
    first.useConvertPrefs.getState().setOutputDir("/Volumes/Gone/fpkg");
    first.useConvertPrefs.getState().setCompression("smallest");
    vi.resetModules();
    const second = await import("./convertPrefs");
    // Review focus 4: a remembered folder that no longer exists still loads as-is;
    // the engine's check reports it, the screen does not crash.
    expect(second.useConvertPrefs.getState().outputDir).toBe("/Volumes/Gone/fpkg");
    expect(second.useConvertPrefs.getState().compression).toBe("smallest");
  });

  it("ignores a corrupt stored level", async () => {
    localStorage.setItem("ps5upload.convert.compression", "turbo");
    const { useConvertPrefs } = await import("./convertPrefs");
    expect(useConvertPrefs.getState().compression).toBe("balanced");
  });
});
```

- [ ] **Step 2: Run to verify it fails**

Run: `cd client && npx vitest run src/state/convertPrefs.test.ts` — Expected: FAIL (module not found).

- [ ] **Step 3: Implement**

```ts
import { create } from "zustand";

import type { FpkgCompression } from "../api/fpkg";
import { safeGetItem, safeSetItem } from "../lib/safeStorage";

/** What Convert remembers between sessions, per viewer: where packages go and how hard to
 *  compress. Reads and writes never throw (the browser build may deny storage). */
const KEY_DIR = "ps5upload.convert.output_dir";
const KEY_LEVEL = "ps5upload.convert.compression";
const LEVELS: FpkgCompression[] = ["fast", "balanced", "smallest"];

interface ConvertPrefs {
  outputDir: string;
  compression: FpkgCompression;
  setOutputDir: (dir: string) => void;
  setCompression: (c: FpkgCompression) => void;
}

function storedLevel(): FpkgCompression {
  const v = safeGetItem(KEY_LEVEL);
  return LEVELS.includes(v as FpkgCompression) ? (v as FpkgCompression) : "balanced";
}

export const useConvertPrefs = create<ConvertPrefs>((set) => ({
  outputDir: safeGetItem(KEY_DIR) ?? "",
  compression: storedLevel(),
  setOutputDir: (dir) => {
    safeSetItem(KEY_DIR, dir);
    set({ outputDir: dir });
  },
  setCompression: (c) => {
    safeSetItem(KEY_LEVEL, c);
    set({ compression: c });
  },
}));
```

- [ ] **Step 4: Run** `cd client && npx vitest run src/state/convertPrefs.test.ts` — Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add client/src/state/convertPrefs.ts client/src/state/convertPrefs.test.ts
git commit -m "feat(client): remember Convert's output folder and compression"
```

### Task 8: `installStream` reports its task

**Files:**
- Modify: `client/src/state/pkgLibrary.ts` (type ~line 845, implementation `installStream: async` and `registerTask` ~line 3847)
- Test: `client/src/state/pkgLibrary.test.ts`

**Interfaces:**
- Produces: `installStream(source, host, opts?: { onTask?: (taskId: string) => void })`; `onTask` is called synchronously right after the task is registered, before any network call.

- [ ] **Step 1: Failing test** (add to `pkgLibrary.test.ts`, following the file's existing mocking of the engine calls for `installStream`; the test only needs the first network call to reject)

```ts
it("tells the caller which task tracks a stream install", async () => {
  const seen: string[] = [];
  // Make the first engine call fail so the install ends quickly.
  mockInstallStartToReject(); // use the helper this file already uses for install/start
  await usePkgLibrary("192.168.1.5").getState().installStream("/out/a.pkg", "192.168.1.5", {
    onTask: (id) => seen.push(id),
  });
  expect(seen).toHaveLength(1);
  expect(useTaskStore.getState().tasks.some((t) => t.id === seen[0])).toBe(true);
});
```

(Name the helper after what the file already provides; if it has none, stub `invoke` for `"pkg_install_start"` to reject.)

- [ ] **Step 2: Run** `cd client && npx vitest run src/state/pkgLibrary.test.ts -t "which task"` — Expected: FAIL (onTask never called).

- [ ] **Step 3: Implement**: add the optional third parameter to the `installStream` type and implementation; immediately after `const taskId = tasks.registerTask({ … })` inside `installStream`, add `opts?.onTask?.(taskId);`. `installUrl` / `installDownloadedLink` pass nothing.

- [ ] **Step 4: Run** `cd client && npx vitest run src/state/pkgLibrary.test.ts && npx tsc --noEmit -p .` — Expected: pass.

- [ ] **Step 5: Commit**

```bash
git add client/src/state/pkgLibrary.ts client/src/state/pkgLibrary.test.ts
git commit -m "feat(client): installStream can report the task that tracks it"
```

### Task 9: The pipeline store

**Files:**
- Replace: `client/src/state/fpkgConversion.ts`
- Create: `client/src/state/fpkgConversion.test.ts`

**Interfaces:**
- Consumes: `fpkg.build`, `fpkg.compress`, `fpkg.deletePackage` (Task 6), `jobStatus`, `jobCancel`, `JobSnapshot.stage` (Task 6), `usePkgLibrary(host).getState().installStream(path, host, { onTask })` (Task 8), `pushNotification`.
- Produces:

```ts
export type PipelineStage =
  | "check" | "plan" | "compress" | "write" | "verify" | "send" | "install";
export type PipelineMode = "convert" | "convert-install" | "install" | "ffpfsc";
export type Pipeline =
  | { phase: "idle" }
  | {
      phase: "running"; mode: PipelineMode; source: string; host: string | null;
      stage: PipelineStage; stageDone: number; stageTotal: number;
      startedMs: number; stageStartedMs: number; stageMs: Partial<Record<PipelineStage, number>>;
      jobId: string | null; installTaskId: string | null; packagePath: string | null;
    }
  | {
      phase: "done"; mode: PipelineMode; source: string; host: string | null;
      packagePath: string; packageBytes: number; convertMs: number; installMs: number;
      stageMs: Partial<Record<PipelineStage, number>>; deleted: boolean;
    }
  | {
      phase: "failed"; mode: PipelineMode; source: string; host: string | null;
      stage: PipelineStage; message: string; packagePath: string | null;
      stageMs: Partial<Record<PipelineStage, number>>;
    };
export interface ConversionState {
  pipeline: Pipeline;
  start: (req: FpkgBuildRequest, opts: { install: boolean; host: string | null }) => Promise<void>;
  compress: (source: string, outputDir?: string) => Promise<void>;
  retryInstall: (host: string) => Promise<void>;
  cancel: () => Promise<void>;
  /** A new source: clears a finished result; ignored while running. */
  reset: () => void;
  deletePackage: () => Promise<void>;
}
export const useFpkgConversion: UseBoundStore<StoreApi<ConversionState>>;
/** Test seam: the poll interval (ms). */
export const POLL_MS = 500;
```

Behaviour:
- `start` while `phase === "running"` is a no-op. It sets `running` with `stage: "check"`, calls `fpkg.build`, stores `jobId`, polls every `POLL_MS`. Each snapshot with `stage` updates `stage/stageDone/stageTotal`, and when the stage id changes records the previous stage's elapsed ms in `stageMs`.
- Job `done`: `packagePath = snapshot.dest`. `mode === "convert"` → `done` (`installMs: 0`). `mode === "convert-install"` → stage `send` and `runInstall(host)`.
- Job `failed`: `failed` at the current stage with `snapshot.error`; `packagePath: null`.
- Five consecutive poll errors → `failed` with message `"The engine stopped responding; the conversion did not finish."` (Review focus 2).
- `runInstall(host)` (internal): if `host` is null → `failed` at `send` with `"Connect to a PS5 to install."`, keeping `packagePath`. Else it calls `installStream(packagePath, host, { onTask: id => set installTaskId })`. While running, the screen derives send/install from the task. On resolve: `ok` → `done` with `installMs`; not ok → `failed` at `install` with `r.message ?? "The install did not finish."`, keeping `packagePath`.
- `retryInstall(host)` from `failed` with a `packagePath`, or from `done` when `!deleted` (the **Install again** button) → `running` (mode `install`, stage `send`) → `runInstall(host)`. The `host` passed at click time is the one used and recorded (Review focus 3). From any other state it is a no-op.
- `reset()`: from `done`/`failed` → `idle`; from `running` → no-op (Review focus 1).
- `deletePackage()` from `done` → `fpkg.deletePackage(packagePath)` → `deleted: true`.
- `cancel()` → `jobCancel(jobId)` when a build job is running (the job then fails; the message becomes "Cancelled").
- Notifications as today: success and failure, link `/convert`.

- [ ] **Step 1: Failing tests**

```ts
import { beforeEach, describe, expect, it, vi } from "vitest";

const build = vi.fn();
const jobStatus = vi.fn();
const installStream = vi.fn();
vi.mock("../api/fpkg", () => ({ fpkg: { build: (...a: unknown[]) => build(...a), compress: vi.fn(), deletePackage: vi.fn(async () => ({ ok: true })) } }));
vi.mock("../api/ps5", () => ({ jobStatus: (...a: unknown[]) => jobStatus(...a), jobCancel: vi.fn() }));
vi.mock("./pkgLibrary", () => ({ usePkgLibrary: () => ({ getState: () => ({ installStream: (...a: unknown[]) => installStream(...a) }) }) }));
vi.mock("./notifications", () => ({ pushNotification: vi.fn() }));

import { useFpkgConversion } from "./fpkgConversion";

const req = { source: "/games/a", outputDir: "/out" };
const flush = () => vi.advanceTimersByTimeAsync(600);

describe("fpkg pipeline", () => {
  beforeEach(() => {
    vi.useFakeTimers();
    useFpkgConversion.setState({ pipeline: { phase: "idle" } });
    build.mockReset().mockResolvedValue({ job_id: "j1" });
    jobStatus.mockReset();
    installStream.mockReset();
  });

  it("converts through the stages and ends done", async () => {
    jobStatus
      .mockResolvedValueOnce({ status: "running", stage: { id: "compress", index: 2, count: 5, done: 5, total: 10 } })
      .mockResolvedValueOnce({ status: "done", dest: "/out/a.pkg", bytes_sent: 99 });
    await useFpkgConversion.getState().start(req, { install: false, host: null });
    await flush();
    expect(useFpkgConversion.getState().pipeline).toMatchObject({ phase: "running", stage: "compress", stageDone: 5 });
    await flush();
    expect(useFpkgConversion.getState().pipeline).toMatchObject({ phase: "done", packagePath: "/out/a.pkg", packageBytes: 99 });
  });

  it("chains into the install on the host of the moment, then ends done", async () => {
    jobStatus.mockResolvedValue({ status: "done", dest: "/out/a.pkg", bytes_sent: 1 });
    installStream.mockResolvedValue({ ok: true });
    await useFpkgConversion.getState().start(req, { install: true, host: "10.0.0.2" });
    await flush();
    await flush();
    expect(installStream).toHaveBeenCalledWith("/out/a.pkg", "10.0.0.2", expect.anything());
    expect(useFpkgConversion.getState().pipeline).toMatchObject({ phase: "done", host: "10.0.0.2" });
  });

  it("keeps the package when the install fails, and retries on the new host", async () => {
    jobStatus.mockResolvedValue({ status: "done", dest: "/out/a.pkg", bytes_sent: 1 });
    installStream.mockResolvedValueOnce({ ok: false, message: "unreachable" }).mockResolvedValueOnce({ ok: true });
    await useFpkgConversion.getState().start(req, { install: true, host: "10.0.0.2" });
    await flush();
    await flush();
    expect(useFpkgConversion.getState().pipeline).toMatchObject({ phase: "failed", stage: "install", packagePath: "/out/a.pkg" });
    await useFpkgConversion.getState().retryInstall("10.0.0.3");
    expect(installStream).toHaveBeenLastCalledWith("/out/a.pkg", "10.0.0.3", expect.anything());
    expect(build).toHaveBeenCalledTimes(1);
    expect(useFpkgConversion.getState().pipeline).toMatchObject({ phase: "done", host: "10.0.0.3" });
  });

  it("without a console, Convert & install stops at send with the package kept", async () => {
    jobStatus.mockResolvedValue({ status: "done", dest: "/out/a.pkg", bytes_sent: 1 });
    await useFpkgConversion.getState().start(req, { install: true, host: null });
    await flush();
    await flush();
    expect(useFpkgConversion.getState().pipeline).toMatchObject({ phase: "failed", stage: "send", packagePath: "/out/a.pkg" });
  });

  it("fails, not spins, when the engine stops answering", async () => {
    jobStatus.mockRejectedValue(new Error("connection refused"));
    await useFpkgConversion.getState().start(req, { install: false, host: null });
    for (let i = 0; i < 7; i++) await flush();
    expect(useFpkgConversion.getState().pipeline).toMatchObject({ phase: "failed", message: expect.stringContaining("stopped responding") });
  });

  it("ignores a reset or a second start while running, and resets a result", async () => {
    jobStatus.mockResolvedValue({ status: "running" });
    await useFpkgConversion.getState().start(req, { install: false, host: null });
    useFpkgConversion.getState().reset();
    await useFpkgConversion.getState().start({ source: "/games/b" }, { install: false, host: null });
    expect(useFpkgConversion.getState().pipeline).toMatchObject({ phase: "running", source: "/games/a" });
    expect(build).toHaveBeenCalledTimes(1);
    useFpkgConversion.setState({ pipeline: { phase: "failed", mode: "convert", source: "/games/a", host: null, stage: "write", message: "x", packagePath: null, stageMs: {} } });
    useFpkgConversion.getState().reset();
    expect(useFpkgConversion.getState().pipeline.phase).toBe("idle");
  });
});
```

- [ ] **Step 2: Run** `cd client && npx vitest run src/state/fpkgConversion.test.ts` — Expected: FAIL (no `pipeline`).

- [ ] **Step 3: Implement** the store per the interface and behaviour above (replace the file; keep `compress` producing `mode: "ffpfsc"` with the same polling, ending `done` with `packagePath = snapshot.dest`). Update the one existing importer (`screens/FpkgConvert/index.tsx`) only as far as needed to compile; Task 11 rewrites it.

- [ ] **Step 4: Run** `cd client && npx vitest run src/state/fpkgConversion.test.ts && npx tsc --noEmit -p .` — Expected: pass.

- [ ] **Step 5: Commit**

```bash
git add client/src/state/fpkgConversion.ts client/src/state/fpkgConversion.test.ts client/src/screens/FpkgConvert/index.tsx
git commit -m "feat(client): Convert runs as one pipeline, build then install"
```

### Task 10: Stage rows (pure model)

**Files:**
- Create: `client/src/screens/FpkgConvert/stages.ts`, `client/src/screens/FpkgConvert/stages.test.ts`

**Interfaces:**
- Consumes: `Pipeline`, `PipelineStage` (Task 9); `Task` from `../../state/tasks`.
- Produces:

```ts
export type RowState = "pending" | "active" | "done" | "failed";
export interface StageRow { stage: PipelineStage; state: RowState; ms?: number; done?: number; total?: number }
/** The rows card ③ shows for a pipeline; `installTask` is the stream-install task when one runs. */
export function stageRows(p: Pipeline, installTask: Task | null): StageRow[];
/** 0..1 across all rows, weighting compress and write by their share of the work. */
export function overallProgress(rows: StageRow[]): number;
```

Rows: build stages always; `send` and `install` only for modes `convert-install` / `install`. `install` mode shows only `send`/`install`. Active install sub-stage: `send` while `installTask?.progress && progress.current < progress.total`, else `install`. Weights: check 2, plan 2, compress 55, write 20, verify 6, send 10, install 5 (normalised over the rows present).

- [ ] **Step 1: Failing tests**

```ts
import { describe, expect, it } from "vitest";

import { overallProgress, stageRows } from "./stages";

const base = { mode: "convert-install" as const, source: "/g", host: "h", startedMs: 0, stageStartedMs: 0, jobId: "j", installTaskId: null, packagePath: null };

describe("stageRows", () => {
  it("marks earlier stages done and the current one active", () => {
    const rows = stageRows({ phase: "running", ...base, stage: "write", stageDone: 5, stageTotal: 10, stageMs: { check: 1, plan: 2, compress: 3 } }, null);
    expect(rows.map((r) => [r.stage, r.state])).toEqual([
      ["check", "done"], ["plan", "done"], ["compress", "done"], ["write", "active"],
      ["verify", "pending"], ["send", "pending"], ["install", "pending"],
    ]);
    expect(rows[3]).toMatchObject({ done: 5, total: 10 });
  });

  it("splits the install into send then install by the task's bytes", () => {
    const p = { phase: "running" as const, ...base, stage: "send" as const, stageDone: 0, stageTotal: 0, stageMs: {} };
    const sending = { progress: { current: 5, total: 10, unit: "bytes" } } as never;
    const installing = { progress: { current: 10, total: 10, unit: "bytes" } } as never;
    expect(stageRows(p, sending).find((r) => r.state === "active")?.stage).toBe("send");
    expect(stageRows(p, installing).find((r) => r.state === "active")?.stage).toBe("install");
  });

  it("shows the failed stage and leaves later ones pending", () => {
    const rows = stageRows({ phase: "failed", mode: "convert", source: "/g", host: null, stage: "verify", message: "bad", packagePath: null, stageMs: {} }, null);
    expect(rows.map((r) => r.state)).toEqual(["done", "done", "done", "done", "failed"]);
  });

  it("has no install rows for Convert only, and only install rows for a retry", () => {
    expect(stageRows({ phase: "failed", mode: "convert", source: "/g", host: null, stage: "write", message: "", packagePath: null, stageMs: {} }, null).map((r) => r.stage)).not.toContain("send");
    expect(stageRows({ phase: "running", ...base, mode: "install", stage: "send", stageDone: 0, stageTotal: 0, stageMs: {} }, null).map((r) => r.stage)).toEqual(["send", "install"]);
  });

  it("weights progress by stage size", () => {
    const rows = stageRows({ phase: "running", ...base, mode: "convert", stage: "compress", stageDone: 50, stageTotal: 100, stageMs: {} }, null);
    expect(overallProgress(rows)).toBeGreaterThan(0.3);
    expect(overallProgress(rows)).toBeLessThan(0.4);
  });
});
```

- [ ] **Step 2: Run** `cd client && npx vitest run src/screens/FpkgConvert/stages.test.ts` — Expected: FAIL (module not found).

- [ ] **Step 3: Implement** `stages.ts` per the interface (≈70 lines: ordered stage lists per mode; `failedAt`/`activeAt` index; `done`/`total` only on the active row; weights table normalised).

- [ ] **Step 4: Run** the test — Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add client/src/screens/FpkgConvert/stages.ts client/src/screens/FpkgConvert/stages.test.ts
git commit -m "feat(client): stage rows for Convert's progress card"
```

### Task 11: The screen — three cards, drop, tiles, run card

**Files:**
- Create: `client/src/lib/useWebviewDrop.ts`, `client/src/screens/FpkgConvert/GameCard.tsx`, `OptionsCard.tsx`, `CompressionTiles.tsx`, `RunCard.tsx`
- Rewrite: `client/src/screens/FpkgConvert/index.tsx`
- Modify: `client/src/i18n/locales/en.ts`, `scripts/i18n-known-missing.json`
- Delete: `client/src/lib/firmware.ts`, `client/src/lib/firmware.test.ts` (the firmware field goes away)

**Interfaces:**
- Consumes: `useFpkgConversion` (Task 9), `stageRows`/`overallProgress` (Task 10), `useConvertPrefs` (Task 7), `fpkg.inspect` (+ `min_firmware`, `estimates` from Task 6), `useTaskStore` (install task by id), `appLaunch(transferAddr(host), titleId)`, `openLocalPath(dirOf(path))`, `pickPath`/`pickLocalPath`, existing `Button`, `Card`, `Input`, `ProgressBar`, `PageHeader`, `Callout`, `ConnectionGate`.
- Produces:
  - `useWebviewDrop(onDrop: (path: string) => void, enabled: boolean): boolean` returns `dropActive`; skips non-Tauri and Android; ignores `.pkg` (AppShell routes those) — same subscription pattern and cleanup as `screens/Upload/index.tsx:229-270`.
  - `GameCard({ source, onSource, inspection, checking, locked, dropActive })`: drop zone, Folder…/Image… buttons, path input; summary line "✓ {title} · {titleId} · {size} · {files} files" and "Runs on FW {min_firmware ?? required_firmware}+" with "(backported)" when `min_firmware` is set; failed checks listed; collapsed passed checks.
  - `OptionsCard({ outputDir, onOutputDir, compression, onCompression, estimates, locked })`.
  - `CompressionTiles({ value, onChange, estimates, disabled })`: three `role="radio"` tiles in a `role="radiogroup"`; ⚡ Fast / ⚖️ Balanced ("Recommended") / 🗜️ Smallest; each shows `~{size} · ~{duration}` from `estimates` or "Estimating…" while inspecting.
  - `RunCard({ pipeline, installTask, host, canInstall, onConvert, onConvertInstall, onCompress, isImage, onCancel, onRetryInstall, onLaunch, onShowFolder, onInstallAgain, onDelete, onAnother })`: idle → two buttons (+ Compress to .ffpfsc when `isImage`); running → rows from `stageRows` (icon per state, per-row ms, active row bar with `done/total`, rate/eta from `installTask` during send), overall bar from `overallProgress`, Cancel (build stages only); done → summary + actions (Launch only when `mode` has install and `host`); failed → reason, "The package was built and kept." when `packagePath`, Retry install when `packagePath`, Show details (the full message in a `<details>`).
  - `index.tsx`: PageHeader; one-line beta Callout; `<details>` About (moves the four existing explanation paragraphs and the checks list); GameCard → OptionsCard → RunCard. Source changes call `pipeline.reset()` then re-inspect; while `pipeline.phase === "running"` source changes and drops are ignored (Review focus 1). `canInstall = payloadStatus === "up" && host.trim() !== ""`; when false, the Convert & install button is disabled with "Connect to a PS5 to install".

- [ ] **Step 1: Failing render tests** (`client/src/screens/FpkgConvert/RunCard.test.tsx`)

```tsx
import { render, screen } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";

import { RunCard } from "./RunCard";

const noop = vi.fn();
const actions = { onConvert: noop, onConvertInstall: noop, onCompress: noop, onCancel: noop, onRetryInstall: noop, onLaunch: noop, onShowFolder: noop, onInstallAgain: noop, onDelete: noop, onAnother: noop };

describe("RunCard", () => {
  it("offers only Convert when no console is connected", () => {
    render(<RunCard pipeline={{ phase: "idle" }} installTask={null} host="" canInstall={false} isImage={false} {...actions} />);
    expect(screen.getByRole("button", { name: /convert only/i })).toBeEnabled();
    expect(screen.getByRole("button", { name: /convert & install/i })).toBeDisabled();
    expect(screen.getByText(/connect to a ps5 to install/i)).toBeInTheDocument();
  });

  it("names the kept package and offers Retry install after an install failure", () => {
    render(<RunCard pipeline={{ phase: "failed", mode: "convert-install", source: "/g", host: "h", stage: "install", message: "unreachable", packagePath: "/out/a.pkg", stageMs: {} }} installTask={null} host="h" canInstall isImage={false} {...actions} />);
    expect(screen.getByText(/built and kept/i)).toBeInTheDocument();
    expect(screen.getByRole("button", { name: /retry install/i })).toBeInTheDocument();
  });

  it("result actions after install, and no Launch for Convert only", () => {
    const done = { phase: "done" as const, source: "/g", host: "h", packagePath: "/out/a.pkg", packageBytes: 1, convertMs: 1, installMs: 1, stageMs: {}, deleted: false };
    const { rerender } = render(<RunCard pipeline={{ ...done, mode: "convert-install" }} installTask={null} host="h" canInstall isImage={false} {...actions} />);
    for (const name of [/launch on ps5/i, /install again/i, /delete package/i, /convert another game/i]) {
      expect(screen.getByRole("button", { name })).toBeInTheDocument();
    }
    rerender(<RunCard pipeline={{ ...done, mode: "convert" }} installTask={null} host="h" canInstall isImage={false} {...actions} />);
    expect(screen.queryByRole("button", { name: /launch on ps5/i })).toBeNull();
  });

  it("asks before deleting the package", async () => {
    const onDelete = vi.fn();
    const done = { phase: "done" as const, mode: "convert" as const, source: "/g", host: null, packagePath: "/out/a.pkg", packageBytes: 1, convertMs: 1, installMs: 0, stageMs: {}, deleted: false };
    render(<RunCard pipeline={done} installTask={null} host="" canInstall={false} isImage={false} {...actions} onDelete={onDelete} />);
    screen.getByRole("button", { name: /delete package/i }).click();
    expect(onDelete).not.toHaveBeenCalled();
    screen.getByRole("button", { name: /confirm delete/i }).click();
    expect(onDelete).toHaveBeenCalledTimes(1);
  });
});
```

- [ ] **Step 2: Run** `cd client && npx vitest run src/screens/FpkgConvert/RunCard.test.tsx` — Expected: FAIL (module not found).

- [ ] **Step 3: Implement** the hook, the four components and the screen per the interfaces. **Delete package** is two-step inside the card (first press turns it into "Confirm delete" for 5 s; no browser dialog), then calls `onDelete`; after `deleted: true` the Install again / Launch / Delete buttons disappear and the path line reads "Package deleted". Every visible string goes through `tr(key, vars, fallback)`; add each key to `en.ts` next to the existing `fpkg.*` keys and to every locale's `missing` list in `scripts/i18n-known-missing.json`. Remove the firmware field, its effects, `lib/firmware.ts(+test)` and the `fpkg.minFirmware*` keys (en.ts and the allowlist). Keep `Compress to .ffpfsc` and its explanation for image sources.

- [ ] **Step 4: Run everything**

Run:
```bash
cd client && npx vitest run && npx tsc --noEmit -p . && npx eslint src/screens/FpkgConvert src/lib/useWebviewDrop.ts src/state/fpkgConversion.ts src/state/convertPrefs.ts
cd .. && npm run i18n:check
```
Expected: all pass; `[i18n-coverage] ok`.

- [ ] **Step 5: Commit**

```bash
git add client/src/lib/useWebviewDrop.ts client/src/screens/FpkgConvert client/src/i18n/locales/en.ts scripts/i18n-known-missing.json
git rm client/src/lib/firmware.ts client/src/lib/firmware.test.ts
git commit -m "feat(ui): Convert as three guided cards with stage progress and a clear result"
```

### Task 12: End to end

**Files:** none new (fixes only, if the run finds any).

- [ ] **Step 1: Full gate**

Run:
```bash
cd engine && cargo fmt --all -- --check && cargo clippy -p ps5upload-fpkg -p ps5upload-engine --all-targets && cargo test -p ps5upload-fpkg -p ps5upload-engine
cd ../client && npx vitest run && npx tsc --noEmit -p .
cd .. && npm run i18n:check
```
Expected: all green.

- [ ] **Step 2: Hardware run** (FW 5.10 console at 192.168.86.99, no other install running; the desktop dev app must be running the rebuilt engine)

In the app: Convert → drop `~/Downloads/PPSA17221-app` → confirm the card shows "Runs on FW x.xx+", three tiles with estimates → **Convert & install**. Watch: check → plan → compress (bar, speed) → write → verify → send (rate/eta) → install → Done. Press **Launch on PS5**; the user confirms on the TV. Then drop another source: the result and its buttons clear, folder and level stay. Then **Delete package** on a finished result removes the file.

- [ ] **Step 3: Commit any fixes** with messages naming what the run found.
