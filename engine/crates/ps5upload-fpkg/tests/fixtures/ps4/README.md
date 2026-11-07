`synthetic.pkg.gz`: a small PS4 fake package of made-up files, written by fthdgn-gh/Alembic's
`alembic convert <folder> synthetic.pkg --to pkg` (which builds as LibOrbisPkg's `pkg_build` does),
so the reader is checked against a writer that isn't ours. No game's data, and no key material.

Blocks the reader never touches (padding, digests) are zeroed, which takes it from 6 MB to this size.
