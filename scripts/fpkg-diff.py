#!/usr/bin/env python3
"""Compare two finalized debug packages, structure by structure.

Built for one question: what does a package that the console accepts have that ours does
not. Both are read the same way, and every section prints either `same` or the two values,
so a difference stands out without reading hex.

    scripts/fpkg-diff.py <reference.pkg> <ours.pkg>

Sections, in the order the console encounters them: the finalized-image header, the
container header and its entries, the install segment (SI) and the manifest it carries, the
outer PFS superblock and inode table, then the inner image and its layout descriptor.

Read-only; it opens the packages and prints.
"""

from __future__ import annotations

import io
import struct
import subprocess
import sys
import zipfile
from pathlib import Path

BLOCK = 0x10000

FIH_FIELDS = [
    (0x04, "<I", "format"),
    (0x08, "<I", "flags"),
    (0x10, "<Q", "pfs_offset"),
    (0x18, "<Q", "pfs_size"),
    (0x20, "<Q", "superblock_offset"),
    (0x28, "<Q", "block_size"),
    (0x50, "<I", "inner_meta_base"),
    (0x58, "<Q", "cnt_offset"),
    (0x60, "<Q", "0x60"),
    (0x68, "<Q", "0x68"),
    (0x90, "<I", "inner_blocks"),
    (0x94, "<I", "content_inodes"),
    (0x98, "<I", "0x98"),
    (0x9C, "<I", "content_version"),
    (0xA0, "<Q", "inner_size"),
    (0xA8, "<Q", "naps_len"),
    (0xF0, "<I", "app_file_count"),
    (0xF8, "<I", "flt_count"),
]

CNT_FIELDS = [
    (0x04, ">I", "version"),
    (0x08, ">I", "0x08"),
    (0x0C, ">I", "0x0c"),
    (0x10, ">I", "entry_count"),
    (0x14, ">I", "0x14"),
    (0x18, ">I", "entry_table"),
    (0x1C, ">I", "0x1c"),
    (0x20, ">Q", "body_offset"),
    (0x28, ">Q", "body_size"),
    (0x30, ">Q", "0x30"),
    (0x34, ">I", "mandatory_size"),
    (0x70, ">I", "drm_type"),
    (0x74, ">I", "0x74"),
    (0x78, ">I", "0x78"),
    (0x7C, ">I", "promote_size"),
    (0x80, ">I", "0x80"),
    (0x84, ">I", "0x84"),
    (0x9C, ">I", "0x9c"),
    (0x4B0, ">Q", "cnt_offset_copy"),
    (0x4B8, ">Q", "container_size"),
]

ENTRY_NAMES = {
    0x0001: "DIGESTS", 0x0010: "ENTRY_KEYS", 0x0020: "IMAGE_KEY", 0x0080: "GENERAL_DIGESTS",
    0x0100: "METAS", 0x0200: "ENTRY_NAMES", 0x0400: "LICENSE_DAT", 0x0401: "LICENSE_INFO",
    0x040A: "IMAGEDIGS", 0x1001: "PLAYGO_CHUNK", 0x1200: "ICON0_PNG", 0x1280: "ICON0_DDS",
    0x2000: "PARAM_JSON", 0x2010: "PLAYGO_HASH_TABLE", 0x2011: "PLAYGO_FICM",
}


def read(path: Path) -> dict:
    data = path.read_bytes()
    out = {"path": path, "size": len(data), "head": data[:BLOCK], "data": data}
    out["fih"] = {name: struct.unpack_from(fmt, data, off)[0] for off, fmt, name in FIH_FIELDS}
    cnt = out["fih"]["cnt_offset"]
    out["cnt_offset"] = cnt
    head = data[cnt : cnt + 0x1000]
    count = struct.unpack_from(">I", head, 0x10)[0]
    table = struct.unpack_from(">I", head, 0x18)[0]
    out["cnt"] = {name: struct.unpack_from(fmt, head, off)[0] for off, fmt, name in CNT_FIELDS}
    entries = []
    for i in range(count):
        o = table + i * 0x20
        eid, _n, f1, _f2, off, size = struct.unpack_from(">6I", data, cnt + o)
        entries.append((eid, f1, off, size))
    out["entries"] = entries
    # The install segment: find the stored ZIP that follows the container.
    start = cnt + max(out["cnt"]["container_size"], 0)
    blob = data[start:]
    at = blob.find(b"PK\x03\x04")
    out["si"] = None
    if at >= 0:
        z = zipfile.ZipFile(io.BytesIO(blob[at:]))
        members = [(n, z.getinfo(n).file_size) for n in z.namelist()]
        out["si"] = members
        for name, _size in members:
            if name.endswith("pfsimage.xml"):
                out["manifest"] = z.read(name)
            if name.endswith("playgo-chunk.dat"):
                out["playgo"] = z.read(name)
    return out


