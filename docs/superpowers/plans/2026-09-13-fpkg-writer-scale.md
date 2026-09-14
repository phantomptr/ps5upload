# FPKG writer at game scale (Plan 7) — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** build packages the size of real games — 20–155 GB, up to ~300k files — with **flat memory**, correct **double-indirect** outer layout, streaming verification, and the progress/cancel/free-space behaviour the engine API needs. Today the writer materializes the inner image, the outer image and a CRC copy (≈3× the package) in RAM, and covers only 12 + 5 × 1820 blocks ≈ 570 MiB.

**Architecture:** keep the existing in-memory writers (`inner::write`, `outer_write::write`) as the *test oracle* — they are verified by gate G2 — and add a streaming pipeline beside them. `build.rs` switches to the streaming path; a test asserts both paths emit **byte-identical** packages (pinned seed + time), so the refactor can never drift from the verified format.

**Tech Stack:** Rust 2021, std only (no new dependencies). Cancel via an `AtomicBool`; progress via the existing `&mut dyn FnMut(&str)` extended with byte counts.

**Spec:** `docs/superpowers/specs/2026-09-13-fpkg-builder-design.md` — "Data flow and resource bounds" already describes this design ("one sequential pass over source bytes", "metadata written after data", "header region last"). Plans: `2026-09-13-fpkg-writer.md` (gate G2 passed), `2026-09-13-fpkg-mount-sources.md` (Plan 5 landed).

**Status 2026-09-13: all five tasks implemented** (branch `feat/fpkg-builder`, commits `e5618033` … `ec64a171`). The package is written block by block with flat memory, the in-memory writers remain as the oracle (`tests/scale.rs` asserts the two files are byte-identical for a pinned seed and time), the outer dinode's slots nest past the first one, verification streams, and a build refuses a volume without room. Two defects the oracle test caught are worth naming: a per-block span array capped at four silently dropped file digests (a game's first block holds every small file), and the metric blob's per-file digests needed the same treatment. The `.ffpkg` walk now batches its inode reads: 286,000 files in 0.26 s.

**Measured facts this plan builds on**

| Fact | Value |
|---|---|
| Block order (outer image) | `data 0..ndblock`, then naps, superblock, inode table, root dirents, FLT, uroot dirents, then the indirect tables (`outer_write.rs`) |
| Metadata depends on data only through digests | The inode table's `{SHA3, block}` records and the CNT's `imagedigs` are the only consumers of the data blocks' hashes — so data can stream first, metadata after, exactly as the spec says |
| Sizes at stake | The user's mounts run 30–155 GB (286,075 files in one `.ffpkg`); Minecraft is 19 GB / 72,821 files. Digest tables are 32 B/block ≈ 76 MB at 155 GB — the one thing that legitimately stays resident |
| The G1 gap | No PS5 debug sample > 600 MB exists locally (the big `.pkg` files here are all PS4 `\x7FCNT`), so the double-indirect slot is a **hypothesis** to be confirmed on hardware (gate G3/G4) — it is isolated behind its own module so a wrong guess is one file to change |

---

## Global Constraints

- Gate G2 must stay green after every task: `cargo test -p ps5upload-fpkg` including `tests/writer.rs`.
- The streaming path must produce **byte-identical** output to the in-memory path for a fixed seed/time. That test is the contract.
- Memory is flat in *source size*: no allocation proportional to the package. Allocations proportional to the **file count** (plan, FLT, dirents) and to the **block count** (`imagedigs`, 32 B/block) are allowed and named in the code.
- `cargo fmt` after every edit; clippy `-D warnings` clean; commits stage only the files the task names.
- Write our own implementation; references are read, never copied.

## File structure

| File | Responsibility |
|---|---|
| `src/stream.rs` (new) | The streaming writer: block source, digest table, ordered output, cancel, progress. |
| `src/outer_write.rs` | Gains `indirect_layout()` (single vs double), used by both paths. |
| `src/outer.rs` | Reader walks the double-indirect slot as well. |
| `src/verify.rs` | Streaming verification for packages too large to hold. |
| `src/build.rs` | Uses the streaming writer; reports bytes and phase progress; checks free space. |
| `src/inner.rs` | Grows `block_source(plan, read)` — yields the inner image block by block. |
| `tests/scale.rs` (new) | Byte-identical oracle test; a >570 MiB round-trip; cancel and free-space behaviour. |

---

### Task 1: A block source for the inner image

- [ ] **Step 1:** In `inner.rs`, add `pub fn block_source<'a>(plan, passcode, read) -> Result<BlockSource>` yielding `(&[u8; BLOCK])` per block index: file payloads at their logical offsets (read from the source in ≤ 64 KiB chunks — a single 20 GB file must never be resident), the block-info table block, and the metadata region (built as today by `metadata_blocks`, which is bounded by the file count).
- [ ] **Step 2:** Test: for the existing fixtures, `block_source` assembled into a `Vec` equals `inner::write(..).image` byte for byte.
- [ ] **Step 3:** Commit — `feat(fpkg): an inner image as a block source`.

