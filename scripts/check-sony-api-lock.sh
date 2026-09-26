#!/bin/sh
# Every payload source that calls sceUserService* or sceRegMgr* must be
# serialized on `sony_api_lock`. Those APIs are not safe to call
# concurrently from multiple connection threads: two threads inside
# sceUserService fault in the host process and take the console down
# with it (error CE-108262-9, seen when the Profile screen was read
# while a Remote Play status poll was in flight).
#
# A file passes by referencing sony_api_lock itself, or by appearing in
# the caller-locked allowlist below with a stated reason.
set -eu

SRC="${1:-payload/src}"
fail=0

# file:reason — locked by the caller rather than by itself.
allow_reason() {
    case "$1" in
    profile.c)
        echo "caller-locked: handle_profile_info holds sony_api_lock across its whole loop" ;;
    sys_registry.c)
        echo "the wrapper layer callers lock around; locking here too would deadlock (mutex is not recursive)" ;;
    shellui_rpc.c)
        echo "ptrace symbol resolution inside SceShellUI, serialized by its own g_rpc_mtx" ;;
    *)  echo "" ;;
    esac
}

for path in "$SRC"/*.c; do
    f=$(basename "$path")
    # Direct Sony calls, plus the sys_registry_* wrappers that reach
    # sceRegMgr indirectly (how profile.c and remoteplay.c get there).
    grep -qE 'sceUserService[A-Za-z]*\(|sceRegMgr[A-Za-z]*\(|sys_registry_[a-z_]+\(' "$path" || continue

    if grep -q 'sony_api_lock' "$path"; then
        printf '  %-18s locked\n' "$f"
        continue
    fi

    reason=$(allow_reason "$f")
    if [ -n "$reason" ]; then
        printf '  %-18s allowed (%s)\n' "$f" "$reason"
        continue
    fi

    printf '  %-18s UNSERIALIZED Sony API calls\n' "$f"
    fail=1
done

if [ "$fail" -ne 0 ]; then
    echo
    echo "ERROR: the file(s) above call sceUserService*/sceRegMgr* without"
    echo "serializing on sony_api_lock, and are not on the caller-locked"
    echo "allowlist in $0."
    echo "Concurrent calls into these APIs crash the host process and shut"
    echo "the console down. Either take sony_api_lock, or add the file to"
    echo "the allowlist with the reason its callers already hold it."
    exit 1
fi
