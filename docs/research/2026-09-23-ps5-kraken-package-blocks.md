# PS5 package compression: how Kraken blocks and their records are stored

Status: **partly decoded, not yet implemented.** Our FPKG builder writes every block
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

## `naps_pkg_layout.dat`

Section order: 16-byte header; outer-block digests (8 B each); shuffle patterns; `fidx`
(`files + 3` entries of 6 B: 40-bit **logical** file offset + 1-byte type); `u2c`
(`ublocks/8 + 1` entries of 10 B; the leading 24 bits are **not** a plain LE index,
their top byte climbs ~8.8 per entry); `cblockinfo` (9 B each), which exactly fills the
rest.

### `cblockinfo` block record (72 bits, little-endian), verified on 72/72 blocks

| Bits | Field |
|---|---|
| 0–13 | constant within the file (`0xA5D` for `d/actor`, `0xA47` for the file before). Unknown. |
| 14–30 | even half's compressed length − 1 |
| 31 | 1 on every compressed block seen |
| 32 | NOT even half's literal mode (1 = mode 0) |
| 33 | 1 on all seen (probably "even half is LZ") |
| 34 | 0 on all seen |
| 35 | NOT odd half's literal mode |
| 36 | 1 on all seen (probably "odd half is LZ") |
| 37–47 | 0 on all seen |
| 48–65 | the block's compressed **end** offset, modulo 256 KiB |
| 66–71 | 4 on most blocks; 5, 17, 29, 45 on a few. Unknown. |

A block's start is the previous record's end, so the odd length is
`end − start − even_length`.

### Interleaved records

One every 16 slots, for example `00 00 00 04 00 00 c5 77 10`,
`00 00 00 08 …`, `01 00 00 10 …`. They repeat the previous record's end offset in
bits 48–65, and bits 24+ climb by 4 per 16 blocks: the absolute compressed position in
64 KiB units (a window anchor). Exact layout still to fit.

## Still to do before an encoder is useful

1. Decode bits 0–13, 66–71, the interleaved record, the `u2c` entries and the header's
   remaining fields. Blocks with other shapes (stored halves, entropy-only halves, a
   file's short last block, the metadata region) will show what the constant bits mean.
2. A Kraken encoder that emits the excess framing, with a decoder in tests to prove
   every block round-trips. A raw-array (type 0) encoder is enough to start;
   Huffman arrays improve the ratio.
3. Write the records, `u2c` and anchors, build a small package, and install and launch
   it on a console. Only the console can confirm the records are right.

Research tooling (probe, patched decoder, ground-truth dump) lived in a session
scratch folder and is not kept. The method above rebuilds it in an hour.
