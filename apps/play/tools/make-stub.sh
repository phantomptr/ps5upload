#!/usr/bin/env bash
# PS5Term - build an SDK-style import stub (.so) for a system module the
# pinned payload SDK has no stub for. Same shape as the SDK's own stubs (e.g.
# libSceKeyboard.so): a shared object whose SONAME is the module file name and
# whose dynamic symbols are the function names; the native tool turns each
# referenced name into a NID import of that module. tools/build.sh runs it for
# libSceMouse on every build:
#   tools/make-stub.sh build/stubs/libSceMouse.so libSceMouse.sprx \
#       sceMouseInit sceMouseOpen sceMouseRead sceMouseClose
# SPDX-License-Identifier: GPL-3.0-or-later
set -euo pipefail
out=$1 soname=$2
shift 2
(( $# > 0 )) || { echo "usage: make-stub.sh OUT.so SONAME SYMBOL..." >&2; exit 2; }
cc=$(command -v clang-18 || command -v clang)
ld=$(command -v ld.lld-18 || command -v ld.lld)
work=$(mktemp -d)
trap 'rm -rf -- "$work"' EXIT
{
    printf '.text\n'
    for symbol in "$@"; do
        [[ $symbol =~ ^[A-Za-z_][A-Za-z0-9_]*$ ]] || { echo "bad symbol $symbol" >&2; exit 2; }
        printf '.globl %s\n.type %s,@function\n%s:\n' "$symbol" "$symbol" "$symbol"
    done
    printf 'ret\n'
} > "$work/stub.s"
"$cc" --target=x86_64-unknown-linux-gnu -c "$work/stub.s" -o "$work/stub.o"
mkdir -p "$(dirname -- "$out")"
"$ld" -shared -soname "$soname" --hash-style=both -o "$out" "$work/stub.o"
