# AVA1 cutover checklist (documentation and tooling)

The `ava1` branch's user-facing docs already describe AVA1. These mentions of
FTX2 or ports 9113/9114 name tooling or settings that still exist on the branch
and change when that code does. The cutover release (project 3) ships only when
every box is ticked and `git grep -i ftx2 -- '*.md' ':!CHANGELOG.md'` is empty.

The CHANGELOG keeps its FTX2 entries: they describe releases that shipped FTX2.

- [x] `README.md` "Test" section — "in-process mock FTX2 server" → the AVA1 mock/host-C tests
- [x] `CONTRIBUTING.md:45` — "mock-FTX2 integration tests"
- [x] `engine/README.md` — `ps5upload-tests` row ("mock FTX2 server"); dev commands using `:9113` / `:9114`
- [x] `TESTING.md` — `PS5_ADDR=…:9113`, `make validate` waiting for `:9113`, curl examples with `:9114`
- [x] `tests/README.md` — "full FTX2 stack", `PS5_ADDR` default `:9113`, `--ps5-addr` description
- [x] `tests/lab/README.md` — `:9113`/`:9114`, `ftx2_control.py`, `ftx2_probe.py`
- [x] `bench/README.md` — `run-ftx2-upload.mjs`, `check-ftx2-baseline.mjs`, `ftx2-upload-main.json` baselines, `--ps5-addr=…:9113`
- [x] `FAQ.md` — `FTX2_ZIP_RAM_THRESHOLD_MB` and `FTX2_ARCHIVE_STAGE_MB` environment variables (P3 Task 17: renamed to `PS5UPLOAD_ZIP_RAM_THRESHOLD_MB` / `PS5UPLOAD_ARCHIVE_STAGE_MB`; the engine still reads the old names once per process with a deprecation line, and both settings are accepted but change nothing now that archives stream)
- [ ] `MGMT_METHODS.md` — every row `hw-verified` (or `n/a` for a retired frame) on both consoles
- [x] In-app strings (`client/src/i18n/locales/*.ts`) that mention FTX2, ports 9113/9114 or "transfer port" (Task 20; pinned by `client/src/i18n/noLegacyPorts.test.ts`)

Release gate: the engine's `auto` mode must not ship before the Task 28 hardware pass.

# Project 2 hand-off: what the FTX2 removal needs

The removal itself is a later, separate change; this list is its prerequisite, not its execution.
Project 2 (the AVA1 data plane, `SPEC.md` §11–§16) ships beside FTX2 and deletes nothing. The boxes
above belong to the project 3 cutover release and stay unticked until it ships. Everything below is
checkable against the tree at the commit that adds this section.

## 1. FTX2 call sites that still exist

Find them again with `git grep -n -i ftx2 -- engine client/src payload`, then
`git grep -n "use_ava1\|route::mode"` for the routing seam.

**Engine handlers with an FTX2 branch** (`engine/crates/ps5upload-engine/src/lib.rs`). Each decides
`use_ava1` and otherwise runs the `ps5upload_core` path; the FTX2 branch is what is deleted.
- Routing and startup: `route::use_ava1` calls at 1972, 2045, 5072, 5378, 5930, 7909, 8260, 8523,
  9062; the startup line at 9616–9631; imports of `transfer::*` at 103–107 and `FrameType` at 70.
- Uploads: `transfer_file_handler` 4880 (FTX2 call 5082), `transfer_dir_handler` 5139 (5388),
  `transfer_zip_handler` 5739 (FTX2 closure 5917, also the fallback for zip entries above 256 MiB),
  `transfer_file_list_handler` 7730 (7920), `transfer_dir_reconcile_handler` 8754 (9080).
- Archives with no AVA1 path at all: `transfer_7z_handler` 7303 (7443), `transfer_rar_handler` 7529
  (7667), the inspect/plan calls at 5662, 6079, 7502, 7551.
- Downloads: `transfer_download_handler` 8175 (the FTX2 enumeration at 8295),
  `transfer_download_zip_handler` 8470 (8543, `download_to_zip_ex` 8612).
- Console file operations: `ps5_fs_move` 1935 (the same-drive rename is still an FTX2 management
  frame; only a cross-mount refusal becomes an AVA1 job), `ps5_fs_copy` 2016 (FTX2 `fs_copy_robust`
  at 2048).
- FTX2 tuning environment: `FTX2_INFLIGHT_SHARDS`, `FTX2_INFLIGHT_BYTES`, `FTX2_PACK_SIZE`,
  `FTX2_PACK_FILE_MAX`, `FTX2_BANDWIDTH_MBPS` (114–150), `FTX2_ZIP_RAM_THRESHOLD_MB` (1395, 5817).
- Routing code: `engine/crates/ps5upload-ava1/src/route.rs` (the whole `Mode` seam),
  and `PS5UPLOAD_TRANSFER` in `engine/crates/ps5upload-lab/src/bench.rs:2206`.

**Engine core** (`engine/crates/ps5upload-core/src`): `transfer.rs` (the FTX2 transfer pipeline, 7z and
RAR streaming), `download.rs`, `connection.rs` (FTX2 framing), `fs_ops.rs` and about twenty management
modules that speak FTX2 frames to :9114 (`hw.rs`, `smp.rs`, `notif.rs`, `users.rs`, `saves.rs`,
`volumes.rs`, `system_control.rs`, `sys_time.rs`, `remoteplay.rs`, `process_mgr.rs`,
`payload_lifecycle.rs`, `fan_curve.rs`, `backup.rs`, and the rest of `grep -l -i ftx2`). These are the
management RPCs below: they cannot go until AVA1 carries them.

**Crates and tests**: `engine/crates/ftx2-proto` (used by core, engine, bench, lab and tests), the mock
server and FTX2 integration tests in `engine/crates/ps5upload-tests/tests/` (`mock_server/mod.rs`,
`transfer_integration.rs`, `transfer_zip_integration.rs`, `transfer_7z_integration.rs`,
`hw_integration.rs`), `engine/crates/ps5upload-bench`, and the `--proto ftx2` arms of
`engine/crates/ps5upload-lab/src/bench.rs` (about 70 references; keep them until the last FTX2
measurement is no longer needed, then delete).

**Client** (`client/src`): `state/connection.ts:49-96` (the :9113 transfer-port probe and its comments),
`lib/addr.ts:20`, `api/ps5.ts:145`, `screens/Upload/index.tsx:1100`, `lib/uploadEta.ts:21`,
`lib/keepAwakeHold.ts:10`, `state/activityWiring.ts:323`, and the strings `About/index.tsx:53` /
`i18n/locales/*.ts` (`en.ts:35`, `en.ts:435`, and the translations of each).

**Payload** (`payload/`): `src/runtime.c` (about 770 references: the frame types from line 103, the
transaction table and its journal files, the spool, the transfer server loop at 16577 and the
management loop at 16876), `src/main.c` (86, 529, 679), `src/takeover.c:129`,
`include/config.h:20-34` (`PS5UPLOAD2_RUNTIME_PORT` 9113, `PS5UPLOAD2_MGMT_PORT` 9114,
`PS5UPLOAD2_TX_DIR`, `PS5UPLOAD2_SPOOL_DIR`), `include/runtime.h:141`, `include/wake_watchdog.h:35`.
The FTX2 journal directories are `/data/ps5upload/tx` (`tx_<id>.json`, `runtime_tx_state.txt`,
`events.log`) and `/data/ps5upload/spool` (`spool_<id>/<shard>`), created at `runtime.c:1398-1401`;
the cutover payload removes both on first start. AVA1's own state is `/data/ps5upload/ava` and is
kept.

## 2. Checklist

