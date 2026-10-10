# AVA1 client contract

What the engine, the client and the payload agree on beyond the wire format in `SPEC.md`: the
payload lifecycle and the job-status fields the client renders. Code that matches on these fields
points here.

## 1. Payload lifecycle

Three starting situations, and who handles each:

1. **Older helper (any release up to v5.41, old protocol only) running, new app.** Nothing answers
   on the AVA1 port, so the status probe reports `helper_not_ava1` and the console shows as having
   no helper (`down`); the usual send-the-helper flow (`ensurePayloadCurrent`, which pushes whenever
   the running version is unknown) sends the current helper. That helper does not speak the old
   protocol and asks nothing of the old one; its startup reap (the instance the ownership record
   names) and, 65 s later, its sweep of processes wearing our name end an old one they recognise.
   One they do not recognise keeps only the old ports, which nothing uses any more, until the
   console restarts.
2. **New payload loaded while an AVA1-era helper is alive** (an autoloader, another sender). When the
   AVA1 port answers, `takeover.c` writes `/data/ps5upload/runtime/takeover` holding its own random
   nonce (kern.arandom, never a time-based id: the app moves the clock with `settimeofday`). The old
   instance polls the file every second and exits when it holds a nonce other than its own that was
   not already there when the poll started. The clock is never read. The flag is unlinked at startup
   and after a successful takeover, so a leftover after a crash or reboot does nothing.
3. **AVA1-era to AVA1-era from the app.** `node.shutdown` (method 5) over the paired session
   (`payload_lifecycle::shutdown_running_payload`, called by every ps5upload helper send), or the flag
   file in 2.

**The exit sequence** (node.shutdown and the flag both reach it): the reply to `node.shutdown` is
written first; 300 ms later a deferred thread sets `shutdown_requested` and wakes the accept loops;
`main` then runs `ava1_payload_stop`: stop accepting, end the sessions (wait up to 2 s), wait up to 3 s
for an in-flight Sony call (`sony_api_lock` free), stop the data layer (jobs stopped, threads joined,
journals closed so durable jobs resume). The 8 s exit watchdog bounds all of it.

## 2. Client contract (Task 20)

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
| `helper_not_ava1`, anything else | `down` | the existing Send helper flow |

A console in `needs_pairing` is a live helper: it does not count as down, so the auto-redeploy
loop never fires on it.

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
ignores any port a caller sends.

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

