RAR archives from libarchive's test suite (`libarchive/test/*.rar.uu`, v3.8.10, BSD 2-clause),
uudecoded and renamed:

| here | libarchive |
| --- | --- |
| rar4.rar | test_read_format_rar.rar |
| rar5.rar | test_read_format_rar5_compressed.rar |
| rar4-encrypted.rar | test_read_format_rar4_encrypted.rar |
| rar5-encrypted.rar | test_read_format_rar5_encrypted.rar |
| rar5-symlink.rar | test_read_format_rar5_symlink.rar |
| rar5-solid.part01-04.rar | test_read_format_rar5_multiarchive_solid.part01-04.rar |
| rar4-multi.part1-6.rar | test_rar_multivolume_multiple_files.part1-6.rar |

`pw-*.rar` (#958, password-protected RAR) were made by `../make_encrypted_rar.py` with RAR 7.23
(RAR5) and RAR 6.24 (RAR 4, `-ma4`) from files that script makes; the passwords are in it.

`crafted-rar5-size-wrap.rar` and `crafted-rar5-enc-header-loop.rar` (password `pw`) are hostile
RAR5 archives built by hand for the extractor's header scanner (xrarc.c): a header whose 64-bit
data size wraps the scan position back onto itself after an encrypted file split mid-block, and a
second encryption header inside the encrypted headers. Both used to loop forever.

`crafted-rar4-many-salts.rar` (password: 127 × `x`) is a 1 KB RAR 4 archive with encrypted
headers, 40 service blocks and 5 salts in turn, so each block costs a fresh key derivation (about
half a second with that password): Stop has to reach the derivation itself.