- [ ] 7z and RAR uploads still run on FTX2. They need sequential AVA1 sources (the decoders are
      forward-only, so the random-access `Source` of `SPEC.md` §10 does not fit) before FTX2 can be
      deleted.
- [ ] Zip entries above 256 MiB (`ZIP_MAX_ENTRY`, `ps5upload-ava1/src/upload.rs:36`) fall back to
      FTX2 (`ZipTooLarge`); they need a streaming entry reader.
- [ ] Management RPCs: every :9114 FTX2 frame the engine core sends (list above) needs an AVA1
      method. `SPEC.md` §7.1 defines only 1–3 and 16–19. This is project 3's main work.
- [ ] Task 9 leftovers: `net.speedtest` now measures round trips on the shared AVA1 session (gate and
      pool included), so its numbers are not comparable with the FTX2 one-connection figures; the AVA1
      event log has no line for a `job.copy` ending (only upload/download receivers and peer-ended
      senders log); a clamped `log.syslog` tail is a note plus the newest 256 KiB, the older kernel text
      is not reachable (FTX2 sent up to 1 MiB).
- [x] A failed multi-chunk `fs.write` (`ps5upload-ava1/src/mgmt.rs`, `write_chunks`) removes its `<path>.ps5upload.tmp` best-effort with a
  `job.run` DELETE (only after a chunk was accepted); if that fails too, the next write of the path truncates it (offset 0).
- [x] Archives AVA1 cannot stream (P3 Task 17 follow-up): a zip, 7z or RAR the AVA1 sources refuse used to be
      handed to the FTX2 pipeline. That fallback lost nothing: the FTX2 path decoded with the same crates
      (`zip`, `sevenz-rust2`, `unrar`) and refused the same inputs (encryption, unsupported methods, unsafe or
      duplicate paths, a 7z solid block with directories between its files). The engine now fails the job with
      `zip_unsupported`, `7z_unsupported`, `ava1_7z_unsupported_layout` or `rar_unsupported`; the client maps
      each to a message that says to extract the archive and upload the folder (`humanizeJobErrorReason`, all 20
      locales), and auto-recovery treats them as terminal.
- [x] File lists with destinations outside the upload root (P3 Task 17 follow-up): one AVA1 job has one root
      (SPEC.md section 11.2), so the list is split into the root's job plus one job per other destination
      directory, run in sequence under one call (`upload_list_in`): progress aggregates, a cancel stops the
      rest, a failure names the first failing path.
- [ ] `ps5_fs_move`'s same-drive rename moves to an AVA1 RPC with the `st_dev` guard (never an
      unguarded `rename()` across mounts: that panics the console's kernel).
- [x] NAS sources: `SourceFs` now has an `mtime` (SMB, FTP and SFTP report one; a backend that does
      not reports unknown), carried into the manifest. `upload::apply_existing_policy` picks
      `skip-existing` when every file has an mtime and `verify` (roots in the manifest) otherwise
      (`SPEC.md` §11.4). The engine's Resume strategy (`/api/transfer/dir-reconcile`, mode `fast`/`safe`) on an AVA1 console now runs
      the folder upload with that choice (`upload_dir_skip_existing`; `fast` = size+mtime or the verify fallback,
      `safe` = always verify), local or remote source; tests: `ava1-ctest/tests/nas_skip.rs`.