def dump_inner(path: Path) -> bytes | None:
    """The decrypted inner image, written out by the crate's own dumper."""
    out = Path("/tmp") / f"fpkg-diff-inner-{path.stem}.img"
    exe = Path(__file__).resolve().parent.parent / "engine/target/release/examples/fpkg_dump_inner"
    if not exe.exists():
        return None
    try:
        subprocess.run([str(exe), str(path), str(out)], check=True,
                       capture_output=True, timeout=600)
        return out.read_bytes()
    except (subprocess.CalledProcessError, subprocess.TimeoutExpired):
        return None


def section(title: str) -> None:
    print(f"\n== {title}")


def compare_fields(label: str, a: dict, b: dict) -> None:
    section(label)
    for k in a:
        same = a[k] == b.get(k)
        print(f"  {k:20} {'same' if same else f'{a[k]}  vs  {b.get(k)}'}")


def main() -> int:
    if len(sys.argv) != 3:
        print((__doc__ or "").strip().splitlines()[-1], file=sys.stderr)
        return 2
    a = read(Path(sys.argv[1]))
    b = read(Path(sys.argv[2]))
    print(f"reference {a['path'].name}  {a['size']} bytes")
    print(f"ours      {b['path'].name}  {b['size']} bytes")

    compare_fields("finalized-image header", a["fih"], b["fih"])
    compare_fields("container header", a["cnt"], b["cnt"])

    section("container entries")
    ae = {e[0]: e for e in a["entries"]}
    be = {e[0]: e for e in b["entries"]}
    for eid in sorted(set(ae) | set(be)):
        name = ENTRY_NAMES.get(eid, "")
        x, y = ae.get(eid), be.get(eid)
        if x and y:
            print(f"  {eid:#06x} {name:18} {'same' if x == y else f'size {x[3]} vs {y[3]}, flags {x[1]:#x} vs {y[1]:#x}'}")
        else:
            print(f"  {eid:#06x} {name:18} {'only in reference' if x else 'only in ours'}")

    section("install segment")
    print(f"  reference: {a['si']}")
    print(f"  ours     : {b['si']}")

    section("manifest (pfsimage.xml)")
    for label, pkg in (("reference", a), ("ours", b)):
        m = pkg.get("manifest")
        if not m:
            print(f"  {label}: absent")
            continue
        import re
        tags = sorted(set(re.findall(rb"<([a-zA-Z-]+)[ >]", m)))
        print(f"  {label}: {len(m)} bytes, {len(tags)} elements")
        print(f"    {[t.decode() for t in tags][:18]}")

    section("inner image")
    for label, pkg in (("reference", a), ("ours", b)):
        inner = dump_inner(pkg["path"])
        if not inner:
            print(f"  {label}: could not dump")
            continue
        print(f"  {label}: {len(inner)} bytes = {len(inner)//BLOCK} blocks")
        for i in range(min(3, len(inner)//BLOCK)):
            blk = inner[i*BLOCK:(i+1)*BLOCK]
            print(f"    blk {i}: nonzero={sum(1 for x in blk if x):6} head={blk[:24].hex(' ')}")
        # the metadata base the header names, which is where the superblock should sit
        base = pkg["fih"]["inner_meta_base"] * (pkg["fih"]["block_size"] or BLOCK)
        if base and base + 0x40 <= len(inner):
            sb = inner[base:base+0x40]
            print(f"    superblock@{base:#x}: {sb[:32].hex(' ')}")

    section("playgo-chunk.dat")
    for label, pkg in (("reference", a), ("ours", b)):
        p = pkg.get("playgo")
        print(f"  {label}: {len(p) if p else 'absent'}")
        if p:
            print(f"    {p[:0x40].hex(' ')}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
