#ifndef PS5UPLOAD2_HELPER_BUSY_H
#define PS5UPLOAD2_HELPER_BUSY_H

#include <stddef.h>
#include <stdio.h>
#include <string.h>

/*
 * What a restart of this helper would stop (12.6.15). Replacing the helper restarts it, and its
 * start-up stops whatever the previous instance ran; an app sending a helper by itself must never
 * do that over work. STATUS_ACK carries it as "busy" (additive: older apps ignore it), and
 * node.shutdown {"if_idle":true} refuses while helper_busy_cause() names something.
 *
 * Header-only and free of console calls so the host self-test covers it; runtime.c fills the
 * struct from the services themselves.
 */
typedef struct {
    int uploads;       /* AVA1 uploads being sent to this console now, every device's */
    int install;       /* the console page's package install */
    int extract;       /* an archive extraction */
    int convert;       /* a conversion */
    int dump;          /* a game dump */
    int input;         /* remote keyboard, mouse or controller input held open */
    int pads;          /* Play Together virtual controllers open */
    int other_devices; /* devices other than the caller with a live session (not work: an idle
                        * connection comes back by itself, so it does not make the helper busy) */
} helper_busy_t;

/* The first thing a restart would stop, as its "busy" key, or NULL when nothing would be. */
static inline const char *helper_busy_cause(const helper_busy_t *b) {
    if (!b) return NULL;
    if (b->uploads > 0) return "uploads";
    if (b->install) return "install";
    if (b->extract) return "extract";
    if (b->convert) return "convert";
    if (b->dump) return "dump";
    if (b->input) return "input";
    if (b->pads > 0) return "pads";
    return NULL;
}

/* The STATUS_ACK object: {"uploads":N,"install":0|1,...,"other_devices":N,"idle":true|false}.
 * Returns the length written, or -1 when it did not fit. */
static inline int helper_busy_json(const helper_busy_t *b, char *out, size_t cap) {
    int n = snprintf(out, cap,
                     "{\"uploads\":%d,\"install\":%d,\"extract\":%d,\"convert\":%d,\"dump\":%d,"
                     "\"input\":%d,\"pads\":%d,\"other_devices\":%d,\"idle\":%s}",
                     b->uploads, b->install ? 1 : 0, b->extract ? 1 : 0, b->convert ? 1 : 0,
                     b->dump ? 1 : 0, b->input ? 1 : 0, b->pads, b->other_devices,
                     helper_busy_cause(b) ? "false" : "true");
    return (n < 0 || (size_t)n >= cap) ? -1 : n;
}

/* Whether a node.shutdown body asks for an exit only when idle: {"if_idle":true}. Anything else
 * (no body, false, unparsable) is a plain shutdown, as every older caller sends it. */
static inline int helper_shutdown_if_idle(const char *body, size_t len) {
    const char *p, *end;
    if (!body || !len) return 0;
    end = body + len;
    for (p = body; p + 9 <= end; p++) {
        if (memcmp(p, "\"if_idle\"", 9) != 0) continue;
        p += 9;
        while (p < end && (*p == ' ' || *p == '\t' || *p == ':')) p++;
        return (end - p >= 4 && memcmp(p, "true", 4) == 0) ? 1 : 0;
    }
    return 0;
}

#endif
