# ps5upload's AVA1 application profile

What ps5upload adds on top of AVA1. The protocol itself is `third_party/ava1/spec/SPEC.md` (the
standalone library, github.com/phantomptr/ava1); this file is the application layer: the methods
and operations ps5upload defines, the paths and switches it chooses, the parts of its own HTTP API
that expose AVA1 state, and the history of the migration from FTX2. Section references of the form
"SPEC §n" are to that spec.

Related files in this directory:

- `app.toml`: ps5upload's extension schema (method numbers, typed bodies, `JOB_OP_*`, `FSW_*`,
  `FSR_*`, `FS_READ_MAX`, `FSW_CHUNK_MAX`, `ENTRY_LINK/OTHER/UNKNOWN`). Generated with
  `ava1-gen --ext` (`make ava1-app-gen`). It is normative for everything below.
- `MGMT_METHODS.md`: one row per method, with its payload handler, its engine caller and its
  host tests; and the reply-size table.
- `vectors/`: the application's own vectors.

## 1. Roles and defaults

| AVA1 term | in ps5upload |
|-----------|--------------|
| node | the payload (the helper ELF running on the PS5): the C library in `third_party/ava1/c/` with `c/platform_ps5.c`, plus ps5upload's handlers in `payload/src/` |
| client | the engine: `engine/crates/ps5upload-ava1` on top of the Rust crate `ava1` |
| application | ps5upload (payload, engine, desktop and web client) |

- Port: the AVA1 default, 9120 (`DEFAULT_PORT`). FTX2 used 9114; nothing listens there any more.
- Trust slot and launch token (SPEC §5.1, §5.2): the engine stamps the helper ELF before it sends
  it to the console's ELF loader, and keeps its launch tokens in `<data dir>/ava/launch_tokens`.
- `CAP_MGMT` is advertised once the payload's management table is installed (`mgmt_rpc_installed()`).
- Pairing (SPEC §5 step 5): the console shows the code in a system notification
  ("enter 123456 in the app").

## 2. Paths and switches on the console

