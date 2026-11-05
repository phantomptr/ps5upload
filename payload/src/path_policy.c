#include "path_policy.h"

#include <limits.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <strings.h>
#include <sys/stat.h>

#ifndef PATH_MAX
#define PATH_MAX 1024
#endif

#define PROTECTED_DEFAULT "/data/ps5upload/ava"
static char g_protected[PATH_MAX] = PROTECTED_DEFAULT;
/* The saved network shares' secrets (smb_shares.h: shares.jsonl and shares.key, the daemon's key
 * and log). Through fs.read or a download, anyone holding the page token could take the key and
 * the file and decrypt every saved share password offline, so the directory is denied like the
 * trust store. The helper and the daemon reach it with plain open(2), never through a policy. The
 * test hook (path_policy_set_protected) moves only the trust store; this one is fixed. */
#define PROTECTED_SMB "/data/ps5upload/smb"

void path_policy_set_protected(const char *dir) {
    snprintf(g_protected, sizeof g_protected, "%s", dir && dir[0] ? dir : PROTECTED_DEFAULT);
}

/* `//`, `.` and `..` collapsed; always absolute; no trailing slash (except the root). */
static void lex_normalize(const char *src, char *out, size_t cap) {
    size_t o = 0;
    const char *p = src;
    if (cap < 2) return;
    out[0] = '\0';
    while (*p) {
        const char *start;
        size_t len;
        while (*p == '/') p++;
        if (!*p) break;
        start = p;
        while (*p && *p != '/') p++;
        len = (size_t)(p - start);
        if (len == 1 && start[0] == '.') continue;
        if (len == 2 && start[0] == '.' && start[1] == '.') {
            while (o > 0 && out[o - 1] != '/') o--;
            if (o > 0) o--;
            out[o] = '\0';
            continue;
        }
        if (o + 1 + len + 1 > cap) {
            /* Too long to judge: the result is "/", which path_in_protected does NOT flag. That
             * is safe because every caller fails closed on such a path anyway:
             * path_resolve_allowed refuses it (its `len >= sizeof tmp` check) and
             * path_contains_protected flags it (the protected dir is "inside" "/"). Do not
             * "fix" this by returning the truncated prefix: that would judge a different path. */
            snprintf(out, cap, "%s", "/");
            return;
        }
        out[o++] = '/';
        memcpy(out + o, start, len);
        o += len;
        out[o] = '\0';
    }
    if (o == 0) snprintf(out, cap, "/");
}

/* The canonical form: realpath, else the canonical deepest existing ancestor joined with the rest. */
static void canonical_of(const char *p, char *out, size_t cap) {
    char norm[PATH_MAX], tmp[PATH_MAX], res[PATH_MAX];
    lex_normalize(p, norm, sizeof norm);
    if (realpath(norm, out) != NULL) return;
    snprintf(tmp, sizeof tmp, "%s", norm);
    for (;;) {
        char *slash = strrchr(tmp, '/');
        if (!slash || slash == tmp) {
            snprintf(out, cap, "%s", norm);
            return;
        }
        *slash = '\0';
        if (realpath(tmp, res) != NULL) break;
    }
    if (strcmp(res, "/") == 0) snprintf(out, cap, "%s", norm + strlen(tmp));
    else snprintf(out, cap, "%s%s", res, norm + strlen(tmp));
}

/* p equals dir, or is below it (case-insensitive). */
static int under(const char *p, const char *dir) {
    size_t n = strlen(dir);
    while (n > 1 && dir[n - 1] == '/') n--;
    if (n == 1) return p[0] == '/'; /* "/" contains every absolute path */
    return strncasecmp(p, dir, n) == 0 && (p[n] == '\0' || p[n] == '/');
}

/* A `..` component cannot be judged: collapsing it lexically BEFORE resolving symlinks gives a different
 * answer than the kernel (`link/../x`). Every allowed path is `..`-free already (the lexical rule and
 * is_safe_unsafe_read_path reject it), so such a path is treated as protected: fail closed. */
static int has_dotdot(const char *p) {
    const char *seg = p;
    while (*seg) {
        const char *end;
        while (*seg == '/') seg++;
        if (!*seg) break;
        end = seg;
        while (*end && *end != '/') end++;
        if (end - seg == 2 && seg[0] == '.' && seg[1] == '.') return 1;
        seg = end;
    }
    return 0;
}

/* /data is a nullfs mount of /user/data (the mount table says so, see volumes.rs), some consoles list
 * /user again as /original_user, and realpath does not see through a mount: /user/data/ps5upload/ava
 * is the store under a name the forms above do not match. Every other name of `dir`, for a dir
 * under /data: the i-th in `out`, returning 1; 0 past the last. */
