#!/usr/bin/env bash
# Builds payload/convert/ps5upload-convert.elf: engine/crates/ps5upload-convert as a PS5 ELF, in
# Docker (`make console-convert`). Adapted from PS5 Dump Forge's ps5/build.sh and the ELF checks of
# its scripts/release-ps5.sh (github.com/quer3q/ps5-dump-forge, GPL-3.0-or-later).
#
#   payload/convert/build.sh            build the image if needed, build the ELF, check it
#   payload/convert/build.sh --check    check the ELF that is already built
#   payload/convert/build.sh --image    print the toolchain image's name (CI saves and loads it)
#
# The image is tagged with a hash of what it is built from (the Dockerfile, the std patch and the
# SDK pin), and not built again while an image with that tag is there: CI restores it from its
# cache with `docker load` and skips the build. Any change to those makes a new tag.
#
# Output: payload/convert/ps5upload-convert.elf and, beside it, ps5upload-convert.elf.txt (the
# ELF header, program headers, dynamic section and the symbols it imports). Cargo's output and
# its registry cache stay in engine/target/ps5/ (git-ignored), apart from the host's target/.
set -euo pipefail
cd "$(dirname "$0")/../.."

image=ps5upload-convert-toolchain
out=engine/target/ps5
elf=payload/convert/ps5upload-convert.elf
# The modules the ELF may ask the console to load. Both are in every payload's process already.
want_needed="libSceLibcInternal.sprx libkernel_web.sprx"

if [[ "${1:-}" != --inside ]]; then
  # The helper's SDK pin, so both are built against one SDK.
  sdk_tag=$(sed -n 's/^PS5_SDK_TAG=//p' scripts/ps5-sdk.env)
  sdk_sha=$(sed -n 's/^PS5_SDK_SHA256=//p' scripts/ps5-sdk.env)
  [[ -n $sdk_tag && -n $sdk_sha ]] || { echo "build.sh: no SDK pin in scripts/ps5-sdk.env" >&2; exit 1; }
  # What the image is built from (its build context holds only these two files that it reads).
  if command -v sha256sum > /dev/null; then sha() { sha256sum; }; else sha() { shasum -a 256; }; fi
  image="$image:$({ cat payload/convert/Dockerfile payload/convert/prepare-rust-std.py; echo "$sdk_tag $sdk_sha"; } | sha | cut -c1-16)"
  if [[ "${1:-}" == --image ]]; then
    echo "$image"
    exit 0
  fi
  [[ -f third_party/ava1/Cargo.toml ]] || {
    echo "build.sh: third_party/ava1 is empty; run: git submodule update --init third_party/ava1" >&2
    exit 1
  }
  if docker image inspect "$image" > /dev/null 2>&1; then
    echo "build.sh: $image is there; not building it again"
  else
    DOCKER_BUILDKIT=1 docker build -t "$image" --build-arg SDK_TAG="$sdk_tag" --build-arg SDK_SHA256="$sdk_sha" payload/convert
  fi
  # The checkout's owner inside the container, so nothing in the tree ends up root-owned.
  exec docker run --rm --user "$(id -u):$(id -g)" -e HOME=/tmp \
    -e CARGO_HOME=/work/$out/cargo -v "$PWD:/work" "$image" \
    payload/convert/build.sh --inside "${1:-}"
fi

# The shims are copied from upstream and stay byte-identical (shim/UPSTREAM lists their hashes).
(cd payload/convert && grep -E '^[0-9a-f]{64}  ' shim/UPSTREAM | sha256sum -c --quiet -)

