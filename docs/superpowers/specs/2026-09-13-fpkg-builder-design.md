# FPKG builder, plus the #319 / #320 fixes

Turn a PS5 game folder (and later an `.exfat` or other mount image) into an
installable debug FPKG on the computer, Docker engine, or phone, then hand it
to the existing Stream install. Alongside it, fix the two open install issues
that the same research explains.

Research for this spec: two local debug FPKG samples taken apart byte by byte,
a live key-derivation test against a real third-party package, and the public
format write-ups of LibProsperoPKG (GPL-3.0), MkPFS (GPL-3.0), drakmor/ppr-patch
and drakmor/ampr_emu. **We write our own implementation.** Reference code is
read to understand the format, never copied or linked.

---

## Part A — fixes that ship first

### A1. Stream install sends package bytes as the `.crc` (#319) — CONFIRMED

Every stream install in the #319 bug report that failed with `0x80b211cd` was
preceded by the console requesting `/pkg-host/<session>/<content-id>.crc`, and
no successful install requested one. The route
`/pkg-host/{session}/{filename}` (`pkg_install.rs:408`) ignores `{filename}`, so
the console received the first N bytes of the `.pkg` instead of its CRC table.

The file the console wants is `config/<content-id>/playgo-chunk.crc`, stored
**inside the package**: a debug FPKG ends in a STORED (uncompressed) ZIP, the
SI segment. Verified on both samples: its length is exactly
`4 × ceil(SI_start / 0x10000)` (webbrowser 0x110000 → 68 B; DLC 0x130000 → 76 B).

Fix, in `serve_handler`:
1. If `{filename}` is the session's package name, serve as today.
2. If it ends in `.crc`: find the ZIP end-of-central-directory in the last
   65 KiB of the package (the last part for split packages), locate
   `config/*/playgo-chunk.crc`, and serve that member's byte range straight from
   the file (STORED, so no inflation; Range requests work unchanged).
3. Otherwise a sibling `<stem>.crc` next to the source file, else **404**.
   Never serve package bytes under another name.

Retail packages (signed byte `0x80`) carry an encrypted SI and no plain ZIP;
they get a 404, which is today's retail behaviour minus the wrong bytes.

Tests: a synthetic package with a hand-built SI ZIP (member found; Range
honoured); a package with no ZIP (404); an unrelated filename (404); a split
package whose SI is in the last part.

### A2. Staged PS5 FPKG reports "couldn't verify" (#319)

5.26 sent `package_type=PS4GD` and a content id derived from the staging file
name for a `\x7FFIH` package, so verification looked for a title that does not
exist even though DPI installed it. The uncommitted FIH parser in
`ps5upload-pkg/src/lib.rs` reads the real content id (FIH `+0x58` → embedded
CNT, content id at CNT `+0x40`). Finish it, test it against both local debug
samples, and make sure the staged-install path uses the parsed id.

### A3. Web UI Stream installs from the server library

In the browser build, Stream opens `<input type="file">`, so the file comes from
the viewer's device. Offer the existing `LocalPathPicker` (server filesystem via
`/api/local/*`) and call `runStreamInstall(serverPath, name)` directly. This is
also what lets a `.crc` sidecar next to a server-side package be found.

### A4. #320 helper dies 5–15 s after start

Not root-caused. The crashing request varies (hw power/temps, volumes,
process list, klog), so the cause is shared state rather than one handler.
Plan:
1. Revert `scripts/ps5-sdk.env` to SDK **v0.42** (v0.43 aborts at startup on a
   failed IOMMU heuristic and was verified only on FW 5.10).
2. Ship the uncommitted hardening already in the tree as a **pre-release**:
   fatal-signal breadcrumb naming the in-flight frame, explicit 512 KiB mgmt
   stack, `-lSceIpmi` link order, SoC temperature channels cut to 0 and 2.
3. Ask both reporters for firmware, loader, and a saved bug report taken right
   after re-sending the ELF (so `stderr.log` holds the breadcrumb).
4. Fix the specific cause the breadcrumb names.

---

## Part B — the FPKG builder

### Decisions