static int data_alias(const char *dir, int i, char *out, size_t cap) {
    static const char *const roots[] = { "/user", "/original_user" };
    if (strncasecmp(dir, "/data/", 6) != 0 || i >= (int)(sizeof roots / sizeof roots[0])) return 0;
    return snprintf(out, cap, "%s%s", roots[i], dir) < (int)cap;
}

/* `norm`/`canon` (the path's two forms) against one protected directory: in_dir = the path is the
 * directory or below it; contains_dir = the directory is the path or below it. */
static int in_dir(const char *norm, const char *canon, const char *dir) {
    char pnorm[PATH_MAX], pcanon[PATH_MAX], alias[PATH_MAX];
    int i;
    lex_normalize(dir, pnorm, sizeof pnorm);
    canonical_of(dir, pcanon, sizeof pcanon);
    for (i = 0; data_alias(pnorm, i, alias, sizeof alias); i++)
        if (under(norm, alias) || under(canon, alias)) return 1;
    return under(norm, pnorm) || under(canon, pnorm) || under(norm, pcanon) || under(canon, pcanon);
}

static int contains_dir(const char *norm, const char *canon, const char *dir) {
    char pnorm[PATH_MAX], pcanon[PATH_MAX], alias[PATH_MAX];
    int i;
    lex_normalize(dir, pnorm, sizeof pnorm);
    canonical_of(dir, pcanon, sizeof pcanon);
    for (i = 0; data_alias(pnorm, i, alias, sizeof alias); i++)
        if (under(alias, norm) || under(alias, canon)) return 1;
    return under(pnorm, norm) || under(pcanon, canon) || under(pnorm, canon) || under(pcanon, norm);
}

int path_in_protected(const char *p) {
    char norm[PATH_MAX], canon[PATH_MAX];
    if (!p || !p[0]) return 0;
    if (has_dotdot(p)) return 1;
    lex_normalize(p, norm, sizeof norm);
    canonical_of(p, canon, sizeof canon);
    return in_dir(norm, canon, g_protected) || in_dir(norm, canon, PROTECTED_SMB);
}

int path_read_allowed(const char *p, int unsafe_read, int (*others)(const char *, int)) {
    return !path_in_protected(p) && others && others(p, unsafe_read);
}

int path_contains_protected(const char *p) {
    char norm[PATH_MAX], canon[PATH_MAX];
    if (!p || !p[0]) return 0;
    if (has_dotdot(p)) return 1;
    lex_normalize(p, norm, sizeof norm);
    canonical_of(p, canon, sizeof canon);
    return contains_dir(norm, canon, g_protected) || contains_dir(norm, canon, PROTECTED_SMB);
}

int path_resolve_allowed(const char *p, int (*lexical_ok)(const char *)) {
    char resolved[PATH_MAX], tmp[PATH_MAX], joined[PATH_MAX];
    struct stat st;
    size_t len;
    if (!lexical_ok || !lexical_ok(p)) return 0;
    if (path_in_protected(p)) return 0; /* the trust store: no peer may touch it */
    if (realpath(p, resolved) != NULL) return lexical_ok(resolved);
    /* realpath failed: either something does not exist, or the final component is a dangling link. */
    if (lstat(p, &st) == 0 && S_ISLNK(st.st_mode)) return 0;
    len = strlen(p);
    if (len >= sizeof tmp) return 0;
    memcpy(tmp, p, len + 1);
    for (;;) {
        char *slash = strrchr(tmp, '/');
        if (!slash || slash == tmp) {
            /* Only the root is left: nothing below it exists, and the lexical rule already passed. */
            return 1;
        }
        *slash = '\0';
        if (realpath(tmp, resolved) != NULL) break;
    }
    /* resolved = the canonical deepest existing ancestor; p + strlen(tmp) = the missing rest ("/a/b"). */
    {
        int n;
        if (strcmp(resolved, "/") == 0) n = snprintf(joined, sizeof joined, "%s", p + strlen(tmp));
        else n = snprintf(joined, sizeof joined, "%s%s", resolved, p + strlen(tmp));
        if (n < 0 || (size_t)n >= sizeof joined) return 0; /* too long to judge: refuse */
    }
    return lexical_ok(joined);
}

int path_tree_op_refused(const char *p) { return path_in_protected(p) || path_contains_protected(p); }

void path_protected_forms(char *lex, char *canon) {
    lex_normalize(g_protected, lex, PATH_MAX);
    canonical_of(g_protected, canon, PATH_MAX);
}

int path_under_forms(const char *p, const char *lex, const char *canon) {
    if (!p || !p[0]) return 0;
    return under(p, lex) || under(p, canon);
}
