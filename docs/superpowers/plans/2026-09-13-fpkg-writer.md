# FPKG writer (Plan 3) — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Turn a PS5 game folder into an installable debug FPKG on the computer: a `.pkg` that our own reader (Plan 2, `ps5upload-fpkg`) re-opens and verifies end to end. Files are stored raw in v1 (no Kraken). This is gate G2; hardware acceptance is G3 and Minecraft is G4.

**Architecture:** The reader crate grows a write path built from the same primitives: `source` (walk a folder) → `plan` (pure layout, no bytes) → `inner` (the data-first inner `pfs_image.dat` + its metadata region) → `naps` (`naps_pkg_layout.dat`) → `outer` (the encrypted outer PFS) → `cnt` (the `\x7FCNT` container) → `fih` + `si` (finalized image and install-metadata ZIP). `build` orchestrates, writes `<name>.pkg.partial`, verifies, then renames. Tests double as format documentation: every structural constant is checked against the real samples wherever the samples expose it.

**Tech Stack:** Rust 2021; existing crate deps (`aes`, `hmac`, `sha2`, `sha3`, `thiserror`) plus `crc32fast`-free ZIP framing (hand-rolled STORED writer, ~80 lines). No new dependencies.

**Spec:** `docs/superpowers/specs/2026-09-13-fpkg-builder-design.md` — Part B. Reader: `docs/superpowers/plans/2026-09-13-fpkg-reader.md`.

**Research basis (2026-09-13):** measured on `/Volumes/Storage/PS5/pkgs/webbrowser.pkg`, `EP7579-PPSA17599_00-EXP33DLC10000PS5.pkg` and `Crimson.Desert.DLC.Unlocker-DUPLEX.pkg`; cross-read against LibProsperoPKG's format write-up and sources (GPL-3.0) and MkPFS's knowledge base. Fact tags below: **[M]** measured on the samples (authoritative), **[R]** read from the reference, **[H]** inference/hypothesis that still needs a measurement.

---

## Global Constraints

- **Write our own implementation.** LibProsperoPKG / MkPFS / PSVIETHOA are read for understanding only; never copy or link their code. Format *constants* (seeds, tables, key moduli) are facts about the format, not code — they are reproduced as data.
- A test that disagrees with a real sample means the code is wrong, not the sample. Where a fact is [R]-only and a sample can settle it, add the sample check.
- Sample-gated tests read `PS5UPLOAD_SAMPLE_PKGS` (default `/Volumes/Storage/PS5/pkgs`) and skip with an `eprintln!` when absent, so CI without the drive stays green.
- `cargo fmt` after every Rust edit; `cargo clippy --workspace --all-targets -- -D warnings` clean; `cargo check --workspace --locked` after any manifest change.
- Commits stage only the files the task names, one conventional commit per task.
- Key material lives in one module (`keys.rs`) with provenance comments; the maintainer's 2026-09-13 decision is to build it in, isolated so it can move to a user-supplied file later.
- v1 never compresses anything. If a build needs a Kraken encoder to succeed, that is out of scope and the build reports it instead of producing a package.

---

## Verified format facts this plan encodes

### FIH finalized image header (little-endian, block 0, 0x10000 bytes) — [M] both samples

| Offset | Field | Value / meaning |
|---|---|---|
| `0x00` | magic | `7F 46 49 48` |
| `0x04` | constant | `0x01` |
| `0x05` | signed byte | `0x00` debug |
| `0x06` | format u16 | `3` |
| `0x08` | u32 | `1` |
| `0x10` / `0x18` | PFS offset / size | `0x10000` / outer image size |
| `0x20` / `0x28` | superblock absolute offset / block size | e.g. `0x70000` / `0x10000` |
| `0x30`, `0x70`, `0xD0` | game digest ×3 | `SHA3-256(plaintext outer superblock block)` |
| `0x50` | u32 | inner mount data-region base block index (= `metaBase / 0x10000`); the loader reads the inner superblock at `FIH[0x50] * 0x10000` [R] |
| `0x58` | CNT offset | |
| `0x60` / `0x68` | constants | `0x10000` / `0x0000_8000_0000_0000` |
| `0x90` | u32 ×2 | `{inner image block count (= sb block index − 1), inner content-inode count}` |
| `0x98` | u32 ×2 | `{same inode count, content-version echo as 2-3-3 BCD}` (web: `0x01001000` = 01.001.000) |
| `0xA0` | u64 | block-aligned inner-image size (= inner blocks × 0x10000) |
| `0xA8` | u64 | `naps_pkg_layout.dat` length |
| `0xB0` / `0xB8` | digest | `SHA3-256(naps_pkg_layout.dat)` [R], verified by construction |
| `0xF0` | u32 | app-payload (non-`sce_sys`) file count: 2 (web) / 0 (DLC) |
| `0xF8` | u32 | non-empty flat-path-table count: 2 (web) / 1 (DLC) |

### CNT container (big-endian) — [M] three samples