| Question | Decision | Why |
|---|---|---|
| Where does conversion run? | Engine side: desktop, Docker, Android (best effort). Never on the console. | Console CPU/RAM limits, 2× storage, and helper stability (#320). |
| Implementation | New crate `engine/crates/ps5upload-fpkg`, pure Rust | Every surface already runs the engine. |
| Output shape | A real `.pkg` file (approach A) | Inspectable and re-verifiable. A virtual on-the-fly package (approach B) can reuse the planner later. |
| Encryption mode | **Native** (AES-XTS, passcode-derived) first; `PlaintextNoAuth` later | Native is what existing debug FPKGs use and installs with kstuff-lite alone. Plaintext also needs drakmor/ppr-patch on the A53 (profiles only 1.00–11.40). |
| Compression | None in v1 (files stored raw) | Raw storage is a valid per-file store rule; Kraken is a large separate project. |
| Default output folder | `~/Downloads/fpkgs` (configurable) | User request. |
| Sony key material | **Built in**, like LibProsperoPKG and PSVIETHOA (decided by the maintainer 2026-09-13), isolated in one `keys.rs` module so it can move to a user-supplied file later | Every existing FPKG builder signs with the leaked publishing keys; an unsigned package's acceptance is unproven. The maintainer accepted the distribution risk. |

### Format facts

**Verified on local samples** (`webbrowser.pkg` 1.1 MB, `EP7579-PPSA17599…` DLC 1.2 MB):

- FIH, little-endian: magic `\x7FFIH`, `+0x05` signed byte `0x00` = debug,
  `+0x06` format 3, `+0x10` PFS offset `0x10000`, `+0x18` PFS size,
  `+0x58` embedded CNT offset, `+0x30` game digest (repeated at `+0x70`/`+0xD0`).
- CNT, big-endian: entry table at `+0x18`, `0x20`-byte records
  `{id, name_off, flags1, flags2, data_off, data_size}`, content id at `+0x40`.
  Debug app entries: `0x0001 0x0010 0x0020 0x0080 0x0100 0x0200 0x040A 0x1001
  0x1200 0x1280 0x2000 0x2010 0x2011` (DLC adds `0x0400`/`0x0401`).
- SI = trailing STORED ZIP: `naps_meta_18/300/301/302/308.dat`, optional
  `pfsimage.xml`, `playgo-chunk.dat`, `config/<cid>/playgo-chunk.crc`.
- **Key derivation works on a real third-party package**: with passcode
  `"0"×32`, `EKPFS = SHA3(SHA3(BE32 1) ‖ SHA3(cid padded to 48) ‖ passcode)`,
  `K = HMAC-SHA256(EKPFS, seed)`, `enc = HMAC-SHA256(K, LE32 1 ‖ seed)`,
  tweak = `enc[0:16]`, data = `enc[16:32]`, seed at outer superblock `+0x370`,
  AES-128-XTS, one 64 KiB block per data unit, sector = block index. Outer
  block 0 decrypted to PFS metadata containing a `\x7FFLT` table.
- Outer superblock found by matching `SHA3-256(block)` against the FIH game digest.

**From reference write-ups, to verify at gate G0:** data-first outer layout
(file data, then superblock, inodes, super-root dirents, FLT, uroot dirents);
signed blocks use sector `bit47 | index`; per-block and dinode hashes are
`SHA3-256(plaintext block)`; superblock ICV at `+0x380`; `imagedigs.dat` is one
32-byte digest per outer block, byte-reversed; CNT digests (`package-digest` at
`CNT+0xFE0`, per-entry table, GeneralDigests) are SHA3-256; metadata is signed
RSA-3072 PKCS#1 v1.5 SHA-256; the inner `pfs_image.dat` is itself data-first
with metadata at the tail and a `naps_pkg_layout.dat` beside it; the FLT path
hash is a reduced-Keccak over the uppercased name.

### Architecture

```
SourceTree ─► Plan ─► InnerImage ─► OuterPfs ─► Cnt ─► Fih + SI ─► Verify
```

| Module | Responsibility | Depends on |
|---|---|---|
| `source` | Walk a root into a sorted tree; junk filter (`._*`, `.DS_Store`, `Thumbs.db`, `desktop.ini`); stat sizes; never read file bodies. Trait `SourceTree` so exFAT and console sources plug in later. | — |
| `readiness` | Report, never block silently: `eboot.bin` present, `param.json` present and no `param.sfo`, content id valid, module magic per executable (fake SELF `54 14 F5 EE`, raw ELF, genuine SELF), `requiredSystemSoftwareVersion` vs. an optional target firmware, `libSceAmpr` imports (needs ampr_emu to launch), free space. | `source` |
| `plan` | Pure layout, no bytes: inode and afid assignment, dirents, FLT, inner block geometry, outer block geometry including indirect blocks, CNT entry list and sizes, SI member sizes, total file size. | `source` |
| `crypto` | EKPFS / XTS / sign-key derivation, AES-128-XTS per block, SHA3-256, CRC-32C, RSA sign with the supplied key. | — |
| `inner` | Stream the inner image: raw file data blocks in afid order, then metadata. | `plan` |
| `outer` | Wrap the inner image and `naps_pkg_layout.dat` in the outer PFS; hash each plaintext block, encrypt, record `imagedigs`. | `plan`, `crypto` |
| `cnt` | Entries (param.json, icons, playgo trio, imagedigs), names, digests, signature. | `plan`, `crypto` |
| `fih_si` | FIH header and digest table; SI ZIP including `playgo-chunk.crc` and `naps_meta_*`. | all above |
| `reader` | Independent parser for any debug FPKG: decrypt, walk both PFS layers, recompute every digest. Used by `Verify` and by tests against real samples. | `crypto` |

`plan` is pure so the layout can be unit-tested without I/O and reused by a
virtual package later.

### Data flow and resource bounds

- **One sequential pass over source bytes.** Sizes come from `stat`, so the plan
  (and every offset) is fixed before the first byte is read. The writer fills
  output at `0x10000 + …` block by block: read ≤ 64 KiB, SHA3 the plaintext
  (dinode signature + imagedigs + inner-image digest), encrypt with
  sector = block index, CRC-32C the ciphertext, write.
- **Metadata written after data**, because data-first layout puts it at the
  tail; its hashes feed the superblock, which is written last.
- **Header region last**: FIH block 0 and the CNT are written after the image,
  then their CRC-32C blocks are computed. The SI ZIP is appended after that
  (excluded from the CRC, as on real packages).
- **Memory flat**: one block buffer per worker, the plan (72k files ≈ tens of
  MB), and the digest tables (32 B × blocks; 300k blocks ≈ 10 MB).
- **Parallelism**: hashing and encryption of independent blocks across a small
  worker pool, with an ordered writer. Correctness first; start single-threaded.
- **Atomic output**: write `<name>.pkg.partial` in the output folder, fsync,
  run `Verify`, then rename within the same folder. Remove the partial on
  cancel or failure.
- **Cancellation** checked per block; **progress** as bytes done / total with
  phase names.

### Engine surface and UI — implemented 2026-09-13 (plan 6)

- `POST /api/fpkg/inspect {source}` → readiness report, planned size, free space. ✅
- `POST /api/fpkg/build {source, output_dir, options}` → job id on the existing
  jobs infrastructure, progress events, final path plus verify summary. ✅ (byte
  progress every 200 ms; `/api/jobs/{id}/cancel` stops it per block; the finished
  job carries the package path)
- Options: passcode (default `"0"×32`), optional firmware-version override for
  `param.json`, key-material location.
- UI: a **Convert to FPKG** screen with source picker (desktop dialog; web UI
  via `LocalPathPicker`), readiness panel, output folder, build progress, then
  **Install** (hands the output path to `runStreamInstall`). ✅ built at
  `/convert` — desktop and Android use `pickPath`; the browser build still takes
  a typed path (the `LocalPathPicker` wiring is the follow-up).
- Key material: an import step storing keys under app data, validated by
  fingerprint; the build refuses with a clear message when a required key is
  missing. — **still open**: v1 builds the keys in (see Decisions).
- Every new UI string goes through `en.ts` and the i18n coverage gate. ✅ 30 keys.

### Sources

| Source | v1 | How |
|---|---|---|
| Game folder | ✓ | Direct walk |
| `.exfat`, any platform | ✓ | Own read-only exFAT reader (raw/MBR/GPT, contiguous and chained streams), no OS mount: `ps5upload-fpkg/src/exfat.rs`. Verified on 27 real mounts. |
| `.ffpkg` | ✓ | The UFS2 reader in `ps5upload-pkg` (its superblock offsets fixed — they read the wrong slots and walked every real image to nothing) wrapped as a source tree. |
| `.ffpfs` / `.ffpfsc` | later | Needs a plain-PFS reader |
| Folder or mounted image on the console | later | `SourceTree` over the existing FTX2 file RPCs |

### What the references say, as of 2026-09-14

Read again after both reference projects moved (PSVIETHOA-FPKG-Builder 2.1.1 → 2.1.6;
LibProsperoPKG commit 748eabf). Nothing here changes our format decisions except the first
item, which was a real bug.

- **`applicationDrmType` must say `standard`.** PSVIETHOA forces it (`Services/ParamJsonDrmSwap.cs`)
  because a package that says anything else "shows a lock on the PS5 and refuses to start"
  (`CHANGELOG.md`). 25 of the 27 real mounts say `standard`, and two — Spider-Man: Miles
  Morales and Death Stranding — say `upgradable`. **Fixed in our build**: the package carries
  `standard`, the source file is untouched (see `source::drm_rewrite`).
- **The nested indirect layout is corroborated** by three independent implementations:
  LibProsperoPKG's *reader* reads slot 1 as a two-level table (`ProsperoPfsReader.cs:314`), its
  inner-PFS *builder* writes `ib[0]` single then `ib[1]` as "signature for block of signatures
  for block of signatures for data blocks" (`ProsperoPfsBuilder.cs:587-611`), and PSVIETHOA's
  bundled engine documents the same escalation (`ProsperoOuterAddressingGeometry`). Two caveats
  to keep for a hardware failure: their publisher puts each file's indirect maps *directly after
  its data* while we cluster them after the uroot dirents (a dinode indexes them, so position is
  free — but it is the first difference to try), and LibProsperoPKG's *outer* writer treats slots
  1–4 as more flat singles and refuses past 570 MiB, calling that reading uncorroborated. A
  ~200 MiB build is therefore the discriminating experiment: it is the range where the two
  readings disagree.
- **Our inner image matches their model of real packages**: LibProsperoPKG's inner reader
  (written against Sony's own `DebugSettings.pkg`) expects a PFS superblock at the metadata base
  with the inode table one block later (`ProsperoPs5InnerImageReader.cs:44-53,170`) — which is
  what we lay out. Their assembler's note about an "inner sblock" two blocks earlier belongs to
  their compressed pipeline, not to the stored image.
- **`.ffpfsc` is a plain PFS v2** whose single file is a PFSC-compressed exFAT image
  (`ExFat/PfsContainer.cs`), so our existing PFS reader plus a PFSC decompressor is the whole job.
- **Kraken blueprint for Plan 8**: signed modules stored verbatim; the keystone whole-block-raw;
  per-file Kraken in 256 KiB logical blocks, kept compressed only when the stream is ≤ 15/16 of
  the source, level 7, with a 16-byte id=5 running-sum signature per block; compressed images go
  in a PFSC container. Their benchmark: 21.3 GB → 8.86 GiB in 2 min 13 s, with an intermediate
  file we would not need.
- **Their exclude masks** (the publishing tools' defaults): names `keystone`, `disc_info.dat`,
  `pfs-version.dat`, `ext_info.dat`; suffixes `.gp4 .gp5 .esbak .dds`. We skip only OS junk, so a
  mount's `eboot.bin.esbak` (153 MB in one real mount) rides into the package. Excluding
  `.esbak` alone is a tempting size win; the rest we keep, because a converted mount should
  carry what the mount carries. **Open question for the maintainer.**

### Verification strategy

Nothing here can be declared working without evidence, so the build proceeds
through gates. A gate failing stops the next one.

- **G0 — reader proves understanding.** ✅ **Passed 2026-09-13** (`ps5upload-fpkg`, `verify_package`): all 40 checks on `webbrowser.pkg`, all 38 on the EP7579 DLC, and a single flipped byte fails. Not yet covered: the inner image (the samples compress it with Kraken), `naps_meta_*`, the RSA signature, the FLT path hash. Our `reader` fully opens both local
  debug samples: decrypts the outer image, walks outer and inner PFS, extracts
  every file, and recomputes **every** digest (game, imagedigs, dinode, ICV,
  CNT per-entry, GeneralDigests, package digest, `playgo-chunk.crc`) with all
  matching. Any mismatch is a format misunderstanding to resolve before writing.
- **G1 — large layout corroborated.** The samples are ~1 MB, so they only
  exercise direct blocks. The reference builder states that only the first
  indirect slot is corroborated (≈570 MiB), while its reader treats slot 1 as
  double-indirect. Minecraft's raw inner image is ~19 GB (~300k blocks).
  **Requires a debug FPKG larger than 600 MB** to confirm the indirect-block
  scheme with the reader. Still open: the writer now emits the single-indirect
  layout (12 direct + five 1820-record slots) from the reference's documented
  coverage, so a large sample would confirm it rather than gate it.
- **G2 — writer round-trip.** ✅ **Passed 2026-09-13** (`engine/crates/ps5upload-fpkg`,
  `tests/writer.rs`): a synthetic tree (eboot.bin, a 600 KiB file, the `sce_sys`
  set, an empty directory) builds into a package whose every check the crate's own
  reader verifies — CNT package digest, header rollup, body digest, per-entry and
  GeneralDigests digests, the finalized-image digest, the outer ICV and every block
  signature, the flat-path table, `playgo-chunk.crc` — and whose inner image walks
  back to the exact source bytes (the generated keystone included). The package's
  `naps_pkg_layout.dat` reconstructs that image block for block. A build missing an
  icon still verifies; a source without a content id is refused without leaving a
  partial file behind.

  Two size facts came out of it. The writer lays out an inner image past twelve data
  blocks with the dinode's indirect tables (1820 `{SHA3, block}` records per 64 KiB
  block, five slots), which covers 12 + 5 × 1820 blocks ≈ 570 MiB of inner image —
  the same coverage the reference documents. Larger sources (Minecraft's 19 GB) need
  the double-indirect slot, which stays unverified. The pipeline refuses a source past that
  ceiling *before* the inner image is built (`build.rs` checks the plan), because a mount
  image would otherwise be read into memory first.

  G2 also covers **image sources**: a package built from the committed 2 MiB exFAT fixture
  (`tests/fixtures/mini.exfat`, formatted by macOS itself and carrying the `._*` sidecars a
  Mac copy leaves behind) verifies and its inner image walks back to the fixture's files.
- **G3 — hardware, small package** (needs your approval at the time): Stream
  install the G2 package on the Pro (FW 9.60) with kstuff-lite 1.12-fpkg loaded.
  Tile appears and the klog shows no PlayGo or mount error. Also try an unsigned
  or placeholder signature to learn whether the RSA key is actually checked.
- **G4 — Minecraft** (`PPSA17221-app`, 19 GB, 72,821 files, backported,
  requires FW 12.70): build into `~/Downloads/fpkgs`, verify, install on the
  Pro. Install success and launch success are separate results; launch may
  need the firmware override and ampr_emu.

Unit tests live next to each module and use the samples as fixtures only when
present (gated, like the existing hardware tests), plus synthetic trees for
`plan` edge cases: empty directories, deep nesting, 12/13-block boundary files,
names needing FLT collision handling, zero-byte files.

### Console-side context (no v1 code)

- **kstuff-lite 1.12-fpkg** hooks the kernel mount path for FPKGs; required to
  install any debug FPKG on a retail console.
- **drakmor/ppr-patch** adds a per-request `PLAINTEXT_NOAUTH` selector on the
  A53 and only matters for plaintext packages. If ps5upload ever deploys it:
  run `--status` first, surface `LEGACY_REBOOT_REQUIRED`,
  `ROLLBACK_REBOOT_REQUIRED` and unsupported-profile results verbatim, and never
  automate `--install --idle`, because only the operator can know PPR is idle.
- **drakmor/ampr_emu** replaces `libSceAmpr`; titles importing it may install
  but not launch without it. The readiness report says so.

### Out of scope for v1

Kraken compression · PLAINTEXT_NOAUTH output · fake-signing raw ELF modules
(ampr_emu's `make_fself.py` and LibProsperoPKG's write-up are the references
for later) · patches, DLC, retail images · `.ffpkg`/`.ffpfs` and in-container
exFAT sources · console sources · deploying console payloads · virtual
on-the-fly packages.

### Risks

| Risk | Mitigation |
|---|---|
| Large-image indirect layout guessed wrong | Gate G1 needs a large debug sample before writing large images. |
| Console checks a key we cannot supply | G3 tests the signature requirement directly; the key-import path covers the case where it is required. |
| `naps_meta_18` / `naps_pkg_layout.dat` values the console validates | G0 reproduces them for the samples exactly before generating new ones. |
| 19 GB output on a volume with 52 GB free | Readiness checks free space against the planned size before starting. |
| Minecraft installs but will not launch | Reported separately; firmware override and ampr_emu warnings in the readiness report. |