| what | path | set by |
|------|------|--------|
| job directories (SPEC §14.3) | `/data/ps5upload/ava/jobs/` | `ava1_data_cfg_t.jobs_dir` in `payload/src/ava1_glue.c` |
| job event log | `/data/ps5upload/ava/events.log` (one line per job open, resume, done and fail with status, bytes, files and lanes; 1 MiB, rolled to `events.log.old`), read with `fs.read` like the other log files | `ava1_events_set_path` in `ava1_glue.c` |
| timing flag (CLIENT_CONTRACT §4.2.1) | `/data/ps5upload/debug/ava1-timing` | `-DAVA1_TIMING_FLAG` in `payload/Makefile` |
| durable-by-log off flag | `/data/ps5upload/debug/ava1-log-small-off` | `-DAVA1_LOG_SMALL_OFF_FLAG` in `payload/Makefile` |
| read-back (#783) | none: the engine asks per upload with `JF_READBACK` when the helper advertises `CAP_READBACK` and the app's `verify_writes` setting is on (default). Each mismatch is a `readback-mismatch` line in the helper's stderr.log and in events.log | `payload/src/ava1_glue.c` (caps) |
| helper stderr (the timing and end-of-job lines) | `/data/ps5upload/stderr.log` | the payload |
| takeover flag (§9.1) | `/data/ps5upload/runtime/takeover` | `payload/src/takeover.c` |
| shared send records (SPEC §18.6) | `/data/ps5upload/c2c/<job id hex>.json` on the receiving console, written with `fs.write` | the engine |
| engine identity, launch tokens, its job directories | `<data dir>/ava/identity`, `<data dir>/ava/launch_tokens`, `<data dir>/ava/jobs/`, `<data dir>/ava/send` | the engine |

Both flag files are plain files, read by the helper; create or delete them over FTP or the file
manager. The environment switches are the library's: `AVA1_TIMING`, `AVA1_LOG_SMALL`, `AVA1_LANES`,
`AVA1_CHUNK`, `AVA1_LANES_FIRST` (formerly `PS5UPLOAD_AVA1_*`).

PS5 platform facts behind generic rules in the spec:

- A cross-device `rename(2)` panics the PS5 kernel. That is why every `st_dev` guard (SPEC §11.6,
  §12.6, `ERR_CROSS_DEVICE`; and ps5upload's `fs.rename`, FTP and shell `mv`) fails closed.
- The descriptor ceiling on firmware 13.60 is about 619 open files while `RLIMIT_NOFILE` reads about
  13,952 (SPEC §15.6).
- Sony's errno encoding is `0x8002xxxx`: a USB drive's hiccup comes back as `0x80020002` (SPEC §12.7's
  fsync retry list covers it).
- Thread stacks are small and an overflow is fatal, hence the 256 KiB / 512 KiB stacks and the
  no-large-stack-array rule (SPEC §7.3, §4 below).
- Files and folders the helper creates are 0777 (`umask(0)` in `main.c`; CE-107750-0 otherwise).

## 3. Methods (moved from SPEC §7.3)

The management methods are the console operations FTX2 carried on :9114. Numbers are assigned by
block; the tracked list, one row per method with its payload handler and engine caller, is
`MGMT_METHODS.md`. The generated constants `METHOD_*` in `app.toml` are normative. `job.run` (20)
and `job.list` (21) are the protocol's (SPEC §7.1.1).

| numbers | block | bodies |
|---------|-------|--------|
| 4–11 | node and diagnostics: `node.status`, `node.shutdown`, `node.cleanup`, `log.klog`, `log.syslog`, `net.interfaces`, `net.reach`, `net.speedtest` | `node.status` replies `NodeStatus`; the rest `MgmtText` |
| 32–44 | filesystem: `fs.volumes`, `fs.list`, `fs.stat`, `fs.mkdir`, `fs.rename`, `fs.chmod`, `fs.read`, `fs.write`, `fs.mount`, `fs.unmount`, `fs.mount_pkg`, `fs.mount_lwfs`, `fs.freespace` (44) | `fs.list`, `fs.stat`, `fs.freespace` (`FsPath` → `FsFreeSpace`: usable bytes = free less the working margin, 1/64th of the drive at most 1 GiB; the nearest existing ancestor of the path is asked), `fs.mkdir`, `fs.rename`, `fs.chmod`, `fs.read`, `fs.write` are typed (`FsList` → `FsListResult`, `FsPath` → `FsStat`, `FsMkdir`, `FsRename`, `FsChmod`, `FsRead` → `FsReadResult`, `FsWrite`); the others `MgmtText` |
| 48–61 | apps, launch, install queries, processes | `MgmtText`; `app.list` pages with `offset`/`limit` and `more` (SPEC §7.4) |
| 64–70 | saves, screenshots, videos, search index | `MgmtText` |
| 72–87 | hardware, power, time, peripherals, `shell.exec` | `MgmtText` |
| 88–100 | profiles, users, backups (97 and 99 are unassigned: backup snapshot and restore run as `job.run` ops) | `MgmtText` |
| 104–128 | cheats, SMP metadata, SDK changer, TMDB, FTP, firmware spoof, notifications, activity | `MgmtText` |
| 136–141 | Remote Play | `MgmtText` |
| 142 and up | later additions (console clean-up, SMB shares, files hand-off and prefs, mesh, ...) | see `MGMT_METHODS.md` |

A `MgmtText` body is the payload handler's existing request or reply (UTF-8 text, in practice
JSON), carried unchanged in `MgmtText.body`; `more = 1` on a reply means the method is paged and
the caller asks again with the next `offset`. Typing the text methods is deferred: the text bodies
are stable and tested, and the cutover did not need them typed. A same-drive `fs.move` over AVA1 is
`fs.rename`, with the `st_dev` guard; a cross-mount move (copy, verify, delete) is an AVA1 job
(`job.copy` with `JF_MOVE`).

Per-method rules:

- `log.klog` and `log.syslog` are clamped tails, not paged reads: when the console's text is longer
  than `RPC_TEXT_MAX` the reply is its newest `RPC_TEXT_MAX` bytes, starting at a line boundary (else a
  UTF-8 boundary), with `more = 1` meaning "older text was left out"; a text that fits is returned
  whole with `more` absent or 0. `log.klog` sets `more` when the read filled the whole ask;
  `log.syslog` returns the newest `RPC_TEXT_MAX` bytes (or `max_bytes`) and sets `more` when older text
  was cut.
- `net.reach` is a probe: its negative answer (`{"ok":false,"timed_out":..,"errno":..,"ms":..}`) is the
  measurement and travels as an ordinary OK reply; only a malformed request (`bad_request`,
  `bad_address`) is `ERR_PROTOCOL`.
- A handler whose failure carries data the caller reads (`net.reach`, `fs.mount_pkg`,
  `fs.mount_lwfs`) answers an error status whose cause is the whole `{"ok":false,...}` body (up to
  1 KiB), which the engine's `call_legacy_ok` hands back as the reply it parses.
- Typed bodies carry their own overhead (`FsReadResult` is `data + 7`).

## 4. Errors, truncation and threads in the payload

Errors. A ported handler's cause is its legacy token (`fs_move_cross_mount`, `cleanup_path_denied`,
...), so the engine can build the same `payload rejected <LABEL>: <cause>` text FTX2 callers produced.
No new error codes were added for management methods. `fs.rename` answers `ERR_CROSS_DEVICE` when the
source and the destination's parent are on different devices (`st_dev`); it never calls `rename(2)`
across devices. A device that cannot be read (unknown) is refused too, with `ERR_IO`
(`fs_move_device_unknown`): only a definite "same" reaches `rename(2)`.

