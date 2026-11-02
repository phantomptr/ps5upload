/* cheats_session.c — see cheats_session.h. Pure C, no console calls. */

#include "cheats_session.h"

#include <stdio.h>
#include <string.h>
#include <strings.h>

void cheats_session_reset(cheats_session_t *s) {
    memset(s, 0, sizeof *s);
}

const char *cheats_status_name(int st) {
    switch (st) {
        case CHEATS_ST_WAITING: return "waiting";
        case CHEATS_ST_APPLIED: return "applied";
        case CHEATS_ST_FAILED: return "failed";
        case CHEATS_ST_CHANGED: return "changed";
        default: return NULL;
    }
}

static cheats_entry_t *find(cheats_session_t *s, uint64_t key) {
    for (int i = 0; i < s->n; i++)
        if (s->e[i].item.key == key) return &s->e[i];
    return NULL;
}

static cheats_entry_t *add(cheats_session_t *s, const cheats_item_t *it) {
    if (s->n >= CHEATS_SESSION_MAX_ITEMS) return NULL;
    cheats_entry_t *e = &s->e[s->n++];
    memset(e, 0, sizeof *e);
    e->item = *it;
    e->status = CHEATS_ST_WAITING;
    return e;
}

static void drop(cheats_session_t *s, int i) {
    if (i < s->n - 1) memmove(&s->e[i], &s->e[i + 1], (size_t)(s->n - 1 - i) * sizeof s->e[0]);
    s->n--;
}

static void copy_reason(cheats_entry_t *e, const char *why) {
    snprintf(e->reason, sizeof e->reason, "%s", why ? why : "");
}

static const char *game_label(const cheats_session_t *s) {
    return s->game[0] ? s->game : s->title;
}

static void log_line(cheats_session_t *s, const cheats_entry_t *e, const char *what) {
    if (!s->ops->log) return;
    char line[320];
    snprintf(line, sizeof line, "%s %s: %s", s->title, e->item.name[0] ? e->item.name : "cheat",
             what);
    s->ops->log(s->ops->ctx, line);
}

static void tell(cheats_session_t *s, const char *msg, int warn) {
    if (s->ops->notify) s->ops->notify(s->ops->ctx, msg, warn);
}

static void fail(cheats_session_t *s, cheats_entry_t *e, const char *why) {
    e->status = CHEATS_ST_FAILED;
    copy_reason(e, why && why[0] ? why : "it could not be applied");
    log_line(s, e, e->reason);
    char msg[256];
    snprintf(msg, sizeof msg, "%s: couldn't apply (%s)", e->item.name[0] ? e->item.name : "Cheat",
             e->reason);
    tell(s, msg, 1);
}

static void wait_more(cheats_session_t *s, cheats_entry_t *e, const char *why) {
    const char *r = why && why[0] ? why : "the game is not ready yet";
    /* Logged when the reason changes, not on every pass. */
    if (e->status != CHEATS_ST_WAITING || strcmp(e->reason, r) != 0) {
        copy_reason(e, r);
        log_line(s, e, r);
    }
    e->status = CHEATS_ST_WAITING;
    e->tries++;
    e->skip = e->tries >= CHEATS_SESSION_FAST_TRIES ? CHEATS_SESSION_SLOW_EVERY - 1 : 0;
}

static int applied(cheats_session_t *s, cheats_entry_t *e, int reapply, uint64_t now) {
    int first = 0;
    e->status = CHEATS_ST_APPLIED;
    e->reason[0] = '\0';
    e->tries = 0;
    e->skip = CHEATS_SESSION_VERIFY_EVERY - 1;
    /* Each cheat's first automatic apply starts (or refreshes) a guard window: a cheat that only
     * applies a minute in can crash the game too. */
    if (!reapply && !e->ever_applied && !s->no_guard) {
        s->armed_pass = 1;
        if (s->ops->guard_applied) s->ops->guard_applied(s->ops->ctx, s->title, s->pid, now);
    }
    if (!e->ever_applied) {
        e->ever_applied = 1;
        if (!e->quiet) {
            if (s->announce == 0) s->announce_since = now;
            s->announce++;
            first = 1;
        }
    }
    return first;
}

