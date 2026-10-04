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

    snprintf(p, sizeof p, "rm -rf %s", base);
    (void)system(p);
    if (failures) { fprintf(stderr, "ftp_trust_store_selftest: %d failure(s)\n", failures); return 1; }
    printf("ftp_trust_store_selftest: ALL PASS\n");
    return 0;
}