Legacy failure bodies. Many FTX2 handlers answered a failure as a *successful* frame with a
`{"ok":false,"err":"..."}` body (`handle_fs_write_bytes`, `handle_net_reach`, `handle_toast_send`,
the TMDB, SDK and cheats handlers, ...). A ported handler never does that: it answers an `ERR_*`
status (the closest of SPEC §7.2; `ERR_INTERNAL` when none fits) with the legacy token as the cause,
and `STATUS_OK` only when the operation succeeded. A body that still contains `"ok":false` under
`STATUS_OK` is a porting bug. Where the legacy body also carried data on failure (a partial list,
a detail object), the cause is that body's `err` token and the data is dropped (the exceptions are
the probes of §3).

Truncation. Every `snprintf` into a reply buffer is checked, a clamped read (`klog`, `syslog`,
`fs.read`) reports a short read as a short read (`eof`, `more`), and a buffer that cannot hold the
whole answer answers `ERR_INTERNAL` with the cause `reply truncated`, through the library's
`ava1_rpc_text` (SPEC §7.3).

Threads. The payload's management RPCs run on the library's 512 KiB management workers
(`AVA1_MGMT_STACK`, the FTX2 management thread's size). Before each handler the worker re-applies
the credential elevation and sets the in-flight frame marker to the handler's legacy FTX2 frame
number (the crash breadcrumb), and clears it after. A ported handler keeps stack buffers small:
**no stack array of 16 KiB or more reachable from a table handler**, enforced by
`payload/tools/mgmt_audit.py stack` (run by the `ps5upload-ctest` test
`c_mgmt_handlers_never_read_the_socket_and_keep_small_stacks`), and the handler must not read
`client_fd` (it is called with -1 behind a capture sink, `payload/src/mgmt_rpc.c`).
`mgmt_audit.py report` lists every array of 2 KiB or more per handler (it is a tripwire, not a
proof: it reads C text, follows calls and function-pointer arguments by name, sizes struct elements
as a lower bound and unknown element types at 8 bytes, and cannot see calls through tables or
dlsym; `mgmt_audit.py selftest` pins the shapes it must catch). The 64 KiB buffers it showed in
`handle_crc32_file`, `handle_shell_builtin` and `copy_file` had to move to the heap before those
handlers were routed.

The engine's RPC gate (`engine/crates/ps5upload-ava1/src/mgmt.rs`) holds 6 permits per console and
reserves the other 2 for `node.status`, `job.status` and `job.cancel`, so a flood of slow calls never
hides a cancel or liveness; it retries `ERR_BUSY` with backoff (3 tries) and never reports it as
"payload failed" (SPEC §7.4, clients).

## 5. `job.run` operations (moved from SPEC §7.1.1)

ps5upload registers these operations (`JOB_OP_*` in `app.toml`; `ava1_op_register` in
`payload/src/fs_jobs.c`, `mgmt_rpc.c`, `archive_extract.c`, `app_dump.c`, `webui_pkg.c`):

| op | name | what | repeatable |
|----|------|------|------------|
| 1 | `JOB_OP_DELETE` | delete a tree | yes |
| 2 | `JOB_OP_CHMOD_R` | recursive chmod | yes |
| 3 | `JOB_OP_HASH` | hash a file | yes |
| 4 | `JOB_OP_CRC32` | CRC-32 of a file | yes |
| 5 | `JOB_OP_FSCK` | UFS fsck (wraps the FTX2 handler) | yes |
| 6 | `JOB_OP_BACKUP_SNAPSHOT` | take a backup snapshot (wraps the FTX2 handler) | no (`AVA1_OP_ONCE`) |
| 7 | `JOB_OP_BACKUP_RESTORE` | restore a backup (wraps the FTX2 handler) | no (`AVA1_OP_ONCE`) |
| 8 | `JOB_OP_CLEANUP` | `node.cleanup` of a long tree (wraps the FTX2 handler) | yes |
| 9 | `JOB_OP_SDK_SCAN` | the SDK changer's scan (wraps the FTX2 handler) | yes |
| 10 | `JOB_OP_FFPFSC_UNPACK` | a `.ffpfsc`'s image written beside it on the console | yes |
| 11 | `JOB_OP_PKG_INSTALL` | the console page installs a `.pkg` (staged from USB/M.2 first) | yes |
| 12 | `JOB_OP_ARCHIVE_EXTRACT` | a `.zip`/`.7z`/`.rar`/`.tar.*` on the console extracted into a folder there | yes |
| 13 | `JOB_OP_APP_DUMP` | an installed, running title's files copied to a drive, optionally decrypted | no (`AVA1_OP_ONCE`) |

- `args` is the operation's request body: the same legacy JSON the FTX2 frame carried (at most
  60 KiB).