void cheats_session_before_write(cheats_session_t *s) {
    if (!s || !s->arming || s->marker_on) return;
    /* A window already running has its marker: the apply refreshes the window. */
    if (s->ops && s->ops->guard_watching && s->ops->guard_watching(s->ops->ctx, s->title, s->pid))
        return;
    s->marker_on = 1;
    if (s->ops && s->ops->marker_set) s->ops->marker_set(s->ops->ctx, s->title);
}

static int try_apply(cheats_session_t *s, cheats_entry_t *e, int reapply, uint64_t now) {
    char why[CHEATS_SESSION_REASON_MAX] = "";
    s->arming = !reapply && !e->ever_applied && !s->no_guard;
    int out = s->ops->apply(s->ops->ctx, s, &e->item, reapply, why, sizeof why);
    s->arming = 0;
    switch (out) {
        case CHEATS_OUT_APPLIED:
            if (reapply) log_line(s, e, "the game undid it; applied again");
            return applied(s, e, reapply, now) ? 1 : 0;
        case CHEATS_OUT_WAITING_SCAN:
            /* Not found in the part of the module that could be read: maybe not loaded yet, maybe
             * another build. Waiting costs a scan under the apply lock each time, so not forever. */
            if (e->scan_waits++ == 0) e->scan_since = now;
            if (e->scan_waits > CHEATS_SESSION_SCAN_WAITS ||
                now - e->scan_since >= CHEATS_SESSION_SCAN_WAIT_MS) {
                fail(s, e, CHEATS_SESSION_SCAN_FAILED);
                return 0;
            }
            wait_more(s, e, why);
            return 0;
        case CHEATS_OUT_WAITING:
            wait_more(s, e, why);
            return 0;
        case CHEATS_OUT_GONE:
            return -1; /* the caller drops it */
        default:
            fail(s, e, why);
            return 0;
    }
}

/* A revert at `now`: 1 when it is one too many inside the window. */
static int too_many_reverts(cheats_entry_t *e, uint64_t now) {
    int k = 0;
    for (int i = 0; i < e->n_reverts; i++)
        if (now - e->revert_ms[i] < CHEATS_SESSION_REVERT_WINDOW_MS) e->revert_ms[k++] = e->revert_ms[i];
    e->n_reverts = k;
    if (k >= CHEATS_SESSION_REVERT_MAX) return 1;
    e->revert_ms[e->n_reverts++] = now;
    return 0;
}

void cheats_session_begin(cheats_session_t *s, int pid, const char *title) {
    if (s->pid == pid && strcmp(s->title, title ? title : "") == 0) return;
    cheats_session_reset(s);
    s->pid = pid;
    snprintf(s->title, sizeof s->title, "%s", title ? title : "");
}

