#!/usr/bin/env bash
# PS5Upload Play spike - build the app folder (runs inside docker/Dockerfile's image,
# or on any Linux host with clang-18, lld-18, llvm-18, python3, zlib and ffmpeg).
# Based on PS5Term's tools/build.sh (GPL-3.0-or-later), cut down to one source file.
# SPDX-License-Identifier: GPL-3.0-or-later
set -euo pipefail

root=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
deps="$root/.deps"
build="$root/build"
dist="$root/dist"
native="$root/tooling/native"
sdk="$deps/ps5-payload-sdk"
sdk_url="https://github.com/ps5-payload-dev/sdk/releases/download/v0.42/ps5-payload-sdk.zip"
sdk_hash="8cfbc7cd5811e719eb4f0c47eea668d3dc7b40bc8ab11c4a5031d40c23ec02da"
title_id=$(python3 -c 'import json,sys; print(json.load(open(sys.argv[1]))["titleId"])' "$root/sce_sys/param.json")

# Loader/container constants from PS5Term (validated there on 6.02, 12.70 and 13.60).
module_sdk=0x02000009
companion_sdk=0x08050001
fself_magic=0x1D3D154F

mkdir -p "$deps" "$build/host" "$build/obj" "$build/stubs" "$build/assets" "$dist"

# --- pinned public payload SDK (headers, libc.a, import stubs, lld wrapper) ---
if [[ ! -x $sdk/bin/prospero-lld ]]; then
    if [[ ! -f $deps/ps5-payload-sdk.zip ]]; then
        wget -q "$sdk_url" -O "$deps/ps5-payload-sdk.zip.download"
        mv "$deps/ps5-payload-sdk.zip.download" "$deps/ps5-payload-sdk.zip"
    fi
    printf '%s  %s\n' "$sdk_hash" "$deps/ps5-payload-sdk.zip" | sha256sum --check --strict
    unzip -q -o "$deps/ps5-payload-sdk.zip" -d "$deps"
fi

# --- host tools: FSELF/link converter and the clean-room libc builder ---
tool="$build/host/ps5-native-tool"
clang++ -std=c++20 -O2 -Wall -Wextra -Werror \
    "$native/native_app_builder.cpp" "$native/self_container.cpp" \
    "$native/elf_object.cpp" "$native/sce_module_writer.cpp" -lz -o "$tool"
clang++ -std=c++20 -O2 -Wall -Wextra -Werror "$native/libc_builder.cpp" -o "$build/host/libc-builder"

# --- clean-room runtime libc.prx (must reproduce PS5Term's released digest) ---
"$build/host/libc-builder" "$native/runtime/api-surface.txt" "$native/runtime/imports.txt" \
    "$build/libc.raw.elf"
"$tool" self --sign --in "$build/libc.raw.elf" --out "$build/libc.prx" >/dev/null
(cd "$build" && sha256sum --check --strict "$root/tooling/libc.prx.sha256")

# --- test streams: 1080p60 Annex B with an access unit delimiter before every picture ---
font=/usr/share/fonts/truetype/dejavu/DejaVuSansMono-Bold.ttf
counter="drawtext=fontfile=$font:text='%{n}':fontsize=200:fontcolor=white:box=1:boxcolor=black@0.8:x=80:y=80"
if [[ ! -f $build/assets/avc.264 ]]; then
    ffmpeg -loglevel error -y -f lavfi -i "testsrc2=size=1920x1080:rate=60:duration=5" \
        -vf "$counter" -pix_fmt yuv420p -c:v libx264 -profile:v high -level 4.2 -preset medium \
        -b:v 15M -maxrate 20M -bufsize 20M -g 60 -bf 0 -x264-params aud=1 \
        -f h264 "$build/assets/avc.264"
fi
if [[ ! -f $build/assets/hevc.265 ]]; then
    ffmpeg -loglevel error -y -f lavfi -i "testsrc2=size=1920x1080:rate=60:duration=5" \
        -vf "$counter" -pix_fmt yuv420p -c:v libx265 -profile:v main -preset fast \
        -x265-params "aud=1:bframes=0:keyint=60:repeat-headers=1:bitrate=15000:log-level=error" \
        -f hevc "$build/assets/hevc.265"
fi
# Reference hashes: FNV-1a 64 of each decoded picture's visible NV12 (Y rows then UV rows).
clang -O2 -Wall -Wextra -Werror "$root/tools/nv12-hashes.c" -o "$build/host/nv12-hashes"
for stream in avc.264 hevc.265; do
    if [[ ! -f $build/assets/$stream.ref ]]; then
        ffmpeg -loglevel error -i "$build/assets/$stream" -f rawvideo -pix_fmt nv12 - |
            "$build/host/nv12-hashes" 1920 1080 > "$build/assets/$stream.ref"
    fi