- Progress: files are non-directories, bytes the regular files' sizes.
- A repeat after the grace runs the operation again, which is harmless for delete, chmod, hash and
  crc32. A backup is kept for the park age instead, because a re-run would take a second snapshot
  (or a second restore).
- Cancel: a delete, chmod, hash, crc32 or backup stops at the next directory entry or read block.
  fsck, cleanup and sdk.scan are one system call and only honour a cancel that arrives before they
  start, so a cancelled fsck holds its job slot (and its worker) until the system call returns.
- An operation that wraps an FTX2 handler (fsck, backup, cleanup, sdk.scan) keeps the handler's
  `{"ok":false,...}` body as its result: the operation ran and the body is the answer. Only an ERROR
  frame is a failed job.
- DELETE refuses a path outside the writable roots and a mount point (a path on another device than
  its parent): `ERR_PATH`, `fs_delete_path_not_allowed` / `fs_delete_path_is_mount_point`.

## 6. Chunked and bounded filesystem calls (moved from SPEC §7.5)

`fs.read` (`FsRead{path, offset, len, flags}` -> `FsReadResult{data, eof}`): `len` is at most
`FS_READ_MAX = RPC_REPLY_MAX - 16 = 262,128` bytes (the reply is `data + 7`). A shorter reply with
`eof = 1` means the end of the file; `eof = 0` with fewer bytes than asked means the node chose a
short read, and the caller continues at `offset + data.len()`. A caller that needs more than
`FS_READ_MAX` (FTX2 allowed 2 MiB per call) loops until `eof` or the byte count it wanted, and the
core wrapper `fs_read_with_timeout` does that for every caller. Callers that can ask for more than
the cap (at the cutover): `ps5upload-engine/src/lib.rs:3974`, `ps5upload-engine/src/fakelibs_api.rs:423`,
`ps5upload-core/src/fs_ops.rs:1703`, `ps5upload-core/src/smp_image_rw.rs:174`,
`ps5upload-core/src/smp_checkout.rs:185`. Existence tests by 1-byte `FsRead` that became `fs.stat`:
`lib.rs:4033`, `smp_checkout.rs:456`, `smp_image_rw.rs:229`, `fakelibs_api.rs:390` and
`fakelibs_api.rs:594`. `FSR_UNSAFE` reads outside the writable roots (the engine sets it only for an
unsafe read).

