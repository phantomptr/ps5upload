#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-or-later
"""Writes the made-up folders and GP4 projects of the PS4 update and add-ons in this folder, for
maxton/LibOrbisPkg's `PkgTool pkg_build` (at 643477263b2644e0803e0f58b8726ea4e3f3b7d4, built as
fthdgn-gh/Alembic's tools/ps4-oracle/compare.sh builds it). The PSF writer and the GP4 layout
follow that repo's tools/ps4-oracle/make_game.py (GPL-3.0-or-later).

    make_extras.py OUT   -> OUT/<case>/game/..., OUT/<case>/Project.gp4 for each case

Cases, all for synthetic.pkg.gz's title (UP0000-CUSA99990_00-TESTGAME00000000, version 01.00):
  patch   volume pkg_ps4_patch, CATEGORY gp, APP_VER 01.05: a new eboot.bin and asset, one added
          file and a changeinfo;
  addon   volume pkg_ps4_ac_data, CATEGORY ac: two data files;
  licence volume pkg_ps4_ac_nodata, CATEGORY ac: no image at all.
No game's data and no key material."""
import os
import struct
import sys
from xml.sax.saxutils import quoteattr

GAME_ID = "UP0000-CUSA99990_00-TESTGAME00000000"
STAMP = "2026-11-07 00:00:00"


def asset(seed, n):
    return b"".join(("%s %d " % (seed, i % 97)).encode() for i in range(n))


def sfo(values):
    """A PSF file; values: (key, str|int, max length)."""
    rows, keys, data = b"", b"", b""
    for key, value, max_length in values:
        if isinstance(value, int):
            raw, fmt, length, max_length = struct.pack("<I", value), 0x0404, 4, 4
        else:
            raw = value.encode() + b"\0"
            fmt, length = 0x0204, len(raw)
        rows += struct.pack("<HHIII", len(keys), fmt, length, max_length, len(data))
        keys += key.encode() + b"\0"
        data += raw.ljust(max_length, b"\0")
    while (0x14 + len(rows) + len(keys)) % 4:
        keys += b"\0"
    header = b"\0PSF" + struct.pack(
        "<IIII", 0x101, 0x14 + len(rows), 0x14 + len(rows) + len(keys), len(values)
    )
    return header + rows + keys + data


def params(category, app_ver, content_id, title):
    return sfo([
        ("APP_TYPE", 1, 4), ("APP_VER", app_ver, 8), ("ATTRIBUTE", 0, 4),
        ("CATEGORY", category, 4), ("CONTENT_ID", content_id, 48), ("FORMAT", "obs", 4),
        ("TITLE", title, 128), ("TITLE_ID", "CUSA99990", 12), ("VERSION", "01.00", 8),
    ])


CASES = {
    "patch": ("pkg_ps4_patch", GAME_ID, {
        "eboot.bin": b"\x7fELF patched eboot 01.05",
        "data/sub/asset.bin": asset("patched", 9000),
        "data/added.bin": b"added by the update",
        "sce_sys/param.sfo": params("gp", "01.05", GAME_ID, "Test PS4 Game"),
        "sce_sys/icon0.png": b"\x89PNG\r\n\x1a\nnew icon",
        "sce_sys/changeinfo/changeinfo.xml": b"<changeinfo><changes app_ver=\"01.05\"/></changeinfo>",
    }),
    "addon": ("pkg_ps4_ac_data", "UP0000-CUSA99990_00-DLCDATA000000001", {
        "dlc/map.bin": asset("map", 8000),
        "dlc/readme.txt": b"an add-on's own file",
        "sce_sys/param.sfo": params("ac", "01.00", "UP0000-CUSA99990_00-DLCDATA000000001", "Test Add-on"),
        "sce_sys/icon0.png": b"\x89PNG\r\n\x1a\ndlc icon",
    }),
    "licence": ("pkg_ps4_ac_nodata", "UP0000-CUSA99990_00-DLCLICENSE000001", {
        "sce_sys/param.sfo": params("ac", "01.00", "UP0000-CUSA99990_00-DLCLICENSE000001", "Test Unlock"),
    }),
}


def project(out, volume, content_id, files):
    game = os.path.join(out, "game")
    for rel, data in files.items():
        path = os.path.join(game, rel)
        os.makedirs(os.path.dirname(path), exist_ok=True)
        with open(path, "wb") as f:
            f.write(data)
    dirs, gp4_files = [], []

    def walk(rel, depth):
        here = os.path.join(game, rel)
        names = sorted(os.listdir(here))
        subdirs = [n for n in names if os.path.isdir(os.path.join(here, n))]
        subdirs.sort(key=lambda n: (not (rel == "" and n == "sce_sys"), n))
        for d in subdirs:
            child = (rel + "/" + d) if rel else d
            dirs.append("  " * depth + "    <dir targ_name=%s>" % quoteattr(d))
            walk(child, depth + 1)
            dirs.append("  " * depth + "    </dir>")
        for n in names:
            if n in subdirs:
                continue
            child = (rel + "/" + n) if rel else n
            gp4_files.append("    <file targ_path=%s orig_path=%s />"
                             % (quoteattr(child), quoteattr("game/" + child)))

    walk("", 0)
    gp4 = "\n".join([
        '<?xml version="1.0" encoding="utf-8" standalone="yes"?>',
        '<psproject fmt="gp4" version="1000">',
        "  <volume>",
        "    <volume_type>%s</volume_type>" % volume,
        "    <volume_id>PS4VOLUME</volume_id>",
        "    <volume_ts>%s</volume_ts>" % STAMP,
        '    <package content_id="%s" passcode="%s" entitlement_key="%s" storage_type="digital50" '
        'app_type="full" c_date="%s" />' % (content_id, "0" * 32, "0" * 32, STAMP),
        "  </volume>",
        '  <files img_no="0">',
        *gp4_files,
        "  </files>",
        "  <rootdir>",
        *dirs,
        "  </rootdir>",
        "</psproject>",
        "",
    ])
    with open(os.path.join(out, "Project.gp4"), "w") as f:
        f.write(gp4)


def main():
    out = sys.argv[1]
    for case, (volume, content_id, files) in CASES.items():
        project(os.path.join(out, case), volume, content_id, files)


main()
