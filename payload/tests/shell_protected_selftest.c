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

    /* A word that starts with '-' is a path to cat/stat/mv: a link named "-L" into the store
     * (made some other way) must not get the key out. The working directory is the test root. */
    if (symlink("ava", "-L") != 0) return 2;
    refused("cat -L/identity");
    refused("stat -L/identity");
    snprintf(cmd, sizeof cmd, "mv -L/identity %s/out", g_root);
    refused(cmd);
    allowed("chmod -R 644 notes.txt", NULL); /* real options still pass */
    /* A path too long to judge is refused, not waved through. */
    {
        size_t at = (size_t)snprintf(cmd, sizeof cmd, "cat ");
        while (at < 1300) at += (size_t)snprintf(cmd + at, sizeof cmd - at, "./");
        snprintf(cmd + at, sizeof cmd - at, "notes.txt");
        refused(cmd);
    }
    /* cp of a file onto itself fails and leaves the file as it was. */
    {
        char *t;
        int code = run("cp notes.txt notes.txt", &t);
        if (code == 0 || !strstr(t, "same file")) {
            fprintf(stderr, "FAIL cp onto itself: %d %s\n", code, t);
            fails++;
        }
        free(t);
        code = run("cp notes.txt .", &t);
        if (code == 0) {
            fprintf(stderr, "FAIL cp into its own folder: %d %s\n", code, t);
            fails++;
        }
        free(t);
        allowed("cat notes.txt", "hello");
    }
    /* More words than the built-ins take: refused whole, not run on the first 31. */
    {
        char *t;
        size_t at = (size_t)snprintf(cmd, sizeof cmd, "touch");
        for (int i = 0; i < 40; i++) at += (size_t)snprintf(cmd + at, sizeof cmd - at, " tf%d", i);
        int code = run(cmd, &t);
        if (code != 1 || !strstr(t, "too many arguments") || access("tf0", F_OK) == 0) {
            fprintf(stderr, "FAIL too many words: %d %s\n", code, t);
            fails++;
        }
        free(t);
    }
    /* chmod -R changes what is IN the tree. A link found there is not followed: its target is
     * somewhere else (another game's files, or the trust store, which the check of the command's
     * own words cannot see behind a link inside the tree). */
    {
        struct stat st;
        char tree[600], inside[700], link_out[700], link_key[700], key[700];
        snprintf(tree, sizeof tree, "%s/tree", g_root);
        snprintf(inside, sizeof inside, "%s/in.txt", tree);
        snprintf(link_out, sizeof link_out, "%s/to-notes", tree);
        snprintf(link_key, sizeof link_key, "%s/to-key", tree);
        snprintf(f, sizeof f, "%s/notes.txt", g_root);
        snprintf(key, sizeof key, "%s/identity", ava);
        if (mkdir(tree, 0755) != 0 || (fp = fopen(inside, "w")) == NULL) return 2;
        fclose(fp);
        if (chmod(f, 0644) != 0 || chmod(key, 0600) != 0 || chmod(inside, 0644) != 0) return 2;
        if (symlink(f, link_out) != 0 || symlink(key, link_key) != 0) return 2;
        snprintf(cmd, sizeof cmd, "chmod -R 700 %s", tree);
        allowed(cmd, NULL);
        if (stat(inside, &st) != 0 || (st.st_mode & 0777) != 0700) {
            fprintf(stderr, "FAIL chmod -R: the file in the tree was not changed\n");
            fails++;
        }
        if (stat(f, &st) != 0 || (st.st_mode & 0777) != 0644) {
            fprintf(stderr, "FAIL chmod -R followed a link out of the tree (%o)\n", (unsigned)(st.st_mode & 0777));
            fails++;
        }
        if (stat(key, &st) != 0 || (st.st_mode & 0777) != 0600) {
            fprintf(stderr, "FAIL chmod -R followed a link into the trust store (%o)\n", (unsigned)(st.st_mode & 0777));
            fails++;
        }
        /* The other tree walkers with the same links in the tree: none reads the key through its
         * link, copies it out, or deletes what a link points at. */
        char *t;
        char copied[800], line[64] = "";
        snprintf(cmd, sizeof cmd, "grep -r SECRET %s", tree);
        (void)run(cmd, &t);
        if (strstr(t, "SECRET")) {
            fprintf(stderr, "FAIL grep -r read the key through a link in the tree: %s\n", t);
            fails++;
        }
        free(t);
        snprintf(cmd, sizeof cmd, "cp -r %s %s/tree-copy", tree, g_root);
        (void)run(cmd, &t);
        free(t);
        snprintf(copied, sizeof copied, "%s/tree-copy/to-key", g_root);
        if (lstat(copied, &st) == 0 && S_ISREG(st.st_mode) && (fp = fopen(copied, "r")) != NULL) {
            if (fgets(line, sizeof line, fp) && strstr(line, "SECRET")) {
                fprintf(stderr, "FAIL cp -r copied the key out through a link in the tree\n");
                fails++;
            }
            fclose(fp);
        }
        snprintf(cmd, sizeof cmd, "rm -r %s", tree);
        (void)run(cmd, &t);
        free(t);
        if (access(key, F_OK) != 0 || access(f, F_OK) != 0) {
            fprintf(stderr, "FAIL rm -r deleted what a link in the tree points at\n");
            fails++;
        }
    }

    snprintf(cmd, sizeof cmd, "rm -rf %s", g_root);
    if (system(cmd) != 0) fprintf(stderr, "warn: could not remove %s\n", g_root);
    if (fails) {
        fprintf(stderr, "%d failure(s)\n", fails);
        return 1;
    }
    printf("shell protected-path selftest: ok\n");
    return 0;
}