- [x] One session per identity (`SPEC.md` §8): two engines sharing an identity file (a copied
      `<data dir>/ava/identity`, a shared data directory, a Docker engine mounted on the
      desktop's directory) evict each other's console session. Give each engine its own data
      directory. The engine warns ("another ps5upload engine using the same identity is
      connected to this console") when a session is superseded 3 times in 120 s.
- [x] Zip downloads resume on a reconnect (review 003 section 5, P3 Task 15): the archive is Stored
      by default and resumes mid-entry from the receiver's journal (`SPEC.md` section 10.1). The
      optional Deflate archive cannot resume and restarts from zero on a drop.
- [x] Zip resume restart window (review 005 section 5; review 006 #3): done in be78641a and 0f45ceeb.
      `StoredZipSink::position` accepts a file
      whose every byte is durable (`x == size`) as in flight, and `commit(id)` writes its descriptor,
      so the archive resumes instead of restarting. Finished entries' data is trusted from the
      journal (only their headers and descriptors are re-read); the in-flight entry's durable
      groups are re-hashed by the receiver's resume check. Checked against the 006 guide 04 in
      this pass: `position` accepts `x == size` as in flight and rejects `x > size`, `commit` writes
      the descriptor, a re-sent file is `zip_restart`, and 0f45ceeb extends it to several whole files
      without a `Done` (so a hole or a second prefix is still refused); the unit tests
      `a_whole_but_unfinished_file_resumes_and_commit_writes_its_descriptor`,
      `several_whole_files_*` and `a_hole_or_a_second_prefix_is_still_refused` cover every case the
      guide names. Nothing missing.
- [ ] A failed download into an existing folder leaves its per-file `.ava-part` behind; cleanup is
      only done for new destinations.
- [ ] The engine never removes `<data dir>/ava/jobs/*` or `<data dir>/ava/send/*` (`SPEC.md` §14.3
      says a node removes a job directory 7 days after its last write; only the console does so, in
      `payload/src/ava1_glue.c:192`). `ava1::journal::gc` exists and has no caller in the engine.
- [x] The Upload screen shows the transfer's `bottleneck` (Task 20: `screens/Upload/Bottleneck.tsx`; see
      "Client contract" below for the fields it reads).
- [x] PS5 → PS5 UI wiring (Task 20: "From another PS5" on the Upload screen, `screens/Upload/Ps5ToPs5.tsx`).
- [x] `PS5UPLOAD_TRANSFER` (P3 Task 17): `route.rs`, the variable and the `Mode` seam are deleted and every engine call site is AVA1 only. A console with no AVA1 listener or an older helper fails with `helper_not_ava1`; one that has not accepted this app fails with `not_paired`. The startup line is now `ava1: dir=<ava_dir> identity=<key prefix> paired=<n>`; each transfer still logs `protocol=ava1`. The benchmark harness calls each protocol directly and no longer cross-checks the variable.
- [ ] The payload's FTX2 journal directories (`/data/ps5upload/tx`, `/data/ps5upload/spool`) are
      removed by the cutover payload on first start.
- [ ] Engine tests that stub or assert FTX2 (list in section 1) are replaced by their AVA1
      equivalents, and `git grep -n -i ftx2` over engine, client and payload is empty except for the
      CHANGELOG.
- [ ] Review 002 L1, deferred: a RAR entry's mtime comes from a DOS local time read through the
      host's time zone with a "more than a day in the future is none" rule, and the mtime is part of the
      manifest hash. A resume after a host time-zone change, or after such a stamp comes within a day of
      now, no longer matches its journal and restarts that upload. Rare and only costs a restart; fix
      by recording the plan-time mtimes in the job, or by dropping RAR mtimes from the hash.
- [ ] Review 002 L3: the sender's tick reads the decode thread's budget-wait counter every tick (fixed
      after review); the call site has no test of its own, only the tracker and the decoder park do.

## 3. Release gates

- [x] **AEAD nonce/counter audit** (review 006 #1, release gate): PASS, `protocol/ava1/AUDIT-nonce.md`.
      Counter ceiling on both stacks, no mid-stream re-key, resends are resealed on the new lane.
- [x] **Receiver progress watchdog** (review 006 #2): a sender that pings but sends no data is ended
      with `ERR_STALLED` after 3 x `dead_after` (`SPEC.md` §12.8), on the engine receiver and the
      console receiver. The optional sender-side source-read deadline was not done (a blocking read
      cannot be cancelled; the receiver guard closes the hang).
- [ ] **Hardware pass on both consoles at the release commit.** The numbers below are from a mix of
      commits: the Pro's part 2 ran at `a0afe3d1`, the Phat's part 1 at `4876e512` (before the
      download fix `b1be8059`), and the Phat never ran part 2. Re-run the whole table on both, on all
      three drives each (Pro: `/data`, `/mnt/usb0`, `/mnt/ext1`; Phat: `/data`, `/mnt/usb0`,
      `/mnt/ext0`).
- [ ] **No release from this branch until the §3 gates are green; the last FTX2 release remains the
      shipped version until then** (review 007 #2, option a). AVA1 is the only transport in this branch
      (there is no in-branch fallback or kill-switch), so the hardware pass below is a hard gate, not a
      default to flip: users stay on the last FTX2 release, and nothing is published from here until every
      box in this section is checked on one commit.
- [ ] **The Pro outage of 2026-10-03 is investigated.** At about 09:10 the Pro stopped answering ping and
      every port shortly after an instrumented helper (extra stderr timing lines only) was sent;
      the last keep-awake acknowledgement was 09:10:50. Cause unknown: the console may have gone to
      rest mode, or an AVA1 helper path may have panicked the kernel (see the cross-device rename
      and ShellUI ptrace incidents). After the console is powered on, read `/data/ps5upload/stderr.log`
      and the console's own crash notice, then repeat the instrumented run. No release until this has
      an explanation.
- [ ] **Opus re-review** (the subagents' Opus weekly limit resets 2026-10-07 11:00 PT; reviews since the
      limit was hit ran on Sonnet). Second pass on: crypto and key handling
      (`payload/ava1/ava1_aead.c`, `ava1_chacha_avx2.c`, `ava1_noise.c`, `ava1_keys.c`, the engine's
      `handshake.rs`, `keys.rs`, `launch.rs`); console code that touches the filesystem and the
      kernel (`ava1_apply.c`, `ava1_recv.c`, `ava1_data.c`, `ava1_copy.c`, `ava1_send.c`, and the
      open-file budget commits `1fdc3849` and `980d96c2`); the Codex-session commits `18d18e2c`,
      `1d1541c9`, `cca7b988`, `543205b5`, `9a3d934e`, `3d9b6344`, `966eb8af`; and the whole-branch
      diff.
- [ ] Code items the SPEC states and the code does not yet do (see the Task 29 report): the console
      receiver's `ERR_CREDIT` handling ends the lane, not the session (`ava1_data.c:1288`);
      (`Resume` credit: done, see review 007 #7 in `EVAL-007.md`: both receivers re-send the grant as a
      `Credit`, `SPEC.md` §11.5, pinned by `wire_upload::a_resume_after_a_dropped_session_sends_credit_and_the_job_completes`
      and `data_rust` `resume`);
      the engine's `LocalSink` re-hashes more than the console does on resume (allowed, see §13.4).
- [ ] The workspace gate is green on the release commit: `cargo fmt --check`, `cargo clippy --workspace
      --all-targets -- -D warnings`, `cargo test --workspace`, `cargo test -p ava1-ctest -- --test-threads=1`,
      `make test-ava1-sanitize` (the ctest suite under ASan + UBSan, review 009 #2), `cargo check --locked`,
      the client lint and vitest, `make ava1-fuzz-c`.
      Two Docker-specific engine wording tests are excluded on Linux today.

## 4. Measured results (Task 28, 2026-10-03)

Medians of warm runs, every run verified. "AVA1" is the committed code at the commit named; "FTX2" is
the same corpus through the existing path. Corpora: **tiny** = 2,000 files of 1–64 KiB (64.5 MiB);
**large** = one 4 GiB file; **ppsa** = 223,000 files (the PPSA01342-shaped mix). The Pro is
192.168.86.100, the Phat 192.168.86.99, both firmware 13.60. Ranges are min–max across runs.

Pro, part 2 at `a0afe3d1` (download and copy) and part 1 at `4876e512` (uploads, resume):

| scenario | /data AVA1 | /data FTX2 | usb0 AVA1 | usb0 FTX2 | ext1 AVA1 | ext1 FTX2 |
|----------|-----------|-----------|-----------|-----------|-----------|-----------|
| 4 GiB upload (MB/s) | 91–104 | 106–110 | 97–108 | 110–113 | 88–105 | 105–108 |
| 2,000 tiny upload (files/s) | 291 | 320–343 | 533–539 | 393–396 | ~380 | ~543 |
| 2,000 tiny download (files/s) | 1,160 | 2,588 | 1,905 | 2,177 | 1,979 | 2,565 |
| console copy, 2,000 tiny (files/s) | 296 | 166 | 463 | 332 | 381 | 332 |
| resume 4 GiB after a helper kill (MB/s) | 90–106 | 66–67 | 99–106 | 57–59 | 88–97 | 58–61 |
| 4 GiB with the link cut every 10 s (MB/s) | 79.7 | not run | 80.6 | not run | 71.0 | not run |

- Download before the fix `b1be8059` was 140–297 files/s on `/data` (about 10x slower than FTX2). After
  it, an interleaved AVA1/FTX2 pairing of the same corpus gave 1,605/1,979, 1,829/2,435, 1,869/2,012 and
  1,606/1,620 files/s: AVA1 is 75–100% of FTX2 on the Pro, and one run to run drift of about 25%
  moved both protocols together. The 1,160 vs 2,588 line above did not reproduce.
- Tiny upload is bounded by the console's file-create rate, not by the protocol: `disk.calibrate`
  (4 KiB files at 1/2/4/8/16 workers) measured `/data` 182–188 / 250–270 / 279–285 / 284–295 /
  287–298 files/s, usb0 502 / 575 / 573 / 573 / 574, ext1 282 / 378 / 404 / 405 / 407. FTX2 reaches
  287 files/s on `/data` already. AVA1 pays for its durability (fsync per batch): -13% on
  `/data` and -30% on ext1, +36% on usb0.
- Large files: AVA1 is 5–8% below FTX2 (encryption and lane overhead); investigate before the release.
- Console copy (on the console, no network): AVA1 is +78% on `/data`, +40% on usb0, +15% on ext1.
- 223,000-file upload to `/data`: AVA1 completed and verified it at 82.5 files/s (2,702 s); FTX2 failed
  after 611 s on all four streams (`read frame header: Resource temporarily unavailable`) and could
  not clean its partial tree (the packed-shard file-count cliff). At game scale AVA1 is the only one
  that finishes. The 82.5 files/s is 3.5x below the drive's measured ceiling and did not reproduce
  on loopback (flat 3–6k files/s); the corpus's 20,075 directories and 1,641 files above 256 KiB are
  the likely cause. The helper now prints a per-job stats line every 10 s to find it.
- Real games over AVA1 to `/data` (cold, single run, verified): Minecraft PPSA17221, 35,260 files /
  1.33 GB in 246.7 s (142.9 files/s); Worms PPSA20052, 10,230 files / 2.64 GB in 62.2 s (42.4 MB/s);
  Minecraft Legends PPSA05510, 489 files / 7.64 GB in 79.4 s (96.3 MB/s). FTX2 runs of the same
  games were still in progress when the Pro went down and have no recorded result.

Phat, part 1 at `4876e512` (the download rows predate `b1be8059`; the Phat never ran part 2):

| scenario | /data AVA1 | /data FTX2 | usb0 AVA1 | usb0 FTX2 | ext0 AVA1 | ext0 FTX2 |
|----------|-----------|-----------|-----------|-----------|-----------|-----------|
| 4 GiB upload (MB/s) | 104–111 | 110–111 | 9–15 | 34–35 | 95–104 | 107–108 |
| 2,000 tiny upload (files/s) | 236–243 | 268–277 | one run, 27 | none completed | 600–671 | 426–442 |
| 2,000 tiny download (files/s) | 119–201 | 2,047–2,501 | 124–149 | 426–2,696 | 111–328 | none recorded |
| resume 4 GiB (MB/s) | 99–109 | none completed | 15 (one run) | none completed | 93–99 | not run |

The Phat's usb0 numbers are far below the Pro's for both protocols and its AVA1 large-file upload is a
third of FTX2's there; that is unexplained and is a reason to re-run the Phat before the release.
FTX2 resume failed in every Phat run (the harness did not wait for the helper's ports after a
restart; fixed in `a67ea278`, re-run pending on the Phat).

### 4.1 Benchmark knobs, bottleneck line and the lane-path changes (review 003)

Engine environment, for benchmarking only (they override the governor; never set them in
production):

| variable | effect |
|----------|--------|
| `PS5UPLOAD_AVA1_LANES=n` | pin the lane count at n (1-8) for the whole job |
| `PS5UPLOAD_AVA1_CHUNK=m` | pin the chunk at m MiB (1-15) for the whole job |
| `PS5UPLOAD_AVA1_LANES_FIRST=0` | turn the lanes-before-chunk governor policy off (default on) to A/B it |

Matrix to run on the Pro and the Phat (4 GiB to `/data`): lanes in {2, 4, 8} x chunk in {1, 4, 15}, plus
`crypto.bench mib=64` and `disk.calibrate` for the drive. Every job now ends with one engine stderr line:

    [ava1] job bottlenecks over N ticks: credit-starved X%, source-starved Y%, receiver-bound Z%; receiver reported disk; avg lanes L, avg chunk C MiB

Copy that line, `crypto.bench` and `disk.calibrate`'s `create_us`/`fsync_us` into each row so a result says
where the time went. Rows to fill (not measured yet; the consoles were reserved):

| lanes x chunk | MB/s | credit-starved | source-starved | receiver-bound | receiver reported |
|---------------|------|----------------|----------------|----------------|-------------------|
| (pending hardware) | | | | | |

Landed in code, all host-tested, none yet measured on a console: 4 MiB socket buffers on every lane
(both ends, effective sizes logged once: the engine on stderr, the console in its log), a frame-buffer
pool on the console (1/4/8/15/16 MiB classes for class-sized chunks, every other size allocated exactly, so live frame memory is the credit window's byte count; idle pool memory capped at the admit budget), a single copy per
sent frame on the engine (the in-flight frame is shared with the writer, which seals into one
buffer), the lanes-first governor policy (default on, to A/B), and `dead_after` 12 s with the ping
at 2 s.

Deferred: decrypt off the lane reader thread (03 section 2) waits for the matrix above; the
`open_us_per_mib` receiver hint (03 section 5) is optional and also waits.

## 5. Payload lifecycle and the migration shim (Task 8)

Three starting situations, and who handles each:

1. **Older helper (any release up to v5.41, old protocol on 9113/9114 only) running, new app.** The
   engine's `legacy_helper` shim (`engine/crates/ps5upload-engine/src/legacy_helper.rs`) recognises
   it by its old-protocol `Hello` reply: a build from before the cutover names no AVA1 port, a new
   build does (`"ava1_port"`, `"ava1": "starting" | "up" | "failed"`). It sends the old `Shutdown`,
   waits up to 10 s for both ports to close, sends the stamped helper to :9021 (trust slot and launch
   token: no pairing code) and waits up to 20 s for :9120. It never sends the new helper over a live
   old one.
2. **New payload loaded while an older helper is alive** (an autoloader, another sender). The new
   payload's `legacy_takeover.c` sends the old takeover request to loopback 9114 (9113 for a
   single-port build) and waits up to 10 s for the ports to free. If the old instance is AVA1-era
   (it still answers on 9120), `takeover.c` writes `/data/ps5upload/runtime/takeover` holding its
   own random nonce (kern.arandom, never a time-based id: the app moves the clock with
   `settimeofday`). The old instance polls the file every second and exits when it holds a nonce
   other than its own that was not already there when the poll started. The clock is never read.
   The flag is unlinked at startup and after a successful takeover, so a leftover after a crash or
   reboot does nothing.
3. **AVA1-era to AVA1-era.** `node.shutdown` (method 5) over the paired session
   (`payload_lifecycle::shutdown_running_payload`, called by every ps5upload helper send), or the flag
   file in 2.

**The exit sequence** (node.shutdown, the flag, the old shutdown frame all reach it): the reply to
`node.shutdown` is written first; 300 ms later a deferred thread sets `shutdown_requested` and wakes
the accept loops; `main` then runs `ava1_payload_stop`: stop accepting, end the sessions (wait up to
2 s), wait up to 3 s for an in-flight Sony call (`sony_api_lock` free), stop the data layer (jobs
stopped, threads joined, journals closed so durable jobs resume). The 8 s exit watchdog bounds all of it.

Engine routes (Task 20 consumes these; the tokens are stable):

| route | answers |
|---|---|
| `GET /api/ps5/helper/state?host=` | `{"state": "ava1" \| "helper_old" \| "starting" \| "ava1_failed" \| "not_running"}` (`ava1`: the AVA1 port accepts connections; `helper_old`: only a pre-cutover helper answers the old protocol; `starting` / `ava1_failed`: a new build on the old ports whose AVA1 server is not up yet / did not start; `not_running`: nothing) |
| `POST /api/ps5/helper/replace {host}` | 200 `{"state": "ava1" \| "starting", "replaced": bool}`; 409 with `error` starting `legacy_helper_wedged` (the older helper did not exit within 10 s: show the console restart), `replace_in_progress` (one is running for this console), `replace_cooldown` (less than 60 s since the last), `helper_starting`, `ava1_failed` (restart the console; replacing would send the same build), or `helper_not_running`; 502 with the send failure. A console already on AVA1 answers `replaced:false`. Only a `helper_old` console is ever replaced. |

State tokens: `helper_old`, `ava1`, `starting`, `ava1_failed`, `not_running`. Error tokens:
`legacy_helper_wedged`, `replace_in_progress`, `replace_cooldown`, `helper_starting`, `ava1_failed`,
`helper_not_running`. The console needs at least 60 s between helper restarts; the route enforces it per
host (a failed attempt counts), and `replace` makes one attempt.

Deleted in the release after the cutover: `payload/src/legacy_takeover.c` (+ `include/legacy_takeover.h`),
`engine/crates/ps5upload-engine/src/legacy_helper.rs`, `legacy_helper_tests.rs`, `legacy_guard.rs` (+ the
two routes) and the Hello `ava1` fields' reader. `payload/src/takeover_flag.c`, `ava1_stop.c` and the
flag-file path in `takeover.c` stay.
## 6. Client contract (Task 20)

The client reads these, all optional, so an engine that does not send a field shows nothing for it.

**Job snapshot** (`GET /api/jobs/{id}`, SSE `job`), fields on a `running` job unless noted:

| field | type | meaning |
|-------|------|---------|
| `phase` | `"skipping"` | 7z/RAR resume: the decoder is discarding data the console already has. Absent otherwise. |
| `skip_done_bytes`, `skip_total_bytes` | u64 | Progress of the skipping phase (decoded vs to skip). |
| `bottleneck` | string | AVA1's words: `network`, `source`, `console drive`, `console workers`, `console memory`, `none`. A finished job carries the same word in `commit_ack.bottleneck` (already sent today). |
| `settling` | bool | Files are still settling on the console after the job finished (the engine sees `unswept` > 0 in `job.status`): the client shows "Finishing on the console…". Send it on the job while it settles; absent or `false` shows nothing. |

**Job summaries** (review 009 #4). Every finished, failed or cancelled transfer leaves one
`job_summary` JSON in `<data dir>/jobs/<job id>.json` on the engine's machine (the newest 200 are
kept; `PS5UPLOAD_JOB_SUMMARIES=0` turns it off). Local only: nothing is sent anywhere. The record names the
console by a hash of its key and holds no address and no local path (the destination is only its
drive, `/data` or `/mnt/usb0`; free text is scrubbed of home folders and IP addresses).
`GET /api/jobs/{id}/summary` returns one (404 while the job runs or if none was recorded),
`GET /api/jobs/summaries?limit=` the newest, newest first (`{"summaries": [...]}`, default 20, at most 200),
`GET /api/metrics` Prometheus counters (`ps5upload_jobs_total{kind,result}`, bytes, stalls, cross-device refusals).
The endpoints sit behind the engine's loopback guard like the rest. Fields: `schema`, `type`, `job_id`, `kind`,
`console`, `started_at_ms`, `ended_at_ms`, `elapsed_ms`, `result` (`done`/`failed`/`cancelled`), `code`, `message`,
`files`, `bytes`, `skipped_files`, `skipped_bytes`, `resumed`, `attempts`, `drive`, `engine_version`,
`shares` (`ticks`, `credit_starved_pct`, `source_starved_pct`, `receiver_bound_pct`, `receiver_bottleneck`),
`lanes_avg`, `lanes_max`, `chunk_avg_kib`, `history` (`[tick, lanes, chunk KiB]`, at most 60 points),
`slow_drive_switch`, `settle_ms`, `unswept_peak`, `resent_bytes`, `console_line` (the console's own end-of-job
text), and `why` (`dominant`, `pct`, `text`). The last 20 ride in the bug bundle's `report.json` as `engine.job_summaries`.

**Console status tokens** the status pill and the banners key on, read as substrings of the error
text of `GET /api/ps5/status` (the one probe; `payload_check`):

| token | state | UI |
|-------|-------|----|
| (a good `node.status` reply) | `connected` | green dot |
| `ava1_not_paired`, `not_paired` | `needs_pairing` | "Pair…" banner and the pairing dialog |
| `helper_old` | `helper_old` | "This PS5 is running an older helper. Update it." with the one-click send |
| `legacy_helper_wedged` | `helper_old` (wedged) | the same banner without the button: "restart the console, then update" |
| `helper_not_ava1`, anything else | `down` | the existing Send helper flow |

A console in `needs_pairing` or `helper_old` is a live helper: it does not count as down, so
the auto-redeploy loop never fires on it.

**Pairing routes** (loopback-guarded like every engine route; passkey entry, SPEC.md §5.5):
`GET /api/ava1/pairing?addr=` is read-only (no handshake): `{state: "accepted"}` for a live session,
`{state: "code", console_name}` while a handshake is pending, else `{state: "none"}`;
`POST /api/ava1/pairing/start` `{addr}` starts or re-reads the handshake and answers
`{state: "code", console_name}` (the user types the six digits the console shows; the
engine never sends its own copy), `{state: "accepted"}` (already trusted), `{state: "closed"}`
(the console's pairing window is shut), `{state: "wrong_console"}` (a different console than
the pinned one answers at this address) or a 502 with `error`.
`POST /api/ava1/pairing/confirm` `{addr, code}` (six digits, a string) answers `accepted`,
`wrong_code` (try again) or `closed`; a malformed code is a 400. `POST /api/ava1/pairing/cancel`
`{addr}` closes the pending handshake (the dialog was dismissed); `POST /api/ava1/pairing/forget`
`{addr}` removes the key pinned for that address ("forget the old console"). The handshake whose
code is on screen is held in `ps5upload_ava1::Pool` until confirmed, so asking twice shows one
code.
Tests: `engine/crates/ava1-ctest/tests/pairing.rs` (the C server).

**Addresses.** The client sends the bare console host (`consoleAddr`); the engine owns the port and
ignores any port a caller sends. While the FTX2 path still exists, `resolve_connect_targets` gives a
bare host the default FTX2 port.

### 4.2 Receive/apply path changes since those runs (perf-apply, review 003; not yet measured on hardware)

Host/loopback tests only (the consoles were reserved); every row of §4 above must be re-run. What
changed and what each is expected to move:

| change | where | expected effect |
|--------|-------|-----------------|
| preallocation outside `j->mu` (§2.1) | `lfile_open` | removes the multi-second stall of every worker and the feeder at each new large file on a slow drive; the Phat's usb0 upload should lose its two-minute preallocation freeze for the other lanes. Preallocation and ENOSPC-first are unchanged. |
| commits on workers (§3.3) | `ava1_apply_commit_ready` | the 1,641 large files of the 223k corpus (four fsyncs each) stop blocking every batch; they overlap with the chunk work. |
| striped directory fsyncs (§3.3, §4) | `ava1_sync_dirset` in prepare and every batch; engine `LocalSink::sync` | prepare's serial ~3 ms x thousands of parents and the per-batch serial directory fsyncs shrink by up to the worker count; this is the expected top item for the 82.5 files/s game upload. |
| per-chunk fsync on a slow drive (§6) | `sync_batch`, `write_chunk` | should bring the Phat's usb0 large-file upload toward FTX2's 34-35 MB/s; the line `slow drive: ... fsync per chunk from now on` shows it fired. |
| per-job stats | `ava1_apply_summary` | one line at every job's end: share of wall time in scan / data fsync / dirs / journal (job thread) and commit / preallocate (summed over workers); plus `preallocate took N ms for M MiB` and, above 1 s per GiB, `preallocation on this drive is slow`. |

The periodic per-batch line (`per batch ms: scan ... data ... dirs ...`) is now opt-in like the sender's
stage timers: `PS5UPLOAD_AVA1_TIMING=1` on a host, or create `/data/ps5upload/debug/ava1-timing` on the
console. The end-of-job line above is always printed. To read where a slow upload spends its time:
turn the flag on, run it, and read `/data/ps5upload/stderr.log`.

Deferred (recorded here, not done):

- **Directory-fsync deferral** (review 003 §3.3's second option) was not taken: striping keeps the
  invariant "no journal record names a file whose directory is unsynced" (SPEC 15.4) and needs no
  `lstat` of every done file on resume. Durable-by-log (`02-design-durable-by-log.md`) supersedes both.
- **Engine side**: `LocalSink` (downloads) still does one `fsync` per file serially and opens each file
  under its state mutex; only its directory fsyncs are parallel now. It never preallocates, so item 1
  has no engine counterpart. A packed-sink design belongs with durable-by-log (§3.4).
- **Compaction while commits are in flight** is skipped for that batch (it retries after the next one);
  on a job that always has a large file committing, the journal grows past `AVA1_JNL_COMPACT_AT` until a
  quiet batch. Not measured; if it shows, compact between a commit's journal record and the next one's
  start instead.
- The slow-drive switch is one-way for the life of the job and never reverts; a drive that recovers
  (a USB hub contention that ends) keeps paying one small flush per chunk.

### 4.2.1 Hardware run notes: debug flag files (review 007)

Both are plain files under `/data/ps5upload/debug/`, read by the helper; create or delete them over FTP or the file manager.

| file | effect | read |
|------|--------|------|
| `ava1-timing` | the periodic per-batch stage line and the sender's stage timers | on every job |
| `ava1-log-small-off` | durable-by-log off for jobs opened while it exists (§4.3); recovery still runs | at every JobOpen |

Capture with the timing flag on for every row of §4: the end-of-job line (`dirs` near 0 is the durable-by-log signature),
`preallocate took N ms`, any `slow drive: ... fsync per chunk from now on`, and the `recovered N logged files, M lost (resent)`
line after a crash test. If a console incident follows a run, repeat it with `ava1-log-small-off` present: if the incident
disappears the log path is implicated, and no reflash was needed to find out.

### 4.3 Durable-by-log small files (review 003 §3.2; not yet measured on hardware)

The receiver (console and engine) appends each small file to a pack log (`<job dir>/pack.<n>`), fsyncs the log
once per batch, journals the batch with the pack range, and makes the files durable in place later (the sweep).
A batch costs two fsyncs (log, journal) instead of N + D + 1 (SPEC 15.7). Host/loopback tests only; every
tiny-file and game-corpus row of §4 must be re-run on the consoles.

| what | expected effect |
|------|-----------------|
| tiny upload to `/data`, ext, usb0 | no longer bounded by the drive's per-file fsync rate (`disk.calibrate`: /data ~290 files/s); bounded by file creation. The 223k-file game should leave the per-file fsync and the per-batch directory fsyncs of §4 behind. |
| Phat usb0 | the 27 files/s run was fsync-bound; the log turns it into sequential appends. |
| JobDone | arrives when the log is durable; files settle for a few seconds behind it (a merge or single file). A new-folder upload settles before its rename, so its tail waits (about the last 3 s of files). |

How to read a run: the console's end-of-job line (`finished in N ms ... data fsync ... dirs ...`) should show
`dirs` near 0 during the transfer; `recovered N logged files, M lost (resent)` appears at helper start or JobOpen
after a crash. **Runtime off-switch (review 007 #5):** create the file `/data/ps5upload/debug/ava1-log-small-off` on the
console (any content) and every upload job *opened from then on* takes the per-file fsync path, with no helper rebuild and no
restart; delete the file to turn the log back on. A job decides once, at open, and never switches mid-job. The helper prints
`[ava1] job XXXXXXXX: durable-by-log OFF (debug flag)` to `/data/ps5upload/stderr.log` at the start of such a job, so a run's
stderr says which path produced its numbers. Recovery ignores the switch: a crashed logged job's pack files are still
replayed at helper start or JobOpen. Knobs (`ava1_data_cfg`): `log_small` (default on; `AVA1_LOG_SMALL_OFF` is the compile-time
equivalent, kept for this release), `pack_segment` (64 MiB), `unswept_max` (256 MiB), `sweep_age_ms` (3000).

Engine (`LocalSink`, downloads): on by default except on macOS, where it measured slower on loopback (2,270 vs
2,870 files/s for 2,000 tiny files; a plain fsync never reaches the drive there). `PS5UPLOAD_AVA1_LOG_SMALL=1/0`
forces it. The engine settles every logged file before it ends a job (it has no thread to settle behind JobDone),
so its unswept cap is soft (one credit window past `unswept_max`).

Failure paths (review dbl): a failed sweep is retried with a backoff and, after five failures, reported in `Status`
(`code`/`current`); the engine fails the upload on it, or reports a warning (`warning` in the job's `commit_ack`) when it
stops waiting (30 s, or a cancel). Housekeeping recovers parked, reaped and crashed job directories that hold a log;
the console's job GC never touches one. The engine's own job GC (`journal::gc`) does not special-case pack files:
an engine download settles everything before it ends, so a directory it left behind promised nothing to anyone.

Sweep content check (review 007 #8, done): the sweep's first pass compares the file's BLAKE3 with its log record's before it
fsyncs and releases the record, so a file with the right size and wrong bytes (zero-filled blocks after a power cut) is re-made
from the log instead of being made durable as is. The reasoning held: the file was just written and is in the page cache, the
record is a sequential read, files are bounded by `PACK_REC_MAX`, and the check never fails a sound file (an unreadable record
skips it). Writing the test found a real bug the check exposed: the sync batch sorted its file ids but not their record
locations, so the sweep queue and the per-segment journal ranges paired ids with other files' records, and a live re-make
failed with `EIO`. Both arrays are now sorted together.

Deferred:

- **Pack preallocation**: segments are not preallocated (the log is fsynced every batch). If a drive shows the sparse
  collapse FTX2 hit, add `posix_fallocate` of the segment at roll time.
- **Syscalls per file**: the console still does open, write, fchmod, utimensat by path, close for a logged file; the design's
  `open(mode)` + `futimens` saves two syscalls and was not done.
- **One pack writer at a time**: the append (offset + pwrite) is under one lock so a batch's range is a run of whole
  records; a 256 KiB record is ~100 us. If a profile shows it, shard the log per worker and journal one range per shard.
- **The sweep on a worker**: a sweep's directory syncs are serial (a worker must not wait on other workers' stripes). If
  `dirs` shows in the end-of-job line, give the sweep its own helper thread.
- **Engine macOS default**: revisit once an engine-side drive where fsync is expensive (a Windows or Linux host) has numbers.

## 7. Fork reconciliation: fixes on `main` that the fork lacks (review 015 #07 §3)

`ava1` forked at v5.41.0 (`a364f7a4`). `git log origin/main ^ava1` is **empty**: nothing has merged to `main`
since the fork, so every item below is an open PR or issue. Classes: (a) moot on AVA1, (b) port as is
(transport-independent), (c) re-implement on AVA1. Checked 2026-10-04 against `gh pr list --state open`.
Design notes are in `docs-research/015-complete-design-set/` on `origin/ava1-design`.

| Item | Class | Action | Link |
|---|---|---|---|
| #351 Convert reads console games through the helper | (c) | Re-implement over AVA1 reads. Owner: agent `p3-convert`. | design 01; PR #351 |
| #365 free-space check double-counts a partial upload | (c) | Re-implement against AVA1 job state. Owner: `p3-space`. | design 02; issue #365 |
| #353 folder resume after rest mode | (a) | Moot: the payload's FTX2 manifest adoption is gone. Add a resume-after-rest test only. Owner: `p3-space`. | design 02 §4; PR #353 |
| #350 progress for copy/paste and Add files | (c) | Re-implement on AVA1 job status. Later, design 03. | design 03; PR #350 |
| #349 installed games listed as installable packages | (b) | **Ported** (cherry-pick, clean): the external scan skips `/mnt/ext*/user`, `/api/pkg/install` refuses `…/user/{app,patch,addcont}/…`. Tests kept (`a_games_own_installed_files_are_never_an_install_source`, `the_scan_skips_installed_games_on_extended_storage`). The design said the guard lives in `pkg_install.rs`; it lives in `install/mod.rs::install_handler`, which is the handler `/api/pkg/install` uses. | design 04 §2; PR #349 |
| #348 Windows installer updates with the installer | (b) | **Ported** (cherry-pick). One conflict in `en.ts` (both sides append keys), kept both. `install_hint_setup_exe` is in the i18n allowlist like the other new keys. Takes effect from the release after the one that ships it. | design 07 §3; PR #348 |
| #360 keep-awake releases on Windows (external, lowbit) | (b) | **Ported** (cherry-pick, author kept) after review; see the review below. Not merged on GitHub. | design 07 §2; PR #360 |
| #364 Windows fpkg output lands in AppData temp | (b) | Not a PR, an issue. Fix is client/engine `default_output_dir`; independent of transport. Pending. | design 05 §1; issue #364 |
| #366 upload queue size chip and free-space warning (external) | (b) with (c) overlap | Review pending, do not port yet. See below. | design 02; PR #366 |
| #367 `PS5UPLOAD_BROWSE_ROOTS` (external) | (b) | Recommend port after one fix. See below. | design 07 §2; PR #367 |
| #355 Persian locale (external) | (b) | Recommend: not mergeable until the key gate passes. See below. | design 07 §2; PR #355 |
| #362 NixOS install docs (external) | (b) docs | Recommend merge after rebase, with a note. See below. | design 07 §2; PR #362 |
| #343 brace-expansion 5.0.9 to 5.0.12 (dependabot, client lockfile) | maintenance | Record only. CI is green; safe to merge on `main`. | design 07 §1; PR #343 |
| #356 engine group, 7 updates (dependabot) | maintenance | Record only. CI fails to compile (8 errors). Redo after #357. | design 07 §1; PR #356 |
| #357 num-bigint 0.4 to 0.5 (dependabot) | maintenance | Record only. Breaking: `rand` feature split. Own branch. | design 07 §1; PR #357 |
| #358 frontend group, 10 updates (dependabot) | maintenance | Record only. Must move with #359 (the npm and Rust Tauri versions must match). | design 07 §1; PR #358 |
| #359 tauri-shell group, 9 updates (dependabot) | maintenance | Record only. Together with #358, one branch. | design 07 §1; PR #359 |
| #361 saved connections EACCES in Docker/NAS | (b) | Issue, not a PR. Designed. | design 04 §1; issue #361 |
| #363 iOS port | n/a | Issue. Maintainer decision; needs a security read of its network and signing code first. | design 07 §2; issue #363 |
| #352 app unstyled on macOS 11 | (b) | Issue, investigation. | design 05 §2; issue #352 |

Counts: 20 rows; (a) moot 1, (b) port 9 (+#366 overlap), (c) re-implement 3 (+1 overlap), maintenance 5, issues or decisions 3.
Ported on branch `p3-port`: #348, #349, #360.

### 7.1 Reviews of the external PRs (recommendations; nothing was merged or commented on GitHub)

**#360 keep-awake (ported).** `SetThreadExecutionState` is per thread, and the keep-awake commands run on tokio workers, so
the release on another worker cleared nothing. The PR moves to a power request object that any thread can release. Reviewed
the `unsafe` Win32 use against the documented ABI:
- `PowerCreateRequest` returns `INVALID_HANDLE_VALUE` on failure, and the code tests for that (not NULL). Correct.
- `REASON_CONTEXT`: `Version`(u32) `Flags`(u32) then the union. The union's `Detailed` arm is
  `HMODULE, ULONG, ULONG, LPWSTR*` and `SimpleReasonString` shares its first pointer, so the Rust struct's trailing fields
  give the struct the right size on 32 and 64 bit. Version 0, flag `SIMPLE_STRING` = 1: correct.
- `POWER_REQUEST_TYPE`: DisplayRequired = 0, SystemRequired = 1: correct.
- The UTF-16 reason buffer outlives `PowerCreateRequest` (it is a local that lives to the end of the function).
- The handle is stored as `isize`, so `Handle` stays `Send`; it is closed exactly once, in `release_inhibitor`, after both
  `PowerClearRequest` calls. On a failed `PowerSetRequest` the handle is closed before returning the error: no leak.
- No new crate. The file was compiled for `x86_64-pc-windows-msvc` in a scratch crate (`cargo check`): clean. Not run
  on Windows hardware, so confirm with `powercfg /requests` after a transfer in the Windows CI or a manual run.

**#366 queue chip and free-space warning.** Client-only, 665 lines, with 219 lines of tests; pure helpers plus one
`fetchVolumes` call that fails open. It is transport-independent, but it overlaps design 02 (the free-space precheck), and it
does not handle the partly uploaded item (issue #365): a resumed item counts its full size, so it over-warns. Recommend: hold
until `p3-space` lands design 02, then rebase and take only the chip. The notification and banner code should go through the
existing notification path. Generated by a tool (Codebuff), so read the QueuePanel changes before merge. The i18n gate needs
the new keys translated or allowlisted.

**#367 browse roots.** Small, tested, defaults to the old behaviour. One issue: `storage_roots()` calls `eprintln!`, and the
project rule is that engine paths must not `eprintln!` (it panics when the parent died); use the engine log helper. The env
tests share a lock with each other only, not with the existing `storage_roots` test, which is benign. Recommend merge after that
change. The README/FAQ hunks apply to `ava1`'s docs with small conflicts.

**#355 Persian.** On `ava1` the PR fails the i18n coverage gate: `fa.ts` lacks about 66 keys that `ava1` added
(`joberr.ava1_*`, `pairing_*` and so on), plus `install_hint_setup_exe`. Applying the diff to `p3-port` and running
`node scripts/i18n-coverage.mjs` fails with "translate the keys above OR add them to scripts/i18n-known-missing.json".
Recommend: ask the contributor to rebase, then allowlist or translate the missing keys; the language-list code in
`lang.ts` already treats `fa` as RTL. Needs a native-speaker check of the strings.

**#362 NixOS docs.** Docs only, 63 lines in the README, pointing at the contributor's own NUR repo. It will conflict with
`ava1`'s README. Recommend merge after rebase, with the sentence that the package is community maintained and pins one
release version. The `allowUnfree` note (UnRAR) is accurate.

## 8. User-reported issues: fixed on `ava1`, verify again after the full migration

Users still run v5.41.x (FTX2) until the cutover release. Every fix below exists only on `ava1`,
so nothing here reaches anyone until that release ships. After the cutover (FTX2 deleted, AVA1
the only transport), re-run the **Verify after migration** column on hardware before closing
the matching GitHub issue. Issues stay open until the fix ships (maintainer rule, 2026-10-04).

Source: the community report on the `discord-reports` branch
(`reports/2026-10-04-issues-and-asks-combined.md`, U1–U18) and the 015 master index.

| # | Issue | Fix on `ava1` | Status | Verify after migration |
|---|---|---|---|---|
| U1 | Stream install unreachable (0x8041013d / 0x80431068 / 0x80431064) | Task E (015/06 §3): every stream-unreachable case gets host-IP guidance | in progress | Stream install from Docker and desktop on both consoles; a blocked port shows the guidance |
| U2 | Console declines an install (0x80b2116f, E2-80B22410 …) | Task F §3 (one-click Retry with Stream, never for a patch) + Task E route matrix | in progress | Force a 0x80b2116f; retry via Stream installs; a patch is never offered the retry |
| U3 | Install very slow (4.78 MiB/s, hours) | Telemetry (009 #4) splits copy vs install time; 015/03 §3 attribution not built | partial | 85 GB RDR2 (PS4 FPKG) upload-and-install: copy MB/s and install MB/s recorded separately |
| U4 | patch.pkg / app files offered as installable | #349 ported (`e33fe158`) | done | Scan an ext drive with an installed update: not listed; `/api/pkg/install` refuses it |
| U5 | Helper not reachable: 9021 closed, "Preparing 0%" | Payload Manager :8084 fallback (v5.41.0); Task E SDK ≥0.43 for 13.60 (#341) | in progress | Fresh console on 13.60: helper loads via 9021 and via the :8084 fallback |
| U6 | Helper crash loop / reset during transfers | AVA1 single-instance gate, `started=0` fix, ownership record (p3-fix007); sanitizers in CI; a connected job waits 90 s for a restarting helper (`0705be56`) | done, HW pending | Benchmark resume runs (helper killed and relaunched mid-upload, every run) complete; stderr.log shows no reap loop. The 30-restart stress test is not required (maintainer, 2026-10-04) |
| U7 | Long upload dies with `insufficient_space` after hours | Resume-aware free space + whole-job reservation (015/02, `9ff6b5e4`) | done, HW pending | `hw/space_test.py`: oversize refused up front, #365 resume admitted, two-at-once refused |
| U8 | Resume fails after rest mode | AVA1 journal resume (moot on AVA1, #353); rest-mode tests in ctest | done, HW pending | Host ctest covers rest-mode resume with and without an engine restart; a hardware rest-mode test is not required (maintainer, 2026-10-04) |
| U9 | Convert from a console game fails (ftpsrv :2121) | Convert over AVA1 `fs.read` (015/01, `3b21257e`); live 104.9 MB/s on the Phat | done | Full-size game Convert with ftpsrv stopped, on both consoles |
| U10 | Uploaded/converted game will not launch | Task E: won't-launch explainer + PS5 fake *game* pkg warning above FW 11.60 (PS4 FPKGs fine) | in progress | On 13.60: warning shown for a PS5 game pkg only; PS4 FPKG installs and launches |
| U11 | Fan curve ramps to 100% at the first step (#354) | **not fixed**; Task G added "Restore default" only | open | Reproduce #354 and fix it before the release, or carry it over openly |
| U12 | Windows installer copy offered the portable zip | #348 ported (`934a4ab2`) | done | Installed Windows copy offers `-setup.exe` and starts it |
| U13 | No progress/ETA in the commit phase | engine `progress.rs` already fills finishing/settling (018); F4 per-file tick optional | verify | 223k-file upload: the finishing phase shows motion and an ETA |
| U14 | Transfers shown under the wrong console | Task G keys tasks by console profile (`b5369c4c`) | done | Two consoles added, edit one's IP mid-transfer: the job stays under its console |
| U15 | Docker / web UI problems (403, perms, host IP) | 5.40/5.41 image fixes; Host allowlist dropped (`f3667441`); #361 perms in Task F §1 | in progress | `scripts/docker-smoke.sh` both images; a read-only state dir gives the descriptive error |
| U16 | White screen on macOS 11 (#352) | Task G boot guard notice (`b5369c4c`); root cause unknown | partial | Open on a macOS 11 / old WebView: a notice, never a blank window |
| U17 | FPKG output path ignores Downloads on Windows (#364) | Task G resolves USERPROFILE (`b5369c4c`) | done | Windows: default output under the user's Downloads |
| U18 | Windows cannot sleep after a transfer | #360 ported (`fcaa360f`) | done | Windows: `powercfg /requests` empty after a transfer |

Backlog requests filed as open issues (not scheduled): #368 (R4), #369 (R5), #370 (R6), #371 (R10),
#372 (R15), #373 (R16).

## 9. Hardware acceptance, 2026-10-04 (both consoles on FW 13.60, gigabit LAN)

Helper built from `5d6590d3` (SDK v0.43), sent to both consoles. The Pro was running an older
non-AVA1 helper, and the new one took it over cleanly. Medians; "n" is the number of runs.
The full three-drive matrix was cut to a light pass at the maintainer's request. The remaining
drive rows, the 223k-file set and the 85 GB install are follow-ups.

| Console / drive | Test | AVA1 | FTX2 | n (AVA1/FTX2) |
|---|---|---|---|---|
| Pro /data | 4 GiB file | 107.5 MB/s | 110.8 MB/s | 39 / 4 |
| Pro /data | 2,000 tiny files up | 235 files/s | 327 files/s | 4 / 4 |
| Pro /data | 2,000 tiny files down | 2,856 files/s | 2,409 files/s | 4 / 4 |
| Pro /data | resume (helper killed + relaunched) | 93.9 MB/s | 66.3 MB/s | 1 / 4 |
| Pro /mnt/usb0 | 4 GiB file | 107.8 MB/s | 111.7 MB/s | 4 / 4 |
| Pro /mnt/usb0 | 2,000 tiny files up | 446 files/s | 330 files/s | 4 / 4 |
| Pro /mnt/usb0 | 2,000 tiny files down | 1,916 files/s | 2,027 files/s | 4 / 4 |
| Pro /mnt/usb0 | resume | 96.5 MB/s | 68.2 MB/s | 4 / 4 |
| Phat /data | 4 GiB file | 105.4 MB/s | 107.7 MB/s | 36 / 1 |
| Phat /data | 2,000 tiny files up | 192 files/s | 260 files/s | 1 / 1 |
| Phat /data | 2,000 tiny files down | 1,513 files/s | 2,345 files/s | 1 / 1 |
| Phat /data | real game (Minecraft Legends, 1,018 files, 7 GiB) | 108.1 MB/s | 88.0 MB/s | 1 / 1 |

- **Lanes × chunk matrix (§4.1).** On Pro /data, every setting from 2/4/8 lanes × 1/4/15 MiB chunks gave
  105–110 MB/s, and lanes-first on/off gave the same. The link is the limit. Console crypto costs 12.8 %
  of one core at 110 MB/s (875 MB/s seal per core). **Decision: no decrypt-off-the-reader change.**
- **Known gap (perf, not correctness).** Tiny files uploaded to the internal SSD run at ~72 % of FTX2.
  FTX2 acknowledges before the files are durable: it measures 112 % of the drive's fsync ceiling. On USB
  and M.2, AVA1 is 30–50 % faster. Tweak later.
- **Free-space check (015/02), Phat /mnt/usb0.**
  - C1: a 500 GiB upload into 464 GiB was refused in 2 s, with 0 B sent and the shortfall named.
  - C2: a second 250 GiB upload was refused in 2 s while the first kept running.
  - C3 (#365): a resume with 165 GiB free and 292 GiB left was admitted, because the kept part file was
    credited. **All PASS.**
- **Convert over AVA1 (015/01).** A real game folder on the Phat (489 files after upload) converted and
  verified in 176 s, with no FTP. A 1 GiB console read ran at 104.9 MB/s.
- **Bugs found and fixed during the run.** Each has a regression test:
  - management calls longer than 60 s were cut short (`b96f9af5`);
  - a helper restart ended the job after three refusals (`0705be56`, then `43b97625`: a connected job
    waits for the console);
  - the `fs.volumes` fallback was sent to the wrong port (`0705be56`);
  - the bench lacked the AVA1 management path (`2954b099`);
  - the drop60 rejoin livelock: after a dropped session the engine kept rejoining lanes to a session the
    console had ended, and was refused with ERR_BAD_JOIN until the 600 s limit. This one is being fixed
    on `p3-rejoin`.

### 8.1 Community asks: maintainer decisions (2026-10-04)

| Ask | Decision |
|---|---|
| R1 copy / Add files / finishing progress and ETA (PR #350) | implement |
| R2 beginner guide (load order, what kstuff/etaHEN/ShadowMount are, quick start) | implement (docs) |
| R3 compress games on the console | **declined**: most games already ship as exFAT images |
| R4 link installer for any file + download-only (#368) | explained, decision pending |
| R5 cancel a running console copy (#369) | implement |
| R6 multi-pkg RAR without local space (#370) | implement: unpack to the console over AVA1, then install each package |
| R7 Discord rich presence | **declined** |
| R8 browse/download games on the console | **declined** |
| R9 restore default fan curve | kept: maps to the stock 60 °C threshold after #354's fix |
| R10 docs: mobile app, USB too small (#371) | implement |
| R11 RDR2 60 fps patch | **declined**: third-party patch |
| R12 iOS app (#363) | **declined**, closed: needs an Apple developer account |
| R13 Persian locale (PR #355) | accept, missing keys filled |
| R14 resume-aware free space (#365) | done (015/02) |
| R15 web UI keeps uploading after the tab closes (#372) | implement |
| R16 cheats: MC4, game names, load notification, filters (#373) | implement |
| R17 Docker browse roots (PR #367) | accept, logging fixed |
| R18 Retry with Stream | kept, merged (`f0d8280e`) |
