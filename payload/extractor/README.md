# ps5upload-extractor

The on-console archive extractor: Files (the console page) and the PC app's File system screen
can extract a `.zip`/`.zipx`, `.7z`, `.rar`, `.tar`, `.tar.gz`/`.tgz`, `.tar.bz2`/`.tbz2`,
`.tar.xz`/`.txz`, `.tar.zst`/`.tzst`, a single `.gz`/`.bz2`/`.xz`/`.zst` file, or a volume set
(`x.7z.001`, `x.zip.001`, `x.part01.rar`, `x.rar` + `x.r00`) that sits on the PS5 into a folder
on the PS5. Nothing crosses the network, and no computer is needed.

## Why a separate payload

Decompression code linked into the helper crashed it: a build with static zlib died with SIGBUS
within minutes and once froze the console (`.ffpfsc` decompress, 7.0.22). So the decoding lives in
this short-lived ELF instead. The helper carries it as bytes (`payload/Makefile` embeds
`ps5upload-extractor.elf`), and for each extraction sends it to the console's own ELF loader on
`127.0.0.1:9021`, the way a PC sends a payload. It exits when the archive is done.

## How the helper and the extractor talk

Files in `/data/ps5upload/extract/` (see `src/xreq.h` and `payload/src/archive_extract.c`):

- `request.json` — written by the helper (mode 0600), read and **deleted** by the extractor at
  start (it can hold the archive's password). `"list":true` asks only for the archive's top-level
  names (nothing is written; "Extract here" compares them with the folder first); `"top"` renames
  or leaves out top-level entries (`"from/to/from2/"`: `from` becomes `to`, `from2` is left out);
- `status.json` — rewritten by the extractor (through a temporary file and a rename in the same
  folder) at most every 250 ms: state, files and bytes done/total, the current file, the counts,
  and on failure a cause token (`fs_extract_no_space: ...`, `fs_extract_wrong_password`, ...);
- `cancel` — created by the helper when Stop is asked; the extractor stops within one buffer. If
  it has not ended 10 s later the helper kills it and removes its temporary files;
- `job.rec` — the helper's note of a running extraction, so a helper that dies mid-way has the
  folder it made removed at its next start;
- `extractor.log` — the last run's stdout/stderr.

The process is named `ps5u-extract`: never `ps5upload-*`, which the helper SIGKILLs as a stray
instance of itself.

## Rules for what is written

`src/xsink.c`:

- names stay inside the destination: an absolute name, a drive letter, a `..` component, or a link
  already on the way (when merging into an existing folder) is refused and counted as `unsafe`;
- links, hard links, devices and other special entries are never created (counted as `links`);
- every file is written to `.ps5u-x-<pid>-<n>.part` in its own folder and renamed into place:
  the rename never crosses a mount (a cross-device rename panics the PS5 kernel);
- an existing file is kept (`skipped`) unless the request asks to replace it; a folder is never
  replaced by a file; a destination that exists is refused unless the request merges into it;
- free space: the unpacked total is checked before anything is written when the archive lists it
  (7z, zip, tar), each file's size before it is written, and a full drive while writing ends the
  extraction (`fs_extract_no_space`) with the partial file removed;
- 7-Zip runs single-threaded (`Z7_ST`) and every buffer is on the heap.

On a failure or a stop, the helper removes the destination when this run created it; when merging
into an existing folder, the files already written stay.

## Freeing the archive as it goes

`"free":true` in the request deletes the archive while it is unpacked, so a game needs about its
own size of free space instead of the archive's and its own (`src/xfree.h` has the rules):

- a set of parts that are read one after another (`x.part1.rar`… or `x.rar` + `x.r00`… not solid
  and not encrypted; `x.7z.001`, `x.zip.001`, `x.tar.001`) is freed part by part. The archive is
  first read through once, whole, writing nothing: damage is found before anything is deleted, and
  it tells where each unit (a file, or a 7z block) starts. A part goes once every unit with a byte
  in it is written and flushed (each file `fsync`ed, its folder too). A solid 7z block that spans
  three parts keeps all three until the whole block is out;
- anything else (one file, a solid or encrypted RAR set, a single compressed stream) is deleted when
  everything is out and flushed: no room is saved while it runs.

The room is checked twice: before the long read, refused only when it can't fit even with every
part given back; after it, against the most the unpack takes at once with the parts going on the
way, which the read measured. Once a part is gone, only the writes themselves say the drive is full
(its free space can come back a moment after the delete).

When it stops after a part went (a failure, a Stop), nothing more is deleted, what was written stays,
and the parts left are renamed `<name>.partial-deleted` in their own folder (never a rename across
drives): without its first part the set can't be opened again. The status says so (`parts_freed`,
`parts_left`, `left_first`), and the helper then keeps the folder instead of removing it. A listing
with `free` reports the plan without decoding anything (`free_mode`, `parts`, `archive_bytes`,
`unpacked`, `free_peak`, `same_drive`, or `free_why` when it can't be done).

## Libraries

- 7-Zip 26.05, decoders only (`../third_party/7zip`, LGPL-2.1+): everything but RAR, including
  7z AES, zip AES and ZipCrypto passwords.
- libarchive 3.8.10, RAR readers only (`../third_party/libarchive`, BSD-2): RAR 1.5–4 and RAR5,
  single and multi-volume. 7-Zip's own RAR decoders come from UnRAR, whose license this GPL build
  cannot take (see `LICENSES/UnRAR-exception.md`).
- Password-protected RAR (#958): libarchive cannot decrypt RAR, so `src/xrarc.c` reads the
  volumes and hands libarchive the same archive decrypted, as a stream: encrypted headers
  (`-hp`) and file data, RAR 3/4 (AES-128) and RAR5 (AES-256), split across volumes or not.
  `src/xrcrypt.c` has the key derivations (RAR 3's SHA-1 rounds, RAR5's PBKDF2-HMAC-SHA256) on
  7-Zip's AES and SHA-256; written from the format, not from UnRAR. Nothing decrypted is written
  anywhere but the extracted files. A missing or wrong password is found before anything is
  made (`fs_extract_password_required`, `fs_extract_wrong_password`), except for RAR 3/4 with
  only the data encrypted, which has no password check: its first file fails its CRC and is
  dropped. RAR5 keys the checksums of encrypted files with the password; libarchive cannot check
  those, so `xrar.c` checks each file as it is written. RAR 1.5/2.0's own encryption is refused
  (`fs_extract_unsupported_method`). An archive whose first volume shows no encryption is read by
  libarchive directly, as before; one that starts encrypting after its first 4096 headers is
  still refused with `fs_extract_rar_encrypted`.

## Building and testing

```console
make -C payload/extractor          # the PS5 ELF (payload/Makefile does this for the helper)
make -C payload/extractor host-test  # host build + tests/run.py (python3; 7zz or 7z for the 7z cases)
```

`tests/run.py` covers name traversal, links (in the archive and already in the destination),
multi-volume sets (7z, RAR4, RAR5) and a missing volume, passwords (7z with and without header
encryption, zip AES, ZipCrypto, a non-ASCII password), password-protected RAR (RAR 3/4 and RAR5:
data or headers encrypted, stored, BLAKE2, multi-part, solid multi-part, wrong and missing
passwords, listing, a missing volume, damage; fixtures from `tests/make_encrypted_rar.py`),
truncated and damaged archives, out of space before and while writing, conflicts and Stop, and
freeing the archive as it goes (split 7z with and without solid blocks, zip and tar sets, RAR sets
solid and not, single archives, damage found first, Stop and a failure half way, the room check).
`tests/xfree_test.c` checks the freeing schedule on its own.
