#ifndef PS5UPLOAD_SANDBOX_UNMOUNT_H
#define PS5UPLOAD_SANDBOX_UNMOUNT_H

#include <stddef.h>
#include <string.h>

/* May we unmount a game sandbox's library overlay?
 *
 * Backporting mounts a `fakelib` folder over a game's `common/lib` with
 * unionfs (BackPork does this at launch). When the game then dies at load
 * — the normal outcome while you are still finding the right library set
 * — nothing cleans that mount up:
 *
 *   - BackPork only unmounts in its own game-exited path, which a crash
 *     never reaches;
 *   - the shell tries and fails, leaving the sandbox behind
 *     (`rmdir(...) failed 0x80020042`, `remains in FsSandbox root`), so
 *     its close dialog never clears;
 *   - the kernel then refuses every later mount of the same source
 *     (`unionfs_domount: The same unionfs mount is prohibited`), so the
 *     title cannot be launched again.
 *
 * Observed on hardware: three consecutive launches blocked, the last with
 * the sandbox directory already deleted, because the mount record outlives
 * the directory. Only a console restart cleared it — which is not a
 * reasonable thing to ask between backport attempts.
 *
 * So FS_UNMOUNT accepts this one extra shape. It is deliberately narrow:
 * the filesystem must be unionfs, and the path must be exactly a game
 * sandbox's library directory. That cannot name a system mount, a user's
 * own /mnt/ext1, or anything ps5upload did not put there — a sandbox
 * library overlay exists only because a backport tool created it.
 *
 * Tests: payload/tests/sandbox_unmount_selftest.c */

#define SANDBOX_ROOT       "/mnt/sandbox/"
#define SANDBOX_LIB_SUFFIX "/common/lib"

/* "PPSA25411_000" / "CUSA00900_007": a GAME title id, five digits, an
 * underscore and three digits.
 *
 * The prefix check is not decoration. Every system app is sandboxed the
 * same way — NPXS40087_000 is the Store — and a generic "four letters
 * and five digits" rule happily matched those, which would have let a
 * request unmount part of a running system app. BackPork only ever
 * overlays PPSA/CUSA titles, so nothing else can legitimately be asked
 * for. Caught by the self-test, not by review. */
static inline int sandbox_dir_name_ok(const char *s, size_t len) {
    if (len != 13) return 0;
    if (memcmp(s, "PPSA", 4) != 0 && memcmp(s, "CUSA", 4) != 0) return 0;
    for (int i = 4; i < 9; i++)
        if (s[i] < '0' || s[i] > '9') return 0;
    if (s[9] != '_') return 0;
    for (int i = 10; i < 13; i++)
        if (s[i] < '0' || s[i] > '9') return 0;
    return 1;
}

static inline int sandbox_path_has_dotdot(const char *p) {
    for (const char *s = p; s; ) {
        const char *slash = strchr(s, '/');
        size_t n = slash ? (size_t)(slash - s) : strlen(s);
        if (n == 2 && s[0] == '.' && s[1] == '.') return 1;
        if (!slash) break;
        s = slash + 1;
    }
    return 0;
}

/* `fstype` is statfs's f_fstypename for the mount at `mount_point`. */
static inline int sandbox_union_unmount_allowed(const char *mount_point,
                                                const char *fstype) {
    if (!mount_point || !fstype) return 0;
    if (strcmp(fstype, "unionfs") != 0) return 0;
    if (sandbox_path_has_dotdot(mount_point)) return 0;

    const size_t root_len = strlen(SANDBOX_ROOT);
    if (strncmp(mount_point, SANDBOX_ROOT, root_len) != 0) return 0;

    const char *dir = mount_point + root_len;
    const char *slash = strchr(dir, '/');
    if (!slash) return 0;
    if (!sandbox_dir_name_ok(dir, (size_t)(slash - dir))) return 0;

    /* One path component for the launcher's per-run directory, then
     * exactly `/common/lib`. */
    const char *rest = slash + 1;
    const char *next = strchr(rest, '/');
    if (!next || next == rest) return 0;
    return strcmp(next, SANDBOX_LIB_SUFFIX) == 0;
}

#endif
