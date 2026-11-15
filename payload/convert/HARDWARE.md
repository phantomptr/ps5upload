# ps5upload-convert: the first runs on a console

Nothing of the converter has run on a PS5 yet. These runs prove it, smallest first. One change
per run; stop at the first run that fails and find out why before the next (`root-cause-first`).
Do them on one console first (FW 13.60), then repeat run 1 and run 3 on the other.

Build first: `make console-convert` (the ELF, checked against the SDK's stubs), then
`payload/convert/import-diff.sh` after `make payload` (what the converter imports that the helper
does not: the list below). The helper must be built *after* the converter, so it embeds it
(`make payload` warns "cannot convert on the console" when it does not).

## What is not proven yet

The SDK's stubs declare every import, but a stub can declare more than a firmware has, and the
startup code is new for this program (our `crt1.o` from `payload/sdk-crt.mk`, the helper's, not
the SDK's). The helper's own imports are proven on every console we support; these are the
converter's imports the helper does not have, and the run that exercises each:

| Import | Who calls it | Proven by |
|---|---|---|
| `_umtx_op` | std's futex (every `Mutex`, `Once`, thread park) | run 1: any output at all. A missing one stops the program before `status.json` |
| `pthread_getthreadid_np` | std's thread id | run 1 (the main thread asks for it) |
| `pthread_attr_get_np`, `pthread_attr_getguardsize`, `pthread_attr_getstack` | std's stack-overflow guard, at start | run 1: a status at all |
| `sigaltstack` | std's stack-overflow handler, at start and per thread | run 1 |
| `pthread_key_delete` | std's thread-locals | run 1 |
| `pthread_rwlock_rdlock` / `wrlock` / `unlock` | std's `env` lock (`getenv`), `RwLock` | run 1 (`std::env::args` / the log) |
| `strerror_r` | `io::Error` text | run 2 (an error's `detail`) |
| `readv`, `writev` | std's vectored I/O | run 1 (the log, `status.json`) |
| `pthread_equal` | `entry.c`'s wrap of `pthread_set_name_np` (a thread naming itself) | run 3 (worker threads); the process list shows `ps5u-convert` throughout |
| `_ioctl`, `_write`, `getsid`, `pause`, `bcmp` | std / the shims | run 1-3 |

The exact current list is what `import-diff.sh` prints; if it names a symbol not in this table,
add it here before the run.

## Run 1: the ELF by hand, `inspect` of a small folder

No helper involved: proves the program starts, its startup code, std and the imports above.

1. Pick a small game folder already on `/data` (a few hundred MiB, e.g. `/data/homebrew/<id>`).
2. Put the request in place (through the PC app's Files or Minion's Files upload, never FTP):
   `/data/ps5upload/convert/request.json` =
   `{"id":"hw1","op":"inspect","path":"/data/homebrew/<id>"}`
3. Send the ELF to the loader (the helper may run; it ignores this program):
   `python3 -c 'import socket,sys; s=socket.create_connection((sys.argv[1],9021),5); s.sendall(open(sys.argv[2],"rb").read()); s.shutdown(1)' <ps5-ip> payload/convert/ps5upload-convert.elf`
4. Within a few seconds read `/data/ps5upload/convert/status.json` and `convert.log`.

Proven when: `request.json` is gone, `status.json` is
`{"id":"hw1","state":"done","result":{"title_id":...,"files":...}}` with the folder's real title
and file count, `convert.log` ends without a panic, the helper is still up, and the process list
showed `ps5u-convert` (not `payload.elf`) while it ran (it is gone after).

Then once more with an `.exfat` image as `path` (the image reader), and once with a path that does
not exist (`state: "failed"`, `cause: "convert_source_unreadable"`, `detail` holds the error's
words: that is `strerror_r`).

## Run 2: through the helper, `inspect`

Proves the helper's side: the embedded ELF, the loader hand-off, the status follower, the job op.

From Minion (the console page, signed in), in the browser's console:
`fetch('/api/convert/inspect',{method:'POST',body:JSON.stringify({path:'/data/homebrew/<id>'})}).then(r=>r.json())`
→ `{"job":"<id>"}`; then `/api/jobs/status` with that job id until it ends.

Proven when: the job ends `ok` with the same result as run 1, `/data/ps5upload/convert/` holds only
`convert.log` afterwards (no `status.json`, `job.rec`, `request.json`, `cancel`), the PC app's
Activity lists the job as "convert".

## Run 3: through the helper, `build` (read-through, no output yet)

The converter's `build` still writes nothing: it reads every byte of the source the way a build
will, reports progress, and ends with `convert_not_built_yet`. That exercises the threads, the
reads at size, progress, Stop and the idle timer before any write code runs.

`fetch('/api/convert/start',{method:'POST',body:JSON.stringify({path:'/data/homebrew/<id>',dest:'/data/out.exfat',format:'exfat'})})`

Proven when: progress moves in the job status, it ends with the message `convert_not_built_yet`,
`convert.log` has a line `read N files, B bytes in S s (X MiB/s)` (write the speed down: the
console's read rate for a game), `/data/out.exfat` and `/data/out.exfat.partial` do not exist, the
helper is up.

Then the same with a large game (50+ GB, takes long enough to see the power tick hold off rest
mode: leave the controller alone past the console's auto-rest time) and press Stop at about 10 %:
the job ends `cancelled` within a second or two, never through the 10 s kill (the helper's log
would say "did not stop; killing it").

## Run 4: a helper that dies mid-job

Start run 3 on a large game, then replace the helper (send `ps5upload.elf` again) while it reads.
Proven when: the new helper starts normally, and once the old converter has finished,
`job.rec` is removed at the next helper start (with a `.partial` beside the destination, if a
test put one there by hand, removed too).

## If it dies

Capture before anything else, and before resending anything:

- `/data/ps5upload/convert/convert.log` (the converter's stdout/stderr, the last run; a Rust panic
  prints there, `panic = abort` then ends the program) and `status.json` if it is still there;
- the kernel log: the PC app's diagnostics (`/api/ps5/klog` on the engine), as soon as possible
  (it is a ring buffer). A missing import shows as the loader's or rtld's symbol-resolution line
  naming the symbol; a crash shows the signal and `ps5u-convert`;
- the process list (`/api/ps5/process/list`): is `ps5u-convert` still there, hung?
- the helper's own log (its `convert:` lines: "did not stop; killing it", "a helper died
  mid-conversion").

A program that never writes `status.json` and leaves `request.json` in place never reached `main`:
the startup code or an import resolved at load time. One that leaves `request.json` gone but no
status died between reading the request and its first write (std's start-up: the stack guard
imports).