`fs.write` (`FsWrite{path, offset, flags, data}`, ext `mode`): FTX2 wrote up to 256 KiB atomically,
and the request cap is 56 KiB, so a larger file is written in chunks of at most
`FSW_CHUNK_MAX = 48 KiB` (49,152) of `data` (the rest of the request is the path, the header and
the extension). Flags: `FSW_APPEND` (write at the end of the temporary file, `offset` ignored),
`FSW_AT_OFFSET` (write at `offset`; a missing temporary file is created empty), `FSW_COMMIT`
(after this chunk: fsync the temporary file, then `rename` it over `path`), `FSW_CREATE` (at commit
fail with `ERR_EXISTS` when `path` exists) and `FSW_OVERWRITE` (replace it; the default when neither
is set; both set is `ERR_PROTOCOL`). Neither `FSW_APPEND` nor `FSW_AT_OFFSET` means "the whole file in
one call": `offset` must be 0, the file is written to the temporary file and committed in the same
call, exactly FTX2's atomic small write (`COMMIT` is implied). Chunked protocol: the temporary file
is `<path>.ps5upload.tmp` in the same directory as `path` (so the commit rename never crosses a
device, with the `st_dev` guard of `fs.rename`); the caller sends chunks with `FSW_AT_OFFSET`
(or `FSW_APPEND`) in order, the last one also carrying `FSW_COMMIT`. A caller that gives up
deletes the temporary file (`fs.rename` is not needed; `job.run` DELETE removes it). A chunk with
`FSW_AT_OFFSET` at offset 0 truncates an abandoned temporary file first, so a retry starts clean.
(`FSW_APPEND` never truncates: it has no offset to say "first", so an `FSW_APPEND` writer starts from
a temporary file that does not exist; the engine uses `FSW_AT_OFFSET`.) `mode` (ext 1) is accepted
but ignored: the file is committed as 0777, like everything else the helper writes (§2). Callers
that need chunking because they write more than 48 KiB: `ps5upload-core/src/cheats.rs:701`,
`ps5upload-core/src/profile.rs:664`, `ps5upload-core/src/smp_image_rw.rs:158` (the others,
`smp_checkout.rs` and `smp_image_rw.rs:305/339`, write small state files). The core wrapper
`fs_write_bytes` chunks transparently.