if [[ "${2:-}" != --check ]]; then
  cc="$PS5_PAYLOAD_SDK/bin/prospero-clang"
  mkdir -p "$out/shim"
  for src in shim/freebsd11.c shim/syscalls.S entry.c compat.c; do
    name=$(basename "${src%.*}")
    "$cc" -O2 -Wall -Wextra -Werror -Ipayload/include -c "payload/convert/$src" -o "$out/shim/$name.o"
  done

  # The startup code is the helper's: our build of the SDK's crt, whose kernel searches are best
  # effort (payload/sdk-crt.mk; the SDK's own stops a payload before main() on FW 5.50). Its
  # objects go under engine/target/ps5, not beside the sources, where the host's build keeps its
  # own (built by another clang).
  make -s -f payload/sdk-crt.mk SDK_CRT_BUILD="/work/$out/sdk-crt" "/work/$out/sdk-crt/crt1.o"
  export CONVERT_CRT1="/work/$out/sdk-crt/crt1.o"

  # build-std on a stable toolchain: the image's rust-src carries the one-line std patch.
  export RUSTC_BOOTSTRAP=1
  # The PS5 kernel fills FreeBSD 11 structs (stat 120 bytes, dirent 264, old statfs); libc and the
  # std built here default to FreeBSD 12's. libc >= 0.2.187 reads this cfg; rustflags reach std too
  # under -Z build-std. ps5upload-convert asserts the struct sizes at compile time, so losing this
  # fails the build instead of producing an ELF that reads every path as neither file nor folder.
  export CARGO_TARGET_X86_64_PS5_FREEBSD_RUSTFLAGS='--cfg libc_unstable_freebsd_version="11"'
  export CARGO_TARGET_DIR="/work/$out"
  export CARGO_TARGET_X86_64_PS5_FREEBSD_LINKER=/work/payload/convert/linker.sh
  export CONVERT_SHIM_OBJ="/work/$out/shim"
  export CC_x86_64_ps5_freebsd="$cc" AR_x86_64_ps5_freebsd="$PS5_PAYLOAD_SDK/bin/prospero-ar"
  # cc-rs (BLAKE3's assembly, libdeflate, zstd) passes --target=x86_64-unknown-freebsd-ps5, which
  # clang refuses; a later --target wins, so the SDK's own target goes back in.
  #
  # The three defines are libdeflate's own switches. For Sony's targets clang's headers declare
  # only the intrinsics of the instruction sets the target CPU has, so libdeflate's AVX-VNNI,
  # AVX-512 and VPCLMULQDQ code paths, which it picks at run time elsewhere, do not compile. The
  # PS5's Zen 2 has none of those sets; its SSE2/AVX2/PCLMULQDQ paths stay.
  #
  # ZSTD_TRACE=0: zstd otherwise leaves weak imports of its tracing hooks (ZSTD_trace_*) for a
  # tracer to provide; no module of the console exports them, and the import check refuses them.
  export CFLAGS_x86_64_ps5_freebsd='--target=x86_64-sie-ps5 -mno-red-zone -femulated-tls
    -DLIBDEFLATE_ASSEMBLER_DOES_NOT_SUPPORT_AVX_VNNI -DLIBDEFLATE_ASSEMBLER_DOES_NOT_SUPPORT_AVX512VNNI
    -DLIBDEFLATE_ASSEMBLER_DOES_NOT_SUPPORT_VPCLMULQDQ -DZSTD_TRACE=0'
  # Cargo sees neither the C objects nor linker.sh: dropping the linked output makes it relink, so
  # a change to a shim or to entry.c always lands in the ELF.
  rm -f "$out"/x86_64-ps5-freebsd/console/deps/ps5upload_convert-*
  (cd engine && cargo build --profile console --locked -p ps5upload-convert \
    --target ../payload/convert/x86_64-ps5-freebsd.json -Z json-target-spec \
    -Z build-std=std,panic_abort -Z build-std-features=panic-unwind)
  cp "$out/x86_64-ps5-freebsd/console/ps5upload-convert" "$elf"
fi

[[ -f $elf ]] || { echo "build.sh: $elf is missing (build first)" >&2; exit 1; }
fail() {
  echo "build.sh: $elf: $*" >&2
  exit 1
}