done
if [[ ! -f $build/icon0.png ]]; then
    ffmpeg -loglevel error -y -f lavfi -i "color=c=0x0b1020:s=512x512" \
        -i "$root/../../branding/png/icon-1024.png" \
        -filter_complex "[1]scale=512:512[l];[0][l]overlay=0:0,format=rgb24" -frames:v 1 \
        "$build/icon0.png"
fi

# --- compile ---
export PS5_PAYLOAD_SDK="$sdk" PS5_CLANG=clang-18
common=(-O2 -Wall -Wextra -ffunction-sections -fdata-sections -fno-exceptions -fno-rtti)
sh "$root/tooling/prospero-clang18" -std=c++20 "${common[@]}" -mavx2 -mfma \
    -c "$root/src/main.cpp" -o "$build/obj/main.o"
sh "$root/tooling/prospero-clang18" -std=c++20 "${common[@]}" \
    -c "$native/app_crt.cpp" -o "$build/obj/app_crt.o"
sh "$root/tooling/prospero-clang18" -std=c++20 "${common[@]}" \
    -c "$native/app_cpp_runtime.cpp" -o "$build/obj/app_cpp_runtime.o"

# The SDK's crt1.o carries the kernel API helpers; its _start is renamed so ours wins.
llvm-objcopy --redefine-sym _start=__sdk_crt_entry --redefine-sym __crt_start=__sdk_crt_start_impl \
    "$sdk/target/lib/crt1.o" "$build/crt1_kernel.o"
rm -f "$build/libps5_kernel_api.a"
llvm-ar rcs "$build/libps5_kernel_api.a" "$build/crt1_kernel.o"

# Import stubs for system modules the SDK has no stub for.
bash "$root/tools/make-stub.sh" "$build/stubs/libSceVideodec2.so" libSceVideodec2.sprx \
    sceVideodec2QueryComputeMemoryInfo sceVideodec2AllocateComputeQueue \
    sceVideodec2ReleaseComputeQueue sceVideodec2QueryDecoderMemoryInfo \
    sceVideodec2CreateDecoder sceVideodec2DeleteDecoder sceVideodec2Decode \
    sceVideodec2Flush sceVideodec2Reset sceVideodec2GetPictureInfo

"$sdk/bin/prospero-lld" -T "$native/ps5-pie.ld" --eh-frame-hdr --gc-sections \
    --version-script "$native/app-symbols.map" -e _start -o "$build/llvm-pie.elf" \
    "$build/obj/app_crt.o" "$build/obj/app_cpp_runtime.o" "$build/obj/main.o" \
    "$build/libps5_kernel_api.a" "$sdk/target/lib/libc.a" \
    --as-needed "$sdk/target/lib/libkernel.so" "$sdk"/target/lib/*.so "$build/stubs/libSceVideodec2.so"

# A raw syscall outside libkernel kills an app on the console (PS5Term finding).
raw=$(llvm-objdump -d --no-show-raw-insn "$build/llvm-pie.elf" |
    awk '/^[0-9a-f]+ <.*>:$/ {fn=$2} /[[:space:]]syscall$/ {print fn}' | sort -u | tr '\n' ' ')
[[ -z $raw ]] || { echo "raw syscall instructions in: $raw" >&2; exit 2; }

"$tool" link --in "$build/llvm-pie.elf" --out "$build/eboot.elf" \
    --stub "$build/stubs/libSceVideodec2.so" --stub-dir "$sdk/target/lib" \
    --module-sdk "$module_sdk" --companion-sdk "$companion_sdk" --file-name eboot.elf >/dev/null

app="$dist/$title_id"
rm -rf -- "$app"
mkdir -p "$app/sce_sys" "$app/sce_module" "$app/assets"
"$tool" self --sign --in "$build/eboot.elf" --out "$app/eboot.bin" --magic "$fself_magic" >/dev/null
cp "$build/libc.prx" "$app/sce_module/libc.prx"
cp "$root/sce_sys/param.json" "$app/sce_sys/param.json"
cp "$build/icon0.png" "$app/sce_sys/icon0.png"
cp "$build/assets/avc.264" "$build/assets/hevc.265" "$app/assets/"
cp "$build/assets/avc.264.ref" "$build/assets/hevc.265.ref" "$dist/"
"$tool" self --inspect --file "$app/eboot.bin" | grep -E 'integrity|segments'
# Imported symbols, for a preflight check against what the console exports.
llvm-nm -D --undefined-only "$build/llvm-pie.elf" | awk '{print $2}' | sort > "$dist/imports.txt"
printf 'App folder: %s (%s files)\n' "$app" "$(find "$app" -type f | wc -l)"
