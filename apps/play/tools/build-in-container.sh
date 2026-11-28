#!/usr/bin/env bash
# PS5Upload Play - build the app folder dist/FAKE99261/ (and, with `release`, the zip).
# Runs inside docker/Dockerfile's image (build.sh starts it), or on any Linux host with
# clang-18, lld-18, llvm-18, python3 (numpy, Pillow, fontTools), git, zlib and zip.
# Based on PS5Term's tools/build.sh (GPL-3.0-or-later), as the phase 0 spike was.
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
opus_ver=1.5.2
opus_url="https://downloads.xiph.org/releases/opus/opus-$opus_ver.tar.gz"
opus_hash="65c1d2f78b9f2fb20082c38cbe47c951ad5839345876e46941612ee87f9a7ce1"
param="$root/sce_sys/param.json"
title_id=$(python3 -c 'import json,sys; print(json.load(open(sys.argv[1]))["titleId"])' "$param")
content_version=$(python3 -c 'import json,sys; print(json.load(open(sys.argv[1]))["contentVersion"])' "$param")
# 13.000.000 -> 13.0.0: the version people read
version=$(python3 -c 'import sys; print(".".join(str(int(p)) for p in sys.argv[1].split(".")))' "$content_version")
[[ $content_version =~ ^[0-9]{2}\.[0-9]{3}\.[0-9]{3}$ ]] || {
    echo "param.json contentVersion must look like 13.000.000 (got $content_version)" >&2
    exit 2
}

# Loader/container constants from PS5Term (validated there on 6.02, 12.70 and 13.60).
module_sdk=0x02000009
companion_sdk=0x08050001
fself_magic=0x1D3D154F

mkdir -p "$deps" "$build/host" "$build/obj" "$build/stubs" "$build/assets" "$dist"

fetch() { # url file sha256
    if [[ ! -f $2 ]]; then
        wget -q "$1" -O "$2.download"
        mv "$2.download" "$2"
    fi
    printf '%s  %s\n' "$3" "$2" | sha256sum --check --strict --quiet
}

# --- pinned public payload SDK (headers, libc.a, import stubs, lld wrapper) ---
if [[ ! -x $sdk/bin/prospero-lld ]]; then
    fetch "$sdk_url" "$deps/ps5-payload-sdk.zip" "$sdk_hash"
    unzip -q -o "$deps/ps5-payload-sdk.zip" -d "$deps"
fi

# --- host tools: FSELF/link converter and the clean-room libc builder ---
tool="$build/host/ps5-native-tool"
if [[ ! -x $tool || $native/native_app_builder.cpp -nt $tool ]]; then
    clang++ -std=c++20 -O2 -Wall -Wextra -Werror \
        "$native/native_app_builder.cpp" "$native/self_container.cpp" \
        "$native/elf_object.cpp" "$native/sce_module_writer.cpp" -lz -o "$tool"
    clang++ -std=c++20 -O2 -Wall -Wextra -Werror "$native/libc_builder.cpp" -o "$build/host/libc-builder"
fi

# --- clean-room runtime libc.prx (must reproduce PS5Term's released digest) ---
if [[ ! -f $build/libc.prx ]]; then
    "$build/host/libc-builder" "$native/runtime/api-surface.txt" "$native/runtime/imports.txt" \
        "$build/libc.raw.elf"
    "$tool" self --sign --in "$build/libc.raw.elf" --out "$build/libc.prx" >/dev/null
fi
(cd "$build" && sha256sum --check --strict --quiet "$root/tooling/libc.prx.sha256")

# --- assets: home-screen art and music (tools/assets), the UI font ---
bash "$root/tools/assets/build-assets.sh" "$build/assets"
if [[ ! -f $build/assets/font.bin || $root/tools/make-font.py -nt $build/assets/font.bin ]]; then
    python3 "$root/tools/make-font.py" "$build/assets/font.bin"
fi

export PS5_PAYLOAD_SDK="$sdk" PS5_CLANG=clang-18
cc=(sh "$root/tooling/prospero-clang18")

# --- libopus (BSD), fixed-point decoder build ---
opus_src="$deps/opus-$opus_ver"
opus_lib="$build/opus/libopus.a"
if [[ ! -f $opus_lib ]]; then
    fetch "$opus_url" "$deps/opus-$opus_ver.tar.gz" "$opus_hash"
    rm -rf "$opus_src" && tar -xzf "$deps/opus-$opus_ver.tar.gz" -C "$deps"
    sources=$(make -s -C "$opus_src" -f - <<'MK'
include celt_sources.mk
include silk_sources.mk
include opus_sources.mk
all:
	@echo $(CELT_SOURCES) $(SILK_SOURCES) $(SILK_SOURCES_FIXED) $(OPUS_SOURCES)
MK
)
    mkdir -p "$build/opus/obj"
    objs=()
    for s in $sources; do
        o="$build/opus/obj/$(echo "$s" | tr / _).o"
        "${cc[@]}" -std=gnu11 -O2 -w -ffunction-sections -fdata-sections \
            -DOPUS_BUILD -DFIXED_POINT -DDISABLE_FLOAT_API -DVAR_ARRAYS -DPACKAGE_VERSION="\"$opus_ver\"" \
            -I"$opus_src/include" -I"$opus_src/celt" -I"$opus_src/silk" -I"$opus_src/silk/fixed" \
            -I"$opus_src" -c "$opus_src/$s" -o "$o"
        objs+=("$o")
    done
    rm -f "$opus_lib"
    llvm-ar rcs "$opus_lib" "${objs[@]}"
