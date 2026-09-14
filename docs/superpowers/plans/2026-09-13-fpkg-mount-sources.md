# FPKG mount sources (Plan 5) — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** the converter accepts game **mounts** — `.exfat` disk images and `.ffpkg` (UFS2) images — as sources, alongside folders, entirely in the engine (desktop, Docker, Android): no OS mounts, no console involvement. This is the first half of the PSVIETHOA capability set; the engine API/UI follows in Plan 6, and the writer's >570 MiB layout in Plan 7 (see the size note below).

**Architecture:** `source.rs` grows a `SourceTree` trait (`files()`, `read()`), with a folder implementation (today's walk), a pure-Rust read-only exFAT reader (`exfat.rs`), and a wrapper over the existing UFS2 reader in `ps5upload-pkg`. `build.rs` takes a path, opens the right tree by extension, and everything downstream is unchanged.

**Tech Stack:** Rust 2021, no new dependencies. `ps5upload-pkg` (workspace) supplies the UFS2 reader.

**Spec:** `docs/superpowers/specs/2026-09-13-fpkg-builder-design.md` — Part B, "Sources". Writer plan: `2026-09-13-fpkg-writer.md` (gate G2 passed).

**Measured facts this plan encodes** (checked on the user's real mounts, 2026-09-13):

| Fact | Value |
|---|---|
| Real mounts | `/Volumes/Storage/PS5/games/game_mounts/*.exfat` (many, 10–30 GB) and `PPSA21159.ffpkg` (75 GB); two more `.exfat` in `~/Downloads` |
| exFAT images are raw volumes | `PPSA09519.exfat`'s boot sector is at offset 0 (`"EXFAT   "` at 0x03); sector shift 9, cluster shift 7 (64 KiB clusters), FAT at sector 2048, heap at 6144, root cluster 4. The reader still needs MBR and GPT offsets for images that are partitioned. |
| `.ffpkg` is UFS2 | `PPSA21159.ffpkg` starts with zeros (UFS2's boot blocks); `ps5upload-pkg/src/ufs2.rs` already opens it (superblock at the standard offset), lists directories and reads files |

**Size note (blocks real-game builds, not this plan's readers):** the writer's outer layout covers 12 direct + 5 × 1820 blocks ≈ **570 MiB** of inner image. Real mounts are 10–75 GB, so a full build from one needs the double-indirect slot (Plan 7). This plan therefore proves the readers against the real mounts and proves the *build path* with a small synthetic exFAT image; the end-to-end build of a real mount waits for Plan 7.

---

## Global Constraints

- Write our own implementation. PSVIETHOA-FPKG-Builder (which solves the same problem with a pure .NET reader) and MkPFS are read for understanding only; never copy their code.
- Read-only: nothing in this plan writes to a source image. Every read is bounds-checked, and a malformed image must produce an error, never a panic (the images come from the internet).
- Sample-gated tests read the mount folder from `PS5UPLOAD_SAMPLE_MOUNTS` (default `/Volumes/Storage/PS5/games/game_mounts`) and skip with an `eprintln!` when absent.
- `cargo fmt` after every edit; clippy `-D warnings` clean; commits stage only the files the task names.

## File Structure

| File | Responsibility |
|---|---|
| `src/source.rs` | `SourceTree` trait, `FolderSource`, `open(path)` dispatch by extension. |
| `src/exfat.rs` | Read-only exFAT: boot sector (raw/MBR/GPT), FAT chains, directory entries, file reads. |
| `src/ufs2_source.rs` | `SourceTree` over `ps5upload-pkg`'s `Ufs2Image`. |
| `src/build.rs` | Open the source tree instead of scanning a folder. |
| `examples/fpkg_build.rs` | Unchanged; the path argument now accepts mounts. |
| `tests/mounts.rs` | Real-mount reader checks; a build from a small exFAT fixture. |
| `tests/fixtures/mini.exfat` | A ~2 MiB exFAT image with a minimal app tree, built once with `hdiutil` + `newfs_exfat`. |

---

### Task 1: The `SourceTree` seam

- [ ] **Step 1:** In `source.rs`, define
  ```rust
  pub trait SourceTree {
      fn files(&self) -> &[SourceFile];
      fn read(&self, path: &str) -> Result<Vec<u8>>;
      fn describe(&self) -> String;   // for logs: "folder /x", "exfat PPSA09519.exfat"
  }
  ```
  and implement it for a `FolderSource { root: PathBuf, files: Vec<SourceFile> }` built from today's `scan` (junk filter and sizes unchanged).
- [ ] **Step 2:** `pub fn open(path: &Path) -> Result<Box<dyn SourceTree>>`: a directory → folder; `.exfat` → exFAT (Task 2); `.ffpkg`/`.ufs2` → UFS2 (Task 3); anything else → an error naming the supported kinds.
- [ ] **Step 3:** Refactor `build.rs` to `let tree = source::open(&request.source)?;` and to read icons/`param.json` through `tree.read(...)` so every source goes through one path. `readiness` takes the tree's file list. Behaviour for folders must not change: all existing tests stay green.
- [ ] **Step 4:** Commit — `refactor(fpkg): a SourceTree seam for folder, exFAT and UFS2 sources`.

### Task 2: The read-only exFAT reader

**Files:** `src/exfat.rs`, `tests/mounts.rs`

- [ ] **Step 1: Boot sector and volume offset.** `ExFat::open(path)` finds the volume: exFAT at offset 0, else an MBR partition (type `0x07`, LBA from the entry at `0x1BE + 16n`), else a GPT entry (header at `0x200`, entries at `header + 72`, 128-byte records, first LBA at `+0x20`) whose type is Microsoft basic data. Validate `"EXFAT   "` at `+0x03`, read sector shift (`+108`), cluster shift (`+109`), volume length (`+72`), FAT offset/length (`+80`/`+84`), cluster heap offset (`+88`), cluster count (`+92`), root cluster (`+96`), and the boot region checksum? (optional; skip with a note).
- [ ] **Step 2: Directory walk.** Entries are 32-byte records: `0x85` file (attributes at `+4`), `0xC0` stream extension (name length `+3`, first cluster `+20`, data length `+24`, general flags `+1` bit 1 = NoFatChain), `0xC1` name fragments (15 UTF-16LE code units each). Skip deleted (type bit 7) and unknown types; `0x00` ends a directory. Assemble names, descends into directories, and return `(path, size, first_cluster, no_fat_chain)`.
- [ ] **Step 3: File reads.** NoFatChain → contiguous clusters; else follow the FAT (u32 entries, `heap_offset - fat_offset` delta). Read in cluster-sized chunks into a `Vec` bounded by the entry's data length; never trust the length blindly (cap at the volume size).
- [ ] **Step 4: Safety tests** (synthetic bytes, no fixtures): a truncated boot sector, a bogus cluster count, a directory entry whose name length exceeds the entry count, and a first cluster past the heap each return an error rather than panicking.
- [ ] **Step 5: Real-mount checks** (`tests/mounts.rs`, sample-gated): for each `*.exfat` in the sample folder — the walk finds `sce_sys/param.json`, `eboot.bin` and at least 100 files; `param.json` parses as JSON with a `contentId`; and the total listed size is within 10% of the image's byte length (a cheap sanity bound). Skip `._*` files.
- [ ] **Step 6: Commit** — `feat(fpkg): read-only exFAT source`.

### Task 3: The UFS2 (`.ffpkg`) source

**Files:** `src/ufs2_source.rs`, `tests/mounts.rs`, `Cargo.toml`

- [ ] **Step 1:** Add `ps5upload-pkg.workspace = true`? (the workspace has no entry for it — use a path dependency `ps5upload-pkg = { path = "../ps5upload-pkg" }`). Confirm no dependency cycle (`ps5upload-pkg` depends on neither).
- [ ] **Step 2:** `Ufs2Source::open(path)`: open the image, walk from the root inode with `list_dir`, skipping the junk filter's names, and record each file's inode. `read(path)` = `read_file(inode, len)` with the entry's own size as the cap (check that `cap` is a limit, not a truncation, for files larger than the cap — if it truncates, add a chunked read helper to `ps5upload-pkg` instead of raising a cap).
- [ ] **Step 3:** `tests/mounts.rs`: for `PPSA21159.ffpkg` (skip when absent), the walk finds `sce_sys/param.json` and `eboot.bin`, `param.json` parses, and `eboot.bin`'s reported size matches the stream's inode size. Reading the first 64 KiB of `eboot.bin` succeeds and is not all zeros.
- [ ] **Step 4:** Commit — `feat(fpkg): UFS2 (.ffpkg) source`.

### Task 4: A build from a mount (G2 for sources)

**Files:** `tests/fixtures/mini.exfat`, `tests/mounts.rs`

- [ ] **Step 1:** Build the fixture once, committed to the repo (~2 MiB):
  ```bash
  hdiutil create -size 4m -fs exFAT -volname MINI -layout NONE /tmp/mini.exfat
  # mount it, copy: eboot.bin (16 KiB), sce_sys/{param.json,icon0.png,icon0.dds,about/right.sprx}, data/one.bin (900 KiB)
  # detach
  ```
  The test copies it to a temp path first (never build from a repo file).
- [ ] **Step 2:** The test builds a package from the fixture exactly as `tests/writer.rs` does from a folder, asserts `report.verify.ok()`, and asserts the inner image's file set equals the fixture's tree (plus the generated keystone).
- [ ] **Step 3:** A folder and its exFAT copy of the same tree must produce the **same file list** (sizes included) — the strongest cheap check that both sources see the same tree.
- [ ] **Step 4:** Commit — `test(fpkg): gate G2 for exFAT sources`.

### Task 5: Surface the sources

- [ ] **Step 1:** `readiness` reports the source kind and, for image sources, the volume geometry (cluster size, cluster count) in the report's detail lines.
- [ ] **Step 2:** The CLI's usage line documents the accepted path kinds; a build error names the detected kind and what was expected.
- [ ] **Step 3:** Commit — `feat(fpkg): name the source kind in readiness and the CLI`.

---

## What this plan does not do

- **Build a real 10–75 GB mount.** The writer's ~570 MiB inner-image ceiling (Plan 7) blocks it; the readers are proven against the real mounts and the build path against the small fixture.
- **`.ffpfs` / `.ffpfsc` (PFS image sources)**: not in the user's list; the spec keeps them for later (they need a plain-PFS reader, which is a small extension of what the reader crate already knows).
- **`.exfat` on Windows/Docker with no sample**: the pure reader is platform-independent, so this is covered by construction — but only macOS/Linux mount samples exist to test against today.
- **Engine API/UI** (Plan 6): until then the CLI is the only entry point.
