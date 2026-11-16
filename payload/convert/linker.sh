#!/usr/bin/env bash
# rustc's linker for the PS5 target, run inside the payload/convert/Dockerfile image by build.sh.
# Adapted from PS5 Dump Forge's ps5/linker.sh (after ps5-ai-cli's tools/ps5-rust-linker.sh).
# FreeBSD splits these symbols into separate libraries; the PS5 SDK has them in libc and Sony's
# modules. A missing function still fails the link: nothing here suppresses unresolved symbols,
# so the link is the first check that the console has everything the program calls.
set -euo pipefail
args=()
for arg in "$@"; do
  case "$arg" in
    -lm|-lrt|-lutil|-lexecinfo|-lkvm|-lmemstat|-lprocstat|-ldevstat) args+=(-lc) ;;
    -lgcc_s) args+=(-lunwind) ;;
    *) args+=("$arg") ;;
  esac
done
# A dependency that also declares a cdylib gets a .so nothing loads: link it bare, without the
# shim objects, whose versioned symbols only resolve in the program's own link.
for arg in "$@"; do
  [[ "$arg" == -shared ]] && exec "$PS5_PAYLOAD_SDK/bin/prospero-clang" "${args[@]}"
done
obj=${CONVERT_SHIM_OBJ:?build.sh sets CONVERT_SHIM_OBJ}
crt1=${CONVERT_CRT1:?build.sh sets CONVERT_CRT1}
mapfile -t wraps < "$(dirname "$0")/shim/syscalls.link"
# rustc passes -nodefaultlibs, so the SDK's libraries are named here. Only libc and the kernel
# module: the program opens no sockets and calls no Sony service (the helper does that for it), so
# the ELF's NEEDED list stays at the two modules every payload already has loaded. A build that
# starts needing another one fails here, and build.sh's check names the list it expects.
# -nostartfiles and our crt1.o, as the helper links (payload/sdk-crt.mk). The flag is for the
# SDK's prospero-clang wrapper, which then leaves its own crt1.o out; clang itself has no use for
# it on this target and would say so on every link.
exec "$PS5_PAYLOAD_SDK/bin/prospero-clang" "${args[@]}" -nostartfiles -Wno-unused-command-line-argument "$crt1" \
  "$obj/freebsd11.o" "$obj/syscalls.o" "$obj/entry.o" "$obj/compat.o" "${wraps[@]}" \
  -Wl,--wrap=main -Wl,--wrap=fcntl -Wl,--wrap=sysctl -Wl,--wrap=cpuset_getaffinity -Wl,--wrap=pthread_set_name_np -Wl,--error-limit=0 \
  -ldl -lunwind -lc -lkernel_web -lSceLibcInternal
