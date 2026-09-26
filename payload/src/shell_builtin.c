#include "shell_builtin.h"

#include <ctype.h>
#include <dirent.h>
#include <errno.h>
#include <fcntl.h>
#include <stdarg.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <strings.h>
#include <sys/stat.h>
#include <sys/sysctl.h>
#include <sys/time.h>
#include <sys/types.h>
#include <time.h>
#include <unistd.h>
#include <fts.h>
#include <pthread.h>
#include <fnmatch.h>
#include <regex.h>
#include <signal.h>
#include <sys/uio.h>
#include <sys/utsname.h>

#include "proc_list.h"
#include "runtime.h"

/* Set in main.c; the `id` builtin reports whether elevation worked. */
extern volatile int g_ucred_elevation_rc;


int shell_split(char *cmd, char *argv[], int max_args) {
    int argc = 0;
    char *p = cmd;
    char *w = cmd; /* write cursor — handles in-place quote-stripping */
    while (*p && argc < max_args) {
        /* Skip leading whitespace. */
        while (*p == ' ' || *p == '\t') p++;
        if (!*p) break;
        /* Each arg starts at the current write cursor — quote-stripping
         * may shift later chars back, so the visible arg pointer is `w`
         * not `p` at this moment. */
        argv[argc++] = w;
        while (*p) {
            char c = *p;
            if (c == ' ' || c == '\t') break;
            if (c == '\'') {
                /* Single quote: copy everything to next ' verbatim. */
                p++;
                while (*p && *p != '\'') *w++ = *p++;
                if (*p == '\'') p++;
                continue;
            }
            if (c == '"') {
                /* Double quote: copy with \" and \\ escapes recognised. */
                p++;
                while (*p && *p != '"') {
                    if (*p == '\\' && (p[1] == '"' || p[1] == '\\')) {
                        *w++ = p[1];
                        p += 2;
                    } else {
                        *w++ = *p++;
                    }
                }
                if (*p == '"') p++;
                continue;
            }
            if (c == '\\' && p[1]) {
                /* Bare backslash escape: take next char literal. */
                *w++ = p[1];
                p += 2;
                continue;
            }
            *w++ = *p++;
        }
        while (*p == ' ' || *p == '\t') p++;
        *w++ = '\0'; /* terminate this arg */
    }
    return argc;
}

/* Append `fmt`-formatted output to a dynamically grown buffer. Returns
 * the new len; updates *cap and *buf on grow. */
size_t shell_appendf(char **buf, size_t *cap, size_t len,
                             const char *fmt, ...) {
    const size_t max_cap = 256u * 1024u;
    va_list ap;
    while (1) {
        if (len >= max_cap - 1) return len;
        if (!*buf || *cap == 0) {
            char *nb = malloc(1024u);
            if (!nb) return len;
            *buf = nb;
            *cap = 1024u;
            (*buf)[0] = '\0';
        }
        va_start(ap, fmt);
        size_t avail = (*cap > len) ? (*cap - len) : 0;
        int n = vsnprintf(*buf + len, avail, fmt, ap);
        va_end(ap);
        if (n < 0) return len;
        if ((size_t)n < avail) return len + (size_t)n;
        size_t want = (*cap == 0 ? 1024u : *cap * 2u);
        if (want < len + (size_t)n + 1) want = len + (size_t)n + 1;
        if (want > max_cap) want = max_cap;
        if (*cap >= max_cap) return len;
        char *nb = realloc(*buf, want);
        if (!nb) return len;
        *buf = nb;
        *cap = want;
    }
}

static pthread_mutex_t g_shell_cwd_mtx = PTHREAD_MUTEX_INITIALIZER;

#define PS5UPLOAD2_SHELL_SESSIONS 16
typedef struct {
    int in_use;
    char session_id[96];
    char cwd[1024];
    uint64_t last_used;
} shell_session_t;
static pthread_mutex_t g_shell_session_mtx = PTHREAD_MUTEX_INITIALIZER;
static shell_session_t g_shell_sessions[PS5UPLOAD2_SHELL_SESSIONS];
static uint64_t g_shell_session_seq = 0;

const char *shell_valid_cwd(const char *cwd) {
    return (cwd && cwd[0] == '/') ? cwd : "/";
}

int shell_valid_session_id(const char *session_id) {
    if (!session_id || !session_id[0]) return 0;
    for (const char *p = session_id; *p; p++) {
        unsigned char c = (unsigned char)*p;
        if (c < 0x21 || c > 0x7e || c == '"' || c == '\\') return 0;
    }
    return 1;
}

void shell_session_get(const char *session_id, const char *fallback,
                              char *out, size_t cap) {
    const char *base = shell_valid_cwd(fallback);
    if (!out || cap == 0) return;
    snprintf(out, cap, "%s", base);
    if (!shell_valid_session_id(session_id)) return;
    pthread_mutex_lock(&g_shell_session_mtx);
    g_shell_session_seq += 1;
    int free_idx = -1;
    int oldest_idx = 0;
    uint64_t oldest = UINT64_MAX;
    for (int i = 0; i < PS5UPLOAD2_SHELL_SESSIONS; i++) {
        shell_session_t *s = &g_shell_sessions[i];
        if (s->in_use && strcmp(s->session_id, session_id) == 0) {
            s->last_used = g_shell_session_seq;
            snprintf(out, cap, "%s", shell_valid_cwd(s->cwd));
            pthread_mutex_unlock(&g_shell_session_mtx);
            return;
        }
        if (!s->in_use && free_idx < 0) free_idx = i;
        if (s->in_use && s->last_used < oldest) {
            oldest = s->last_used;
            oldest_idx = i;
        }
    }
    int idx = free_idx >= 0 ? free_idx : oldest_idx;
    shell_session_t *s = &g_shell_sessions[idx];
    memset(s, 0, sizeof(*s));
    s->in_use = 1;
    s->last_used = g_shell_session_seq;
    snprintf(s->session_id, sizeof(s->session_id), "%s", session_id);
    snprintf(s->cwd, sizeof(s->cwd), "%s", base);
    snprintf(out, cap, "%s", s->cwd);
    pthread_mutex_unlock(&g_shell_session_mtx);
}

void shell_session_set(const char *session_id, const char *cwd) {
    if (!shell_valid_session_id(session_id)) return;
    const char *next = shell_valid_cwd(cwd);
    pthread_mutex_lock(&g_shell_session_mtx);
    g_shell_session_seq += 1;
    int free_idx = -1;
    int oldest_idx = 0;
    uint64_t oldest = UINT64_MAX;
    for (int i = 0; i < PS5UPLOAD2_SHELL_SESSIONS; i++) {
        shell_session_t *s = &g_shell_sessions[i];
        if (s->in_use && strcmp(s->session_id, session_id) == 0) {
            snprintf(s->cwd, sizeof(s->cwd), "%s", next);
            s->last_used = g_shell_session_seq;
            pthread_mutex_unlock(&g_shell_session_mtx);
            return;
        }
        if (!s->in_use && free_idx < 0) free_idx = i;
        if (s->in_use && s->last_used < oldest) {
            oldest = s->last_used;
            oldest_idx = i;
        }
    }
    int idx = free_idx >= 0 ? free_idx : oldest_idx;
    shell_session_t *s = &g_shell_sessions[idx];
    memset(s, 0, sizeof(*s));
    s->in_use = 1;
    s->last_used = g_shell_session_seq;
    snprintf(s->session_id, sizeof(s->session_id), "%s", session_id);
    snprintf(s->cwd, sizeof(s->cwd), "%s", next);
    pthread_mutex_unlock(&g_shell_session_mtx);
}

int shell_json_string_field(const char *body, uint64_t body_len,
                                   const char *field, char *out, size_t cap) {
    if (!body || !field || !out || cap == 0) return -1;
    out[0] = '\0';
    char key[80];
    int kn = snprintf(key, sizeof(key), "\"%s\"", field);
    if (kn < 0 || (size_t)kn >= sizeof(key)) return -1;
    const char *end = body + body_len;
    const char *p = body;
    while (p < end) {
        const char *hit = strstr(p, key);
        if (!hit || hit >= end) return -1;
        p = hit + (size_t)kn;
        while (p < end && (*p == ' ' || *p == '\t' || *p == '\r' || *p == '\n')) p++;
        if (p >= end || *p != ':') continue;
        p++;
        while (p < end && (*p == ' ' || *p == '\t' || *p == '\r' || *p == '\n')) p++;
        if (p >= end || *p != '"') return -1;
        p++;
        size_t n = 0;
        while (p < end && *p != '"') {
            unsigned char c = (unsigned char)*p++;
            if (c == '\\' && p < end) {
                unsigned char e = (unsigned char)*p++;
                if (e == '"' || e == '\\' || e == '/') c = e;
                else if (e == 'n') c = '\n';
                else if (e == 'r') c = '\r';
                else if (e == 't') c = '\t';
                else if (e == 'b') c = '\b';
                else if (e == 'f') c = '\f';
                else if (e == 'u') {
                    if (p + 4 <= end) p += 4;
                    c = '?';
                } else {
                    c = e;
                }
            }
            if (n + 1 < cap) out[n++] = (char)c;
        }
        if (p >= end || *p != '"') return -1;
        out[n] = '\0';
        return 0;
    }
    return -1;
}



static int shell_join_path(const char *cwd, const char *path,
                           char *out, size_t cap) {
    if (!path || !*path) path = "/";
    if (path[0] == '/') {
        int n = snprintf(out, cap, "%s", path);
        return (n >= 0 && (size_t)n < cap) ? 0 : -1;
    }
    if (!cwd || cwd[0] != '/') cwd = "/";
    int n = snprintf(out, cap, "%s%s%s",
                     cwd,
                     strcmp(cwd, "/") == 0 ? "" : "/",
                     path);
    return (n >= 0 && (size_t)n < cap) ? 0 : -1;
}

int shell_resolve_dir(const char *cwd, const char *path,
                             char *out, size_t cap, char *err, size_t err_cap) {
    char joined[1024];
    if (shell_join_path(cwd, path, joined, sizeof(joined)) != 0) {
        snprintf(err, err_cap, "path too long");
        return -1;
    }
    char resolved[1024];
    if (!realpath(joined, resolved)) {
        snprintf(err, err_cap, "%s", strerror(errno));
        return -1;
    }
    struct stat st;
    if (stat(resolved, &st) != 0) {
        snprintf(err, err_cap, "%s", strerror(errno));
        return -1;
    }
    if (!S_ISDIR(st.st_mode)) {
        snprintf(err, err_cap, "not a directory");
        return -1;
    }
    int n = snprintf(out, cap, "%s", resolved);
    if (n < 0 || (size_t)n >= cap) {
        snprintf(err, err_cap, "path too long");
        return -1;
    }
    return 0;
}

