# libarchive (RAR readers only) — vendored for the on-console extractor

Upstream: libarchive 3.8.10, <https://github.com/libarchive/libarchive> (tag `v3.8.10`).
License: BSD 2-clause (see `COPYING`; a few files carry their own permissive notices, listed
there).

Only the RAR (1.5–4.x) and RAR5 readers and what they need are kept, built with the hand-written
`config.h` (no zlib, iconv or crypto library). These readers are independent implementations, not
derived from UnRAR, so they carry no field-of-use restriction. They cannot decrypt: an encrypted
RAR is refused by the extractor (`fs_extract_rar_encrypted`).

The RAR test archives in `payload/extractor/tests/fixtures` come from libarchive's own test suite
(`libarchive/test/*.rar.uu`, decoded). `rar5-dict128m*.rar` were made with RAR 7.23 (`-md128m`) for #801.

## PS5Upload changes

One file is changed, `archive_read_support_format_rar5.c` (#801), each change marked `PS5Upload`:

- The largest RAR5 window (dictionary) is `PS5U_RAR5_MAX_WINDOW`, set by the extractor's Makefile
  (`RAR5_MAX_WINDOW`, 4 GiB: RAR5's whole range). libarchive's own cap was 64 MiB, so archives
  packed with a bigger dictionary (common for large games) failed with "Declared dictionary size
  is not supported". A PS5 Pro extracted a 4 GiB-dictionary volume set this way (2.4 GB file).
- A file that is not solid gets a window only as large as itself (rounded up to a power of two),
  as UnRAR does: it never refers back further than its own start.
- `init_unpack` no longer allocates a window-sized `filtered_buf`: `run_filter` frees it and
  allocates the filter's own size before every use, so it only doubled the memory.

Keep these when updating.

## Updating

Copy the same file names from the new release's `libarchive/` folder, then
`make -C payload/extractor host-test` and the PS5 build.
