#!/usr/bin/env python3
"""Apply the narrowly scoped FreeBSD 11 adaptation inside the build container."""
from pathlib import Path
import subprocess

sysroot = Path(subprocess.check_output(["rustc", "--print", "sysroot"], text=True).strip())
source = sysroot / "lib/rustlib/src/rust/library/std/src/sys/fs/unix.rs"
before = "d_ino: (*entry_ptr).d_fileno,"
after = "d_ino: (*entry_ptr).d_fileno.into(),"
text = source.read_text()
if after not in text:
    if text.count(before) != 1:
        raise SystemExit("Rust std patch context changed; refusing to patch")
    source.write_text(text.replace(before, after))
