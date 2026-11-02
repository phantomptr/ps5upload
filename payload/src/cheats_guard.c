/* cheats_guard — see cheats_guard.h. */
#include "cheats_guard.h"

#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <string.h>
#include <unistd.h>

/* Write `text` to `path` through a same-folder temp + rename: a crash mid-write leaves the old
 * file, never half of the new one. */
static int write_whole(const char *path, const char *text) {
    char tmp[512];
    size_t len = strlen(text);
    int n = snprintf(tmp, sizeof(tmp), "%s.tmp", path);
    if (n < 0 || (size_t)n >= sizeof(tmp)) return -1;
    int fd = open(tmp, O_WRONLY | O_CREAT | O_TRUNC, 0666);
    if (fd < 0) return -1;
    ssize_t wr = write(fd, text, len);
    if (close(fd) != 0 || wr != (ssize_t)len) {
        unlink(tmp);
        return -1;
    }
    if (rename(tmp, path) != 0) {
        unlink(tmp);
        return -1;
    }
    return 0;
}

/* The first `cap - 1` bytes of `path`, NUL-terminated. Bytes read, or -1. */
static int read_small(const char *path, char *out, size_t cap) {
    if (cap == 0) return -1;
    out[0] = '\0';
    int fd = open(path, O_RDONLY);
    if (fd < 0) return -1;
    ssize_t rd = read(fd, out, cap - 1);
    close(fd);
    if (rd < 0) return -1;
    out[rd] = '\0';
    return (int)rd;
}

static int path_in(char *out, size_t cap, const char *dir, const char *name, const char *ext) {
    int n = snprintf(out, cap, "%s/%s%s", dir, name, ext);
    return n > 0 && (size_t)n < cap ? 0 : -1;
}

/* A title id as cheats.c accepts it: letters and digits only, so it is safe in a file name. */
static int title_ok(const char *t) {
    size_t n = t ? strlen(t) : 0;
    if (n == 0 || n >= CHEATS_GUARD_TITLE_MAX) return 0;
    for (size_t i = 0; i < n; i++) {
        char c = t[i];
        if (!((c >= 'A' && c <= 'Z') || (c >= 'a' && c <= 'z') || (c >= '0' && c <= '9'))) return 0;
    }
    return 1;
}

int cheats_engine_load(const char *root) {
    char path[512], buf[64];
    if (path_in(path, sizeof(path), root, "engine", ".json") != 0) return 0;
    if (read_small(path, buf, sizeof(buf)) <= 0) return 0;
    const char *p = strstr(buf, "\"enabled\"");
    if (!p) return 0;
    p += 9;
    while (*p == ' ' || *p == '\t' || *p == '\n' || *p == '\r' || *p == ':') p++;
    return strncmp(p, "true", 4) == 0;
}

int cheats_engine_save(const char *root, int on) {
    char path[512];
    if (path_in(path, sizeof(path), root, "engine", ".json") != 0) return -1;
    return write_whole(path, on ? "{\"enabled\":true}\n" : "{\"enabled\":false}\n");
}

int cheats_held(const char *state_dir, const char *title) {
    char path[512];
    if (!title_ok(title) || path_in(path, sizeof(path), state_dir, title, ".held") != 0) return 0;
    return access(path, F_OK) == 0;
}

int cheats_hold(const char *state_dir, const char *title) {
    char path[512];
    if (!title_ok(title) || path_in(path, sizeof(path), state_dir, title, ".held") != 0) return -1;
    return write_whole(path, title);
}

void cheats_unhold(const char *state_dir, const char *title) {
    char path[512];
    if (!title_ok(title) || path_in(path, sizeof(path), state_dir, title, ".held") != 0) return;
    unlink(path);
}

int cheats_marker_set(const char *root, const char *title) {
    char path[512];
    if (!title_ok(title) || path_in(path, sizeof(path), root, "applying", "") != 0) return -1;
    return write_whole(path, title);
}

void cheats_marker_clear(const char *root) {
    char path[512];
    if (path_in(path, sizeof(path), root, "applying", "") == 0) unlink(path);
}

int cheats_marker_take(const char *root, char *out, size_t cap) {
    char path[512], buf[CHEATS_GUARD_TITLE_MAX + 8];
    if (cap) out[0] = '\0';
    if (path_in(path, sizeof(path), root, "applying", "") != 0) return 0;
    int n = read_small(path, buf, sizeof(buf));
    if (n < 0) return 0;
    unlink(path);
    while (n > 0 && (buf[n - 1] == '\n' || buf[n - 1] == '\r' || buf[n - 1] == ' ')) buf[--n] = '\0';
    if (!title_ok(buf) || strlen(buf) >= cap) return 0;
    memcpy(out, buf, strlen(buf) + 1);
    return 1;
}

/* The run that survived its windows is over (closed, or another process): no quick close in it,
 * so its game's run of quick closes is over. */
static void ok_run_ended(cheats_guard_t *g, int pid) {
    if (!g->ok_pid || pid == g->ok_pid) return;
    if (strcmp(g->strike_title, g->ok_title) == 0) g->strikes = 0;
    g->ok_pid = 0;
    g->ok_title[0] = '\0';
}

void cheats_guard_applied(cheats_guard_t *g, const char *title, int pid, uint64_t now_ms) {
    ok_run_ended(g, pid);
    snprintf(g->title, sizeof(g->title), "%s", title ? title : "");
    g->pid = pid;
    g->applied_ms = now_ms;
}

int cheats_guard_tick(cheats_guard_t *g, int pid, uint64_t now_ms) {
    if (!g->title[0]) {
        ok_run_ended(g, pid);
        return CHEATS_GUARD_NONE;
    }
    int inside = now_ms - g->applied_ms < CHEATS_GUARD_WINDOW_MS;
    if (pid == g->pid && inside) return CHEATS_GUARD_NONE; /* still in the window */
    int closed_early = pid != g->pid && inside;
    char title[CHEATS_GUARD_TITLE_MAX];
    memcpy(title, g->title, sizeof(title));
    int was_pid = g->pid;
    g->title[0] = '\0';
    g->pid = 0;
    if (!closed_early) {
        /* It ran the window. Its run of quick closes ends when this run ends without one (a
         * later cheat of the same run may still open a window it closes inside). */
        if (pid == was_pid) {
            g->ok_pid = was_pid;
            memcpy(g->ok_title, title, sizeof(g->ok_title));
        } else if (strcmp(g->strike_title, title) == 0) {
            g->strikes = 0; /* it ran the window and closed after it */
        }
        return CHEATS_GUARD_SURVIVED;
    }
    if (g->ok_pid == was_pid) {
        g->ok_pid = 0;
        g->ok_title[0] = '\0';
    }
    if (strcmp(g->strike_title, title) != 0) {
        memcpy(g->strike_title, title, sizeof(g->strike_title));
        g->strikes = 0;
    }
    if (++g->strikes >= CHEATS_GUARD_STRIKES) {
        g->strikes = 0;
        return CHEATS_GUARD_HOLD;
    }
    return CHEATS_GUARD_STRIKE;
}