Other filesystem methods, as built (`payload/src/mgmt_fs.c`, host-tested): `fs.list` pages by
`offset`/`limit` (`limit` 0 = 256, at most 256; `more` = entries remain; names that are not valid
UTF-8 are listed with `?` for their high bytes; `total_scanned` counts what the walk passed).
`fs.stat` takes any absolute path without a `..` component (the policy of `fs.list`, not of
`fs.read`; see "Scope of `fs.stat` and `fs.list`" below), follows a link (`kind` is `link` only for a
dangling one), and answers `ERR_IO` with `fs_stat_failed_errno_<n>` for an absent path. `fs.mkdir`
ignores `mode` (the new directory and any intermediate ones are 0777, like every file and folder the
helper creates) and honours `parents` (0: a missing parent is `fs_mkdir_failed`); an existing
directory succeeds, an existing non-directory is `ERR_EXISTS`. `fs.rename` with `overwrite = 0`
refuses an existing destination (`ERR_EXISTS`, `fs_move_exists`); with `overwrite = 1` it replaces,
as FTX2's move did. `fs.read` reads at most `FS_READ_MAX` and loops internally until the ask or the
end, so a reply shorter than the ask always has `eof = 1`.

Scope of `fs.stat` and `fs.list` (review 006, checklist D; decided: not narrowed). Both answer for any
absolute, `..`-free path, as the FTX2 handlers did, because the product needs it: the Volumes and file
browsers list `/`, `/mnt/*`, `/user` and `/system_data`, and existence probes (installed titles, SMP
overlays, backport libraries) ask about paths outside every writable root. What a paired peer learns
is metadata only: names, kind, size, mtime, mode, device. Never contents (`fs.read` and the data plane
keep their own read policy), and the trust store's contents are refused by the read and write
policies (S2), so no key material or peer list is reachable (its name and size are visible, like any
other file's). A peer that is paired can already upload, delete, rename, launch and read through
`fs.read`'s allowlist, so metadata of the rest adds no capability. Narrowing these two would break
the browsers for no gain; the decision is to revisit it only if a read-only or guest pairing tier is
ever added (then `fs.list`/`fs.stat` would take the read policy).

Typed bodies decoded by the adapters: `NodeStatus.ucred_elevated` is a `u8` on the wire; the engine
adapter restores the JSON boolean the client reads (`true`/`false`) and rebuilds the legacy
`/api/ps5/status` object. `NodeStatus.prior_instance` is one of `clean`, `killed_externally`,
`wedged`, `stale` or `replaced` (the values of `instance_verdict_name`). `FsEntry.kind` and
`FsStat.kind` are `ENTRY_FILE` (0), `ENTRY_DIR` (1), `ENTRY_LINK` (2, a symbolic link, not followed),
`ENTRY_OTHER` (3, a device, socket or fifo) or `ENTRY_UNKNOWN` (4, the node could not stat the entry;
FTX2 said `"other"`). `FsListResult` carries no `path` and no returned-entry count (FTX2's reply had
both); the adapter reconstructs `path` from the request and the count from `entries.len()`.

## 7. Engine conventions over the data plane

- Job ids: the engine uses the HTTP API's `tx_id` as the AVA1 `job_id` (SPEC §11.1).
- Skip policy (SPEC §11.4): the engine's "safe" resume mode always uses `verify`.
- `JF_UNSAFE_READ`: set only for a download the user marked unsafe.
- `ERR_BUSY` on a `JobOpen` (SPEC §15.7): the engine retries uploads, downloads and relays with a
  bounded jittered backoff and fails with reason `ava1_busy` once the bound (12 tries by default) is
  spent.
- Shared identity (SPEC §8): two engines (two processes, two computers, a desktop app and a Docker
  engine) that share one identity file evict each other. The engine logs a warning naming this cause
  when a console's session ends on its own 3 times within 120 s (at most once per window per
  console). Every engine needs its own data directory.
- Console to console (SPEC §18): `engine/crates/ps5upload-ava1/src/c2c.rs` and `c2c_record.rs` (the
  shared send record of SPEC §18.6, at the path in §2).
- Downloads into a `.zip` (SPEC §10.1): `engine/crates/ps5upload-ava1/src/zip_stored.rs`.

