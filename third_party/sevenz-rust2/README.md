# sevenz-rust2 0.23.0 — vendored, with one fix and one addition

Upstream: <https://github.com/hasenbanck/sevenz-rust2> (Apache-2.0).
Only `src/`, `Cargo.toml` and the licence are vendored; upstream's examples,
tests and test data are not, so this stays a small patch rather than a fork.

Both workspaces redirect to this copy via `[patch.crates-io]`:

```text
engine/Cargo.toml              sevenz-rust2 = { path = "../third_party/sevenz-rust2" }
client/src-tauri/Cargo.toml    sevenz-rust2 = { path = "../../third_party/sevenz-rust2" }
```

## The change: archive sizes stay `u64` (#434)

Upstream turns 7z sizes, which are `u64` in the format, into `usize` before
using them as read limits:

- a block's packed size, which caps how much compressed input the decoder may
  read (`BoundedReader::new(source, pack_size as usize)`),
- a block's and each file's unpacked size, which cap the output and decide
  when the CRC is checked (`Crc32VerifyingReader`, the per-file
  `BoundedReader`),
- the unpacked size handed to each coder (`add_decoder`), and the remaining
  length in `SharedBoundedReader`.

On a 64-bit target that is lossless. On the 32-bit Android build (armv7,
shipped in the APK next to arm64) `usize` is 32 bits, so every limit past
4 GiB wraps: a 4.7 GB block was cut to 405 MB of input. The decoder then ran
out of input inside an LZMA chunk and reported `dist overflow` (or, for
stored chunks, a CRC failure), and a file over 4 GiB was cut short. Game
backups are routinely tens of GB in one block, so 32-bit Android could not
upload most 7z games.

The patch keeps those sizes `u64` end to end (`BoundedReader::new` and
`Crc32VerifyingReader::new` take `u64`; `add_decoder`'s `uncompressed_len`
is `u64`; bounds are clamped with `usize::try_from(..).map_or(buf.len(), ..)`
instead of a cast), and the two pre-allocation hints saturate instead of
wrapping. Nothing changes on 64-bit.

Verified by decoding a 4.7 GB single-block archive on `linux/arm/v7`:
unpatched, block 0 fails after 405 MB; patched, all 4,700,000,000 bytes
come out with the CRC checked.

Reported upstream: <https://github.com/hasenbanck/sevenz-rust2/issues/148>. Drop this copy once
upstream ships the same fix.

## The addition: `Archive::block_pack_range`

`Archive::block_pack_range(block)` gives the file offsets of a block's packed bytes (the
offset of its first packed stream, and the offset just past its last). Upstream keeps what it
is worked out from (`StreamMap::block_first_pack_stream_index`, `pack_stream_offsets`,
`Block::packed_streams`) private to the crate. The engine needs it to unpack a `.7z` from its
last block backwards and cut the archive short behind each one
(`ps5upload-core/src/archive_extract/free.rs`). It reads nothing and changes no behaviour; if
this copy is dropped, that code needs another way to learn the offsets.
