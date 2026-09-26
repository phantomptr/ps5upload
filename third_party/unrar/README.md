# unrar 0.5.8 — vendored, with one fix

Upstream: <https://github.com/muja/unrar.rs> (MIT OR Apache-2.0).
Only `src/` and the licences are vendored; upstream's examples, tests and
`.rar` test data are not, so this stays a small patch rather than a fork.

Both workspaces redirect to this copy via `[patch.crates-io]`:

```text
engine/Cargo.toml              unrar = { path = "../third_party/unrar" }
client/src-tauri/Cargo.toml    unrar = { path = "../../third_party/unrar" }
```

## The change

One hunk, in `src/open_archive.rs`, in the `UCM_CHANGEVOLUMEW` arm of
`Internal::callback`. Upstream:

```text
WideCString::from_ptr_truncate(p1 as *const _, 2048)
```

`from_ptr_truncate(p, len)` builds `slice::from_raw_parts(p, 2048)` and
hands it to `from_vec_truncate`, which takes `impl Into<Vec<_>>` — so all
2048 elements are **copied first**, and only then truncated at the NUL.

UnRAR passes `NextName.data()` from a `std::wstring` holding a path
(`DllVolNotify`, `volume.cpp:274`). For a ~60-character volume name that
copy reads 8 KB out of a far smaller allocation: a ~7.7 KB out-of-bounds
heap read, on **every volume transition**.

The fix scans for the NUL first (still capped at UnRAR's 2048) and copies
only what is actually there.

### Second change: a streaming sink mode

`StreamSink`, `ReadToSink`, `process_file_raw_seeded` and `read_to_sink` are
local additions (all marked `ps5upload local addition` in the source).

Upstream's only way to get an entry's bytes without writing them to disk is
`read()`, which returns a `Vec<u8>` — the whole entry in memory. PS5 game
dumps carry single entries of 30 GB and more, so ps5upload needs the chunks
as UnRAR produces them.

The C API already delivers data incrementally through `UCM_PROCESSDATA`;
upstream's `ReadToVec` just accumulates it. `ReadToSink` forwards each chunk
to a bounded channel instead, under the same `Operation::Test` that
`ReadToVec` uses — which decompresses *without* writing files. Sending on a
full channel blocks, which is exactly the backpressure that keeps peak
memory at a few chunks.

`process_file_raw_seeded` exists because `process_file_raw` builds its
`Output` with `Default::default()`, and a channel cannot be conjured from
`Default`.

## Why vendored rather than upgraded

`0.5.8` is the latest release; there is no upstream fix to take. The
analysis is written up as a ready-to-send issue in
`docs/unrar-upstream-bug.md`.

## What it looked like

Any multi-part `.rar` aborted a debug build:

```text
unsafe precondition(s) violated: ptr::copy_nonoverlapping
  unrar::open_archive::callback   (open_archive.rs:519)
  DllVolNotify / MergeArchive     <- switching to the next volume
```

Release builds compiled the check out and appeared to work, because the
copy is still truncated at the true NUL — the value was right and only
the over-read was unsound.

## If you update this crate

Re-apply the hunk, or drop the vendoring entirely once upstream ships a
release containing the fix (check the `UCM_CHANGEVOLUMEW` arm). The patch
is marked in the source with a `ps5upload local fix` comment.