int shell_ls_path(const char *path, char **out_text, int *out_exit) {
    if (!path || !out_text || !out_exit) return -1;
    *out_text = NULL;
    *out_exit = 0;
    char *out = NULL;
    size_t cap = 0, len = 0;
    DIR *dp = opendir(path);
    if (!dp) {
        len = shell_appendf(&out, &cap, len, "ls: %s: %s\n",
                            path, strerror(errno));
        *out_text = out;
        *out_exit = 1;
        return 0;
    }
    struct dirent *e;
    while ((e = readdir(dp)) != NULL) {
        if (e->d_name[0] == '.' && e->d_name[1] == '\0') continue;
        if (e->d_name[0] == '.' && e->d_name[1] == '.' && e->d_name[2] == '\0') continue;
        char child[1024];
        struct stat st;
        char type_c = '?';
        long long size = 0;
        int cn = snprintf(child, sizeof(child), "%s/%s",
                          strcmp(path, "/") == 0 ? "" : path,
                          e->d_name);
        if (cn >= 0 && (size_t)cn < sizeof(child) && stat(child, &st) == 0) {
            if (S_ISDIR(st.st_mode)) type_c = 'd';
            else if (S_ISREG(st.st_mode)) type_c = 'f';
            else if (S_ISLNK(st.st_mode)) type_c = 'l';
            else type_c = 'o';
            size = (long long)st.st_size;
        }
        len = shell_appendf(&out, &cap, len, "%c %12lld  %s\n",
                            type_c, size, e->d_name);
    }
    closedir(dp);
    if (!out) out = strdup_safe("");
    *out_text = out;
    return 0;
}

