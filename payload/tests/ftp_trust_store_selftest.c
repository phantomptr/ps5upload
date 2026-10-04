/* Host-side test (review S2): the AVA1 trust store is unreachable over FTP, directly OR through an
 * ANCESTOR. Renaming /data/ps5upload away (or an attacker tree onto it) moves/replaces ava/{identity,peers}
 * as surely as touching them, and so does deleting the directory that holds them. The protected
 * directory of this test is <tmp>/d/ava (stand-in for /data/ps5upload/ava). Drives the real
 * handle_rnfr/handle_rnto/handle_dele/handle_rmd of src/ftp_server.c over a socketpair. */
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/socket.h>
#include <sys/stat.h>
#include <unistd.h>

#define PS5UPLOAD_FTP_HOST_SELFTEST 1
#include "../src/ftp_server.c"
#include "../src/path_policy.c"

static int failures = 0;
#define CHECK(expr)                                                     \
    do {                                                                \
        if (!(expr)) {                                                  \
            fprintf(stderr, "FAIL line %d: %s\n", __LINE__, #expr);     \
            failures++;                                                 \
        }                                                               \
    } while (0)

static int sv[2];

/* The three-digit code of the last reply. */
static int reply(void) {
    char b[256];
    ssize_t n = read(sv[1], b, sizeof b - 1);
    if (n < 3) return -1;
    b[n] = '\0';
    return atoi(b);
}

static void put(const char *path, const char *text) {
    FILE *f = fopen(path, "w");
    if (f) { fputs(text, f); fclose(f); }
}

static int has(const char *path, const char *text) {
    char b[64] = {0};
    FILE *f = fopen(path, "r");
    if (!f) return 0;
    if (!fgets(b, sizeof b, f)) b[0] = '\0';
    fclose(f);
    return strcmp(b, text) == 0;
}


/* ---- Review 010 follow-up: over-long paths and the denied-path sentinel. ---- */

/* An absolute path of exactly n bytes made of components of at most 200 characters. */
static void long_path(char *out, size_t n) {
    size_t o = 0;
    while (o < n) {
        size_t comp = n - o - 1 > 200 ? 200 : n - o - 1;
        if (comp == 0) comp = 1;
        out[o++] = '/';
        for (size_t i = 0; i < comp && o < n; i++) out[o++] = 'a';
    }
    out[n] = '\0';
}

static void overflow_and_sentinel_tests(const char *base) {
    static char lp[2100], out[1024];
    struct ftp_session s;
    char p[PATH_MAX + 64];
    const size_t lens[3] = {1023, 1024, 1025};
    memset(&s, 0, sizeof s);
    if (socketpair(AF_UNIX, SOCK_STREAM, 0, sv) != 0) { failures++; return; }
    s.ctrl_fd = sv[0];
    s.authenticated = 1;
    snprintf(s.root, sizeof s.root, "/");
    snprintf(s.cwd, sizeof s.cwd, "/");
    snprintf(p, sizeof p, "%s/evil/x", base);
    put(p, "x"); /* a rename source that exists */

    /* abs_path: a path that fits comes back unchanged; one that does not becomes the sentinel, never a prefix. */
    long_path(lp, 900);
    abs_path(&s, lp, out, sizeof out);
    CHECK(strcmp(out, lp) == 0);
    for (int i = 0; i < 3; i++) {
        long_path(lp, lens[i]);
        abs_path(&s, lp, out, sizeof out);
        CHECK(strcmp(out, FTP_DENIED_PATH) == 0);
    }
    /* A short relative name under a root whose join overflows. */
    snprintf(s.root, sizeof s.root, "%.500s", base);
    long_path(lp, 1020);
    abs_path(&s, lp, out, sizeof out);
    CHECK(strcmp(out, FTP_DENIED_PATH) == 0);
    snprintf(s.root, sizeof s.root, "/");

    /* CWD / STOR / MKD / RNTO / CDUP with 1023-, 1024- and 1025-byte paths: refused, cwd reset to the root. */
    for (int i = 0; i < 3; i++) {
        long_path(lp, lens[i]);
        snprintf(s.cwd, sizeof s.cwd, "%.500s", base);
        handle_cwd(&s, lp);
        CHECK(reply() == 550);
        CHECK(strcmp(s.cwd, "/") == 0 || strcmp(s.cwd, base) == 0); /* never a truncated path */
        CHECK(strlen(s.cwd) < 600);
        handle_stor(&s, lp);
        CHECK(reply() == 550);
        handle_mkd(&s, lp);
        CHECK(reply() == 550);
        snprintf(p, sizeof p, "%s/evil/x", base);
        handle_rnfr(&s, p);
        CHECK(reply() == 350);
        handle_rnto(&s, lp);
        CHECK(reply() == 550);
        snprintf(s.cwd, sizeof s.cwd, "%.1023s", lp); /* a session already sitting in a long directory */
        handle_cdup(&s);
        CHECK(reply() == 550);
        CHECK(strlen(s.cwd) < sizeof s.cwd);
    }
    /* The root of a cwd that cannot be extended: CWD that overflows the cwd buffer resets to the root. */
    snprintf(s.root, sizeof s.root, "%.500s", base);
    snprintf(s.cwd, sizeof s.cwd, "%.500s", base);
    long_path(lp, 1010 - strlen(base));
    handle_cwd(&s, lp);
    CHECK(reply() == 550);
    CHECK(strcmp(s.cwd, base) == 0);
    snprintf(s.root, sizeof s.root, "/");
    snprintf(s.cwd, sizeof s.cwd, "/");

    /* RNTO, STOR and MKD onto the sentinel are refused outright (a root process could create it). */
    (void)unlink(FTP_DENIED_PATH);
    snprintf(p, sizeof p, "%s/evil/x", base);
    handle_rnfr(&s, p);
    CHECK(reply() == 350);
    handle_rnto(&s, FTP_DENIED_PATH);
    CHECK(reply() == 550);
    handle_stor(&s, FTP_DENIED_PATH);
    CHECK(reply() == 550);
    handle_mkd(&s, FTP_DENIED_PATH);
    CHECK(reply() == 550);
    CHECK(access(FTP_DENIED_PATH, F_OK) != 0);
    CHECK(access(p, F_OK) == 0);
    (void)unlink(FTP_DENIED_PATH);
    (void)rmdir(FTP_DENIED_PATH);
    close(sv[0]);
    close(sv[1]);
}

int main(void) {
    char base[PATH_MAX], prot[PATH_MAX + 32], p[2 * PATH_MAX + 64], q[2 * PATH_MAX + 64];
    struct ftp_session s;
    snprintf(base, sizeof base, "/tmp/ps5-ftp-s2-%d", (int)getpid());
    snprintf(p, sizeof p, "rm -rf %s", base);
    (void)system(p);
    snprintf(p, sizeof p, "mkdir -p %s/d/ava %s/evil/ava", base, base);
    if (system(p) != 0) return 2;
    /* realpath of the temp dir (macOS /tmp is a link) so spellings compare */
    {
        char rp[PATH_MAX];
        if (!realpath(base, rp)) return 2;
        snprintf(base, sizeof base, "%s", rp);
    }
    snprintf(prot, sizeof prot, "%s/d/ava", base);
    path_policy_set_protected(prot);
    snprintf(p, sizeof p, "%s/d/ava/peers", base); put(p, "trusted");
    snprintf(p, sizeof p, "%s/d/ava/identity", base); put(p, "secret");
    snprintf(p, sizeof p, "%s/evil/ava/peers", base); put(p, "attacker");
    snprintf(p, sizeof p, "%s/evil/x", base); put(p, "x");
    if (socketpair(AF_UNIX, SOCK_STREAM, 0, sv) != 0) return 2;

    memset(&s, 0, sizeof s);
    s.ctrl_fd = sv[0];
    s.authenticated = 1;
    snprintf(s.root, sizeof s.root, "/");
    snprintf(s.cwd, sizeof s.cwd, "/");

    /* RNFR of the store, of a file in it, and of its ANCESTOR is refused. */
    snprintf(p, sizeof p, "%s/d", base);
    handle_rnfr(&s, p);
    CHECK(reply() == 550);
    CHECK(s.rename_path[0] == '\0');
    snprintf(p, sizeof p, "%s/d/ava/peers", base);
    handle_rnfr(&s, p);
    CHECK(reply() == 550);
    snprintf(p, sizeof p, "%s", base);       /* a higher ancestor too */
    handle_rnfr(&s, p);
    CHECK(reply() == 550);

    /* RNTO onto the ancestor (replace /d with the attacker's tree): the destination is refused. */
    snprintf(p, sizeof p, "%s/evil", base);
    handle_rnfr(&s, p);
    CHECK(reply() == 350);                    /* an ordinary source is fine */
    snprintf(q, sizeof q, "%s/d", base);
    handle_rnto(&s, q);
    CHECK(reply() == 550);
    snprintf(q, sizeof q, "%s/d/ava", base);  /* and onto the store itself */
    snprintf(p, sizeof p, "%s/evil", base);
    handle_rnfr(&s, p);
    CHECK(reply() == 350);
    handle_rnto(&s, q);
    CHECK(reply() == 550);

    /* DELE/RMD of the store's files, the store, and its ancestor. */
    snprintf(p, sizeof p, "%s/d/ava/peers", base);
    handle_dele(&s, p);
    CHECK(reply() == 550);
    snprintf(p, sizeof p, "%s/d/ava", base);
    handle_rmd(&s, p);
    CHECK(reply() == 550);
    snprintf(p, sizeof p, "%s/d", base);
    handle_rmd(&s, p);
    CHECK(reply() == 550);

    /* Nothing changed on disk. */
    snprintf(p, sizeof p, "%s/d/ava/peers", base);
    CHECK(has(p, "trusted"));
    snprintf(p, sizeof p, "%s/d/ava/identity", base);
    CHECK(has(p, "secret"));
    snprintf(p, sizeof p, "%s/evil/x", base);
    CHECK(access(p, F_OK) == 0);

    /* Unrelated paths still work (the rule is not "deny everything"). */
    snprintf(p, sizeof p, "%s/evil/x", base);
    snprintf(q, sizeof q, "%s/evil/y", base);
    handle_rnfr(&s, p);
    CHECK(reply() == 350);
    handle_rnto(&s, q);
    CHECK(reply() == 250);
    handle_dele(&s, q);
    CHECK(reply() == 250);

    overflow_and_sentinel_tests(base);

    snprintf(p, sizeof p, "rm -rf %s", base);
    (void)system(p);
    if (failures) { fprintf(stderr, "ftp_trust_store_selftest: %d failure(s)\n", failures); return 1; }
    printf("ftp_trust_store_selftest: ALL PASS\n");
    return 0;
}
