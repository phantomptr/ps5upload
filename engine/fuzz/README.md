# Fuzzing the readers

[cargo-fuzz](https://github.com/rust-fuzz/cargo-fuzz) targets for every reader of a file a user
can hand the engine. A crash here is a real bug: these parsers run on files downloaded from
anywhere, and a panic in one takes a conversion (or the engine) down with it.

This folder is its own cargo workspace. The engine's workspace, its tests and CI never build it;
it needs a nightly toolchain and `cargo install cargo-fuzz`, which nothing else here does.

The layout, the sparse image encoding and several targets come from
[PS5 Dump Forge](https://github.com/quer3q/ps5-dump-forge)'s `fuzz/` (GPL-3.0-or-later, as this is).

## Targets

| Target | What it reads |
|---|---|
| `exfat` | `.exfat` images: boot sector, FAT, directory walk, file reads (`ps5upload_fpkg::exfat`) |
| `ufs2` | `.ffpkg` images: superblock, cylinder groups, inodes, block maps (`ufs2_source`) |
| `pfs` | `.ffpfs` and `.ffpfsc` through both PFS readers (`pfs_source`, `ps5-dump-forge-pfs`) |
| `pfsc` | `.ffpfsc`: the PFSC block table and its zlib blocks (`pfsc_reader`) |
| `fih_cnt` | A package's headers: FIH, the container and its digest checks (`fih`, `cnt`) |
| `pkg_source` | A whole package as a game tree, PS5 and PS4 (`pkg_source`, `ps4_pkg`) |
| `pkg_meta` | The Library's package inspector, PS3 included (`ps5upload_pkg`) |
| `kraken_block` | The Kraken block decoder (`kraken::decode_block`) |
| `kraken_descriptor` | A package's layout descriptor and the blocks it describes (`kraken_image`, `naps`) |
| `lz4_index` | The `ampr_emu.index` path index (`ampr_index`) |
| `lz4_manifest` | An LZ4 pack set's manifest, `.crc`, `.runtime` and volume header (`ampr_pack::format`) |
| `lz4_unpack` | A game read out of its LZ4 packs (`ampr_pack::image`) |
| `lz4_profile` | An LZ4 pack profile in TOML (`ampr_pack::Config`) |
| `zstd_index` | A zstd pack set's index and volume header (`ps5-dump-forge-zstd`) |
| `zstd_unpack` | A game read out of its zstd packs (`ps5-dump-forge-zstd::reader`) |
| `param` | `param.json` and `param.sfo`, through every parser the app has for them |
| `game_checks` | The checks a build runs on a game's own files (`source::readiness`) |
| `cheat` | A local cheat file, JSON or XML (`ps5upload_core::cheats::prepare_local_cheat`) |

Every target reads its input from memory: none writes a file per run. Disk images are not fed
raw. A real one is 100+ MiB of mostly zeros, so an input is a length plus the few extents that
are not zero (`src/sparse.rs`), which keeps a whole valid image to a few hundred KiB.

Not here: the archive readers (`.zip`, `.7z`, `.rar`). They take a path, and the parsing is in
their own crates, which fuzz themselves.

## Running

From this folder:

```sh
# Seed corpora: one small valid file per target, made with the engine's own writers (stable).
cargo run --release -p ps5upload-fuzz-seedgen -- corpus

# One target, for a minute.
cargo +nightly fuzz run exfat -- -max_total_time=60 -max_len=2097152 -rss_limit_mb=4096

# All of them, 20 s each (FUZZ_SECONDS=120 for longer; names after it for only some).
../../scripts/fuzz-smoke.sh          # or `make fuzz-smoke` from the repo root
```

`-max_len` has to stay above the largest seed (about 1.3 MiB), or libFuzzer cuts the seeds
short. Run the seed generator again after changing a writer or a format.

## When one crashes

The input is saved as `artifacts/<target>/crash-<hash>`.

```sh
cargo +nightly fuzz run exfat artifacts/exfat/crash-…        # see it again
cargo +nightly fuzz tmin exfat artifacts/exfat/crash-…       # make it small
```

Then fix the reader and keep the input: add a test beside the reader, in its own crate, with
the bytes inline (after `tmin` they are small), that fails without the fix. The `corpus/` and
`artifacts/` folders are not checked in.
