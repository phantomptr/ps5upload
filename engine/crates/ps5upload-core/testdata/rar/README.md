# RAR test fixtures

Small `.rar` archives used by the `rar` feature unit tests in
`src/transfer.rs`. Copied from the `unrar` crate's own test `data/` (its
wrapper is MIT/Apache); they contain that crate's source file listing, not
ps5upload content.

- `crypted.rar` — content-encrypted (names listable without a password);
  password is `unrar`. First entry `.gitignore` = `target\nCargo.lock\n`.
- `comment-hpw-password.rar` — header-encrypted (names need the password);
  password is `password`. First entry `.gitignore` = same content.
- `archive.part1.rar` — first volume of a multi-volume set (parts 2+ are not
  shipped, so only listing/first-volume entries are exercised here; full
  multi-volume extraction is verified on hardware).