### Task 2: The streaming writer

**Files:** `src/stream.rs`, `src/build.rs`, `tests/scale.rs`

- [ ] **Step 1:** `StreamWriter::new(path, plan, seed, content_id, passcode, time, cancel, progress)`. It writes, in order: the data blocks (encrypted, sector = index), then naps, superblock, inode table, root dirents, FLT, uroot dirents and the indirect tables, then seeks back to write the FIH block at 0, then the CNT after the outer image, then the SI archive. It returns the same values `build.rs` needs (digests, `OuterImage`-equivalent metadata for the FIH/CNT builders, cnt offset, size).
- [ ] **Step 2:** The CRC for `playgo-chunk.crc` is computed **as bytes are written** (never by re-reading the package); the digest table stays resident and is named in a comment.
- [ ] **Step 3:** `progress` gets a second shape: `build()` takes a `BuildProgress` trait object with `phase(&str)` and `bytes(done, total)`; the CLI keeps printing phases, and cancelling sets the flag.
- [ ] **Step 4:** `tests/scale.rs`: build the same synthetic tree twice — once streaming, once in-memory — with pinned seed/time, and assert the two files are byte-identical.
- [ ] **Step 5:** Commit — `feat(fpkg): stream the package out block by block`.

### Task 3: The double-indirect outer slot

**Files:** `src/outer_write.rs`, `src/outer.rs`, `src/verify.rs`, `tests/scale.rs`

- [ ] **Step 1:** `indirect_layout(inner_blocks)` returns the plan for the dinode's slots: slot 0 covers `PER_INDIRECT` blocks, slot 1 is a **double-indirect** table whose 1820 records each point at a child table of 1820 data records, and so on. Coverage: `12 + 1820 + 1820²` blocks ≈ 200 GiB with two slots.
- [ ] **Step 2:** Implement it in the streaming writer: child tables are written as they fill (the file's block order puts all indirect tables after the uroot dirents), so no table is held in memory. Keep the single-indirect path byte-identical for images under the old ceiling.
- [ ] **Step 3:** Reader: `outer.rs` walks slot *n* for n ≥ 1 as a table of child tables; `verify.rs` checks each child table's digest from its parent's record, and each data record's digest from the CNT's `imagedigs`.
- [ ] **Step 4:** Test with a **sparse** source (700 MiB, so the second level is actually exercised; ~10 s): build, verify with our own reader, and walk the inner image back. Marked `#[ignore]` with a note that it is the G1-corollary check — run explicitly, since the layout is unverified against a real sample.
- [ ] **Step 5:** Commit — `feat(fpkg): double-indirect outer layout for images past 570 MiB`.

### Task 4: Streaming verification

**Files:** `src/verify.rs`, `src/build.rs`, `tests/scale.rs`

- [ ] **Step 1:** `verify_streaming(path, passcode, progress)`: decrypt block by block, hash, compare against `imagedigs`, keep only the metadata blocks the walk needs (superblock, inode table, dirents, FLT, uroot, indirect tables) — a 155 GB package must verify in a few hundred MB.
- [ ] **Step 2:** `build()` uses it for the post-write self-check; `verify_package` stays for the tests and small packages (and the sample gate G0).
- [ ] **Step 3:** Test: both verifiers agree on the fixtures (same check names, same verdicts).
- [ ] **Step 4:** Commit — `feat(fpkg): verify a package without holding it`.

### Task 5: Free space, cancel, and the cheap fixes

- [ ] **Step 1:** Before the first write, compare the volume's free space against the planned package size (+1%) and refuse with the numbers; if the platform cannot report free space, proceed and say so in the report.
- [ ] **Step 2:** Cancel: the partial file is removed, and the error names the phase it stopped in. Test: cancel during the data pass leaves no `.pkg.partial` and no half-written `.pkg`.
- [ ] **Step 3:** Batch the `.ffpkg` inode walk (one read per cylinder group's inode table region instead of one per inode) — 286k reads made a cold walk take 4 minutes.
- [ ] **Step 4:** Commit — `feat(fpkg): free-space check, cancel, and a batched UFS2 walk`.

---

## What this plan does not do

- **Kraken.** Output stays ≈ source size; a 155 GB mount needs 155 GB free and an hour of I/O. Compression is its own project.
- **Confirm the double-indirect layout.** No sample exists offline; hardware (G4, Minecraft 19 GB) is the judge. If the console rejects it, the layout is one module to change.
- **Engine API and UI** (Plan 6) — that follows, and needs Task 2's progress shape.
- **Resumable builds.** A cancel restarts from zero; resuming needs a checkpoint format the console's mount would not see anyway.
