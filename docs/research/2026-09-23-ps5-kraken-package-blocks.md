# PS5 package compression: how Kraken blocks and their records are stored

Status: **layout fully decoded (records, sections, anchors); writer not yet implemented.** Our FPKG builder writes every block
stored (uncompressed), which installs. This note records what is known about the
compressed form, so the work can resume without redoing it.

## Why this matters

A PS5 package's inner image (`pfs_image.dat`) can be Kraken-compressed. The console
decompresses it in hardware as the game reads. The package installer only accepts
Kraken here; a zlib image is refused (`ppfs_create_cmpc_for_naps`, EOPNOTSUPP). So a
compressed FPKG has to use Kraken.

Nobody had published the per-block record format. LibProsperoPKG's reader guesses each
block's mode by trial decoding, and its record layout does not match Sony's packages.

## Method: ground truth from a real package

Sample: Marvel's Spider-Man 2 BASE, built by the sdk-fpkg279 kit (Sony Publishing
Tools, compression level 7), plus the decrypted source folder it was built from.

1. Read `pfs_image.dat` and `naps_pkg_layout.dat` out of the outer PFS lazily (the
   package is 108 GB).
2. Every compressed block begins with its first 8 plain bytes (the Kraken seed), so
   searching the image for each source file's first 8 bytes locates that file's data.
   `d/actor` (18,837,504 bytes) starts at image offset `0x21A17`.
3. Decode each 256 KiB block against the known file content, searching for each
   sub-chunk's compressed length and literal mode until the output matches byte for
   byte. All 72 blocks of `d/actor` decode exactly.
4. Map each block to its `cblockinfo` record by end offset, then fit the record's bits.

The decoder was oozextract (a Rust port of powzix/ooz) with the "excess" framing
below added.

## The block format (what the console's decoder reads)

- The image is data-first: files' compressed data concatenated, small files unaligned.
- Each 256 KiB block is two 128 KiB halves ("even", "odd"), stored **bare**: no Oodle
  stream header, no quantum header, no chunk header. Sizes and modes come only from
  the record.
- A half is a Kraken LZ chunk body. The even half of every block starts with an 8-byte
  raw seed (each block decodes independently); the odd half continues the block's
  window.
- **Excess framing (newer Oodle than ooz supports):** after the seed, a control byte
  with bit 7 set. `count = flag & 0x3F` (plus `next_byte * 0x20` when `count > 0x1F`).
  The last `count` bytes of the half are a separate forward/backward bit stream
  holding the long-length escapes. In this form the escape count is **not** gamma-coded
  at the start of the backward stream; it is the number of 255 bytes in the packed
  length array.
- Literal mode 0 (delta literals) or 1 (raw), per half.
- The decoder copies matches 8 bytes at a time, so an encoder must never emit a
  distance under 8.

## `naps_pkg_layout.dat` — complete structure

Verified by walking four packages (Spider-Man 2, EA FC 26 unlocker, a DLC unlocker, the Web
Browser homebrew): every block tiles the logical mount contiguously, every compressed block
decodes, and the rebuilt mount has the inner PFS superblock exactly at the metadata base.

In order:

1. **Header**, 16 bytes: `word0 = (files−1) | comp<<24 | (keys−1)<<26 | shuffles<<28 |
   ublocks<<32`, `word1 = outer_blocks | (records−2)<<24`. `comp` is 2 (Kraken).
2. **Outer digests**, 8 bytes per outer 64 KiB block — all zero in every sample.
3. **fidx**, exactly `files` entries of 6 bytes (40-bit logical offset, 1-byte type): each
   file's logical start, then the data end, the metadata base, and the mount size with type
   `0x40`.
4. **u2c**, `floor(ublocks/8) + 1` groups of 10 bytes (24-bit base + 7 one-byte deltas),
   covering ublocks `0 ..= ublocks`. The value for ublock *n* is the record index of the first
   block record whose logical start is `>= n × 256 KiB` (anchors are counted in the indices but
   never pointed at); the entry past the mount end points at the end sentinel.
