#!/usr/bin/env bash
# PS5Upload Play - build the presentation and in-app assets.
# SPDX-License-Identifier: GPL-3.0-or-later
#
# Usage (inside the ps5upload-play-build image, from apps/play):
#   bash tools/assets/build-assets.sh build/assets
#
# Writes to OUTDIR:
#   icon0.png            512x512 home-screen tile
#   pic0.dds, pic1.dds   3840x2160 BC7 backgrounds (pic0.png, pic1.png previews)
#   snd0.at9             30 s ATRAC9 selection music, whole-track loop
#   music.wav/.pcm       the same loop as 16-bit 48 kHz stereo (.pcm is raw s16le)
#   logo.rgba, ui-bg.rgba  in-app logo and background (u32 w, u32 h, RGBA8)
# Intermediates live in OUTDIR/work. Steps whose outputs are newer than their
# inputs are skipped. The ATRAC9 converter is fetched once into
# .deps/ps5-at9-converter at a pinned commit and is used only at build time.

set -euo pipefail

if [[ $# -ne 1 ]]; then
    echo "usage: $0 OUTDIR" >&2
    exit 2
fi
here=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
play=$(cd -- "$here/../.." && pwd)
branding=$(cd -- "$play/../../branding/png" && pwd)
mkdir -p "$1/work"
out=$(cd -- "$1" && pwd)
work=$out/work

AT9_REPO=https://github.com/BlackBearRevived/ps5-at9-converter
AT9_COMMIT=74a722898a6b97fd52256770bf45187e40c3fa2e
at9_dir=$play/.deps/ps5-at9-converter
CC=${CC:-clang-18}

# stale OUT... -- IN...: true when an output is missing or older than an input
stale() {
    local outs=() o i
    while [[ $1 != -- ]]; do outs+=("$1"); shift; done
    shift
    for o in "${outs[@]}"; do
        [[ -e $o ]] || return 0
        for i in "$@"; do [[ $i -nt $o ]] && return 0; done
    done
    return 1
}

step() { printf '== %s\n' "$*"; }

# 1. art: icon0, pic0/pic1 (PNG + raw RGBA), logo, in-app background
art_outs=("$out/icon0.png" "$out/pic0.png" "$out/pic1.png" "$work/pic0.rgba" "$work/pic1.rgba"
          "$out/logo.rgba" "$out/ui-bg.rgba")
icon_svg=$play/../../branding/icon-play.svg
if stale "$work/icon-play-512.png" -- "$icon_svg"; then
    step "icon (branding/icon-play.svg)"
    rsvg-convert -w 512 -h 512 "$icon_svg" -o "$work/icon-play-512.png"
fi
if stale "${art_outs[@]}" -- "$here/make-art.py" "$work/icon-play-512.png" "$branding/mark-white-1024.png"; then
    step "art"
    python3 "$here/make-art.py" "$branding" "$out" "$work"
fi

# 2. BC7 encoder, then pic0.dds / pic1.dds and the decode self-test
if stale "$work/bc7enc" -- "$here/bc7enc.c"; then
    step "compile bc7enc"
    "$CC" -O2 -std=c11 -Wall -Wextra -Werror -pthread -o "$work/bc7enc" "$here/bc7enc.c" -lm
fi
for pic in pic0 pic1; do
    if stale "$out/$pic.dds" -- "$work/$pic.rgba" "$work/bc7enc"; then
        step "encode $pic.dds"
        "$work/bc7enc" -e 3840 2160 "$work/$pic.rgba" "$out/$pic.dds.tmp"
        mv "$out/$pic.dds.tmp" "$out/$pic.dds"
    fi
    "$work/bc7enc" -t "$out/$pic.dds" "$work/$pic.rgba" 38
done

# 3. music
if stale "$out/music.wav" "$out/music.pcm" -- "$here/make-music.py"; then
    step "music"
    python3 "$here/make-music.py" "$out"
fi

# 4. snd0.at9 with the pinned open-source ATRAC9 converter
if [[ ! -d $at9_dir/.git ]]; then
    step "fetch ps5-at9-converter"
    mkdir -p "$(dirname -- "$at9_dir")"
    git clone -q "$AT9_REPO" "$at9_dir"
fi
if [[ $(git -C "$at9_dir" rev-parse HEAD) != "$AT9_COMMIT" ]]; then
    git -C "$at9_dir" fetch -q origin
    git -C "$at9_dir" checkout -q --detach "$AT9_COMMIT"
fi
[[ $(git -C "$at9_dir" rev-parse HEAD) == "$AT9_COMMIT" ]] || {
    echo "ps5-at9-converter is not at $AT9_COMMIT" >&2
    exit 1
}
if stale "$out/snd0.at9" -- "$out/music.wav" "$here/build-assets.sh"; then
    step "encode snd0.at9"
    # The loop is seamless, so no fades; loudnessSnd0 (-28.00) is set in
    # sce_sys/param.json by hand, so the converter does not touch it.
    PYTHONDONTWRITEBYTECODE=1 python3 "$at9_dir/ps5_at9.py" \
        --input "$out/music.wav" --output "$out/snd0.at9" \
        --duration 30 --loudness -28 --fade-in 0 --fade-out 0
fi

# 5. validate
python3 "$here/check-assets.py" "$out"
