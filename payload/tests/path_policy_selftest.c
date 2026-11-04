/* The trust store and the saved shares' key, under every name a console gives them: /data is a
 * nullfs mount of /user/data, so a read of /user/data/ps5upload/ava reaches the same files. */
#include <stdio.h>

#include "path_policy.h"

static int failed;

static void check(const char *what, const char *p, int got, int want) {
    if (got != want) {
        fprintf(stderr, "FAIL %s(%s) = %d, want %d\n", what, p, got, want);
        failed = 1;
    }
}

int main(void) {
    static const char *const in[] = {
        "/data/ps5upload/ava",
        "/data/ps5upload/ava/identity",
        "/data/ps5upload/smb/shares.key",
        "/user/data/ps5upload/ava",
        "/user/data/ps5upload/ava/identity",
        "/user/data/ps5upload/smb/shares.key",
        "/USER/Data/ps5upload/AVA/identity",
        "//user/./data/ps5upload/smb",
    };
    static const char *const out[] = {
        "/data/ps5upload/avatars/a.png",
        "/data/ps5upload/stderr.log",
        "/user/data/ps5upload/pkg_temp/a.pkg",
        "/user/data/ps5upload/smbx",
        "/user/app/PPSA00000/eboot.bin",
        "/user/home/1/savedata_prospero/x",
        "/mnt/ext0/data/ps5upload/ava",
    };
    static const char *const ancestors[] = {
        "/", "/data", "/data/ps5upload", "/user", "/user/data", "/user/data/ps5upload",
    };
    static const char *const elsewhere[] = {
        "/user/app", "/user/home/1", "/user/data/ps5upload/pkg_temp", "/mnt/ext0",
    };
    size_t i;
    for (i = 0; i < sizeof in / sizeof in[0]; i++) {
        check("path_in_protected", in[i], path_in_protected(in[i]), 1);
        check("path_tree_op_refused", in[i], path_tree_op_refused(in[i]), 1);
    }
    for (i = 0; i < sizeof out / sizeof out[0]; i++) {
        check("path_in_protected", out[i], path_in_protected(out[i]), 0);
        check("path_tree_op_refused", out[i], path_tree_op_refused(out[i]), 0);
    }
    for (i = 0; i < sizeof ancestors / sizeof ancestors[0]; i++) {
        check("path_contains_protected", ancestors[i], path_contains_protected(ancestors[i]), 1);
        check("path_tree_op_refused", ancestors[i], path_tree_op_refused(ancestors[i]), 1);
    }
    for (i = 0; i < sizeof elsewhere / sizeof elsewhere[0]; i++) {
        check("path_contains_protected", elsewhere[i], path_contains_protected(elsewhere[i]), 0);
    }
    if (failed) return 1;
    printf("path policy: the protected folders under both names\n");
    return 0;
}