int cheats_session_pass(cheats_session_t *s, int pid, const char *title, uint64_t now, int force,
                        const cheats_session_ops_t *ops) {
    if (pid <= 0 || !title || !title[0]) {
        cheats_session_reset(s);
        return 0;
    }
    if (!force && !ops->engine_on(ops->ctx)) {
        cheats_session_reset(s);
        return 0;
    }
    if (force) cheats_session_reset(s);
    cheats_session_begin(s, pid, title);
    /* An explicit re-apply is a choice, like a switch: the guard does not watch it. */
    s->no_guard = force;
    s->ops = ops;

    if (!force && ops->held(ops->ctx, title)) {
        if (!s->held_told) {
            char msg[192];
            snprintf(msg, sizeof msg,
                     "Cheats for %s are paused. Switch one of its cheats in PS5Upload to resume.",
                     title);
            tell(s, msg, 0);
            s->held_told = 1;
        }
        s->n = 0;
        return 0;
    }
    s->held_told = 0;

    cheats_item_t items[CHEATS_SESSION_MAX_ITEMS];
    char game[CHEATS_SESSION_NAME_MAX] = "";
    int ni = ops->list(ops->ctx, title, items, CHEATS_SESSION_MAX_ITEMS, game, sizeof game);
    int listed = ni >= 0;
    if (!listed) ni = 0; /* could not be listed now: keep what we have */
    else {
        if (game[0]) snprintf(s->game, sizeof s->game, "%s", game);
        for (int i = 0; i < s->n; i++) s->e[i].seen = 0;
        for (int i = 0; i < ni; i++) {
            cheats_entry_t *e = find(s, items[i].key);
            if (!e) e = add(s, &items[i]);
            if (!e) continue;
            e->item = items[i]; /* the flat index or name may have moved */
            e->seen = 1;
        }
        for (int i = s->n - 1; i >= 0; i--)
            if (!s->e[i].seen) drop(s, i); /* switched off */
    }
    if (force && listed && ni == 0) {
        char msg[192];
        snprintf(msg, sizeof msg, "No cheats enabled for %s", game_label(s));
        tell(s, msg, 0);
        return 0;
    }

    s->marker_on = 0;
    s->armed_pass = 0;
    int fresh = 0;
    for (int i = 0; i < s->n; i++) {
        cheats_entry_t *e = &s->e[i];
        int r = 0;
        if (e->status == CHEATS_ST_WAITING) {
            if (e->skip > 0) {
                e->skip--;
                continue;
            }
            r = try_apply(s, e, 0, now);
        } else if (e->status == CHEATS_ST_APPLIED) {
            if (e->skip > 0) {
                e->skip--;
                continue;
            }
            e->skip = CHEATS_SESSION_VERIFY_EVERY - 1;
            int v = ops->verify(ops->ctx, &e->item);
            if (v == CHEATS_VERIFY_REVERTED) {
                if (too_many_reverts(e, now)) {
                    fail(s, e, "the game keeps undoing it");
                } else {
                    r = try_apply(s, e, 1, now);
                }
            } else if (v == CHEATS_VERIFY_CHANGED) {
                e->status = CHEATS_ST_CHANGED;
                copy_reason(e, "the game changed these bytes; left alone");
                log_line(s, e, e->reason);
                char msg[256];
                snprintf(msg, sizeof msg, "%s: the game changed it, so PS5Upload left it alone",
                         e->item.name[0] ? e->item.name : "Cheat");
                tell(s, msg, 1);
            }
        }
        if (r < 0) {
            drop(s, i);
            i--;
        } else {
            fresh += r;
        }
    }

    /* The marker was written for an apply that did not land: nothing to watch. */
    if (s->marker_on && !s->armed_pass && ops->marker_clear) ops->marker_clear(ops->ctx);
    s->marker_on = 0;
    s->armed_pass = 0;

    if (s->announce > 0) {
        int waiting = 0;
        for (int i = 0; i < s->n; i++)
            if (s->e[i].status == CHEATS_ST_WAITING) waiting++;
        if (force || !waiting || now - s->announce_since >= CHEATS_SESSION_SUMMARY_WAIT_MS) {
            char msg[256];
            snprintf(msg, sizeof msg, "%d %scheat%s applied to %s", s->announce,
                     s->summary_sent ? "more " : "", s->announce == 1 ? "" : "s", game_label(s));
            tell(s, msg, 0);
            s->summary_sent = 1;
            s->announce = 0;
        }
    }
    return fresh;
}

void cheats_session_manual(cheats_session_t *s, uint64_t key, int on) {
    for (int i = 0; i < s->n; i++) {
        if (s->e[i].item.key != key) continue;
        if (!on) {
            drop(s, i);
            return;
        }
        cheats_entry_t *e = &s->e[i];
        cheats_item_t it = e->item;
        memset(e, 0, sizeof *e);
        e->item = it;
        e->status = CHEATS_ST_APPLIED;
        e->ever_applied = e->quiet = 1;
        e->skip = CHEATS_SESSION_VERIFY_EVERY - 1;
        return;
    }
    /* Not tracked yet: the next pass lists it and finds its bytes already on, quietly. */
    if (on && s->n < CHEATS_SESSION_MAX_ITEMS) {
        cheats_entry_t *e = &s->e[s->n++];
        memset(e, 0, sizeof *e);
        e->item.key = key;
        e->item.flat = -2; /* filled in by the next list */
        e->status = CHEATS_ST_APPLIED;
        e->ever_applied = e->quiet = 1;
        e->skip = CHEATS_SESSION_VERIFY_EVERY - 1;
    }
}