int handle_shell_builtin(const char *cmd_in, char **out_text,
                                 int *out_exit) {
    if (!cmd_in || !out_text || !out_exit) return -1;
    *out_text = NULL;
    *out_exit = 0;
    char cmdbuf[2100];
    snprintf(cmdbuf, sizeof(cmdbuf), "%s", cmd_in);
    char *argv[32];
    int argc = shell_split(cmdbuf, argv, 32);
    if (argc == 0) return -1;
    const char *prog = argv[0];

    char *out = NULL;
    size_t cap = 0, len = 0;

    if (strcmp(prog, "help") == 0) {
        len = shell_appendf(&out, &cap, len,
            "ps5upload built-in shell commands (PS5 has no /bin/sh).\n"
            "Quoting: 'literal', \"weak\", \\X — paths with spaces OK.\n"
            "\n"
            "Inspect:\n"
            "  help                    show this list\n"
            "  ls [path]               list directory (default /)\n"
            "  cat <path>              print file contents (8 KiB cap)\n"
            "  head [-n N] <path>      first N lines (default 10)\n"
            "  tail [-n N] <path>      last N lines (default 10)\n"
            "  wc [-lwc] <path>        line / word / byte counts\n"
            "  stat <path>             show file metadata\n"
            "  file <path>...          detect file type by magic bytes\n"
            "  xxd | hexdump [-C] <p>  canonical hex+ASCII (16 KiB cap)\n"
            "  find [path] [-name G] [-type f|d|l]   FTS walker\n"
            "  grep [-riElc] PAT path  POSIX regex search\n"
            "  du [-sh] <path>...      disk usage\n"
            "  sfoinfo <path>          parse param.sfo key/value pairs\n"
            "\n"
            "Filesystem:\n"
            "  cd [path]               change working dir (default /)\n"
            "  pwd                     print working dir\n"
            "  touch <path>...         create or bump mtime\n"
            "  mkdir [-p] <path>...    create directory\n"
            "  rmdir <path>...         remove empty directory\n"
            "  rm [-rf] <path>...      delete (refuses /system, /preinst)\n"
            "  cp [-r] SRC... DST      file or dir copy (256 MiB cap)\n"
            "  mv SRC... DST           rename, cross-FS copy+unlink\n"
            "  chmod [-R] OCT <path>   change mode (octal only)\n"
            "  ln -s TARGET LINK       create symbolic link\n"
            "  which <name>            find homebrew binary by name\n"
            "  mount                   active mount table\n"
            "  mtrw [/path]            remount /system rw (needs kstuff)\n"
            "  df                      filesystem usage\n"
            "\n"
            "Processes:\n"
            "  ps                      running processes (pid + name)\n"
            "  pid <name>              find pid(s) by substring match\n"
            "  kill [-N] <pid>...      send signal N (default 15/TERM)\n"
            "\n"
            "System:\n"
            "  date [+FMT]             current UTC time (strftime format)\n"
            "  uname [-a]              kernel info\n"
            "  hostname                kern.hostname sysctl\n"
            "  id                      effective uid/gid/authid\n"
            "  env                     environment variables\n"
            "  sysctl <name>           read a sysctl by name\n"
            "  sleep <secs>            sleep N seconds (1-30)\n"
            "  sync                    flush dirty buffers\n"
            "  klog [-n N]             last N bytes of /dev/klog\n"
            "  notify <msg...>         PS5 toast notification\n"
            "\n"
            "Path utils:\n"
            "  basename <path>         strip dir part\n"
            "  dirname <path>          strip file part\n"
            "\n"
            "Misc:\n"
            "  true | false            exit code 0 / 1\n"
            "  echo <args...>          print args verbatim\n");
        *out_text = out;
        return 0;
    }
    if (strcmp(prog, "true") == 0) { *out_text = strdup_safe(""); return 0; }
    if (strcmp(prog, "false") == 0) {
        *out_text = strdup_safe("");
        *out_exit = 1;
        return 0;
    }
    if (strcmp(prog, "cd") == 0) {
        const char *path = argc >= 2 ? argv[1] : "/";
        if (chdir(path) != 0) {
            len = shell_appendf(&out, &cap, len, "cd: %s: %s\n",
                                 path, strerror(errno));
            *out_text = out;
            *out_exit = 1;
            return 0;
        }
        char cwd[1024];
        if (getcwd(cwd, sizeof(cwd))) {
            len = shell_appendf(&out, &cap, len, "%s\n", cwd);
        }
        if (!out) out = strdup_safe("");
        *out_text = out;
        return 0;
    }
    if (strcmp(prog, "pwd") == 0) {
        char cwd[1024];
        if (getcwd(cwd, sizeof(cwd))) {
            len = shell_appendf(&out, &cap, len, "%s\n", cwd);
        } else {
            len = shell_appendf(&out, &cap, len, "/\n");
        }
        *out_text = out;
        return 0;
    }
    if (strcmp(prog, "echo") == 0) {
        for (int i = 1; i < argc; i++) {
            len = shell_appendf(&out, &cap, len, "%s%s",
                                 argv[i], i + 1 < argc ? " " : "\n");
        }
        if (argc == 1) len = shell_appendf(&out, &cap, len, "\n");
        *out_text = out;
        return 0;
    }
    if (strcmp(prog, "uname") == 0) {
        struct utsname u;
        if (uname(&u) != 0) {
            *out_text = strdup_safe("uname: failed\n");
            *out_exit = 1;
            return 0;
        }
        int all = (argc >= 2 && strcmp(argv[1], "-a") == 0);
        if (all) {
            len = shell_appendf(&out, &cap, len, "%s %s %s %s %s\n",
                                 u.sysname, u.nodename, u.release,
                                 u.version, u.machine);
        } else {
            len = shell_appendf(&out, &cap, len, "%s\n", u.sysname);
        }
        *out_text = out;
        return 0;
    }
    if (strcmp(prog, "hostname") == 0) {
        char host[256] = {0};
        size_t hl = sizeof(host);
        int mib[2] = {CTL_KERN, KERN_HOSTNAME};
        if (sysctl(mib, 2, host, &hl, NULL, 0) == 0) {
            len = shell_appendf(&out, &cap, len, "%s\n", host);
        } else {
            len = shell_appendf(&out, &cap, len, "(unknown)\n");
            *out_exit = 1;
        }
        *out_text = out;
        return 0;
    }
    if (strcmp(prog, "id") == 0) {
        uid_t uid = getuid(), euid = geteuid();
        gid_t gid = getgid(), egid = getegid();
        pid_t pid = getpid();
        len = shell_appendf(&out, &cap, len,
                            "uid=%u euid=%u gid=%u egid=%u pid=%d "
                            "ucred_elevation_rc=%d\n",
                            (unsigned)uid, (unsigned)euid,
                            (unsigned)gid, (unsigned)egid, (int)pid,
                            g_ucred_elevation_rc);
        *out_text = out;
        return 0;
    }
    if (strcmp(prog, "env") == 0) {
        extern char **environ;
        for (char **e = environ; e && *e; e++) {
            len = shell_appendf(&out, &cap, len, "%s\n", *e);
        }
        if (!out) out = strdup_safe("");
        *out_text = out;
        return 0;
    }
    if (strcmp(prog, "sleep") == 0) {
        int s = argc >= 2 ? atoi(argv[1]) : 0;
        if (s < 1) s = 1;
        if (s > 30) s = 30;
        sleep((unsigned)s);
        *out_text = strdup_safe("");
        return 0;
    }
    if (strcmp(prog, "ls") == 0) {
        const char *path = argc >= 2 ? argv[1] : "/";
        return shell_ls_path(path, out_text, out_exit);
    }
    if (strcmp(prog, "cat") == 0) {
        if (argc < 2) {
            *out_text = strdup_safe("cat: missing operand\n");
            *out_exit = 1;
            return 0;
        }
        const char *path = argv[1];
        int fd = open(path, O_RDONLY);
        if (fd < 0) {
            len = shell_appendf(&out, &cap, len, "cat: %s: %s\n",
                                 path, strerror(errno));
            *out_text = out;
            *out_exit = 1;
            return 0;
        }
        char chunk[8192];
        ssize_t total = 0;
        ssize_t r;
        while ((r = read(fd, chunk, sizeof(chunk))) > 0) {
            len = shell_appendf(&out, &cap, len, "%.*s", (int)r, chunk);
            total += r;
            if (total > 8 * 1024) break;
        }
        close(fd);
        if (total > 8 * 1024) {
            len = shell_appendf(&out, &cap, len,
                                 "\n(... cat output capped at 8 KiB)\n");
        }
        if (!out) out = strdup_safe("");
        *out_text = out;
        return 0;
    }
    if (strcmp(prog, "stat") == 0) {
        if (argc < 2) {
            *out_text = strdup_safe("stat: missing operand\n");
            *out_exit = 1;
            return 0;
        }
        struct stat st;
        if (stat(argv[1], &st) != 0) {
            len = shell_appendf(&out, &cap, len, "stat: %s: %s\n",
                                 argv[1], strerror(errno));
            *out_exit = 1;
        } else {
            len = shell_appendf(&out, &cap, len,
                                 "path: %s\nsize: %lld\nmode: 0%o\n"
                                 "uid: %u\ngid: %u\nmtime: %lld\n"
                                 "type: %s\n",
                                 argv[1], (long long)st.st_size,
                                 (unsigned)st.st_mode,
                                 (unsigned)st.st_uid, (unsigned)st.st_gid,
                                 (long long)st.st_mtime,
                                 S_ISDIR(st.st_mode) ? "dir" :
                                 S_ISREG(st.st_mode) ? "file" :
                                 S_ISLNK(st.st_mode) ? "link" : "other");
        }
        *out_text = out;
        return 0;
    }
    if (strcmp(prog, "mount") == 0) {
        struct statfs *mnts = NULL;
        int n = mntinfo_snapshot(&mnts);
        for (int i = 0; i < n && mnts; i++) {
            len = shell_appendf(&out, &cap, len, "%-10s %-30s %s\n",
                                 mnts[i].f_fstypename,
                                 mnts[i].f_mntfromname,
                                 mnts[i].f_mntonname);
        }
        free(mnts);
        if (!out) out = strdup_safe("");
        *out_text = out;
        return 0;
    }
    if (strcmp(prog, "df") == 0) {
        struct statfs *mnts = NULL;
        int n = mntinfo_snapshot(&mnts);
        len = shell_appendf(&out, &cap, len,
                             "%-30s %12s %12s %12s  use%%\n",
                             "filesystem", "blocks", "used", "avail");
        for (int i = 0; i < n && mnts; i++) {
            uint64_t bs = mnts[i].f_bsize;
            uint64_t total = (uint64_t)mnts[i].f_blocks * bs;
            uint64_t free_b = (uint64_t)mnts[i].f_bfree * bs;
            uint64_t used = total - free_b;
            int pct = total > 0 ? (int)((used * 100) / total) : 0;
            len = shell_appendf(&out, &cap, len,
                                 "%-30s %12llu %12llu %12llu  %3d%%\n",
                                 mnts[i].f_mntonname,
                                 (unsigned long long)(total / 1024),
                                 (unsigned long long)(used  / 1024),
                                 (unsigned long long)(free_b / 1024),
                                 pct);
        }
        free(mnts);
        if (!out) out = strdup_safe("");
        *out_text = out;
        return 0;
    }
    if (strcmp(prog, "ps") == 0) {
        /* proc_list_get_json returns a JSON blob like
         *   {"ok":true,"procs":[{"pid":123,"name":"SceShellUI"},...]}
         * Re-parse the pid+name pairs into a pretty 2-column listing.
         * Avoids reaching for a JSON library — the format is fixed and
         * we own both producer + consumer. */
        char *jbuf = malloc(64 * 1024);
        if (!jbuf) {
            *out_text = strdup_safe("ps: oom\n");
            *out_exit = 1;
            return 0;
        }
        size_t jwritten = 0;
        const char *jerr = NULL;
        if (proc_list_get_json(jbuf, 64 * 1024, &jwritten, &jerr) != 0) {
            len = shell_appendf(&out, &cap, len,
                                 "ps: %s\n", jerr ? jerr : "failed");
            free(jbuf);
            *out_text = out;
            *out_exit = 1;
            return 0;
        }
        /* Walk the JSON looking for "pid":N pairs and "name":"..." pairs.
         * Robust enough for our fixed shape; not a general JSON parser. */
        char *p = jbuf;
        while (p && *p) {
            char *pp = strstr(p, "\"pid\":");
            if (!pp) break;
            int pid_val = atoi(pp + 6);
            char *np = strstr(pp, "\"name\":\"");
            if (!np) break;
            np += 8;
            char *ne = strchr(np, '"');
            if (!ne) break;
            char name_buf[64];
            size_t nl = (size_t)(ne - np);
            if (nl >= sizeof(name_buf)) nl = sizeof(name_buf) - 1;
            memcpy(name_buf, np, nl);
            name_buf[nl] = '\0';
            len = shell_appendf(&out, &cap, len, "%6d  %s\n",
                                 pid_val, name_buf);
            p = ne + 1;
        }
        free(jbuf);
        if (!out) out = strdup_safe("");
        *out_text = out;
        return 0;
    }
    if (strcmp(prog, "sysctl") == 0) {
        if (argc < 2) {
            *out_text = strdup_safe("sysctl: missing name\n");
            *out_exit = 1;
            return 0;
        }
        char val[1024] = {0};
        size_t vl = sizeof(val);
        if (sysctlbyname(argv[1], val, &vl, NULL, 0) != 0) {
            len = shell_appendf(&out, &cap, len,
                                 "sysctl: %s: %s\n", argv[1], strerror(errno));
            *out_exit = 1;
        } else {
            len = shell_appendf(&out, &cap, len, "%s\n", val);
        }
        *out_text = out;
        return 0;
    }
    /* ── 2.13.0 Tier 1 additions ───────────────────────────────────── */
    if (strcmp(prog, "date") == 0) {
        /* `date` (no arg) → default RFC-like format. `date +FMT` → user
         * format. UTC only — PS5 system clock is stored UTC, and the
         * Hardware tab has the proper TZ display if the user wants
         * local. */
        time_t t = time(NULL);
        struct tm tm_utc;
        gmtime_r(&t, &tm_utc);
        const char *fmt = "%Y-%m-%d %H:%M:%S UTC";
        char user_fmt[128];
        if (argc >= 2 && argv[1][0] == '+') {
            snprintf(user_fmt, sizeof(user_fmt), "%s", argv[1] + 1);
            fmt = user_fmt;
        }
        char buf[256];
        if (strftime(buf, sizeof(buf), fmt, &tm_utc) == 0) {
            *out_text = strdup_safe("date: bad format or output too long\n");
            *out_exit = 1;
            return 0;
        }
        len = shell_appendf(&out, &cap, len, "%s\n", buf);
        *out_text = out;
        return 0;
    }
    if (strcmp(prog, "basename") == 0) {
        if (argc < 2) {
            *out_text = strdup_safe("basename: missing operand\n");
            *out_exit = 1;
            return 0;
        }
        const char *p = strrchr(argv[1], '/');
        const char *base = p ? p + 1 : argv[1];
        if (*base == '\0') base = "/"; /* "/" → "/" */
        len = shell_appendf(&out, &cap, len, "%s\n", base);
        *out_text = out;
        return 0;
    }
    if (strcmp(prog, "dirname") == 0) {
        if (argc < 2) {
            *out_text = strdup_safe("dirname: missing operand\n");
            *out_exit = 1;
            return 0;
        }
        char tmp[1024];
        snprintf(tmp, sizeof(tmp), "%s", argv[1]);
        /* Strip trailing slashes except for root. */
        size_t L = strlen(tmp);
        while (L > 1 && tmp[L - 1] == '/') tmp[--L] = '\0';
        char *p = strrchr(tmp, '/');
        const char *d = ".";
        if (p == tmp) d = "/";
        else if (p) { *p = '\0'; d = tmp; }
        len = shell_appendf(&out, &cap, len, "%s\n", d);
        *out_text = out;
        return 0;
    }
    if (strcmp(prog, "touch") == 0) {
        if (argc < 2) {
            *out_text = strdup_safe("touch: missing operand\n");
            *out_exit = 1;
            return 0;
        }
        int any_err = 0;
        for (int i = 1; i < argc; i++) {
            /* Create if missing, then bump mtime+atime to now. */
            int fd = open(argv[i], O_CREAT | O_WRONLY, 0644);
            if (fd < 0) {
                len = shell_appendf(&out, &cap, len,
                                     "touch: %s: %s\n", argv[i], strerror(errno));
                any_err = 1;
                continue;
            }
            close(fd);
            /* Use utimes(NULL) = bump both to current wall clock. */
            if (utimes(argv[i], NULL) != 0) {
                len = shell_appendf(&out, &cap, len,
                                     "touch: %s: utimes: %s\n", argv[i],
                                     strerror(errno));
                any_err = 1;
            }
        }
        if (any_err) *out_exit = 1;
        if (!out) out = strdup_safe("");
        *out_text = out;
        return 0;
    }
    if (strcmp(prog, "mkdir") == 0) {
        int parents = 0;
        int first_path = 1;
        if (argc >= 2 && strcmp(argv[1], "-p") == 0) {
            parents = 1;
            first_path = 2;
        }
        if (argc <= first_path) {
            *out_text = strdup_safe("mkdir: missing operand\n");
            *out_exit = 1;
            return 0;
        }
        int any_err = 0;
        for (int i = first_path; i < argc; i++) {
            if (!parents) {
                if (mkdir(argv[i], 0755) != 0) {
                    len = shell_appendf(&out, &cap, len,
                                         "mkdir: %s: %s\n", argv[i],
                                         strerror(errno));
                    any_err = 1;
                }
                continue;
            }
            /* -p: walk components, mkdir each ignoring EEXIST. */
            char tmp[1024];
            int tn = snprintf(tmp, sizeof(tmp), "%s", argv[i]);
            if (tn < 0 || (size_t)tn >= sizeof(tmp)) {
                len = shell_appendf(&out, &cap, len,
                                     "mkdir: %s: path too long\n", argv[i]);
                any_err = 1;
                continue;
            }
            for (char *p = tmp + (tmp[0] == '/' ? 1 : 0); *p; p++) {
                if (*p == '/') {
                    *p = '\0';
                    if (mkdir(tmp, 0755) != 0 && errno != EEXIST) {
                        len = shell_appendf(&out, &cap, len,
                                             "mkdir: %s: %s\n", tmp,
                                             strerror(errno));
                        any_err = 1;
                        break;
                    }
                    *p = '/';
                }
            }
            if (mkdir(tmp, 0755) != 0 && errno != EEXIST) {
                len = shell_appendf(&out, &cap, len,
                                     "mkdir: %s: %s\n", tmp, strerror(errno));
                any_err = 1;
            }
        }
        if (any_err) *out_exit = 1;
        if (!out) out = strdup_safe("");
        *out_text = out;
        return 0;
    }
    if (strcmp(prog, "rmdir") == 0) {
        if (argc < 2) {
            *out_text = strdup_safe("rmdir: missing operand\n");
            *out_exit = 1;
            return 0;
        }
        int any_err = 0;
        for (int i = 1; i < argc; i++) {
            if (rmdir(argv[i]) != 0) {
                len = shell_appendf(&out, &cap, len,
                                     "rmdir: %s: %s\n", argv[i],
                                     strerror(errno));
                any_err = 1;
            }
        }
        if (any_err) *out_exit = 1;
        if (!out) out = strdup_safe("");
        *out_text = out;
        return 0;
    }
    if (strcmp(prog, "kill") == 0) {
        int sig = SIGTERM;
        int first = 1;
        /* Accept `kill -9 PID` or `kill -SIGKILL PID` (numeric only for
         * now — symbolic names would need a table). */
        if (argc >= 3 && argv[1][0] == '-') {
            int n = atoi(argv[1] + 1);
            if (n > 0 && n < 64) sig = n;
            first = 2;
        }
        if (argc <= first) {
            *out_text = strdup_safe("kill: missing PID\n");
            *out_exit = 1;
            return 0;
        }
        int any_err = 0;
        for (int i = first; i < argc; i++) {
            int pid = atoi(argv[i]);
            if (pid <= 0) {
                len = shell_appendf(&out, &cap, len,
                                     "kill: %s: not a pid\n", argv[i]);
                any_err = 1;
                continue;
            }
            if (kill((pid_t)pid, sig) != 0) {
                len = shell_appendf(&out, &cap, len,
                                     "kill: %d: %s\n", pid, strerror(errno));
                any_err = 1;
            }
        }
        if (any_err) *out_exit = 1;
        if (!out) out = strdup_safe("");
        *out_text = out;
        return 0;
    }
    if (strcmp(prog, "sync") == 0) {
        sync();
        *out_text = strdup_safe("");
        return 0;
    }
    if (strcmp(prog, "notify") == 0) {
        /* Join argv[1..] with spaces and fire as a PS5 toast. Useful
         * for "I'm done, see this notification" from scripts. */
        if (argc < 2) {
            *out_text = strdup_safe("notify: missing message\n");
            *out_exit = 1;
            return 0;
        }
        char msg[1024] = {0};
        size_t mi = 0;
        for (int i = 1; i < argc && mi + 1 < sizeof(msg); i++) {
            int n = snprintf(msg + mi, sizeof(msg) - mi, "%s%s",
                             argv[i], i + 1 < argc ? " " : "");
            if (n < 0) break;
            mi += (size_t)n;
        }
        pop_notification(msg);
        len = shell_appendf(&out, &cap, len, "notified: %s\n", msg);
        *out_text = out;
        return 0;
    }
    if (strcmp(prog, "head") == 0) {
        /* head [-n N] PATH — first N lines (default 10). N is clamped
         * to [1, 1000] so a malicious script can't ask for 1B lines. */
        int n_lines = 10;
        int first = 1;
        if (argc >= 4 && strcmp(argv[1], "-n") == 0) {
            int v = atoi(argv[2]);
            if (v >= 1 && v <= 1000) n_lines = v;
            first = 3;
        }
        if (argc <= first) {
            *out_text = strdup_safe("head: missing PATH\n");
            *out_exit = 1;
            return 0;
        }
        FILE *fp = fopen(argv[first], "r");
        if (!fp) {
            len = shell_appendf(&out, &cap, len,
                                 "head: %s: %s\n", argv[first], strerror(errno));
            *out_text = out;
            *out_exit = 1;
            return 0;
        }
        char line[4096];
        int emitted = 0;
        while (emitted < n_lines && fgets(line, sizeof(line), fp)) {
            len = shell_appendf(&out, &cap, len, "%s", line);
            emitted++;
        }
        fclose(fp);
        if (!out) out = strdup_safe("");
        *out_text = out;
        return 0;
    }
    if (strcmp(prog, "tail") == 0) {
        /* tail [-n N] PATH — last N lines (default 10). For O(file-size)
         * walking we read the whole file (capped 256 KiB) then count
         * newlines backward. Simpler than seeking-back-and-rescanning
         * and more than fast enough for shell-tab use cases. */
        int n_lines = 10;
        int first = 1;
        if (argc >= 4 && strcmp(argv[1], "-n") == 0) {
            int v = atoi(argv[2]);
            if (v >= 1 && v <= 1000) n_lines = v;
            first = 3;
        }
        if (argc <= first) {
            *out_text = strdup_safe("tail: missing PATH\n");
            *out_exit = 1;
            return 0;
        }
        int fd = open(argv[first], O_RDONLY);
        if (fd < 0) {
            len = shell_appendf(&out, &cap, len,
                                 "tail: %s: %s\n", argv[first], strerror(errno));
            *out_text = out;
            *out_exit = 1;
            return 0;
        }
        off_t fsize = lseek(fd, 0, SEEK_END);
        if (fsize < 0) fsize = 0;
        off_t want = fsize;
        if (want > 256 * 1024) want = 256 * 1024;
        if (lseek(fd, fsize - want, SEEK_SET) == (off_t)-1) {
            close(fd);
            *out_text = strdup_safe("tail: seek failed\n");
            *out_exit = 1;
            return 0;
        }
        char *buf = malloc((size_t)want + 1);
        if (!buf) {
            close(fd);
            *out_text = strdup_safe("tail: oom\n");
            *out_exit = 1;
            return 0;
        }
        ssize_t rd = read(fd, buf, (size_t)want);
        close(fd);
        if (rd <= 0) {
            free(buf);
            if (!out) out = strdup_safe("");
            *out_text = out;
            return 0;
        }
        buf[rd] = '\0';
        /* Walk backward N+1 newlines (or fewer = print everything). */
        int seen = 0;
        ssize_t i = rd - 1;
        /* Skip trailing newline so we count "real" line ends. */
        if (i >= 0 && buf[i] == '\n') i--;
        for (; i >= 0; i--) {
            if (buf[i] == '\n') {
                seen++;
                if (seen >= n_lines) { i++; break; }
            }
        }
        if (i < 0) i = 0;
        len = shell_appendf(&out, &cap, len, "%s", buf + i);
        if (rd > 0 && buf[rd - 1] != '\n') {
            len = shell_appendf(&out, &cap, len, "\n");
        }
        free(buf);
        *out_text = out;
        return 0;
    }
    if (strcmp(prog, "wc") == 0) {
        /* wc [-l|-c|-w] PATH — line / byte / word counts. Default
         * shows all three. */
        int show_l = 1, show_w = 1, show_c = 1;
        int first = 1;
        if (argc >= 3 && argv[1][0] == '-') {
            show_l = show_w = show_c = 0;
            const char *flags = argv[1] + 1;
            for (const char *f = flags; *f; f++) {
                if (*f == 'l') show_l = 1;
                else if (*f == 'w') show_w = 1;
                else if (*f == 'c') show_c = 1;
            }
            first = 2;
        }
        if (argc <= first) {
            *out_text = strdup_safe("wc: missing PATH\n");
            *out_exit = 1;
            return 0;
        }
        int fd = open(argv[first], O_RDONLY);
        if (fd < 0) {
            len = shell_appendf(&out, &cap, len,
                                 "wc: %s: %s\n", argv[first], strerror(errno));
            *out_text = out;
            *out_exit = 1;
            return 0;
        }
        unsigned long long lines = 0, words = 0, bytes = 0;
        int in_word = 0;
        char chunk[8192];
        ssize_t r;
        while ((r = read(fd, chunk, sizeof(chunk))) > 0) {
            bytes += (unsigned long long)r;
            for (ssize_t k = 0; k < r; k++) {
                unsigned char c = (unsigned char)chunk[k];
                if (c == '\n') lines++;
                if (c == ' ' || c == '\t' || c == '\n' || c == '\r') {
                    in_word = 0;
                } else if (!in_word) {
                    in_word = 1;
                    words++;
                }
            }
        }
        close(fd);
        if (show_l) len = shell_appendf(&out, &cap, len, "%8llu", lines);
        if (show_w) len = shell_appendf(&out, &cap, len, "%8llu", words);
        if (show_c) len = shell_appendf(&out, &cap, len, "%8llu", bytes);
        len = shell_appendf(&out, &cap, len, " %s\n", argv[first]);
        *out_text = out;
        return 0;
    }
    if (strcmp(prog, "which") == 0) {
        /* Look for NAME in a short list of common PS5 dirs that
         * homebrew ELFs land in. PS5 has no system PATH, so this is
         * a convention not a shell-var lookup. */
        if (argc < 2) {
            *out_text = strdup_safe("which: missing NAME\n");
            *out_exit = 1;
            return 0;
        }
        static const char *dirs[] = {
            "/data/bin/", "/data/", "/user/homebrew/bin/",
            "/mnt/usb0/homebrew/bin/", "/system/vsh/app/", NULL,
        };
        int found = 0;
        for (int d = 0; dirs[d]; d++) {
            char p[1024];
            snprintf(p, sizeof(p), "%s%s", dirs[d], argv[1]);
            if (access(p, F_OK) == 0) {
                len = shell_appendf(&out, &cap, len, "%s\n", p);
                found = 1;
            }
        }
        if (!found) {
            len = shell_appendf(&out, &cap, len,
                                 "which: %s: not found\n", argv[1]);
            *out_exit = 1;
        }
        *out_text = out;
        return 0;
    }
    if (strcmp(prog, "klog") == 0) {
        /* klog [-n N] — last N bytes from /dev/klog (default 4 KiB,
         * clamped to 64 KiB). Useful for quick kernel-log peeks from
         * shell without leaving the tab for /logs?tab=kernel. */
        size_t n_bytes = 4 * 1024;
        if (argc >= 3 && strcmp(argv[1], "-n") == 0) {
            long v = atol(argv[2]);
            if (v > 0 && v <= 64 * 1024) n_bytes = (size_t)v;
        }
        int fd = open("/dev/klog", O_RDONLY | O_NONBLOCK);
        if (fd < 0) {
            len = shell_appendf(&out, &cap, len,
                                 "klog: open /dev/klog: %s\n", strerror(errno));
            *out_text = out;
            *out_exit = 1;
            return 0;
        }
        char *buf = malloc(n_bytes);
        if (!buf) {
            close(fd);
            *out_text = strdup_safe("klog: oom\n");
            *out_exit = 1;
            return 0;
        }
        ssize_t r = read(fd, buf, n_bytes);
        close(fd);
        if (r < 0) r = 0;
        len = shell_appendf(&out, &cap, len, "%.*s", (int)r, buf);
        if (r > 0 && buf[r - 1] != '\n') {
            len = shell_appendf(&out, &cap, len, "\n");
        }
        free(buf);
        *out_text = out;
        return 0;
    }
    if (strcmp(prog, "pid") == 0) {
        /* pid NAME — print the pid(s) of processes matching NAME
         * (substring). Saves users a `ps | grep`. */
        if (argc < 2) {
            *out_text = strdup_safe("pid: missing NAME\n");
            *out_exit = 1;
            return 0;
        }
        char *jbuf = malloc(64 * 1024);
        if (!jbuf) {
            *out_text = strdup_safe("pid: oom\n");
            *out_exit = 1;
            return 0;
        }
        size_t jwritten = 0;
        const char *jerr = NULL;
        if (proc_list_get_json(jbuf, 64 * 1024, &jwritten, &jerr) != 0) {
            len = shell_appendf(&out, &cap, len,
                                 "pid: %s\n", jerr ? jerr : "failed");
            free(jbuf);
            *out_text = out;
            *out_exit = 1;
            return 0;
        }
        const char *needle = argv[1];
        int found = 0;
        char *p = jbuf;
        while (p && *p) {
            char *pp = strstr(p, "\"pid\":");
            if (!pp) break;
            int pid_val = atoi(pp + 6);
            char *np = strstr(pp, "\"name\":\"");
            if (!np) break;
            np += 8;
            char *ne = strchr(np, '"');
            if (!ne) break;
            char name_buf[64];
            size_t nl = (size_t)(ne - np);
            if (nl >= sizeof(name_buf)) nl = sizeof(name_buf) - 1;
            memcpy(name_buf, np, nl);
            name_buf[nl] = '\0';
            if (strstr(name_buf, needle)) {
                len = shell_appendf(&out, &cap, len, "%d %s\n",
                                     pid_val, name_buf);
                found++;
            }
            p = ne + 1;
        }
        free(jbuf);
        if (!found) *out_exit = 1;
        if (!out) out = strdup_safe("");
        *out_text = out;
        return 0;
    }
    /* ── 2.13.0 Tier 2a: file operations ─────────────────────────── */
    if (strcmp(prog, "ln") == 0) {
        /* Symlink only — `ln -s TARGET LINK`. Hardlinks (`ln`) are
         * possible on PS5 but rarely useful since /system is RO. */
        if (argc < 4 || strcmp(argv[1], "-s") != 0) {
            *out_text = strdup_safe("ln: usage: ln -s TARGET LINK\n");
            *out_exit = 1;
            return 0;
        }
        if (symlink(argv[2], argv[3]) != 0) {
            len = shell_appendf(&out, &cap, len,
                                 "ln: %s -> %s: %s\n",
                                 argv[3], argv[2], strerror(errno));
            *out_exit = 1;
        }
        if (!out) out = strdup_safe("");
        *out_text = out;
        return 0;
    }
    if (strcmp(prog, "chmod") == 0) {
        /* chmod [-R] MODE PATH... — accepts octal (0644, 644) only.
         * Symbolic mode (u+x etc.) is omitted for simplicity; octal
         * is what most PS5 use-cases want anyway (chmod 755 on a
         * homebrew ELF). */
        int recursive = 0;
        int mode_at = 1;
        if (argc >= 4 && strcmp(argv[1], "-R") == 0) {
            recursive = 1;
            mode_at = 2;
        }
        if (argc <= mode_at + 1) {
            *out_text = strdup_safe("chmod: usage: chmod [-R] MODE PATH...\n");
            *out_exit = 1;
            return 0;
        }
        const char *mode_s = argv[mode_at];
        long mode = strtol(mode_s, NULL, 8);
        /* `chmod 000` is legitimate (clear all bits) so allow mode==0;
         * only reject negative or out-of-range. strtol returns 0 on
         * pure-junk input, so additionally require the first char
         * was a digit to distinguish 0 from "garbage". */
        if (mode < 0 || mode > 07777 ||
            (mode == 0 && (mode_s[0] < '0' || mode_s[0] > '7'))) {
            *out_text = strdup_safe("chmod: invalid octal mode\n");
            *out_exit = 1;
            return 0;
        }
        int any_err = 0;
        for (int i = mode_at + 1; i < argc; i++) {
            if (!recursive) {
                if (chmod(argv[i], (mode_t)mode) != 0) {
                    len = shell_appendf(&out, &cap, len,
                                         "chmod: %s: %s\n", argv[i],
                                         strerror(errno));
                    any_err = 1;
                }
                continue;
            }
            /* -R: walk via fts. */
            char *paths[2] = { (char *)argv[i], NULL };
            FTS *fts = fts_open(paths, FTS_PHYSICAL | FTS_NOCHDIR, NULL);
            if (!fts) {
                len = shell_appendf(&out, &cap, len,
                                     "chmod: %s: fts_open: %s\n",
                                     argv[i], strerror(errno));
                any_err = 1;
                continue;
            }
            FTSENT *ent;
            while ((ent = fts_read(fts)) != NULL) {
                if (ent->fts_info == FTS_DP) continue; /* post-order dirs */
                if (ent->fts_info == FTS_DNR || ent->fts_info == FTS_ERR) {
                    len = shell_appendf(&out, &cap, len,
                                         "chmod: %s: %s\n", ent->fts_path,
                                         strerror(ent->fts_errno));
                    any_err = 1;
                    continue;
                }
                if (chmod(ent->fts_accpath, (mode_t)mode) != 0) {
                    len = shell_appendf(&out, &cap, len,
                                         "chmod: %s: %s\n", ent->fts_path,
                                         strerror(errno));
                    any_err = 1;
                }
            }
            fts_close(fts);
        }
        if (any_err) *out_exit = 1;
        if (!out) out = strdup_safe("");
        *out_text = out;
        return 0;
    }
    if (strcmp(prog, "mv") == 0) {
        /* `mv SRC... DST` — POSIX semantics. If DST is a dir, src is
         * placed inside; otherwise rename. Falls back to copy+delete
         * across filesystems (errno EXDEV). */
        if (argc < 3) {
            *out_text = strdup_safe("mv: usage: mv SRC... DST\n");
            *out_exit = 1;
            return 0;
        }
        const char *dst = argv[argc - 1];
        struct stat dst_st;
        int dst_is_dir = (stat(dst, &dst_st) == 0 && S_ISDIR(dst_st.st_mode));
        int n_src = argc - 2;
        if (n_src > 1 && !dst_is_dir) {
            *out_text = strdup_safe("mv: multi-src requires DST to be a directory\n");
            *out_exit = 1;
            return 0;
        }
        int any_err = 0;
        for (int i = 1; i <= n_src; i++) {
            char target[1024];
            int tn;
            if (dst_is_dir) {
                const char *base = strrchr(argv[i], '/');
                base = base ? base + 1 : argv[i];
                tn = snprintf(target, sizeof(target), "%s/%s", dst, base);
            } else {
                tn = snprintf(target, sizeof(target), "%s", dst);
            }
            if (tn < 0 || (size_t)tn >= sizeof(target)) {
                len = shell_appendf(&out, &cap, len,
                                     "mv: %s: destination path too long\n", argv[i]);
                any_err = 1;
                continue;
            }
            /* A cross-DEVICE rename() panics this kernel instead of returning
             * EXDEV (see handle_fs_move). Only attempt the rename when source
             * and dest are on the SAME device; otherwise skip straight to the
             * copy-then-unlink path below. Never call rename() across mounts. */
            int mv_same_dev = 0;
            {
                struct stat mv_sf, mv_dd;
                char mv_dpar[1024];
                const char *mv_ds = strrchr(target, '/');
                if (mv_ds && mv_ds != target) {
                    size_t dl = (size_t)(mv_ds - target);
                    if (dl >= sizeof(mv_dpar)) dl = sizeof(mv_dpar) - 1;
                    memcpy(mv_dpar, target, dl);
                    mv_dpar[dl] = '\0';
                } else {
                    mv_dpar[0] = '/';
                    mv_dpar[1] = '\0';
                }
                mv_same_dev = (stat(argv[i], &mv_sf) == 0 &&
                               stat(mv_dpar, &mv_dd) == 0 &&
                               mv_sf.st_dev == mv_dd.st_dev);
            }
            if (mv_same_dev) {
                if (rename(argv[i], target) == 0) continue;
                if (errno != EXDEV) {
                    len = shell_appendf(&out, &cap, len,
                                         "mv: %s -> %s: %s\n",
                                         argv[i], target, strerror(errno));
                    any_err = 1;
                    continue;
                }
            }
            /* Cross-FS — copy then unlink. Single file only; cross-FS
             * directory mv is too complex for shell tab (use cp -r +
             * rm -r explicitly). */
            struct stat sst;
            if (stat(argv[i], &sst) != 0 || !S_ISREG(sst.st_mode)) {
                len = shell_appendf(&out, &cap, len,
                                     "mv: %s -> %s: cross-FS, only files supported\n",
                                     argv[i], target);
                any_err = 1;
                continue;
            }
            int sfd = open(argv[i], O_RDONLY);
            if (sfd < 0) {
                len = shell_appendf(&out, &cap, len,
                                     "mv: %s: %s\n", argv[i], strerror(errno));
                any_err = 1;
                continue;
            }
            int dfd = open(target, O_WRONLY | O_CREAT | O_TRUNC,
                           sst.st_mode & 0777);
            if (dfd < 0) {
                close(sfd);
                len = shell_appendf(&out, &cap, len,
                                     "mv: %s: %s\n", target, strerror(errno));
                any_err = 1;
                continue;
            }
            char buf[64 * 1024];
            ssize_t r;
            int copy_err = 0;
            while ((r = read(sfd, buf, sizeof(buf))) > 0) {
                ssize_t off = 0;
                while (off < r) {
                    ssize_t w = write(dfd, buf + off, (size_t)(r - off));
                    if (w <= 0) { copy_err = 1; break; }
                    off += w;
                }
                if (copy_err) break;
            }
            close(sfd);
            close(dfd);
            if (copy_err || r < 0) {
                len = shell_appendf(&out, &cap, len,
                                     "mv: %s -> %s: copy failed\n",
                                     argv[i], target);
                any_err = 1;
                unlink(target);
                continue;
            }
            if (unlink(argv[i]) != 0) {
                len = shell_appendf(&out, &cap, len,
                                     "mv: %s: copied but couldn't unlink: %s\n",
                                     argv[i], strerror(errno));
                any_err = 1;
            }
        }
        if (any_err) *out_exit = 1;
        if (!out) out = strdup_safe("");
        *out_text = out;
        return 0;
    }
    if (strcmp(prog, "cp") == 0) {
        /* `cp [-r] SRC... DST` — file (or recursive dir) copy. Caps
         * single-file size at 256 MiB to avoid OOM (PS5 RAM is
         * tight; users who want bigger transfers should use the
         * Upload tab). */
        int recursive = 0;
        int first_src = 1;
        if (argc >= 4 && strcmp(argv[1], "-r") == 0) {
            recursive = 1;
            first_src = 2;
        }
        if (argc <= first_src + 1) {
            *out_text = strdup_safe("cp: usage: cp [-r] SRC... DST\n");
            *out_exit = 1;
            return 0;
        }
        const char *dst = argv[argc - 1];
        int n_src = argc - first_src - 1;
        struct stat dst_st;
        int dst_is_dir = (stat(dst, &dst_st) == 0 && S_ISDIR(dst_st.st_mode));
        if (n_src > 1 && !dst_is_dir) {
            *out_text = strdup_safe("cp: multi-src requires DST to be a directory\n");
            *out_exit = 1;
            return 0;
        }
        int any_err = 0;
        for (int i = first_src; i < first_src + n_src; i++) {
            const char *src = argv[i];
            struct stat sst;
            if (stat(src, &sst) != 0) {
                len = shell_appendf(&out, &cap, len,
                                     "cp: %s: %s\n", src, strerror(errno));
                any_err = 1;
                continue;
            }
            if (S_ISDIR(sst.st_mode) && !recursive) {
                len = shell_appendf(&out, &cap, len,
                                     "cp: %s: is a directory (use -r)\n", src);
                any_err = 1;
                continue;
            }
            char target[1024];
            int tn;
            if (dst_is_dir) {
                const char *base = strrchr(src, '/');
                base = base ? base + 1 : src;
                tn = snprintf(target, sizeof(target), "%s/%s", dst, base);
            } else {
                tn = snprintf(target, sizeof(target), "%s", dst);
            }
            if (tn < 0 || (size_t)tn >= sizeof(target)) {
                len = shell_appendf(&out, &cap, len,
                                     "cp: %s: destination path too long\n", src);
                any_err = 1;
                continue;
            }
            /* For -r: refuse if TARGET is under SRC. Without this,
             * `cp -r /data /data/copy` would recurse into the newly-
             * created /data/copy and copy it again, etc, until path
             * truncation aborts or the disk fills. Compare via
             * device+inode of every existing ancestor of TARGET
             * against SRC's inode. Target itself doesn't exist yet,
             * so start the walk from the parent. */
            if (recursive && S_ISDIR(sst.st_mode)) {
                struct stat src_st = sst;
                char anc[1024];
                snprintf(anc, sizeof(anc), "%s", target);
                /* Strip the target's leaf to start at its parent. */
                char *slash0 = strrchr(anc, '/');
                if (slash0 == anc) anc[1] = '\0';
                else if (slash0) *slash0 = '\0';
                else snprintf(anc, sizeof(anc), ".");
                int cycle = 0;
                while (1) {
                    struct stat ast;
                    if (stat(anc, &ast) == 0 &&
                        ast.st_dev == src_st.st_dev &&
                        ast.st_ino == src_st.st_ino) {
                        cycle = 1;
                        break;
                    }
                    char *slash = strrchr(anc, '/');
                    if (!slash || slash == anc) break;
                    *slash = '\0';
                }
                if (cycle) {
                    len = shell_appendf(&out, &cap, len,
                                         "cp: %s -> %s: destination is inside source\n",
                                         src, target);
                    any_err = 1;
                    continue;
                }
            }
            if (!recursive || S_ISREG(sst.st_mode)) {
                /* Single-file copy. */
                if (sst.st_size > 256LL * 1024 * 1024) {
                    len = shell_appendf(&out, &cap, len,
                                         "cp: %s: %lld bytes exceeds 256 MiB cap "
                                         "— use Upload tab instead\n",
                                         src, (long long)sst.st_size);
                    any_err = 1;
                    continue;
                }
                int sfd = open(src, O_RDONLY);
                if (sfd < 0) {
                    len = shell_appendf(&out, &cap, len,
                                         "cp: %s: %s\n", src, strerror(errno));
                    any_err = 1;
                    continue;
                }
                int dfd = open(target, O_WRONLY | O_CREAT | O_TRUNC,
                               sst.st_mode & 0777);
                if (dfd < 0) {
                    close(sfd);
                    len = shell_appendf(&out, &cap, len,
                                         "cp: %s: %s\n", target, strerror(errno));
                    any_err = 1;
                    continue;
                }
                char buf[64 * 1024];
                ssize_t r;
                int copy_err = 0;
                while ((r = read(sfd, buf, sizeof(buf))) > 0) {
                    ssize_t off = 0;
                    while (off < r) {
                        ssize_t w = write(dfd, buf + off, (size_t)(r - off));
                        if (w <= 0) { copy_err = 1; break; }
                        off += w;
                    }
                    if (copy_err) break;
                }
                close(sfd);
                close(dfd);
                if (copy_err || r < 0) {
                    len = shell_appendf(&out, &cap, len,
                                         "cp: %s -> %s: copy failed\n",
                                         src, target);
                    any_err = 1;
                    unlink(target);
                }
                continue;
            }
            /* Recursive directory copy via FTS. */
            char *paths[2] = { (char *)src, NULL };
            FTS *fts = fts_open(paths, FTS_PHYSICAL | FTS_NOCHDIR, NULL);
            if (!fts) {
                len = shell_appendf(&out, &cap, len,
                                     "cp: %s: fts_open: %s\n",
                                     src, strerror(errno));
                any_err = 1;
                continue;
            }
            size_t src_prefix_len = strlen(src);
            FTSENT *ent;
            while ((ent = fts_read(fts)) != NULL) {
                if (ent->fts_info == FTS_DP) continue;
                if (ent->fts_info == FTS_DNR || ent->fts_info == FTS_ERR) {
                    len = shell_appendf(&out, &cap, len,
                                         "cp: %s: %s\n", ent->fts_path,
                                         strerror(ent->fts_errno));
                    any_err = 1;
                    continue;
                }
                /* dst_path = target + (fts_path - src) */
                const char *rel = ent->fts_path + src_prefix_len;
                while (*rel == '/') rel++;
                char dpath[1024];
                int dn;
                if (*rel)
                    dn = snprintf(dpath, sizeof(dpath), "%s/%s", target, rel);
                else
                    dn = snprintf(dpath, sizeof(dpath), "%s", target);
                if (dn < 0 || (size_t)dn >= sizeof(dpath)) {
                    len = shell_appendf(&out, &cap, len,
                                         "cp: %s: path too long\n", ent->fts_path);
                    any_err = 1;
                    continue;
                }
                if (ent->fts_info == FTS_D) {
                    if (mkdir(dpath, ent->fts_statp->st_mode & 0777) != 0
                        && errno != EEXIST) {
                        len = shell_appendf(&out, &cap, len,
                                             "cp: %s: mkdir: %s\n",
                                             dpath, strerror(errno));
                        any_err = 1;
                    }
                    continue;
                }
                if (ent->fts_info != FTS_F) continue;
                /* File copy — same byte loop as the single-file path. */
                int sfd = open(ent->fts_accpath, O_RDONLY);
                if (sfd < 0) {
                    len = shell_appendf(&out, &cap, len,
                                         "cp: %s: %s\n", ent->fts_path,
                                         strerror(errno));
                    any_err = 1;
                    continue;
                }
                int dfd = open(dpath, O_WRONLY | O_CREAT | O_TRUNC,
                               ent->fts_statp->st_mode & 0777);
                if (dfd < 0) {
                    close(sfd);
                    len = shell_appendf(&out, &cap, len,
                                         "cp: %s: %s\n", dpath, strerror(errno));
                    any_err = 1;
                    continue;
                }
                char buf[64 * 1024];
                ssize_t rd;
                while ((rd = read(sfd, buf, sizeof(buf))) > 0) {
                    ssize_t off = 0;
                    while (off < rd) {
                        ssize_t w = write(dfd, buf + off, (size_t)(rd - off));
                        if (w <= 0) { rd = -1; break; }
                        off += w;
                    }
                }
                close(sfd);
                close(dfd);
                if (rd < 0) {
                    len = shell_appendf(&out, &cap, len,
                                         "cp: %s -> %s: copy failed\n",
                                         ent->fts_path, dpath);
                    any_err = 1;
                    unlink(dpath);
                }
            }
            fts_close(fts);
        }
        if (any_err) *out_exit = 1;
        if (!out) out = strdup_safe("");
        *out_text = out;
        return 0;
    }
    if (strcmp(prog, "rm") == 0) {
        /* `rm [-r] [-f] PATH...` — refuses /system* and /preinst* to
         * avoid bricking the console. -f silences "missing operand"
         * errors AND ENOENT but does NOT suppress permission errors
         * (these are real bugs the user wants to know about). */
        int recursive = 0;
        int force = 0;
        int first = 1;
        while (first < argc && argv[first][0] == '-') {
            for (const char *f = argv[first] + 1; *f; f++) {
                if (*f == 'r' || *f == 'R') recursive = 1;
                else if (*f == 'f') force = 1;
            }
            first++;
        }
        if (argc <= first) {
            if (!force) {
                *out_text = strdup_safe("rm: missing operand\n");
                *out_exit = 1;
                return 0;
            }
            *out_text = strdup_safe("");
            return 0;
        }
        int any_err = 0;
        for (int i = first; i < argc; i++) {
            const char *p = argv[i];
            /* Path safety: delegate to is_path_allowed(), which covers
             * the writable-root allowlist (e.g. /data, /user, /mnt),
             * rejects ".." traversal, and catches symlink escapes via
             * realpath(). This replaces a broken ad-hoc normalizer +
             * incomplete banned-prefix list that missed /system_data
             * and /dev, and could be bypassed by /data/../system_ex. */
            if (!is_path_allowed(p)) {
                len = shell_appendf(&out, &cap, len,
                                     "rm: %s: refusing to touch system path\n", p);
                any_err = 1;
                continue;
            }
            struct stat sst;
            if (lstat(p, &sst) != 0) {
                if (!force) {
                    len = shell_appendf(&out, &cap, len,
                                         "rm: %s: %s\n", p, strerror(errno));
                    any_err = 1;
                }
                continue;
            }
            if (S_ISDIR(sst.st_mode) && !recursive) {
                len = shell_appendf(&out, &cap, len,
                                     "rm: %s: is a directory (use -r)\n", p);
                any_err = 1;
                continue;
            }
            if (!S_ISDIR(sst.st_mode)) {
                if (unlink(p) != 0) {
                    len = shell_appendf(&out, &cap, len,
                                         "rm: %s: %s\n", p, strerror(errno));
                    any_err = 1;
                }
                continue;
            }
            /* Recursive directory remove via FTS post-order. */
            char *paths[2] = { (char *)p, NULL };
            FTS *fts = fts_open(paths,
                                 FTS_PHYSICAL | FTS_NOCHDIR, NULL);
            if (!fts) {
                len = shell_appendf(&out, &cap, len,
                                     "rm: %s: fts_open: %s\n", p,
                                     strerror(errno));
                any_err = 1;
                continue;
            }
            FTSENT *ent;
            while ((ent = fts_read(fts)) != NULL) {
                if (ent->fts_info == FTS_DNR || ent->fts_info == FTS_ERR) {
                    len = shell_appendf(&out, &cap, len,
                                         "rm: %s: %s\n", ent->fts_path,
                                         strerror(ent->fts_errno));
                    any_err = 1;
                    continue;
                }
                if (ent->fts_info == FTS_D) continue; /* pre-order */
                if (ent->fts_info == FTS_DP) {
                    if (rmdir(ent->fts_accpath) != 0) {
                        len = shell_appendf(&out, &cap, len,
                                             "rm: %s: %s\n", ent->fts_path,
                                             strerror(errno));
                        any_err = 1;
                    }
                    continue;
                }
                if (unlink(ent->fts_accpath) != 0) {
                    len = shell_appendf(&out, &cap, len,
                                         "rm: %s: %s\n", ent->fts_path,
                                         strerror(errno));
                    any_err = 1;
                }
            }
            fts_close(fts);
        }
        if (any_err) *out_exit = 1;
        if (!out) out = strdup_safe("");
        *out_text = out;
        return 0;
    }
    /* ── 2.13.0 Tier 2b: search + inspect ───────────────────────── */
    if (strcmp(prog, "find") == 0) {
        /* `find [PATH] [-name GLOB] [-type f|d|l]` — walk PATH (default
         * .) and print entries matching the filters. Cap at 10000
         * entries to stay within 256 KiB response budget. */
        const char *path = ".";
        const char *name_glob = NULL;
        char type_filter = 0;
        int i = 1;
        if (i < argc && argv[i][0] != '-') {
            path = argv[i++];
        }
        while (i < argc) {
            if (i + 1 < argc && strcmp(argv[i], "-name") == 0) {
                name_glob = argv[i + 1];
                i += 2;
            } else if (i + 1 < argc && strcmp(argv[i], "-type") == 0) {
                type_filter = argv[i + 1][0];
                i += 2;
            } else {
                len = shell_appendf(&out, &cap, len,
                                     "find: unknown arg %s\n", argv[i]);
                *out_exit = 1;
                *out_text = out;
                return 0;
            }
        }
        char *paths[2] = { (char *)path, NULL };
        FTS *fts_h = fts_open(paths, FTS_PHYSICAL | FTS_NOCHDIR, NULL);
        if (!fts_h) {
            len = shell_appendf(&out, &cap, len,
                                 "find: %s: %s\n", path, strerror(errno));
            *out_text = out;
            *out_exit = 1;
            return 0;
        }
        int count = 0;
        int truncated = 0;
        FTSENT *ent;
        while ((ent = fts_read(fts_h)) != NULL) {
            if (ent->fts_info == FTS_DP) continue;
            if (ent->fts_info == FTS_DNR || ent->fts_info == FTS_ERR) continue;
            if (type_filter) {
                char t = 0;
                if (ent->fts_info == FTS_D) t = 'd';
                else if (ent->fts_info == FTS_F) t = 'f';
                else if (ent->fts_info == FTS_SL || ent->fts_info == FTS_SLNONE) t = 'l';
                if (t != type_filter) continue;
            }
            if (name_glob) {
                const char *base = strrchr(ent->fts_path, '/');
                base = base ? base + 1 : ent->fts_path;
                if (fnmatch(name_glob, base, 0) != 0) continue;
            }
            len = shell_appendf(&out, &cap, len, "%s\n", ent->fts_path);
            count++;
            if (count >= 10000) {
                truncated = 1;
                break;
            }
        }
        fts_close(fts_h);
        if (truncated) {
            len = shell_appendf(&out, &cap, len,
                                 "(... find result capped at 10000 entries; "
                                 "narrow with -name or -type)\n");
        }
        if (!out) out = strdup_safe("");
        *out_text = out;
        return 0;
    }
    if (strcmp(prog, "grep") == 0) {
        /* `grep [-r] [-i] [-E] [-l] [-c] PATTERN PATH...` — POSIX
         * regex via regex.h. -r walks dirs via FTS. -l prints only
         * matching filenames. -c prints only counts. Caps output at
         * 5000 match lines. */
        int recursive = 0, case_i = 0, list_only = 0, count_only = 0;
        int i = 1;
        while (i < argc && argv[i][0] == '-' && argv[i][1]) {
            for (const char *f = argv[i] + 1; *f; f++) {
                if (*f == 'r' || *f == 'R') recursive = 1;
                else if (*f == 'i') case_i = 1;
                else if (*f == 'E') { /* default; accepted */ }
                else if (*f == 'l') list_only = 1;
                else if (*f == 'c') count_only = 1;
                else {
                    len = shell_appendf(&out, &cap, len,
                                         "grep: bad flag -%c\n", *f);
                    *out_exit = 1;
                    *out_text = out;
                    return 0;
                }
            }
            i++;
        }
        if (i >= argc) {
            *out_text = strdup_safe("grep: usage: grep [-riElc] PATTERN PATH...\n");
            *out_exit = 1;
            return 0;
        }
        const char *pat = argv[i++];
        regex_t re;
        int flags = REG_EXTENDED | (case_i ? REG_ICASE : 0);
        int rrc = regcomp(&re, pat, flags);
        if (rrc != 0) {
            char errbuf[256];
            regerror(rrc, &re, errbuf, sizeof(errbuf));
            len = shell_appendf(&out, &cap, len,
                                 "grep: bad pattern: %s\n", errbuf);
            *out_exit = 1;
            *out_text = out;
            return 0;
        }
        if (i >= argc) {
            regfree(&re);
            *out_text = strdup_safe("grep: missing PATH (stdin not supported)\n");
            *out_exit = 1;
            return 0;
        }
        int total_matches = 0;
        int total_capped = 0;
        for (; i < argc && !total_capped; i++) {
            const char *p = argv[i];
            struct stat st_p;
            int is_dir = (stat(p, &st_p) == 0 && S_ISDIR(st_p.st_mode));
            char *paths[2] = { (char *)p, NULL };
            FTS *fts_h = NULL;
            if (recursive && is_dir) {
                fts_h = fts_open(paths,
                                FTS_PHYSICAL | FTS_NOCHDIR, NULL);
                if (!fts_h) {
                    len = shell_appendf(&out, &cap, len,
                                         "grep: %s: %s\n", p, strerror(errno));
                    continue;
                }
            } else if (is_dir) {
                len = shell_appendf(&out, &cap, len,
                                     "grep: %s: is a directory (use -r)\n", p);
                continue;
            }
            const char *next_path = NULL;
            FTSENT *ent = NULL;
            while (1) {
                if (fts_h) {
                    ent = fts_read(fts_h);
                    if (!ent) break;
                    if (ent->fts_info != FTS_F) continue;
                    next_path = ent->fts_path;
                } else {
                    if (next_path) break;
                    next_path = p;
                }
                FILE *fp = fopen(next_path, "r");
                if (!fp) {
                    len = shell_appendf(&out, &cap, len,
                                         "grep: %s: %s\n", next_path, strerror(errno));
                    if (!fts_h) break;
                    continue;
                }
                char line[8192];
                int file_match_count = 0;
                while (fgets(line, sizeof(line), fp)) {
                    size_t L = strlen(line);
                    if (L > 0 && line[L - 1] == '\n') line[L - 1] = '\0';
                    if (regexec(&re, line, 0, NULL, 0) != 0) continue;
                    file_match_count++;
                    total_matches++;
                    if (!list_only && !count_only) {
                        len = shell_appendf(&out, &cap, len, "%s%s%s\n",
                                             (recursive && fts_h) ? next_path : "",
                                             (recursive && fts_h) ? ":" : "",
                                             line);
                    }
                    if (list_only) break;
                    if (total_matches >= 5000) {
                        total_capped = 1;
                        break;
                    }
                }
                fclose(fp);
                if (list_only && file_match_count > 0) {
                    len = shell_appendf(&out, &cap, len, "%s\n", next_path);
                }
                if (count_only) {
                    if (recursive && fts_h) {
                        len = shell_appendf(&out, &cap, len, "%s:%d\n",
                                             next_path, file_match_count);
                    } else {
                        len = shell_appendf(&out, &cap, len, "%d\n",
                                             file_match_count);
                    }
                }
                if (!fts_h) break;
                if (total_capped) break;
            }
            if (fts_h) fts_close(fts_h);
        }
        regfree(&re);
        if (total_matches == 0 && !count_only) *out_exit = 1;
        if (total_capped) {
            len = shell_appendf(&out, &cap, len,
                                 "(... grep result capped at 5000 matches)\n");
        }
        if (!out) out = strdup_safe("");
        *out_text = out;
        return 0;
    }
    if (strcmp(prog, "du") == 0) {
        /* `du [-sh] PATH...` — summary in human-readable units. */
        int human = 0;
        int i = 1;
        if (i < argc && argv[i][0] == '-') {
            for (const char *f = argv[i] + 1; *f; f++) {
                if (*f == 'h') human = 1;
                else if (*f == 's') { /* default; accepted */ }
            }
            i++;
        }
        if (i >= argc) {
            *out_text = strdup_safe("du: usage: du [-sh] PATH...\n");
            *out_exit = 1;
            return 0;
        }
        for (; i < argc; i++) {
            char *paths[2] = { (char *)argv[i], NULL };
            FTS *fts_h = fts_open(paths,
                                 FTS_PHYSICAL | FTS_NOCHDIR, NULL);
            if (!fts_h) {
                len = shell_appendf(&out, &cap, len,
                                     "du: %s: %s\n", argv[i], strerror(errno));
                *out_exit = 1;
                continue;
            }
            unsigned long long total_bytes = 0;
            FTSENT *ent;
            while ((ent = fts_read(fts_h)) != NULL) {
                if (ent->fts_info == FTS_DP) continue;
                if (ent->fts_info == FTS_DNR || ent->fts_info == FTS_ERR) continue;
                if (ent->fts_info == FTS_F) {
                    total_bytes += (unsigned long long)ent->fts_statp->st_size;
                }
            }
            fts_close(fts_h);
            if (human) {
                const char *unit = "B";
                double v = (double)total_bytes;
                if (v >= 1024) { v /= 1024; unit = "K"; }
                if (v >= 1024) { v /= 1024; unit = "M"; }
                if (v >= 1024) { v /= 1024; unit = "G"; }
                if (v >= 1024) { v /= 1024; unit = "T"; }
                len = shell_appendf(&out, &cap, len, "%.1f%s\t%s\n",
                                     v, unit, argv[i]);
            } else {
                len = shell_appendf(&out, &cap, len, "%llu\t%s\n",
                                     total_bytes / 1024, argv[i]);
            }
        }
        if (!out) out = strdup_safe("");
        *out_text = out;
        return 0;
    }
    if (strcmp(prog, "xxd") == 0 || strcmp(prog, "hexdump") == 0) {
        /* `xxd PATH` — canonical `hexdump -C` layout. 16 bytes per
         * row, offset + hex + ASCII. Cap at 16 KiB. */
        int first = 1;
        if (strcmp(prog, "hexdump") == 0 && argc >= 2 &&
            strcmp(argv[1], "-C") == 0) {
            first = 2;
        }
        if (argc <= first) {
            *out_text = strdup_safe("xxd: missing PATH\n");
            *out_exit = 1;
            return 0;
        }
        int fd = open(argv[first], O_RDONLY);
        if (fd < 0) {
            len = shell_appendf(&out, &cap, len,
                                 "xxd: %s: %s\n", argv[first], strerror(errno));
            *out_text = out;
            *out_exit = 1;
            return 0;
        }
        unsigned char chunk[16];
        long offset = 0;
        ssize_t r;
        long total = 0;
        while (total < 16 * 1024 && (r = read(fd, chunk, sizeof(chunk))) > 0) {
            len = shell_appendf(&out, &cap, len, "%08lx  ", offset);
            for (int k = 0; k < 16; k++) {
                if (k < r) {
                    len = shell_appendf(&out, &cap, len, "%02x ", chunk[k]);
                } else {
                    len = shell_appendf(&out, &cap, len, "   ");
                }
                if (k == 7) {
                    len = shell_appendf(&out, &cap, len, " ");
                }
            }
            len = shell_appendf(&out, &cap, len, " |");
            for (int k = 0; k < r; k++) {
                unsigned char c = chunk[k];
                len = shell_appendf(&out, &cap, len, "%c",
                                     (c >= 0x20 && c < 0x7f) ? c : '.');
            }
            len = shell_appendf(&out, &cap, len, "|\n");
            offset += r;
            total += r;
        }
        close(fd);
        if (total >= 16 * 1024) {
            len = shell_appendf(&out, &cap, len,
                                 "(... xxd output capped at 16 KiB)\n");
        }
        *out_text = out;
        return 0;
    }
    if (strcmp(prog, "file") == 0) {
        /* Magic-byte detection for PS5-relevant file types. */
        if (argc < 2) {
            *out_text = strdup_safe("file: missing PATH\n");
            *out_exit = 1;
            return 0;
        }
        int any_err = 0;
        for (int i = 1; i < argc; i++) {
            int fd = open(argv[i], O_RDONLY);
            if (fd < 0) {
                len = shell_appendf(&out, &cap, len,
                                     "%s: %s\n", argv[i], strerror(errno));
                any_err = 1;
                continue;
            }
            unsigned char m[32] = {0};
            ssize_t r = read(fd, m, sizeof(m));
            close(fd);
            const char *kind = "data";
            if (r >= 4) {
                if (m[0] == 0x4f && m[1] == 0x15 && m[2] == 0x3d && m[3] == 0x1d)
                    kind = "PS5 SELF (Sony-signed)";
                else if (m[0] == 0x7f && m[1] == 'E' && m[2] == 'L' && m[3] == 'F')
                    kind = "ELF executable";
                else if (m[0] == 0x7f && m[1] == 'P' && m[2] == 'R' && m[3] == 'X')
                    kind = "PRX library";
                else if (r >= 8 && m[0] == 'S' && m[1] == 'C' && m[2] == 'E' &&
                         m[3] == 'U' && m[4] == 'F')
                    kind = "PS5 PUP firmware update";
                else if (m[0] == 0x7f && m[1] == 'C' && m[2] == 'N' && m[3] == 'T')
                    kind = "PS5 PKG content package";
                else if (m[0] == 0x89 && m[1] == 'P' && m[2] == 'N' && m[3] == 'G')
                    kind = "PNG image";
                else if (r >= 3 && m[0] == 0xff && m[1] == 0xd8 && m[2] == 0xff)
                    kind = "JPEG image";
                else if (m[0] == 'P' && m[1] == 'K' && m[2] == 0x03 && m[3] == 0x04)
                    kind = "ZIP archive";
                else if (m[0] == 0x00 && m[1] == 'P' && m[2] == 'S' && m[3] == 'F')
                    kind = "param.sfo metadata";
                else if (r >= 8 && m[0] == 0 && m[1] == 0 && m[2] == 0 &&
                         m[3] == 0 && m[4] == 'M' && m[5] == 'O' && m[6] == 'V')
                    kind = "MOV/MP4 video";
                else {
                    int printable = 1;
                    for (ssize_t k = 0; k < r; k++) {
                        unsigned char c = m[k];
                        if (c < 0x09 || (c > 0x0d && c < 0x20) || c >= 0x7f) {
                            printable = 0;
                            break;
                        }
                    }
                    if (printable && r > 0) kind = "ASCII text";
                }
            } else if (r == 0) {
                kind = "empty";
            }
            len = shell_appendf(&out, &cap, len, "%s: %s\n", argv[i], kind);
        }
        if (any_err) *out_exit = 1;
        *out_text = out;
        return 0;
    }
    /* ── 2.13.0 Tier 3: PS5-specific niche ───────────────────────── */
    if (strcmp(prog, "mtrw") == 0) {
        /* Remount /system (or arbitrary mount) read-write — one of the
         * most-asked PS5 verbs. Sony mounts /system + /system_ex
         * read-only; turning them rw lets users patch system
         * resources or install custom UI assets. Requires kernel R/W
         * (kstuff) — otherwise nmount returns EACCES.
         *
         * Usage: `mtrw` (= /system), `mtrw /system_ex`, `mtrw /preinst`. */
        const char *mnt = argc >= 2 ? argv[1] : "/system";
        /* nmount(MNT_UPDATE) with the iovec containing the mount
         * point + fstype keeps the same fs but flips rdonly. The
         * "fstype" must match what's already mounted there (ufs or
         * nullfs typically); we look it up via getmntinfo. */
        struct statfs *mnts = NULL;
        int n = mntinfo_snapshot(&mnts);
        const char *fstype = NULL;
        const char *from = NULL;
        for (int i = 0; i < n && mnts; i++) {
            if (strcmp(mnts[i].f_mntonname, mnt) == 0) {
                fstype = mnts[i].f_fstypename;
                from = mnts[i].f_mntfromname;
                break;
            }
        }
        /* NOTE: fstype/from alias INTO `mnts`, so the snapshot must stay alive
         * until after the nmount iovec below is built and used — free it only
         * on each return path past that point (and here, where they're unused). */
        if (!fstype) {
            len = shell_appendf(&out, &cap, len,
                                 "mtrw: %s: not a mounted filesystem\n", mnt);
            *out_text = out;
            *out_exit = 1;
            free(mnts);
            return 0;
        }
        /* nmount(2) iovec: each option name + value pair, iov_len
         * INCLUDES the trailing NUL byte (per man page). `from`
         * comes from the existing mount's f_mntfromname so the
         * kernel matches the underlying device/source correctly. */
        struct iovec iov[6];
        iov[0].iov_base = (void *)"fstype";
        iov[0].iov_len = strlen("fstype") + 1;
        iov[1].iov_base = (void *)fstype;
        iov[1].iov_len = strlen(fstype) + 1;
        iov[2].iov_base = (void *)"fspath";
        iov[2].iov_len = strlen("fspath") + 1;
        iov[3].iov_base = (void *)mnt;
        iov[3].iov_len = strlen(mnt) + 1;
        iov[4].iov_base = (void *)"from";
        iov[4].iov_len = strlen("from") + 1;
        iov[5].iov_base = (void *)from;
        iov[5].iov_len = strlen(from) + 1;
        if (nmount(iov, 6, MNT_UPDATE) != 0) {
            len = shell_appendf(&out, &cap, len,
                                 "mtrw: %s: %s (need kernel R/W via kstuff?)\n",
                                 mnt, strerror(errno));
            *out_text = out;
            *out_exit = 1;
            free(mnts); /* last use of `from` (iov[5]) was above */
            return 0;
        }
        len = shell_appendf(&out, &cap, len, "%s remounted rw (%s)\n",
                             mnt, fstype);
        *out_text = out;
        free(mnts); /* last use of `fstype` was the line above */
        return 0;
    }
    if (strcmp(prog, "sfoinfo") == 0) {
        /* `sfoinfo PATH` — parse SCE param.sfo and print key/value
         * pairs. Format:
         *   magic    0x00 0x50 0x53 0x46 (\x00PSF)
         *   version  uint32 LE (usually 0x01010000)
         *   k_table  uint32 LE — offset to key table (UTF-8 names)
         *   d_table  uint32 LE — offset to data table (values)
         *   n_entries uint32 LE
         *   entries[n_entries]: {
         *      uint16 LE key_off (into k_table)
         *      uint8  align
         *      uint8  fmt    (0x04=utf8, 0x02=utf8-sz, 0x04=int32)
         *      uint32 LE used_size
         *      uint32 LE total_size
         *      uint32 LE data_off (into d_table)
         *   }
         * We read the whole file (cap 64 KiB) into memory and parse
         * in-place. */
        if (argc < 2) {
            *out_text = strdup_safe("sfoinfo: missing PATH\n");
            *out_exit = 1;
            return 0;
        }
        int fd = open(argv[1], O_RDONLY);
        if (fd < 0) {
            len = shell_appendf(&out, &cap, len,
                                 "sfoinfo: %s: %s\n", argv[1], strerror(errno));
            *out_text = out;
            *out_exit = 1;
            return 0;
        }
        unsigned char *sfo = malloc(64 * 1024);
        if (!sfo) {
            close(fd);
            *out_text = strdup_safe("sfoinfo: oom\n");
            *out_exit = 1;
            return 0;
        }
        ssize_t r = read(fd, sfo, 64 * 1024);
        close(fd);
        if (r < 20 || sfo[0] != 0x00 || sfo[1] != 'P' || sfo[2] != 'S' ||
            sfo[3] != 'F') {
            free(sfo);
            len = shell_appendf(&out, &cap, len,
                                 "sfoinfo: %s: not a SFO file\n", argv[1]);
            *out_text = out;
            *out_exit = 1;
            return 0;
        }
        /* little-endian uint32 read */
        #define LE32(off) ((uint32_t)sfo[off] | ((uint32_t)sfo[off+1] << 8) |    \
                          ((uint32_t)sfo[off+2] << 16) | ((uint32_t)sfo[off+3] << 24))
        uint32_t k_table = LE32(8);
        uint32_t d_table = LE32(12);
        uint32_t n = LE32(16);
        if (n > 256 || k_table >= (uint32_t)r || d_table >= (uint32_t)r) {
            free(sfo);
            len = shell_appendf(&out, &cap, len,
                                 "sfoinfo: %s: corrupt SFO header\n", argv[1]);
            *out_text = out;
            *out_exit = 1;
            return 0;
        }
        uint32_t r32 = (uint32_t)r;
        for (uint32_t i = 0; i < n; i++) {
            uint32_t e_off = 20 + i * 16;
            if (e_off + 16 > r32) break;
            uint16_t k_off = (uint16_t)(sfo[e_off] | (sfo[e_off+1] << 8));
            uint8_t fmt    = sfo[e_off + 3];
            uint32_t used  = LE32(e_off + 4);
            uint32_t d_off = LE32(e_off + 12);
            /* Bounds checks — attacker-controlled offsets from the
             * file, so do all arithmetic as overflow-safe
             * subtractions: `a + b > r` becomes `b > r - a` after
             * verifying a <= r. CRIT audit caught this on 2.13.0. */
            if (k_off > r32 || k_table > r32 - k_off) break;
            if (d_table > r32) break;
            if (d_off > r32 - d_table) break;
            if (used > r32 - d_table - d_off) break;
            const char *key = (const char *)(sfo + k_table + k_off);
            const unsigned char *data = sfo + d_table + d_off;
            /* `key` is read as a NUL-terminated string by `%s` —
             * verify a NUL exists before end-of-buffer. memchr
             * walks at most r-(k_table+k_off) bytes and bails. */
            uint32_t key_max = r32 - k_table - k_off;
            if (!memchr(key, '\0', key_max)) break;
            len = shell_appendf(&out, &cap, len, "%-24s = ", key);
            if (fmt == 0x04 && used == 4) {
                /* int32 */
                uint32_t v = (uint32_t)data[0] | ((uint32_t)data[1] << 8) |
                              ((uint32_t)data[2] << 16) | ((uint32_t)data[3] << 24);
                len = shell_appendf(&out, &cap, len, "%u (0x%08x)\n", v, v);
            } else {
                /* utf-8 string — used may include trailing NUL */
                size_t print_len = used;
                if (print_len > 0 && data[print_len - 1] == 0) print_len--;
                /* Be defensive about non-printables. */
                int ok = 1;
                for (size_t k = 0; k < print_len; k++) {
                    if (data[k] < 0x09 ||
                        (data[k] > 0x0d && data[k] < 0x20)) {
                        ok = 0;
                        break;
                    }
                }
                if (ok) {
                    len = shell_appendf(&out, &cap, len, "\"%.*s\"\n",
                                         (int)print_len, data);
                } else {
                    len = shell_appendf(&out, &cap, len, "(binary, %u bytes)\n",
                                         used);
                }
            }
        }
        #undef LE32
        free(sfo);
        *out_text = out;
        return 0;
    }
    /* Unknown command — let the caller decide whether to fall through
     * to popen or surface a "not supported" error. */
    (void)len;
    if (out) free(out);
    return -1;
}


