/* Host-side test for the game-sandbox library-overlay unmount rule.
 *
 * FS_UNMOUNT otherwise only accepts mounts ps5upload made itself. That
 * guard is right, and it is also why a backport attempt that crashes
 * leaves a unionfs mount nothing can clear: BackPork only unmounts on a
 * clean game exit, the shell fails to rmdir the sandbox, and the kernel
 * then refuses every later mount of the same source. Measured on a
 * console: three consecutive launches blocked, the last with the sandbox
 * directory already gone.
 *
 * The extra rule has to be narrow enough that it can never name anything
 * else, so most of this file is about what it must REFUSE. */
#include <stdio.h>
#include <string.h>

#include "../include/sandbox_unmount.h"

static int failures = 0;

#define CHECK(expr)                                                     \
    do {                                                                \
        if (!(expr)) {                                                  \
            fprintf(stderr, "FAIL line %d: %s\n", __LINE__, #expr);     \
            failures++;                                                 \
        }                                                               \
    } while (0)

#define ALLOW(p, t) CHECK(sandbox_union_unmount_allowed((p), (t)) == 1)
#define DENY(p, t)  CHECK(sandbox_union_unmount_allowed((p), (t)) == 0)

int main(void) {
    /* The shape actually observed on hardware. */
    ALLOW("/mnt/sandbox/PPSA25411_000/jv2WjLTopA/common/lib", "unionfs");
    ALLOW("/mnt/sandbox/CUSA00900_007/0BhkDyNjRW/common/lib", "unionfs");

    /* Only unionfs. A sandbox's own pfs mount is not ours to touch, even
     * at a path that otherwise matches. */
    DENY("/mnt/sandbox/PPSA25411_000/jv2WjLTopA/common/lib", "pfs");
    DENY("/mnt/sandbox/PPSA25411_000/jv2WjLTopA/common/lib", "nullfs");
    DENY("/mnt/sandbox/PPSA25411_000/jv2WjLTopA/common/lib", "");

    /* Only the library directory. The sandbox root, app0 and the
     * per-run directory are the system's, and unmounting one would take
     * a running game's filesystem out from under it. */
    DENY("/mnt/sandbox/PPSA25411_000", "unionfs");
    DENY("/mnt/sandbox/PPSA25411_000/jv2WjLTopA", "unionfs");
    DENY("/mnt/sandbox/PPSA25411_000/app0", "unionfs");
    DENY("/mnt/sandbox/PPSA25411_000/jv2WjLTopA/common", "unionfs");
    DENY("/mnt/sandbox/PPSA25411_000/jv2WjLTopA/common/lib/sub", "unionfs");

    /* Only a real sandbox directory name. */
    DENY("/mnt/sandbox/PPSA25411/jv2WjLTopA/common/lib", "unionfs");   /* no _NNN */
    DENY("/mnt/sandbox/PPSA25411_00/x/common/lib", "unionfs");          /* 2 digits */
    DENY("/mnt/sandbox/PPSA2541X_000/x/common/lib", "unionfs");         /* letter in id */
    DENY("/mnt/sandbox/ppsa25411_000/x/common/lib", "unionfs");         /* lowercase */
    DENY("/mnt/sandbox/NPXS40087_000/x/common/lib", "unionfs");         /* system app */

    /* Nothing outside the sandbox root, however it is dressed up. */
    DENY("/mnt/ext1/PPSA25411_000/x/common/lib", "unionfs");
    DENY("/mnt/ps5upload/PPSA25411_000/x/common/lib", "unionfs");
    DENY("/mnt/sandboxes/PPSA25411_000/x/common/lib", "unionfs");
    DENY("/user/app/PPSA25411_000/x/common/lib", "unionfs");
    DENY("/", "unionfs");
    DENY("", "unionfs");

    /* Traversal must not be able to walk out of the sandbox root. */
    DENY("/mnt/sandbox/PPSA25411_000/../../../mnt/ext1/common/lib", "unionfs");
    DENY("/mnt/sandbox/PPSA25411_000/x/../../../common/lib", "unionfs");

    /* A filename that merely CONTAINS dots is fine — the check is
     * component-aware, matching is_path_allowed's semantics. */
    ALLOW("/mnt/sandbox/PPSA25411_000/my..dir/common/lib", "unionfs");

    /* Null-safe. */
    DENY(NULL, "unionfs");
    CHECK(sandbox_union_unmount_allowed("/mnt/sandbox/PPSA25411_000/x/common/lib",
                                        NULL) == 0);

    printf("sandbox_unmount_selftest: %s\n", failures == 0 ? "ALL PASS" : "FAILED");
    return failures == 0 ? 0 : 1;
}