int cheats_session_status(const cheats_session_t *s, int flat, const char **reason) {
    if (reason) *reason = "";
    if (flat < 0) return CHEATS_ST_NONE;
    for (int i = 0; i < s->n; i++) {
        if (s->e[i].item.flat != flat) continue;
        if (reason) *reason = s->e[i].reason;
        return s->e[i].status;
    }
    return CHEATS_ST_NONE;
}

/* ── The saved switches ─────────────────────────────────────────── */

int cheats_state_get(const char *buf, int flat) {
    char key[24];
    if (!buf || flat < 0) return CHEATS_SW_UNSET;
    snprintf(key, sizeof key, "\"%d\"", flat);
    const char *p = strstr(buf, key);
    if (!p) return CHEATS_SW_UNSET;
    p += strlen(key);
    while (*p == ' ' || *p == '\t' || *p == '\n' || *p == '\r' || *p == ':') p++;
    if (strncmp(p, "true", 4) == 0) return CHEATS_SW_ON;
    if (strncmp(p, "false", 5) == 0) return CHEATS_SW_OFF;
    return CHEATS_SW_UNSET;
}

int cheats_state_build(const char *old, int flat, int value, int max_keys, char *out, size_t cap) {
    size_t off = 0;
    int first = 1;
    if (!out || cap < 3) return -1;
    out[off++] = '{';
    for (int i = 0; i < max_keys; i++) {
        int v = i == flat ? value : cheats_state_get(old, i);
        if (v != CHEATS_SW_ON && v != CHEATS_SW_OFF) continue;
        int n = snprintf(out + off, cap - off, "%s\"%d\":%s", first ? "" : ",", i,
                         v == CHEATS_SW_ON ? "true" : "false");
        if (n < 0 || (size_t)n >= cap - off) return -1;
        off += (size_t)n;
        first = 0;
    }
    if (off + 2 > cap) return -1;
    out[off++] = '}';
    out[off] = '\0';
    return (int)off;
}

int cheats_switch_on(int saved, int from_file, int pending_off) {
    if (pending_off) return 0;
    switch (saved) {
        case CHEATS_SW_ON: return 1;
        case CHEATS_SW_OFF: return 0;
        case CHEATS_SW_UNKNOWN: return 1;
        default: return from_file ? 1 : 0;
    }
}

int cheats_effective_on(const char *state, int flat, int from_file, int pending_off) {
    int saved = state ? cheats_state_get(state, flat) : CHEATS_SW_UNKNOWN;
    return cheats_switch_on(saved, from_file, pending_off);
}

static int offs_same(const cheats_offs_t *o, const char *title) {
    return o->n > 0 && strcasecmp(o->title, title ? title : "") == 0;
}

void cheats_offs_add(cheats_offs_t *o, int pid, const char *title, int flat) {
    if (o->n > 0 && (o->pid != pid || !offs_same(o, title))) o->n = 0;
    if (cheats_offs_has(o, title, flat)) return;
    if (o->n == 0) {
        o->pid = pid;
        snprintf(o->title, sizeof o->title, "%s", title ? title : "");
    }
    if (o->n < CHEATS_OFFS_MAX) o->flat[o->n++] = flat;
}

void cheats_offs_del(cheats_offs_t *o, const char *title, int flat) {
    if (!offs_same(o, title)) return;
    for (int i = 0; i < o->n; i++) {
        if (o->flat[i] != flat) continue;
        o->flat[i] = o->flat[--o->n];
        return;
    }
}

int cheats_offs_has(const cheats_offs_t *o, const char *title, int flat) {
    if (!offs_same(o, title)) return 0;
    for (int i = 0; i < o->n; i++)
        if (o->flat[i] == flat) return 1;
    return 0;
}

void cheats_offs_session(cheats_offs_t *o, int pid, const char *title) {
    if (o->n > 0 && (pid <= 0 || o->pid != pid || !offs_same(o, title))) o->n = 0;
}

/* ── Recorded locations ─────────────────────────────────────────── */

int cheats_loc_still_ours(const cheats_module_t *was, const cheats_module_t *now, uint64_t addr,
                          int len) {
    if (!was || !now || len <= 0 || !now->base || !now->size) return 0;
    if (now->base != was->base || now->size != was->size) return 0;
    if (addr < now->base || addr - now->base > now->size) return 0;
    return (uint64_t)len <= now->size - (addr - now->base);
}