/* Run a builtin with the session's working directory applied.
 *
 * chdir() is process-global, so this has to serialise against every
 * other shell session — the mutex and the save/restore live here rather
 * than in the caller, which previously reached across the module
 * boundary for the lock. Returns 0 when the command was recognised. */
int shell_run_in_cwd(const char *cwd, const char *cmd, char **out_text,
                     int *out_exit) {
    if (!cmd || !out_text || !out_exit) return -1;
    *out_text = NULL;
    *out_exit = -1;

    int rc = -1;
    pthread_mutex_lock(&g_shell_cwd_mtx);
    char saved_cwd[1024];
    int have_saved_cwd = getcwd(saved_cwd, sizeof(saved_cwd)) != NULL;
    if (cwd && cwd[0] && chdir(cwd) != 0) {
        size_t cap = 0, len = 0;
        *out_exit = 1;
        len = shell_appendf(out_text, &cap, len, "shell: cwd %s: %s\n", cwd,
                            strerror(errno));
        (void)len;
        rc = 0;
    } else {
        rc = handle_shell_builtin(cmd, out_text, out_exit);
    }
    if (have_saved_cwd) {
        (void)chdir(saved_cwd);
    } else {
        (void)chdir("/");
    }
    pthread_mutex_unlock(&g_shell_cwd_mtx);
    return rc;
}
