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
| `log2f` | the `.pkg` builder's Kraken encoder (its symbol costs), any level but `stored` | run 3d at `fast` |

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

## Run 3b: a `.ffpfsc`

No new imports (the ELF imports what it did before `.ffpfsc` output was added). New on the
console: the deflate encoders (libdeflate, zlib-rs) on 3 worker threads, the PFSC reader in the
read-back, and the container synced every 32 MiB as it is written.

`fetch('/api/convert/start',{method:'POST',body:JSON.stringify({path:'/data/homebrew/<id>',dest:'/data/out.ffpfsc',format:'ffpfsc',inner:'exfat',ffpfsc_level:'balanced'})})`

Proven when: the stages go `plan` → `compress` → `verify`, `convert.log` has `.ffpfsc holding
out.exfat, balanced compression on 3 threads` and the `ffpfsc: compression=balanced ... threads=3`
line (write down `stream_ms` and `raw_bytes`: the console's compression rate), the image mounts
through ShadowMount+ and the game starts. Then `inner` `ffpkg` at `fast`, and one large game at
`smallest` (the slow end: the page says 20-40 minutes for 30 GB) with Stop at about 50 %.

## Run 3c: a game in a `.zip` and a `.7z`

No import the helper lacks (`import-diff.sh` lists the same 24 as before); `getcwd` is new to
the converter (std's `current_dir`, through the `zip` crate), and the helper imports it. New on the
console: the archive reader's own thread (it decodes the archive front to back and hands the
build the files it asks for), deflate and LZMA/LZMA2 decoding, and its memory: 4 MiB of chunks in
flight, up to 64 MiB of small files kept, and a `.7z` block's dictionary (refused past 256 MiB
on the console).

Put a small game folder in a `.zip` (deflate) and in a `.7z` made with 7-Zip's defaults (solid
LZMA2) on `/data`, then from Minion's Convert pick each (the picker lists `.zip`, `.7z` and `.rar`):

- inspect shows the game's title and file count, and "read straight out of the archive";
- `.zip` → `.exfat` and `.7z` → folder: the stages go `plan` → `write` → `verify`, the output
  mounts or runs, and `convert.log` ends with `... (.7z archive, solid, read in place ...): N
  pass(es) over the archive` (write N down: 1 is ideal; the console's decode speed is the
  `built ... in S s` line);
- Stop during a `.7z` → `.exfat` write: it ends `cancelled` within a second or two (the archive
  thread stops too), nothing left;
- a `.rar`: refused at once with "A .rar can't be converted on the PS5" (the helper refuses it;
  no converter starts).

Proven when all of that holds and `getcwd` raised nothing (a missing import stops the program
before its first status, see "If it dies").

## Run 3d: a `.pkg`

One import the helper lacks: `log2f` (Kraken's cost model; a `stored` package never calls it).
`statvfs` is new to the converter (the builder's free-space check) and the helper imports it.
New on the console: the package builder (`ps5upload-fpkg`, a fake-signed debug package; it is
written and read back, never installed here), Kraken on 3 threads (the converter caps every
worker pool at 3), and a thread that syncs the `.pkg.partial` every 32 MiB it grows (U5: the
builder writes through its own file, so it cannot be wrapped).

`fetch('/api/convert/start',{method:'POST',body:JSON.stringify({path:'/data/homebrew/<id>',dest:'/data/out.pkg',format:'pkg',pkg_level:'stored'})})`

Proven when: the stages go `check` → `plan` → `compress` → `write` → `verify`, it ends `ok`,
`convert.log` has `.pkg at Kraken level Stored on 3 threads` and `built /data/out.pkg in S s:
pkg, ... N syncs while written, verified: ok` (N > 0 for anything over 32 MiB), no `.partial` is
left, and the package installs through the usual debug-package route (outside this program).
Then `pkg_level` `fast` on the same game (`log2f`; write down the time: the page estimates
about 15 minutes for 30 GB), then Stop during `compress`: `cancelled` within a second or two,
nothing left.

## Run 3e: a `.pkg` of a game in asset packs, unpacked

No new import (the ELF's import list is unchanged): the LZ4 and zstd pack readers were already
linked for a packed game that is packed again (`zstd_packs::readable_for_packing`), and the unpack
code is the engine's (`ps5upload-convert-core` `unpack_packs`). New on the
console: a package built from a source that is decoded as it is read, and the emulator rule
without a download. Not run on a console yet.

1. A libSceAmpr game in zstd packs made by PS5Upload (ampr-emu-ng in `fakelib`). Minion, Convert,
   `.pkg`: "Unpack the asset packs" is on, and Convert is held with the sentence that the game has
   no emulator that works with plain files. Turn on "Leave out the AMPR emulator" and convert.
   Proven when: it ends `ok`; `convert.log` has `unpack packs: zstd packs decoded, N plain files, a
   new ampr_emu.index lists them; left out fakelib/libSceAmpr.sprx` and `.pkg at Kraken level ...
   asset packs unpacked, the AMPR emulator left out`; the package is about the plain game's size;
   it installs, and the game starts. The last is the open question: only Spider-Man 2 on FW 5.10
   has started from such a package.
2. The same request without leaving the emulator out, sent by hand:
   `fetch('/api/convert/start',{method:'POST',body:JSON.stringify({path:'<game>',dest:'/data/out.pkg',format:'pkg',unpack_packs:true})})`
   ends `convert_unpack_needs_emulator` before anything is written (no `.pkg`, no `.partial`).
3. A game in LZ4 packs with ampr_emu 0.4.2.1 or newer: unpacked with the emulator kept (`ampr_emu
   0.4.2.1 at fakelib/libSceAmpr.sprx stays`), it installs and starts.
4. A game that does not read through libSceAmpr, in zstd packs: unpacked with the switch off it
   keeps its emulator; with it on the package has neither the emulator nor an `ampr_emu.index`.
5. Stop during `compress` of run 1: `cancelled` within a second or two, nothing left. Write down
   the time of run 1 against run 3d's on the same game unpacked: decoding the packs is the
   difference.
6. "Unpack the asset packs" off: the package holds the packs as before (run 3d's result).

## Run 3f: the AMPR emulator put on the console from the app

New: the converter reads `/data/ps5upload/convert/runtime/ampr_emu-<version>/libSceAmpr.sprx`
(written by the app's Settings › This PS5 › "Put the AMPR emulator on this PS5", never on its
own). No new import. Not run on a console yet.

1. Press the button: it says `ampr_emu 0.4.2.1 is on this PS5.`; Files shows that file, about
   1 MiB. Pressed again: the same, nothing written (the engine log has no new "put ampr_emu").
2. Run 3e's game (ampr-emu-ng in `fakelib`) in Minion, Convert, `.pkg`: no sentence holds Convert
   now, "Leave out the AMPR emulator" is off. Convert. Proven when: it ends `ok`; `convert.log` has
   `ampr_emu 0.4.2.1 on the console goes in where the game's own emulator has to go` and
   `ampr-emu-ng ... at fakelib/libSceAmpr.sprx is replaced by ampr_emu`; the package's
   `fakelib/libSceAmpr.sprx` is byte for byte the file on the console; it installs and the game
   starts. The same game made into a `.pkg` by the desktop app holds the same files.
3. Delete the `ampr_emu-0.4.2.1` folder in Files: the page holds Convert again with the sentence
   that names the app's button (run 3e step 1).

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