| Offset | Field | Notes |
|---|---|---|
| `0x00` | magic `\x7FCNT` | |
| `0x04` / `0x08` / `0x0C` | constants | `0x20001` / `0x80000000` / `0xC` |
| `0x10` | u32 entry count | 13 (app) / 15 (DLC +license) |
| `0x14` / `0x16` | u16 ×2 | `6` / entry count again |
| `0x18` | u32 | entry-table offset = the METAS entry (`0x100`) payload offset. All three samples: `0x3560` |
| `0x1C` / `0x20` | u32 size / u64 offset | rollup region; `0x18A0` / `0x2000` (app) |
| `0x28` | u64 body size | `0x4E000` (web) |
| `0x30` | u64 mandatory size | the `imagedigs` entry offset (`0x3E20`) |
| `0x40` | content id | 36 ASCII + pad |
| `0x70` / `0x74` / `0x78` | drm / content type / flags | `0`/`0x26`/`0x6020000` app; `0x10`/`0x21`/`0xa020000` DLC |
| `0x7C` | u32 promote size | web `0x50000` = inner image size; DLC `0` [H] |
| `0x80` / `0x84` | version date / hash | `0x20240508`/`0x90FBFC1` (web), `0x20200722`/`0x1FE52E9` (DLC/DUPLEX) |
| `0x100` | rollup digest | `SHA3-256(CNT[off .. off+size])`, off/size from `0x20`/`0x1C` — **[M] verified** |
| `0x140` | digest-table digest | `SHA3-256(entry 0x0001 payload)` |
| `0x160` | body digest | `SHA3-256(CNT[body .. body+body size])` — **[M] verified** |
| `0x200` | content id copy | |
| `0x400` / `0x404` / `0x408` | constants | `1` / `1` / `0xA0000000` |
| `0x410` / `0x418` | u64 ×2 | `0x10000` / outer image size |
| `0x428` | u64 | mount image size = CNT offset + CNT size |
| `0x438` | u32 | `0x10000` |
| `0x440` / `0x460` | digest ×2 | `0x440` = game digest; `0x460` = `SHA3-256(FIH block)` — **[M] verified** |
| `0x4A0` | seed | the outer superblock seed |
| `0x510`/`0x514`, `0x518`/`0x51C` | u32 pairs | image-key entry (offset,size), imagedigs entry (offset,size) |
| `0x520` / `0x540` | digest ×2 | `SHA3-256(image-key payload)` / `SHA3-256(imagedigs payload)` |
| `0xFE0` | package digest | `SHA3-256(CNT[0 .. 0xFE0])` |
| `0x1000` | header signature | 384 B = RSA PKCS#1 v1.5 **public-key encrypt** of `SHA3-256(CNT[0 .. 0x1000])` under the metadata modulus, e=65537 — **[M] verified** (private-key op recovers the digest on all three samples) |
| `0x1180`–`0x2000` | zeros | |

**Entries** (0x20 bytes, big-endian: id, name_off, flags1, flags2, data_off, data_size, 8 pad). Debug app set, in this order:
`0x0001` digests (13×32, own slot zero), `0x0010` entry keys (2944 B), `0x0020` image key (2048 B), `0x0080` general digests (480 B), `0x0100` metas (= the entry table itself), `0x0200` entry names, `0x040A` imagedigs, `0x1001` playgo-chunk.dat, `0x1200` icon0.png, `0x1280` icon0.dds, `0x2000` param.json, `0x2010` playgo-hash-table.dat, `0x2011` playgo-ficm.dat. DLC adds `0x0400`/`0x0401` (license).

Sample body layout (metas entry = the table): `0x2000` keys(2944) → `0x2B80` image key(2048) → `0x3380` general digests(480) → `0x3560` table(416) → `0x3700` digest table(416) → `0x38A0` names → `0x3900` param.json → `0x3E20` imagedigs → `0x3F80` playgo-chunk → `0x4120` icon0.png → `0xAF70` icon0.dds → `0x4B010` playgo-hash-table → `0x4B070` playgo-ficm.

| Entry payload | Formula — all **[M] verified on `webbrowser.pkg`** |
|---|---|
| `0x0010` entry keys | `32 B = SHA3-256(content id padded to 48)`; then 7 × 32 B digests `= SHA3-256(key_i) XOR key_i`; then 7 × 384 B RSA-PKCS#1-wrap of `key_i` under `passcode.bin[i]` (`key_0` wraps the raw passcode ASCII) [R for the key slots] |
| `0x0020` image key | `0x800` B of back-to-back RSA-PKCS#1 wraps of the EKPFS under the mount-image modulus (5 whole wraps + 128 B of a 6th) [R] |
| `0x0080` general digests | `{u16 0xD256, u16 0x0102, 24 B zeros, u32 0x10DE}` then 14 × 32 B slots `{Content, Game, Header, System, MajorParam, Param, Playgo, Trophy, Manual, Keymap, Origin, Target, OriginGame, TargetGame}` |
| — Content | `SHA3(CNT[0x40:0x78] ‖ game-digest ‖ 32 zeros)` |
| — Game / Target | game digest (Target is a copy) |
| — Header | `SHA3(CNT[0x00:0x40] ‖ CNT[0x400:0x480])` |
| — System | `SHA3(SHA3(icon0.png) ‖ SHA3(icon0.dds))` |
| — Param | `SHA3(param.json payload)` |
| — Playgo | `SHA3(SHA3(playgo-chunk.dat) ‖ SHA3(playgo-hash-table.dat) ‖ SHA3(playgo-ficm.dat))` |
| `0x040A` imagedigs | one `SHA3-256(plaintext outer block)` per outer block, each **byte-reversed** |
| `0x1001` playgo-chunk.dat | 416 B `plgx` container (v0x1000, 1 image / 1 chunk / 1 scenario; content id at 0x40; `{0, mchunk0}` at 0x140 and `{mchunk0, mchunk1}` at 0x150 tiling `[0, CNT offset)`) [R; sizes match samples] |
| `0x2010` playgo-hash-table.dat | `{version 1, flags 0x08000000, table off 0x38, table size 8n, "\x7FFLT" @0x18, chunk count @0x24, 16-B constant seed @0x28, n × 8-B constant entries}`; `n = ficm file count / 2` [R; sample 96 B = n 5] |
| `0x2011` playgo-ficm.dat | `{u32 1, u32 0 @0x04, u32 0x10 @0x08, u32 fileCount @0x0C}` + `fileCount` zero bytes [R; sample 26 B] |

### Outer PFS — [M] both samples

