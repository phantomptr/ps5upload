# ps5upload-convert: the first runs on a console

Nothing of the converter has run on a PS5 yet; since Phase B its `build` writes real output. These runs prove it, smallest first. One change
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
| `cpuset_getaffinity` | std's CPU count (`available_parallelism`), answered by `compat.c`'s wrap from the thread's mask | run 3 (the build's threads); `convert.log` shows no "16 CPUs" sized pool |
| `fdatasync` | `safe_write`: the image writer's sync every 32 MiB and the final sync | run 3 |
| `fdopendir` | std's `remove_dir_all` (a stopped folder build's `.partial/`) | run 4 (Stop a folder output) |
| `sched_yield` | rayon's idle workers (the read-back's BLAKE3) and std's `yield_now` | run 3 |
| `log2` | the image writers' layout maths (UFS2/PFS) | run 3 with `ffpkg` |

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

## Run 3: through the helper, `build` of a small image

The converter now builds (Phase B): the same build code as the engine
(`ps5upload-convert-core`), writing `<dest>.partial` beside `dest`, reading it back, then
renaming it in its own folder. First a small game (a few hundred MiB) to `.exfat` on `/data`:

`fetch('/api/convert/start',{method:'POST',body:JSON.stringify({path:'/data/homebrew/<id>',dest:'/data/out.exfat',format:'exfat'})})`

Proven when:

- the job's stage moves `plan` → `write` → `verify` with progress (the helper's messages
  `convert_stage_plan`, `convert_stage_write`, `convert_stage_verify`), and it ends `ok`;
- `/data/out.exfat` exists and no `/data/out.exfat.partial`, `.partial.resume` or `.partial.stamp`
  is left;
- `convert.log` has `destination /data: <fs type> free=... read_only=false` (the probe: write the
  filesystem's name down), `stage write`, `stage verify` and `built /data/out.exfat in S s: ...`
  (write the time down: the console's build rate for that size), and no panic;
- the image mounts (ShadowMount+) and the game starts from it;
- the helper is up, and the process list showed `ps5u-convert` throughout (the worker threads
  name themselves; the wrap keeps the one name).

Then once each with `format` `ffpkg` (`dest` `/data/out.ffpkg`), `ffpfs` and `folder`
(`dest` `/data/out`), and a `.exfat` image as the source.

Then the same with a large game (50+ GB, long enough to see the power tick hold off rest mode:
leave the controller alone past the console's auto-rest time): the speed, and the converter's
memory in the process list (the hash queue holds 4 reads of up to 8 MiB on the console).

## Run 4: Stop, and a full drive

Start run 3 on a large game and press Stop at about 10 %, then at about 50 %, then during
`verify`: each job ends `cancelled` within a second or two, never through the 10 s kill (the
helper's log would say "did not stop; killing it"), and nothing of the output is left (no
`.partial` file or folder). Then a folder output stopped mid-`write` (`fdopendir`: its
`.partial/` is removed).

A USB drive with less room than the game as `dest`'s folder: refused before the first byte with
`convert_no_space`. A FAT32 (`msdosfs`) USB drive with a game over 4 GiB to an image: refused with
`convert_too_big_for_drive`.

## Run 5: zstd packs

The helper does not pass the emulator yet, so `zstd: true` from Minion ends with
`convert_no_zstd_runtime` (the converter never downloads anything). To try packs by hand, put
ampr-emu-ng's `libSceAmpr.sprx` on the console (Files upload) and write the request yourself as in
run 1: `{"id":"hw5","op":"build","path":"/data/homebrew/<id>","dest":"/data/z.exfat",
"format":"exfat","zstd":true,"zstd_runtime":"/data/ps5upload/bin/libSceAmpr.sprx"}`. Proven when
the stages start with `pack`, the image is built, and the game starts from it.

## Run 6: a helper that dies mid-job

Start run 3 on a large game, then replace the helper (send `ps5upload.elf` again) while it writes.
Proven when: the new helper starts normally, and once the old converter has finished,
`job.rec` is removed at the next helper start, with the `.partial` (and `.partial.resume`,
`.partial.stamp`) beside the destination removed too.

## If it dies

Capture before anything else, and before resending anything:

- `/data/ps5upload/convert/convert.log` (the converter's stdout/stderr, the last run; a Rust panic
  prints there, `panic = abort` then ends the program) and `status.json` if it is still there;
- the kernel log: the PC app's diagnostics (`/api/ps5/klog` on the engine), as soon as possible
  (it is a ring buffer). A missing import shows as the loader's or rtld's symbol-resolution line
  naming the symbol; a crash shows the signal and `ps5u-convert`;
- the process list (`/api/ps5/process/list`): is `ps5u-convert` still there, hung?
- the helper's own log (its `convert:` lines: "did not stop; killing it", "removing what an
  unfinished conversion left").

A program that never writes `status.json` and leaves `request.json` in place never reached `main`:
the startup code or an import resolved at load time. One that leaves `request.json` gone but no
status died between reading the request and its first write (std's start-up: the stack guard
imports).