5. **Padding to 8 bytes, then `00 00 04`** (24-bit 0x40000, the ublock size).
6. **Records**, 9 bytes each, `records` of them. Index 0 is an all-zero start anchor.
7. Zero padding (to 8 or 16 bytes; readers use the counts).

Records are 72-bit little-endian integers.

**Block record** (bit 31 = 1):

| Bits | Field |
|---|---|
| 0–13 | logical start within its 256 KiB window, / 16 (floor) |
| 14–30 | even half's stored length − 1 (a raw 256 KiB block: `0x1FFFF`) |
| 31 | 1 |
| 32 | NOT even half's literal mode (1 = mode 0, delta literals) |
| 33 | even half is an LZ chunk |
| 34 | set on halves stored as bare entropy arrays (the all-zero blocks) |
| 35 | NOT odd half's literal mode |
| 36 | odd half is an LZ chunk |
| 37–47 | 0 |
| 48–65 | the block's stored **end** offset, mod 256 KiB |
| 66 | **the next record is an anchor.** This is the only thing that tells a reader an anchor from a block (it separates all 1.14 M Spider-Man records; no bit inside an anchor does), so two anchors never follow each other |
| 67–68 | hint, meaning unknown (Sony writes 0, 2 or 4; 0 appears on compressed metadata blocks) |
| 69–71 | hint, constant within a file (0–5); 0 appears on compressed metadata blocks |

A raw half is one whose stored length equals its logical length; its flags are 0.

**Anchor record** — recognised only by the previous record's bit 66 (index 0 is always one):
bits 0–25 = the next stored position in 1 MiB units, bits 26–47 = the same in 256 KiB windows,
bits 48–65 = its offset within that window. (Bit 31 is just window bit 5: set on anchors past
8 MiB, so it cannot identify blocks.) Anchors appear (a) wherever the next block's
stored data does not follow the previous block's (each file group starts on a 64 KiB
boundary), and (b) at every 16th record index. The **terminator** is an anchor at the end of
the stored data with bits 67–68 = 1 (value 2 in bits 66–68). The **end sentinel** is the last
record: bits 0–13 = the mount size mod 256 KiB / 16, all else 0.

A block's stored start is the previous record's end (or the anchor's position). Its logical
length is `min(256 KiB, next file boundary − start)`; blocks never cross a fidx boundary.

**All-zero data** (for example the gap between the data and the metadata base) is stored as
16 bytes per 256 KiB block: each half is `27 ff fc 00 03 00 40 00`, a one-symbol Huffman
array decoding to 128 KiB of zeros, flags `0x04`.

Sony's encoder also uses forms ours never emits: blocks with flag `0x4` and no stored bytes
(all-zero or deduplicated), anchors that jump **backwards** to reuse stored data, entropy-only
halves. `kraken_image::describe` walks all of them: all 1,039,050 of Spider-Man's blocks tile its
272 GB mount.

### Why our uncompressed layouts looked right

Our writer placed records 4 bytes off this framing and packed fields 32 bits off, which
reproduced Sony's bytes for the uncompressed samples without describing them. A small
test package built that way installed and mounted (2026-09-14). A compressed image cannot
work that way: the writer has to follow this model.

## Still to do before an encoder is useful

1. The `naps_meta_18.dat` block map in the install segment must describe compressed blocks
   too; decode it from Spider-Man's (the XTS key is already in `si_write`).
2. A Kraken encoder that emits the excess framing, with a decoder in tests to prove
   every block round-trips. A raw-array (type 0) encoder is enough to start;
   Huffman arrays improve the ratio.
3. Write the records, `u2c` and anchors, build a small package, and install and launch
   it on a console. Only the console can confirm the records are right.

Research tooling (probe, patched decoder, ground-truth dump) lived in a session
scratch folder and is not kept. The method above rebuilds it in an hour.
