`synthetic.pkg.gz`: a small PS4 fake package of made-up files, written by fthdgn-gh/Alembic's
`alembic convert <folder> synthetic.pkg --to pkg` (which builds as LibOrbisPkg's `pkg_build` does),
so the reader is checked against a writer that isn't ours. No game's data, and no key material.

Blocks the reader never touches (padding, digests) are zeroed, which takes it from 6 MB to this size.

`update.pkg.gz`, `addon.pkg.gz`, `licence.pkg.gz`: an update (`pkg_ps4_patch`, CATEGORY `gp`,
APP_VER 01.05) for that game, an add-on with files (`pkg_ps4_ac_data`) and an add-on that is a
licence (`pkg_ps4_ac_nodata`), written by maxton/LibOrbisPkg's `PkgTool pkg_build` at
643477263b2644e0803e0f58b8726ea4e3f3b7d4 from the folders and GP4 projects `make_extras.py` writes.
Alembic writes games only, so these come from LibOrbisPkg directly, built for .NET 10 with the
PFSC read fix Alembic's `tools/ps4-oracle/compare.sh` applies, and one more change to a copy of it:
its GP4 validator refuses `pkg_ps4_patch` though its `PkgBuilder` writes one (content type DP,
flag `SUBSEQUENT_PATCH`), so the validator accepts it as it accepts `pkg_ps4_app`.

LibOrbisPkg's own `pkg_extract --passcode 0…0` of the update and the add-on gives the same bytes,
file for file, as this reader. Every 4 KiB the reader never reads was then zeroed by
`cargo run -p ps5upload-fpkg --example ps4_fixture_trim -- <in.pkg> <out.pkg>`, which checks that
the copy reads back the same, and the result gzipped (`gzip -9 -n`). The update and the add-on stay
around 1 MB because their image blocks are encrypted.