# The payload's shape, as Forge's release script checks it: a 64-bit little-endian x86-64 PIE with
# no interpreter and no TLS segment (the loader maps neither), its entry point inside executable
# bytes, and NEEDED exactly the modules above.
readelf=$(llvm-readelf --wide -h -l -d "$elf")
field() {
  sed -n "s/^  $1: *//p" <<< "$readelf" | head -n1
}
[[ $(field Class) == ELF64 ]] || fail "class is [$(field Class)], want ELF64"
[[ $(field Data) == "2's complement, little endian" ]] || fail "data is [$(field Data)], want little endian"
[[ $(field Machine) == "Advanced Micro Devices X86-64" ]] || fail "machine is [$(field Machine)], want X86-64"
[[ $(field Type) == "DYN (Shared object file)" ]] || fail "type is [$(field Type)], want DYN"
flags_1=$(sed -n 's/.*(FLAGS_1) *//p' <<< "$readelf")
[[ " $flags_1 " == *" PIE "* ]] || fail "DT_FLAGS_1 is [$flags_1], want PIE"
needed=$(sed -n 's/.*(NEEDED) *Shared library: \[\(.*\)\]$/\1/p' <<< "$readelf" | LC_ALL=C sort | xargs)
[[ $needed == "$want_needed" ]] || fail "NEEDED is [$needed], want [$want_needed]"
entry=$(field 'Entry point address')
[[ $entry == 0x* ]] || fail "no entry point address"
size=$(wc -c < "$elf")
in_exec_load=0
# Program headers: `Type Offset VirtAddr PhysAddr FileSiz MemSiz Flg Align`, where Flg may hold a
# space ("R E"): the flags are everything between MemSiz and Align.
while read -r type offset vaddr _ filesz _ rest; do
  case $type in
    INTERP) fail "has a PT_INTERP program header" ;;
    TLS) fail "has a PT_TLS program header" ;;
    LOAD)
      (( offset + filesz <= size )) || fail "a PT_LOAD runs past the end of the file"
      flags=${rest% *}
      if [[ $flags == *E* ]] && (( vaddr <= entry && entry < vaddr + filesz )); then
        in_exec_load=1
      fi ;;
  esac
done < <(sed -n '/^Program Headers:/,/^$/p' <<< "$readelf" | sed '1,2d')
[[ $in_exec_load == 1 ]] || fail "entry point $entry is not inside an executable PT_LOAD"

# Every symbol the ELF imports must be one the SDK's stub of a NEEDED module exports: the stubs
# are generated from the console's own export tables, so a name outside them is a function the PS5
# does not have, and the loader would refuse the ELF (or the call would crash) on the console.
imports=$(llvm-readelf --wide --dyn-syms "$elf" | awk '$7 == "UND" && $8 != "" { sub(/@.*/, "", $8); print $8 }' | LC_ALL=C sort -u)
exports=$(for lib in $want_needed; do
  llvm-readelf --wide --dyn-syms "$PS5_PAYLOAD_SDK/target/lib/${lib%.sprx}.so" | awk '$7 != "UND" && $8 != "" { sub(/@.*/, "", $8); print $8 }'
done | LC_ALL=C sort -u)
missing=$(LC_ALL=C comm -23 <(echo "$imports") <(echo "$exports"))
[[ -z $missing ]] || fail "imports the console does not export: $(xargs <<< "$missing")"

{
  echo "$readelf"
  echo
  echo "Imported symbols ($(wc -l <<< "$imports" | xargs)), all exported by: $want_needed"
  echo "$imports"
} > "$elf.txt"
echo "ELF64 x86-64 PIE DYN, NEEDED [$needed], no INTERP/TLS, entry $entry executable," \
  "$(wc -l <<< "$imports" | xargs) imports all exported by the SDK's stubs"
ls -l "$elf"
sha256sum "$elf"