- **Fixed 5-inode template.** ino 0 = root dir (`mode 0o40555`, `nlink 1`, `flags 0x2000C`, dirents `inode_flat_path_table` → 1, `uroot` → 2); ino 1 = `inode_flat_path_table` (`mode 0o100555`, `flags 0x2000C`, 96 B); ino 2 = uroot dir (`mode 0o40555`, `nlink 3`, `flags 0xC`, dirents `.`, `..`, `pfs_image.dat` → 3, `naps_pkg_layout.dat` → 4); ino 3 = `pfs_image.dat` (`flags 0xD`); ino 4 = `naps_pkg_layout.dat` (`flags 0xD`).
- **Block order**: `[pfs_image.dat blocks][naps block][superblock (plaintext)][inode table][root dirents][FLT][uroot dirents]` — i.e. superblock at `imageBlocks − 5` for this shape.
- **Superblock** = `ProsperoPfsHeader`: `version u64 0x00 = 2`; `magic u64 0x08 = 20130315`; `Id u64 0x10 = 0`; `0x18` = Fmode 0, Clean 0, ReadOnly 1, Rsv 0; `0x1C` u16 Mode = `0xD` (Signed|Encrypted|0x8); `0x1E` u16 0; `0x20` u32 block size `0x10000`; `0x24` u32 0; `0x28` u64 NBlock = 1; `0x30` u64 dinode count; `0x38` u64 ndblock; `0x40` u64 DinodeBlockCount = 1; `0x48` u64 0; `0x50` inode-table block signature (a `ProsperoDinodeS64`-shaped record: at `0xB8` the 32-B `SHA3(plaintext inode-table block)`, at `0xD8` its u32 block); `0x368` = 1 (no seed) or `0x36C` unknown index + seed at `0x370`; `0x380` ICV = `SHA3(sb[0..0x5A0] with the ICV zeroed)`; `0x5A0` end of the signed region. Both samples' superblocks are byte-identical except `ndblock`, seed, signatures and ICV.
- **Dinode** 0x2C8 B: `mode u16 0x00`, `nlink u16 0x02`, `flags u32 0x04`, `size u64 0x08` (stored size), `size_compressed u64 0x10` (logical size), 4 × `i64` seconds `0x18`, 4 × `u32` nsec `0x38`, `uid 0x48`, `gid 0x4C`, `unk1 0x50`, `unk2 0x58`, `blocks u32 0x60`, 12 × `{sig 32 B, block u32}` at `0x64`, 5 × at `0x214` (36-byte stride; **Plan 2 currently reads `0x1F4` — wrong, unexercised by the samples, fixed in Task 1**).
- **Dirent**: `ino u32, type i32, name_len u32, ent_size u32, name`; `ent_size = align8(name_len + 17)`; types 2 file, 3 dir, 4 `.`, 5 `..`.
- **Flat path table** `\x7fFLT` (96 B in the samples): header 0x40 `{u32 version=1 @0x00, u8 0x10 @0x04, u32 0x40 @0x08, magic "\x7fFLT" @0x20, u32 entry count @0x2C, 16-B constant seed @0x30}` then entries `{u64 hash, u64 packed}` sorted ascending. The 16-B seed is `51 4F A2 26 AB 8A CA 92 4D C4 1B A4 61 B7 BB 09`. The hash is a custom three-lane Keccak-like sponge over the **uppercased path with the leading `/` stripped**, seeded with `0x92ca8aab26a24f51` / `0x09bbb761a41bc44d` and round constant `0x8000000080008081` — **[M] the hash function reproduces both sample entries exactly** (`PFS_IMAGE.DAT → 0xa65627bdd8154701`, `NAPS_PKG_LAYOUT.DAT → 0xc683f67a1dececaf`).
- **XTS**: AES-128-XTS, one 0x10000 data unit per block, sector = block index (data) or `1<<47 | index` (metadata); superblock plaintext.
- **SI**: trailing STORED ZIP, member order `common/etc/naps_meta_18.dat`, `naps_meta_300/301/302/308.dat`, `common/etc/pfsimage.xml`, `common/etc/playgo-chunk.dat`, `config/<content-id>/playgo-chunk.crc`; the CRC covers the 64 KiB blocks of everything before the ZIP [M, already implemented in Plan 2's `si.rs`].

### Inner image (`pfs_image.dat`) — [R] structure, [M] three points of confirmation

The inner image is a **data-first** image: `[file payloads][block-info table][metadata region]`. There is no on-disk block table; the geometry lives in `naps_pkg_layout.dat`. Confirmed on the samples: the on-disk offsets and sizes come from the outer inode (`size` = on-disk, `size_compressed` = logical mount size); the `\x7fFLT` seed + dirent names `inode_flat_path_table` / `apr_flat_path_table` / `afid_to_ino_table` / `uroot` appear inside the samples' images; and the block-info table's variable entry on `webbrowser.pkg` decodes to `0x27373C − 4·(Σ uroot file sizes mod 0x40000)` with `Σ = 29 686` matching the sample's real file sizes exactly.

- **Mount** (the logical image the console mounts; `Ndblock` × 0x10000):
  `[file data at contiguous afid-order offsets][zero padding][metadata region]`.
  `metaBase` = metadata region base = inner superblock offset; must be 256 KiB-aligned so the NAPS u2c entry points straight at the metadata block [R]. The samples pad hard (web: data ends 0xA626, `metaBase` = 0x400000); v1 uses `metaBase = RoundUp(dataEnd, 0x40000)` and records it in `FIH[0x50]`.
- **Metadata region blocks**: `[superblock][inode table][super-root dirents][inode_flat_path_table][apr_flat_path_table][afid_to_ino_table][uroot dirents][each sub-directory's dirent block in pre-order][one empty trailing block]`.
- **Inner inode** 0xA8 B: `mode u16 0x00`, `nlink u16 0x02`, `flags u32 0x04`, `size u64 0x08`, `size u64 0x10`, 4 × `i64` at `0x18`, 4 × `u32` at `0x38`, `0x48 uid`, `0x4C gid`, then the union area: **logical offset u64 at `0x60`**, `0x64` = 0, `0x68` = afid (files) or −1, `0x6C` = parent inode (−1 for super-root children), `0x70` = dirent byte offset within the parent.
- **Inodes**: 0 super-root (dir `0x416d`, flags `0x00020010`), 1 inode FLT, 2 apr FLT, 3 afid table (all files `0x816d`), 4 uroot (dir, flags `0x10`), then directories pre-order, then files (directories post-order, files ordinal by name).
- **Inner superblock** (block 0 of the metadata region): `{i64 2 @0x00, i64 20130315 @0x08, ReadOnly 1 @0x1A, u16 Mode 0x18 @0x1C, u32 BlockSize @0x20, i64 NBlock 1 @0x28, i64 inodeCount @0x30, i64 ndblock @0x38, i64 1 @0x40, u32 BlockSize @0x50/0x58/0x60, u32 0x10 @0x54, times, i64 1 @0xB0, i64 0x89 @0xD8, byte 1 @0x368}` [R] — the outer superblock's field *positions* agree with this at `0x00/0x08/0x1C/0x20/0x30/0x38` [M].
- **Keystone** (`sce_sys/keystone`, 96 B, stored raw, block-aligned before and after): `"keystone"` + `u16 3` @0x08 + `0x01` @0x0A + zeros to 0x20; `@0x20` `HMAC-SHA256(k_keystone_1, passcode ASCII)`; `@0x40` `HMAC-SHA256(k_keystone_2, keystone[0x00:0x40])` [R; the sample's file matches this shape byte for byte].
- **Block-info table**: 0x100 B (32 × `{u32 value, u32 version}` with `version = 0x00400003`), block-aligned between the data and the metadata; 31 entries carry the template `0x00FCFF27` and the last carries the size-derived value above [M, one sample].
- **afid order**: `sce_sys` subtree files (pre-order, name-ordinal), then all other uroot files (name-ordinal).
- **naps_pkg_layout.dat** — [R] format, [M] `webbrowser.pkg` parses exactly (432 B consumed with no slack):
  header 16 B bit-packed `{numFiles−1 :24, compType :2, numKeys−1 :2, numShuffle :4, ublocks :24}{outerBlocks :24, cblockInfo−2 :24}`; sections in order `outer digests (8 B each, all zero in the sample)`, `shuffle patterns (8 B)`, `fidx (6 B: 40-bit LE offset + type; the last entry is `mountSize` with type 0x40)`, `u2c (10 B: u24 base + 7 deltas)`, `cblockinfo (9 B: bit-packed run-base or per-block record — `coffsetStart`, `uoffset`, `clenEven−1`, even/odd flags, KDE predictor (2 = Kraken, 4 = stored), shuffle index; run-bases carry tweak/key/coffsetStart256K)`. In the sample: `numFiles 8 = 5 afids + dataEnd + metaBase + mountSize`, `ublocks 19 = ceil(mountSize/0x40000)`, `outerBlocks 5`, `u2c 4 = ceil(ublocks/8) + 1` [H on the +1].

### Gates

- **G1 — large layout** (unchanged, still open): a >600 MB debug FPKG to confirm outer indirect blocks. Do not build large images before it.
- **G2 — writer round-trip** (this plan): build → our reader verifies every check → inner image walks back to the exact source bytes.
- **G3 — hardware** (user approval required at the time): stream-install a small built package on the Pro (FW 9.60, kstuff-lite 1.12-fpkg); klog clean; tile appears. Also A/B the `CNT+0x1000` signature against a placeholder to learn whether it is checked.
- **G4 — Minecraft** (after G1/G3).

---

## Open questions (each maps to a gate or a task)

| # | Question | Where it is answered |
|---|---|---|
| Q1 | Outer dinode indirect-block offset: `0x1F4` (Plan 2) vs `0x214` (36-byte stride from `0x64`) | Task 1 fixes to `0x214`; G1 confirms on a large sample |
| Q2 | Block-info table entry count (31 template + 1 derived) and whether the count is fixed at 32 | Task 4 emits the measured shape; G3 rejects if wrong |
| Q3 | Is a raw (uncompressed) inner image accepted? | G3 |
| Q4 | Mount padding rule (`metaBase = RoundUp(dataEnd, 0x40000)` vs the sample's 4 MiB jump) | G3; if it rejects, the pads are the first knob |
| Q5 | `FIH[0x94]` (inode count) and `FIH[0xF0]/[0xF8]` semantics | hypothesis encoded; verified structurally on the samples where possible |
| Q6 | Does the console read `pfsimage.xml`? | [R] says no; v1 emits a structurally complete file anyway |
| Q7 | The DLC sample's deviations (24-B naps_meta_300, no block-info table, `FIH[0x50] = 0`) | note only; v1 follows the app-package shape (`webbrowser.pkg`) |
| Q8 | `CNT[0x14] = 6` (sc entry count) and `0x04/0x08/0x0C` constants | copied from the samples |
| Q9 | Whether a built package installs at all | G3 |

---

## File Structure

| File | Responsibility |
|---|---|
| `src/keys.rs` | Built-in key material + provenance comments (public moduli, keystone HMAC keys, naps_meta_18 XTS keys). |
| `src/rsa.rs` | Fixed-size modular exponentiation for PKCS#1 v1.5 public-key encryption (e = 65537). |
| `src/flt.rs` | PS5 flat-path-table hash, entry packing, serialization. |
| `src/plan.rs` | Pure layout: tree → inodes, afids, dirents, FLTs, geometry, CNT entry list. No I/O. |
| `src/inner.rs` | Inner-image writer (data-first, raw) + inner-image reader (for round-trip verification). |
| `src/naps.rs` | `naps_pkg_layout.dat` builder + parser. |
| `src/outer_write.rs` | Outer PFS writer (template inodes, superblock, encryption). |
| `src/cnt_write.rs` | CNT writer (header, entries, digests, signature). |
| `src/fih_write.rs` | FIH header block writer. |
| `src/si_write.rs` | SI ZIP writer (STORED framing) + `naps_meta_18/30x`. |
| `src/build.rs` | Orchestration: source walk, plan, write stages, atomic output, progress, cancel. |
| `examples/fpkg_build.rs` | CLI: `fpkg_build <source-folder> <output.pkg>`. |
| `src/verify.rs` (extend) | New checks from Task 1 (writer acceptance criteria). |
| `tests/writer.rs` | G2: synthetic build → full verify → inner round trip. |

---

### Task 1: Reader hardening — make the reader the writer's acceptance test

**Files:**
- Modify: `engine/crates/ps5upload-fpkg/src/outer.rs`, `src/verify.rs`, `src/cnt.rs`, `src/si.rs`
- Test: `tests/samples.rs`

Everything added here was verified against the real samples during the Plan 3 research on 2026-09-13; these checks are what the writer must satisfy.

- [ ] **Step 1: Fix the dinode indirect offset.** `outer.rs` reads indirect blocks at `o + 0x1F4`; the 36-byte stride from `0x64` puts them at `o + 0x214`. The samples never exercise `ib[]`, so nothing regresses. Add a unit test that builds a synthetic inode table with a populated `ib[0]` and asserts the reader finds it at `0x214`.

- [ ] **Step 2: Add the CNT checks** to `Cnt`/`verify.rs`:
  - `cnt header rollup` — `SHA3(CNT[off..off+size])` with `off = be64(bytes, 0x20)`, `size = be32(bytes, 0x1C)`, compared to `bytes[0x100..0x120]`.
  - `cnt body digest` — `SHA3(CNT[body_offset .. body_offset + body_size])` vs `bytes[0x160..0x180]`.
  - `cnt pfs signed digest` — `SHA3(fih block)` vs `nth(0x460)`; needs the FIH block passed into `verify_package`.
  - `cnt general digests` — content, header, system, param, playgo, target (formulas in the facts table).
  - `cnt descriptor pairs` — `0x510/0x514` = the `0x0020` entry's (offset, size); `0x518/0x51C` = the `0x040A` entry's.

- [ ] **Step 3: Add the flat-path-table check.** Add `flt::hash_path` (the PS5 sponge hash — the writer reuses it in Task 2). Read the outer `inode_flat_path_table` file (inode 1), recompute the hash of each uroot dirent name, and assert each entry's unpacked inode matches the dirent's inode. Test the hash against the sample's two entries.

- [ ] **Step 4: Run the suite.** `cd engine && cargo fmt && cargo test -p ps5upload-fpkg -- --nocapture`. Expected: every check passes on `webbrowser.pkg` and the DLC; add `Crimson.Desert.DLC.Unlocker-DUPLEX.pkg` as a third sample (15 entries) and confirm it passes too.

- [ ] **Step 5: Commit** — `test(fpkg): verify CNT rollup, body, general digests and FLT hashes on real samples`.

---

### Task 2: Key material, RSA public op, flat-path table

**Files:**
- Create: `src/keys.rs`, `src/rsa.rs`, `src/flt.rs`
- Modify: `src/lib.rs`

**Interfaces:**
- `keys::METADATA_MODULUS: [u8; 384]`, `keys::PASSCODE_MODULI: [[u8; 384]; 7]`, `keys::MOUNT_IMAGE_MODULUS: [u8; 384]`, `keys::KEYSTONE_HMAC_1/2: [u8; 32]`, `keys::NAPS_META_18_DATA/TWEAK_KEY: [u8; 16]`, `keys::NAPS_META_18_TWEAK: [u8; 16]`
- `rsa::pkcs1_encrypt(modulus: &[u8], message: &[u8]) -> Vec<u8>`
- `flt::pack_inode_entry(inode, is_dir, is_subtree, afid) -> u64`; `flt::pack_apr_entry(size, afid) -> u64`; `flt::write(entries: &[(u64, u64)]) -> Vec<u8>` (the hash itself landed in Task 1)

- [ ] **Step 1: `keys.rs`** — the moduli from LibProsperoPKG's `Keys/Data/{passcode,mount_image}.bin` and the metadata key (`passcode.bin` slot 3, identical to their `pkg_meta_rsa_key.pem` public half; record its SHA-256 `4a585ee47fea88d6b121d5e8afc9829438a27ee325cebec56a2ff537129a37c5` in a comment), plus the keystone HMAC keys and the `naps_meta_18` XTS key/tweak from their `Util/CryptoKeys` / `ProsperoNapsMeta`. One doc comment stating provenance, the maintainer's decision, and that the module is the single place to swap in user-supplied keys. A unit test hashes each modulus and asserts the recorded fingerprints, so a bad paste fails loudly.

- [ ] **Step 2: `rsa.rs`** — modexp over a 384-byte big-endian modulus with the fixed exponent 65537: PKCS#1 v1.5 `EM = 00 01 FF..FF 00 || message`, `c = EM^e mod n`, 384-byte big-endian output. Implementation: parse the modulus into `u64` limbs, square-and-multiply (16 squarings + 1 multiply), schoolbook with carry. Test with the recorded vector: modulus = the metadata key, `message = 00 01 02 … 1F`, `padding bytes = 0xAA`, expected ciphertext `6de821e8c4db5bae…` (full hex in the test). A second test round-trips: `pkcs1_encrypt` output must decrypt with the reference PEM in a gated test that run only when `PS5UPLOAD_REFERENCE_PEM` is set.

- [ ] **Step 3: `flt.rs`** — the hash (uppercase the path, strip a leading `/`, sponge over 8-byte little-endian words with seeds `0x92ca8aab26a24f51`, `0x09bbb761a41bc44d`, round constant `0x8000000080008081`, three rotate/xor lanes, tail word `tail ^ s0 ^ Seed1`), the packing, and the 0x40-byte header + sorted 16-byte entries. Tests:
  - `hash_path("pfs_image.dat") == 0xa65627bdd8154701`, `hash_path("naps_pkg_layout.dat") == 0xc683f67a1dececaf` (from the real sample's FLT).
  - **Byte-exact reproduction**: build the outer FLT for `["pfs_image.dat", "naps_pkg_layout.dat"]` with packed values 3 and 4, and assert the 96 bytes equal the block the sample's inode 1 points at (`SHA3` must equal `77186728…`, the digest the reader already verifies on both samples).

- [ ] **Step 4: Run** `cargo fmt && cargo clippy -p ps5upload-fpkg --all-targets -- -D warnings && cargo test -p ps5upload-fpkg`.

- [ ] **Step 5: Commit** — `feat(fpkg): key material, RSA public-key wrap and the PS5 flat-path table`.

---

### Task 3: Source walk and pure layout (`plan.rs`)

**Files:**
- Create: `src/source.rs`, `src/plan.rs`
- Modify: `src/lib.rs`

**Interfaces:**
- `source::SourceFile { pub path: String, pub size: u64 }` (path relative to the user root, `/`-separated, `sce_sys/...` for the system tree)
- `source::scan(root: &Path) -> Result<Vec<SourceFile>>` — junk filter (`.DS_Store`, `._*`, `Thumbs.db`, `desktop.ini`, `.Spotlight-V100`, `.fseventsd`, `System Volume Information`), sizes from `stat`, never opens a file body
- `source::readiness(root: &Path, files: &[SourceFile]) -> Readiness` — reports (never blocks silently): `eboot.bin` present, `param.json` present and no `param.sfo`, content id parsed from `param.json`, `sce_sys/icon0.png` and `sce_sys/icon0.dds` present, `sce_sys/about/right.sprx` present, per-module magic sanity
- `plan::Plan { pub files: Vec<PlannedFile>, pub dirs: Vec<PlannedDir>, pub flt_inode: Vec<(u64,u64)>, pub flt_apr: Vec<(u64,u64)>, pub afid_to_ino: Vec<i32>, pub data_end: u64, pub meta_base: u64, pub ndblock: u64, pub metadata_blocks: u64 }`
- `plan::build(files: &[SourceFile], content_id: &str, passcode: &str, build_time: (i64, u32)) -> Plan`

**Design notes:** the plan is byte-free and fully unit-testable, mirroring the reference model:
- inode numbers: 0 super-root, 1 inode FLT, 2 apr FLT, 3 afid table, 4 uroot, dirs pre-order (name-ordinal), then files (dirs post-order, files name-ordinal).
- afids: `sce_sys` subtree files pre-order first, then the rest, name-ordinal; `afid_to_ino = [first file inode, inode per afid, -1, -1]` (i32 LE).
- FLT entries: inode table = every dir except uroot plus every file, packed `{inode :24, dir bit 30, subtree bit 31, afid :40+}` (`0xffffff` value for dirs); apr table = non-`sce_sys` files, packed `{size :40, afid :40+}`.
- inner flags/modes: file mode `0x816d` (`0x8168` under `sce_sys`), dir `0x416d` (uroot) / `0x4168`; inode flags `0x10 | (module ? 0x40 : 0x20) | (sce_sys ? 0x20000 : 0)`; uroot flags `0x10`, other dirs/tables `0x00020010`.
- keystone is generated, not read: `(path = sce_sys/keystone, size = 96)` is appended to the file list if absent, and flagged `generated`.
- geometry: `data_end = Σ sizes` (raw ⇒ on-disk = logical), `meta_base = round_up(data_end, 0x40000)`, `metadata_blocks = 2 + 4 + dirs + 1`, `ndblock = meta_base/0x10000 + metadata_blocks`.

**Tests** (synthetic trees, no I/O): empty-dir-only tree; deep nesting orders; a tree with no `sce_sys`; name-ordinal ordering with mixed case; the web sample's numbers reproduced from a fixture list of the five file sizes + 2 dirs (`data_end = 0xA626`, `meta_base = 0x400000`, `ndblock = 74`, `FIH[0x94] = 7`, `0xF0 = 2`, `0xF8 = 2`) — this pins the geometry model to the measured sample.

- [ ] **Steps:** implement, test, `cargo fmt`, commit `feat(fpkg): source walk and pure layout planner`.

---

### Task 4: Inner image writer + reader

**Files:**
- Create: `src/inner.rs`
- Modify: `src/lib.rs`

**Interfaces:**
- `inner::write(plan: &Plan, read: &mut impl FnMut(&str) -> Result<Vec<u8>>, passcode: &str) -> Result<InnerImage>` where `InnerImage { pub image: Vec<u8>, pub block_info_offset: u64, pub meta_base: u64, pub ndblock: u64, pub afid_offsets: Vec<u64>, pub placements: Vec<(u64, u64)>, pub content_inodes: u32 }`
- `inner::read(image: &[u8], meta_base: u64) -> Result<InnerMount>` — for verification: `InnerMount { pub files: Vec<(String, u64, u64)>, pub flt_ok: bool, pub dirents_ok: bool }`

**Writer steps:**
1. keystone bytes from the passcode (HMAC chain with `keys::KEYSTONE_HMAC_1/2`).
2. On-disk data region: keystone block-aligned before/after; other files pack contiguously, starting a fresh block when a file would straddle one; record each placement.
3. Block-info table (0x100 B, block-aligned): 31 × `{0x00FCFF27, 0x00400003}` then the derived entry `{swap_bytes24(0x27373C − 4·(Σ uroot sizes mod 0x40000)), 0x00400003}` (u32 LE pair per entry). Unit test: the web sample's Σ (29 686) yields the sample's stored value `0x00646725`.
4. Mount: file bytes at their logical offsets, zeros to `meta_base`, then the metadata region blocks (`superblock`, `inode table` with the 0xA8 records, `super-root dirents`, both FLTs, afid table, `uroot dirents`, dir dirents, empty trailing block).
5. `image` = the on-disk stream (data region + block-info table + metadata region) — for v1 this is a byte-identical copy of the mount except the data region is repacked with the block-alignment rule, so state that plainly in the module doc and let the *reader* prove it.

**Reader steps:** parse the inner superblock (version/magic/ndblock/inode count), walk the inode table, dirents from the super-root to uroot, verify each FLT entry's hash against its path, verify the afid table, and return every file's (path, offset, size).

**Tests:** round-trip the synthetic tree (Task 3 fixtures) through write → read and compare file bytes exactly; assert the block-info variable entry; assert `flt_ok`; a 13-block file to exercise multi-block placement; a file that lands mid-block after keystone.

- [ ] **Steps:** implement, test, `cargo fmt`, commit `feat(fpkg): data-first inner image writer and reader`.

---

### Task 5: `naps_pkg_layout.dat`

**Files:**
- Create: `src/naps.rs`
- Modify: `src/lib.rs`

**Interfaces:** `naps::build(plan: &Plan, inner: &InnerImage) -> Vec<u8>`; `naps::parse(blob: &[u8]) -> Result<NapsLayout>` (header counts, fidx, u2c, cblockinfo) for verification.

**Steps:**
1. Header: `numFiles = afids + 3` (afid offsets + `dataEnd` + `metaBase` + `mountSize`), `compType = 2`, `numKeys = 1`, `shuffle = 0`, `ublocks = ceil(ndblock/4)`, `outerBlocks = ceil(inner.image.len()/0x10000)`, `cblockInfo` = the record count.
2. Sections: outer digests (8 B each, zeros — the sample's are all zero), no shuffle patterns, fidx (`{offset :40 LE, type}`, last entry `mountSize` with type `0x40`), u2c (`ceil(ublocks/8)`+1 entries: u24 base + 7 deltas indexing the cblockinfo list), cblockinfo (per 256 KiB ublock: run-base records at each raw file start and at the metadata anchor; per-block records with `coffsetStart`, `uoffset`, `clenEven−1 = stored length − 1`, `kde = 4` for stored, shuffle 0).
3. Write a *parser first* and validate it against the real sample: `naps::parse(webbrowser's naps)` must report `numFiles 8, ublocks 19, outerBlocks 5, cblockInfo 32, u2c 4` and the fidx offsets `0, 0x60, 0x3230, 0x323A, 0x6C98, 0xA626, 0x400000, 0x4A0000`. This is the task's anchor test.
4. Round-trip: build → parse must return the geometry the plan and inner image were built from.

- [ ] **Steps:** implement parser + anchor test, then the builder + round-trip test; commit `feat(fpkg): naps_pkg_layout.dat builder and parser`.

---

### Task 6: Outer PFS writer

**Files:**
- Create: `src/outer_write.rs`
- Modify: `src/lib.rs`

**Interfaces:** `outer_write::write(inner_image: &[u8], naps: &[u8], seed: [u8; 16], time: (i64, u32)) -> Result<Vec<u8>>` returning the encrypted outer image, plus `outer_write::content_id_check`? (no) — and the list of plaintext block digests for `imagedigs`.

**Steps:**
1. Assemble the plaintext blocks: `pfs_image.dat` (inner image blocks) — wait, no: the *inner image* is the file `pfs_image.dat`; its blocks are the outer's data blocks (sector = index), then the `naps` block, then the fixed metadata blocks (`superblock`, `inode table`, root dirents, FLT, uroot dirents) with the measured inode/dirent template.
2. Superblock: the constant template with `dinode_count = 5`, `ndblock`, the inode-table block signature (digest at `0xB8`, block at `0xD8`), `seed` at `0x370`, ICV = `SHA3(sb[0..0x5A0] with 0x380..0x3A0 zeroed)` — recompute; the sample's ICV passes the reader, so a byte-diff against the template is the natural unit test (only `ndblock`, signature, seed, ICV may differ).
3. `pfs_image.dat`/`naps` dinodes: `flags 0xD`, `size` = stored size, `size_compressed` = the logical size (for v1 the same as stored for `naps`; for the inner image it is `ndblock × 0x10000`).
4. Encrypt each block: data sector = index, metadata sector = `1<<47 | index`, superblock plaintext. Record `SHA3(plaintext)` per block for `imagedigs` (reversed at CNT time).
5. Test: write the outer for the Task 3 fixtures and read it back with the existing `outer::open`, asserting every block decrypts to its recorded digest, the superblock ICV holds, and the uroot dirents hold `pfs_image.dat` + `naps_pkg_layout.dat`.

- [ ] **Steps:** implement, test, commit `feat(fpkg): outer PFS writer`.

---

### Task 7: CNT, FIH and SI writers

**Files:**
- Create: `src/cnt_write.rs`, `src/fih_write.rs`, `src/si_write.rs`
- Modify: `src/lib.rs`

**Steps:**
1. `cnt_write`: build the header constants from the facts table; place the 13 entries in a body layout of our choosing (keep Sony's: keys → image key → general digests → the table (as the metas entry) → digest table → names → param.json → imagedigs → playgo-chunk → icon0.png → icon0.dds → playgo-hash-table → playgo-ficm, which reproduces `table = 0x3560` on byte-identical prefixes); then the digests in dependency order: entry digests → table digest (`0x140`) → general digests (needs the game digest and the icon digests) → rollup (`0x100`) → body digest (`0x160`) → package digest (`0xFE0`) → `pfs_signed_digest` (`0x460` = `SHA3(FIH block)`; the FIH block is built first without it, so compute the FIH block inside this task and return it) → header signature (`0x1000`).
2. `fih_write`: the 0x10000 header block from the facts table, taking the outer image (for the superblock offset + game digest), `naps`, the inner geometry and the counts.
3. `si_write`: `playgo-chunk.crc` (CRC-32C per 64 KiB over `FIH ‖ PFS ‖ CNT`), `playgo-chunk.dat` (416 B `plgx`, `mchunk0 = 0x10000`, `mchunk1 = pfs size`, content id), `playgo-hash-table.dat`, `playgo-ficm.dat`, `naps_meta_300/301/302/308.dat` (48 B: `R = innerImageSize − 0x10000` at `0x10`/`0x20`, kind `0x3E9` at `0x18`, `0x10000` at `0x28`), `naps_meta_18.dat` (the TLV blob: `phdr`, `file`, `ibcl`, `i2ob`, `i2op`, `ihsh`, `rhsh`, `fstr`, `twek`, `obdg`, `pgpl`/`pgil`/`pgpi`/`pgpu`, `zero`, each record `{tag reversed, u8 version 1, 3 zero, u64 length, payload}` then AES-128-XTS with the fixed key/tweak as one data unit), `pfsimage.xml` (structurally complete descriptor — the console does not read it [R], so v1 emits it as a deterministic minimal document with the same section names and our own values), all as STORED ZIP members in the sample's order.
4. Tests: `si` member list + `playgo-chunk.crc` recomputation via the reader; the CNT self-checks on a built CNT (package digest, rollup, table digest, general digests); the signature round-trips through `rsa` (encrypt then a Python/PEM-based check only in the gated test).

- [ ] **Steps:** implement, test, commit `feat(fpkg): CNT, FIH and SI writers`.

---

### Task 8: Orchestration and CLI

**Files:**
- Create: `src/build.rs`, `examples/fpkg_build.rs`
- Modify: `src/lib.rs`

**Interfaces:** `build::build(BuildRequest) -> Result<BuildReport>` with `BuildRequest { source: PathBuf, output_dir: PathBuf, passcode: String, content_id: Option<String>, build_time: Option<(i64,u32)>, firmware_override: Option<String> }` and `BuildReport { path: PathBuf, verify: verify::Report, sizes: (u64, u64) }`; `BuildEvent::{Phase(&'static str), Progress { done: u64, total: u64 }}` through a callback.

**Steps:**
1. Pipeline: readiness → plan → inner → naps → outer → CNT (+FIH) → SI → assemble to `<name>.pkg.partial` → `verify_package` on the partial → rename. Delete the partial on failure or cancel. One sequential pass over source bytes: sizes come from `stat`, the plan fixes every offset before the first read, and each file is read once while filling its blocks.
2. CLI example: `cargo run -p ps5upload-fpkg --example fpkg_build -- <folder> <out.pkg> [--passcode …] [--content-id …]`, printing phases and the verification report; exit 1 when verification fails.
3. Tests: an end-to-end run inside `tests/writer.rs` on a temp dir (see Task 9).

- [ ] **Steps:** implement, test, commit `feat(fpkg): package build pipeline and CLI`.

---

### Task 9: Gate G2 — writer round-trip

**Files:**
- Create: `tests/writer.rs`

- [ ] **Step 1:** A synthetic app tree (`eboot.bin` stub, `sce_sys/param.json` with a real content id, `sce_sys/icon0.png`, `sce_sys/icon0.dds`, `sce_sys/about/right.sprx`, one 600 KiB data file and one 100 B file to cross a block boundary, one empty directory) in a temp dir.
- [ ] **Step 2:** Build it; assert `verify_package` returns `ok()` and that the report contains **at least** the checks added in Task 1 (CNT rollup/body/general digests, FLT hashes, all imagedigs, ICV).
- [ ] **Step 3:** Assert `inner::read` recovers every source file's bytes exactly, and `naps::parse(built naps)` agrees with the plan's geometry.
- [ ] **Step 4:** A structural diff against the reference shape: the built package's outer layout must match the sample's block ordering and the superblock template must differ from the web sample's only in `ndblock`, signature, seed and ICV.
- [ ] **Step 5:** Commit `test(fpkg): gate G2 — built packages verify with our own reader` and update the spec's verification section with the G2 result.

---

### Task 10: G3 preparation (hardware) — user-gated

**Files:**
- Modify: `docs/superpowers/specs/2026-09-13-fpkg-builder-design.md` (record the G3 plan), memory note.

- [ ] Build a small installable package into `~/Downloads/fpkgs` (from a real small app tree, e.g. the Minecraft folder's `sce_sys` plus a small module) and a second copy with `CNT+0x1000` zeroed (the placeholder-signature A/B).
- [ ] Ask the user for the hardware test: stream-install on the Pro (FW 9.60) with kstuff-lite 1.12-fpkg loaded; capture the klog; report `0x80b2…` codes verbatim.
- [ ] Record the outcome in the spec and memory either way.

---

## Risks

| Risk | Mitigation |
|---|---|
| The inner-image model is [R]-only (samples are Kraken-compressed, so no byte comparison) | Confirmed where possible (FLT seed/names, block-info value, dense ranges); G3 is the real test; keep the model isolated in `inner.rs` so a correction is local |
| Raw (uncompressed) inner images may be rejected | G3; fallback is the Kraken path, explicitly out of scope for v1 |
| `naps_pkg_layout.dat` values are encoder-defined | Our parser pins the sample's exact values; our builder emits the raw special case; G3 rejects ⇒ first knob to turn |
| Indirect blocks unverified (G1) | v1 targets images under 12 data blocks (768 KiB) plus the metadata; larger sources need G1 first |
| Hardware acceptance depends on kstuff-lite/firmware | G3 with the user; the reference's own packages are unverified on console too |
| Wrong `FIH[0x50]`/metaBase ⇒ console reads the wrong offset | `FIH[0x50] * 0x10000` must equal `metaBase`; asserted in Task 9 and recorded in the report |
