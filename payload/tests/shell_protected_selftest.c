/* Host self-test: the built-in shell refuses the protected folders (the AVA1 trust store) like every
 * other file operation. A connected peer's `xxd`, `cat`, `rm -r`, `mv` or `chmod` must not read,
 * delete, move or lock the console's identity key, by any spelling: absolute, relative to the
 * working directory, or through a symlink. Built with stubs for the runtime pieces it does not use. */
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/mount.h>
#include <sys/stat.h>
#include <sys/uio.h>
#include <unistd.h>

#include "path_policy.h"
#include "shell_builtin.h"

/* --- stubs: runtime.c, indicator.c, proc_list.c ------------------------------------------- */
volatile int g_ucred_elevation_rc = 0;
static char g_root[512];
char *strdup_safe(const char *s) { return strdup(s ? s : ""); }
int mntinfo_snapshot(struct statfs **out) {
    *out = NULL;
    return 0;
}
int nmount(struct iovec *iov, unsigned n, int flags) {
    (void)iov, (void)n, (void)flags;
    return -1;
}
void pop_notification(const char *m) { (void)m; }
int proc_list_get_json(char *b, size_t c, size_t *w, const char **e) {
    (void)b, (void)c, (void)w, (void)e;
    return -1;
}
int indicator_shell(int argc, char *argv[], char **t, int *x) {
    (void)argc, (void)argv, (void)t, (void)x;
    return 0;
}
/* The runtime's allowlist: anything under the test root (the shell's own rules still apply). */
int is_path_allowed(const char *p) { return strncmp(p, g_root, strlen(g_root)) == 0; }

static int fails;

static int run(const char *cmd, char **text) {
    int code = -1;
    *text = NULL;
    if (handle_shell_builtin(cmd, text, &code) != 0) code = -2;
    if (!*text) *text = strdup("");
    return code;
}

static void refused(const char *cmd) {
    char *t;
    int code = run(cmd, &t);
    if (code != 1 || !strstr(t, "refused") || strstr(t, "SECRET")) {
        fprintf(stderr, "FAIL refused: %s -> %d %s\n", cmd, code, t);
        fails++;
    }
    free(t);
}

static void allowed(const char *cmd, const char *want) {
    char *t;
    int code = run(cmd, &t);
    if (code != 0 || (want && !strstr(t, want))) {
        fprintf(stderr, "FAIL allowed: %s -> %d %s\n", cmd, code, t);
        fails++;
    }
    free(t);
}

int main(void) {
    char tmpl[] = "/tmp/ps5u-shell-XXXXXX", cmd[2048], ava[600], f[700];
    if (!mkdtemp(tmpl)) return 2;
    if (!realpath(tmpl, g_root)) return 2;
    snprintf(ava, sizeof ava, "%s/ava", g_root);
    mkdir(ava, 0700);
    path_policy_set_protected(ava);
    snprintf(f, sizeof f, "%s/identity", ava);
    FILE *fp = fopen(f, "w");
    fputs("SECRET-KEY\n", fp);
    fclose(fp);
    snprintf(f, sizeof f, "%s/notes.txt", g_root);
    fp = fopen(f, "w");
    fputs("hello\n", fp);
    fclose(fp);

    /* Reads of the key, any reader, any spelling. */
    const char *readers[] = {"cat", "xxd", "hexdump -C", "head", "tail", "wc", "stat", "file", "sfoinfo"};
    for (size_t i = 0; i < sizeof readers / sizeof *readers; i++) {
        snprintf(cmd, sizeof cmd, "%s %s/identity", readers[i], ava);
        refused(cmd);
    }
    snprintf(cmd, sizeof cmd, "cat %s/./ava//identity", g_root);
    refused(cmd);
    snprintf(cmd, sizeof cmd, "cat %s/AVA/identity", g_root); /* case-insensitive */
    refused(cmd);
    snprintf(cmd, sizeof cmd, "ln -s %s/identity %s/link", ava, g_root);
    refused(cmd); /* the shell will not point a link at it */
    snprintf(cmd, sizeof cmd, "%s/identity", ava);
    snprintf(f, sizeof f, "%s/link", g_root);
    if (symlink(cmd, f) != 0) return 2; /* one made some other way: reading through it is refused */
    snprintf(cmd, sizeof cmd, "cat %s/link", g_root);
    refused(cmd);
    snprintf(cmd, sizeof cmd, "cd %s", g_root);
    allowed(cmd, NULL);
    refused("cat ava/identity");
    refused("ls ava");

    /* Changes to the store or anything holding it. */
    snprintf(cmd, sizeof cmd, "rm -r %s", g_root);
    refused(cmd);
    snprintf(cmd, sizeof cmd, "rm %s/identity", ava);
    refused(cmd);
    snprintf(cmd, sizeof cmd, "mv %s %s/moved", ava, g_root);
    refused(cmd);
    snprintf(cmd, sizeof cmd, "mv %s/notes.txt %s/identity", g_root, ava);
    refused(cmd);
    snprintf(cmd, sizeof cmd, "chmod -R 000 %s", g_root);
    refused(cmd);
    snprintf(cmd, sizeof cmd, "cp -r %s %s/copy", g_root, g_root);
    refused(cmd);
    snprintf(cmd, sizeof cmd, "grep -r SECRET %s", g_root);
    refused(cmd);
    snprintf(f, sizeof f, "%s/identity", ava);
    if (access(f, F_OK) != 0) {
        fprintf(stderr, "FAIL: the key is gone\n");
        fails++;
    }

    /* Everything else still works. */
    snprintf(cmd, sizeof cmd, "cat %s/notes.txt", g_root);
    allowed(cmd, "hello");
    allowed("cat notes.txt", "hello");
    snprintf(cmd, sizeof cmd, "ls %s", g_root);
    allowed(cmd, "notes.txt");
    snprintf(cmd, sizeof cmd, "du -s %s", g_root);
    allowed(cmd, NULL);
    snprintf(cmd, sizeof cmd, "grep hello %s/notes.txt", g_root);
    allowed(cmd, "hello");
    allowed("echo /data/ps5upload/ava/identity", "identity");

    snprintf(cmd, sizeof cmd, "rm -rf %s", g_root);
    if (system(cmd) != 0) fprintf(stderr, "warn: could not remove %s\n", g_root);
    if (fails) {
        fprintf(stderr, "%d failure(s)\n", fails);
        return 1;
    }
    printf("shell protected-path selftest: ok\n");
    return 0;
}