fi

# --- the app ---
common=(-O2 -Wall -Wextra -Werror -ffunction-sections -fdata-sections -fno-exceptions -fno-rtti
        -fno-threadsafe-statics -fno-use-cxa-atexit -fno-math-errno -mavx2 -mfma
        -DPLAY_PS5 "-DPLAY_VERSION=\"$version\"" -I"$opus_src/include")
objs=()
for s in "$root"/src/core/*.cpp "$root"/src/net/*.cpp "$root"/src/ui/*.cpp "$root"/src/ps5/*.cpp \
         "$root"/src/app/app.cpp "$root"/src/app/guest.cpp "$root"/src/app/companion.cpp "$root"/src/app/log.cpp; do
    o="$build/obj/$(basename "$(dirname "$s")")_$(basename "${s%.cpp}").o"
    "${cc[@]}" -std=c++20 "${common[@]}" -c "$s" -o "$o"
    objs+=("$o")
done
"${cc[@]}" -std=c11 -O2 -Wall -ffunction-sections -fdata-sections -DNDEBUG \
    -c "$root/third_party/qrcodegen/qrcodegen.c" -o "$build/obj/qrcodegen.o"
objs+=("$build/obj/qrcodegen.o")
for s in app_crt app_cpp_runtime; do
    "${cc[@]}" -std=c++20 -O2 -Wall -Wextra -ffunction-sections -fdata-sections -fno-exceptions -fno-rtti \
        -c "$native/$s.cpp" -o "$build/obj/$s.o"
done

# The SDK's crt1.o carries the kernel API helpers; its _start is renamed so ours wins.
llvm-objcopy --redefine-sym _start=__sdk_crt_entry --redefine-sym __crt_start=__sdk_crt_start_impl \
    "$sdk/target/lib/crt1.o" "$build/crt1_kernel.o"
rm -f "$build/libps5_kernel_api.a"
llvm-ar rcs "$build/libps5_kernel_api.a" "$build/crt1_kernel.o"

# Import stubs for a system module the SDK has no stub for.
bash "$root/tools/make-stub.sh" "$build/stubs/libSceVideodec2.so" libSceVideodec2.sprx \
    sceVideodec2QueryComputeMemoryInfo sceVideodec2AllocateComputeQueue \
    sceVideodec2ReleaseComputeQueue sceVideodec2QueryDecoderMemoryInfo \
    sceVideodec2CreateDecoder sceVideodec2DeleteDecoder sceVideodec2Decode \
    sceVideodec2Flush sceVideodec2Reset sceVideodec2GetPictureInfo

"$sdk/bin/prospero-lld" -T "$native/ps5-pie.ld" --eh-frame-hdr --gc-sections \
    --version-script "$native/app-symbols.map" -e _start -o "$build/llvm-pie.elf" \
    "$build/obj/app_crt.o" "$build/obj/app_cpp_runtime.o" "${objs[@]}" "$opus_lib" \
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
cp "$param" "$app/sce_sys/param.json"
cp "$build/assets/icon0.png" "$build/assets/pic0.dds" "$build/assets/pic1.dds" "$build/assets/snd0.at9" "$app/sce_sys/"
cp "$build/assets/font.bin" "$build/assets/ui-bg.rgba" "$build/assets/logo.rgba" "$build/assets/music.pcm" "$app/assets/"
cp "$root/../../LICENSE" "$app/LICENSE"
cp "$root/NOTICE.md" "$app/NOTICE.md"
"$tool" self --inspect --file "$app/eboot.bin" | grep -E 'integrity|segments'
# Imported symbols, for a preflight check against what the console exports.
llvm-nm -D --undefined-only "$build/llvm-pie.elf" | awk '{print $2}' | sort > "$dist/imports.txt"
printf 'App folder: %s (version %s, %s files)\n' "$app" "$version" "$(find "$app" -type f | wc -l)"

if [[ ${1:-} == release ]]; then
    zip_name="PS5Upload-Play-$title_id-$version.zip"
    rm -f "$dist/$zip_name"
    # Fixed times and order: the same sources give the same zip.
    find "$app" -exec touch -h -d '2026-01-01 00:00:00' {} +
    (cd "$dist" && find "$title_id" -type f | LC_ALL=C sort | zip -q -X -D -9 "$zip_name" -@)
    (cd "$dist" && sha256sum "$zip_name" > "$zip_name.sha256")
    printf 'Release zip: %s\n' "$dist/$zip_name"
fi
