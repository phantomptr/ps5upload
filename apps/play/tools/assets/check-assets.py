#!/usr/bin/env python3
# PS5Upload Play - validate the presentation assets in OUTDIR.
# Adapted from PS5Term's tools/validate-assets.sh,
# Copyright (C) 2026 BlackBearReloaded.
# SPDX-License-Identifier: GPL-3.0-or-later
#
# Usage: check-assets.py OUTDIR
#
# Checks icon0.png (512x512 PNG), pic0.dds / pic1.dds (single 3840x2160
# BC7_UNORM DX10 2D surface, no mipmaps) and snd0.at9 (looped 48 kHz ATRAC9
# RIFF/WAVE within the 2 MiB limit).
import struct
import sys
from pathlib import Path

SND0_LIMIT = 2_097_152
ATRAC9_GUID = bytes.fromhex("D242E147BA368D4D88FC61654F8C836C")


def fail(message):
    raise SystemExit(f"check-assets: {message}")


def check_icon(path):
    icon = path.read_bytes()
    if (len(icon) < 24 or icon[:8] != b"\x89PNG\r\n\x1a\n"
            or struct.unpack(">II", icon[16:24]) != (512, 512)):
        fail(f"{path} must be a 512x512 PNG")


def check_dds(path):
    data = path.read_bytes()
    if len(data) < 148 or data[:4] != b"DDS ":
        fail(f"{path} is not a DDS file")
    header_size = struct.unpack_from("<I", data, 4)[0]
    height, width = struct.unpack_from("<II", data, 12)
    mipmaps = struct.unpack_from("<I", data, 28)[0]
    format_id, dimension = struct.unpack_from("<II", data, 128)
    array_size = struct.unpack_from("<I", data, 140)[0]
    expected = 148 + width // 4 * (height // 4) * 16
    profile = (header_size, width, height, mipmaps, data[84:88], format_id,
               dimension, array_size, len(data))
    if profile != (124, 3840, 2160, 1, b"DX10", 98, 3, 1, expected):
        fail(f"{path} must be a single 3840x2160 BC7_UNORM DX10 2D DDS without mipmaps")


def check_at9(path):
    data = path.read_bytes()
    if (len(data) < 128 or data[:4] != b"RIFF" or data[8:12] != b"WAVE"
            or struct.unpack_from("<I", data, 4)[0] + 8 != len(data)
            or len(data) > SND0_LIMIT):
        fail(f"{path} is not a complete RIFF/WAVE file within {SND0_LIMIT} bytes")
    chunks = {}
    position = 12
    while position + 8 <= len(data):
        chunk_id = data[position:position + 4]
        size = struct.unpack_from("<I", data, position + 4)[0]
        payload = position + 8
        if payload + size > len(data):
            fail(f"{path} contains a truncated RIFF chunk")
        chunks[chunk_id] = (payload, size)
        position = payload + size + size % 2
    if not all(c in chunks for c in (b"fmt ", b"fact", b"smpl", b"data")):
        fail(f"{path} is missing an fmt/fact/smpl/data chunk")
    (fmt, fmt_size), (smpl, smpl_size) = chunks[b"fmt "], chunks[b"smpl"]
    if fmt_size < 40 or smpl_size < 32:
        fail(f"{path} has undersized format or loop metadata")
    tag, channels, rate, byte_rate = struct.unpack_from("<HHII", data, fmt)
    loops = struct.unpack_from("<I", data, smpl + 28)[0]
    if (tag != 0xFFFE or channels not in (1, 2) or rate != 48_000
            or not 0 < byte_rate <= channels * 12_000
            or data[fmt + 24:fmt + 40] != ATRAC9_GUID or loops < 1):
        fail(f"{path} must be looped 48 kHz ATRAC9 within the supported bit rate")
    return len(data)


def main():
    if len(sys.argv) != 2:
        raise SystemExit("usage: check-assets.py OUTDIR")
    root = Path(sys.argv[1])
    for name in ("icon0.png", "pic0.dds", "pic1.dds", "snd0.at9"):
        if not (root / name).is_file():
            fail(f"missing {root / name}")
    check_icon(root / "icon0.png")
    check_dds(root / "pic0.dds")
    check_dds(root / "pic1.dds")
    size = check_at9(root / "snd0.at9")
    print(f"check-assets: presentation assets OK in {root} (snd0.at9 {size:,} bytes)")


if __name__ == "__main__":
    main()