## 8. The engine's HTTP API for AVA1 state (moved from CLIENT_CONTRACT §2)

The client (desktop and web UI) reads these, all optional, so an engine that does not send a field
shows nothing for it. Code that matches on these fields points here (`client/src/lib/jobLive.ts`,
`client/src/api/ps5.ts`).

**Job snapshot** (`GET /api/jobs/{id}`, SSE `job`), fields on a `running` job unless noted:

| field | type | meaning |
|-------|------|---------|
| `phase` | `"skipping"` or `"measuring"` | `skipping`: 7z/RAR resume, the decoder is discarding data the console already has. `measuring`: see `measure_done_bytes`. Absent otherwise. |
| `skip_done_bytes`, `skip_total_bytes` | u64 | Progress of the skipping phase (decoded vs to skip). |
| `measure_done_bytes` | u64 | With `phase: "measuring"`: a RAR's entries of unknown size are being decoded to learn them, this many bytes so far. `total_bytes` includes them once it ends. |
| `bottleneck` | string | AVA1's words: `network`, `source`, `console drive`, `console workers`, `console memory`, `none`. A finished job carries the same word in `commit_ack.bottleneck`. |
| `settling` | bool | Files are still settling on the console after the job finished (the engine sees `unswept` > 0 in `job.status`): the client shows "Finishing on the console…". Sent on the job while it settles; absent or `false` shows nothing. |
| `verifying` | bool | The job asked for the read-back (`JF_READBACK`, SPEC §13.5; #783), every byte is durable on the console and files are still being committed: the console is reading the last large files back from its drive. The client shows "Verifying on the PS5…". Never with `settling`. |

A finished upload's `commit_ack` also carries `readback`: `"on"` (the console read each large file back
from its drive before it counted it written), `"off"` (the `verify_writes` setting is off) or
`"unsupported"` (the helper does not advertise `CAP_READBACK`; the engine logs it once per job), and
`rechecked_bytes`: bytes the console asked for again because a file did not check out (its read-back,
or the root of what arrived); the engine logs a warning when it is not 0. A job whose file kept reading
back wrong ends with reason `ava1_verify_failed` (not retried by the queue).

**Job summaries** (review 009 #4). Every finished, failed or cancelled transfer leaves one
`job_summary` JSON in `<data dir>/jobs/<job id>.json` on the engine's machine (the newest 200 are
kept; `PS5UPLOAD_JOB_SUMMARIES=0` turns it off). Local only: nothing is sent anywhere. The record
names the console by a hash of its key and holds no address and no local path (the destination is
only its drive, `/data` or `/mnt/usb0`; free text is scrubbed of home folders and IP addresses).
`GET /api/jobs/{id}/summary` returns one (404 while the job runs or if none was recorded),
`GET /api/jobs/summaries?limit=` the newest, newest first (`{"summaries": [...]}`, default 20, at most
200), `GET /api/metrics` Prometheus counters (`ps5upload_jobs_total{kind,result}`, bytes, stalls,
cross-device refusals). The endpoints sit behind the engine's loopback guard like the rest. Fields:
`schema`, `type`, `job_id`, `kind`, `console`, `started_at_ms`, `ended_at_ms`, `elapsed_ms`, `result`
(`done`/`failed`/`cancelled`), `code`, `message`, `files`, `bytes`, `skipped_files`, `skipped_bytes`,
`resumed`, `attempts`, `drive`, `engine_version`, `shares` (`ticks`, `credit_starved_pct`,
`source_starved_pct`, `receiver_bound_pct`, `receiver_bottleneck`), `lanes_avg`, `lanes_max`,
`chunk_avg_kib`, `history` (`[tick, lanes, chunk KiB]`, at most 60 points), `slow_drive_switch`,
`settle_ms`, `unswept_peak`, `resent_bytes`, `console_line` (the console's own end-of-job text), and
`why` (`dominant`, `pct`, `text`). The last 20 ride in the bug bundle's `report.json` as
`engine.job_summaries`.

**Console status tokens** the status pill and the banners key on, read as substrings of the error
text of `GET /api/ps5/status` (the one probe; `payload_check`):

| token | state | UI |
|-------|-------|----|
| (a good `node.status` reply) | `connected` | green dot |
| `ava1_not_paired`, `not_paired` | `needs_pairing` | "Pair…" banner and the pairing dialog |
| `helper_not_ava1`, anything else | `down` | the existing Send helper flow |

A console in `needs_pairing` is a live helper: it does not count as down, so the auto-redeploy
loop never fires on it.

**Pairing routes** (loopback-guarded like every engine route; passkey entry, SPEC §5 step 5):
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
`POST /api/ava1/same-console` `{old, new}` answers `{"result": "same" | "different" | "unknown"}`:
whether the console at `new` proves the key pinned for `old` (a console whose address changed). It
pins nothing; `unknown` when `old` has no pin or nothing answers at `new`.
Tests: `engine/crates/ps5upload-ctest/tests/pairing.rs` (the C server).

**Addresses.** The client sends the bare console host (`consoleAddr`); the engine owns the port and
ignores any port a caller sends.

## 9. History: from FTX2 to AVA1

AVA1 replaced FTX2, ps5upload's earlier PC↔PS5 protocol on port 9114 (unencrypted frames, one
connection, a frame number per operation). The cutover kept what callers depended on:

