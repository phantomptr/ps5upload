#!/usr/bin/env python3
"""Makes the password-protected RAR fixtures (tests/fixtures/pw-*.rar).

Needs RAR itself (rarlab.com): RAR 7 for the RAR5 ones and RAR 6 for the RAR 3/4 ones (RAR 7 no
longer writes RAR 4). Run once; the archives are committed and run.py checks what they hold
against the same files, made by encrypted_rar_files() below.

    python3 make_encrypted_rar.py /path/to/rar7 /path/to/rar6
"""
import hashlib
import os
import shutil
import subprocess
import sys
import tempfile

HERE = os.path.dirname(os.path.abspath(__file__))
FIXTURES = os.path.join(HERE, "fixtures")

# RAR5 takes the password as UTF-8; the RAR 4 one is 46 characters, which takes RAR 3's key
# derivation through its long-password quirk.
PW5 = "pässwörd RAR5"
PW4 = "this is a long RAR 4 password, 46 characters!!"


def encrypted_rar_files():
    """The files in every pw-* fixture: name -> bytes."""
    noise, h = b"", b"seed"
    while len(noise) < 40000:
        h = hashlib.sha256(h).digest()
        noise += h
    return {
        "a.txt": b"hello\n",
        "sub/b.bin": noise[:40000],
        "sub/deep/c.txt": b"deep " * 3000,
        "sub/empty.txt": b"",
        "sub/odd.bin": noise[:1001],
    }


# name, rar binary (5 or 4), switches
CASES = [
    ("pw-rar5-data.rar", 5, ["-p" + PW5]),
    ("pw-rar5-headers.rar", 5, ["-hp" + PW5]),
    ("pw-rar5-stored.rar", 5, ["-m0", "-p" + PW5]),
    ("pw-rar5-blake2.rar", 5, ["-htb", "-p" + PW5]),
    ("pw-rar5-multi.rar", 5, ["-hp" + PW5, "-v17k"]),
    ("pw-rar5-solid-multi.rar", 5, ["-s", "-p" + PW5, "-v17k"]),
    ("pw-rar5-stored-multi.rar", 5, ["-m0", "-p" + PW5, "-v17k"]),
    ("pw-rar4-data.rar", 4, ["-ma4", "-p" + PW4]),
    ("pw-rar4-headers.rar", 4, ["-ma4", "-hp" + PW4]),
    ("pw-rar4-stored.rar", 4, ["-ma4", "-m0", "-p" + PW4]),
    ("pw-rar4-multi.rar", 4, ["-ma4", "-hp" + PW4, "-v17k"]),
]


def main():
    rar5, rar4 = sys.argv[1], sys.argv[2]
    src = tempfile.mkdtemp(prefix="pw-rar-src-")
    try:
        for name, data in encrypted_rar_files().items():
            p = os.path.join(src, name)
            os.makedirs(os.path.dirname(p), exist_ok=True)
            with open(p, "wb") as f:
                f.write(data)
        for name, ver, sw in CASES:
            for old in os.listdir(FIXTURES):
                if old == name or old.startswith(name[:-4] + ".part"):
                    os.unlink(os.path.join(FIXTURES, old))
            exe = rar5 if ver == 5 else rar4
            subprocess.run([exe, "a", "-idq", "-r", "-ep1", *sw, os.path.join(FIXTURES, name), "a.txt", "sub"],
                           cwd=src, check=True)
    finally:
        shutil.rmtree(src)


if __name__ == "__main__":
    main()