- Every FTX2 management frame became an AVA1 method in a numbered block (§3). `MGMT_METHODS.md`
  keeps each handler's FTX2 frame number (`MGMT_FRAME_*`) in its `frame` column; the payload still
  sets it as the crash breadcrumb (§4).
- Bodies stayed the FTX2 JSON inside `MgmtText`; only the filesystem, node and job methods were
  typed. Error causes stayed the FTX2 tokens (§4).
- FTX2's long operations (delete, chmod -R, hash, crc32, fsck, backup, cleanup, sdk.scan) became
  `job.run` operations (§5); FTX2's 2 MiB reads and 256 KiB atomic writes became the bounded calls of
  §6.
- The FTX2 management thread's 512 KiB stack became `AVA1_MGMT_STACK`.
- The library was then split out of ps5upload (`third_party/ava1`, published as
  github.com/phantomptr/ava1): ps5upload's methods, typed bodies and constants moved from the core
  schema to `app.toml`, its paths became settings of the application (§2), and its environment
  switches lost the `PS5UPLOAD_` prefix.

### 9.1 Helper lifecycle (moved from CLIENT_CONTRACT §1)

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
   (`payload_lifecycle::shutdown_running_payload`, called by every ps5upload helper send), or the
   flag file in 2.

**The exit sequence** (node.shutdown and the flag both reach it): the reply to `node.shutdown` is
written first; 300 ms later a deferred thread sets `shutdown_requested` and wakes the accept loops;
`main` then runs `ava1_payload_stop` (`payload/src/ava1_stop.c`): stop accepting, end the sessions
(wait up to 2 s), wait for the RPC workers, wait up to 3 s for an in-flight Sony call
(`sony_api_lock` free; a call that outlives it is reported but never cut), stop the data layer (jobs
stopped, threads joined, journals closed so durable jobs resume). The 8 s exit watchdog bounds all
of it.

### 9.2 Hardware context of the perf notes

CLIENT_CONTRACT §4.2 and §4.3 were written against ps5upload's hardware runs: the Phat's usb0 upload
lost a two-minute preallocation freeze; the 223k-file game corpus has 1,641 large files and ran at
82.5 files/s; a slow USB drive was expected to approach FTX2's 34-35 MB/s; `disk.calibrate` measured
`/data` at ~290 files/s; the Phat's usb0 tiny-file run was fsync-bound at 27 files/s; and the FTX2
sparse-file collapse is what pack preallocation would guard against.
