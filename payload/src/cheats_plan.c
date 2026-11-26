/* cheats_plan.c — see cheats_plan.h. Pure C, no console calls. */

#include "cheats_plan.h"

#include <errno.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <strings.h>

static int hex_val(char c) {
    if (c >= '0' && c <= '9') return c - '0';
    if (c >= 'a' && c <= 'f') return c - 'a' + 10;
    if (c >= 'A' && c <= 'F') return c - 'A' + 10;
    return -1;
}

static int is_sep(char c) {
    return c == ' ' || c == ',' || c == '-' || c == ':' || c == '\t' || c == '\n' || c == '\r';
}

int cheat_parse_hex(const char *s, uint8_t *out, int max) {
    if (!s || !out || max <= 0) return -1;
    int n = 0;
    int hi = -1;
    for (const char *p = s; *p; p++) {
        if (is_sep(*p)) continue;
        int v = hex_val(*p);
        if (v < 0) return -1;
        if (hi < 0) {
            hi = v;
        } else {
            if (n >= max) return -1;
            out[n++] = (uint8_t)(hi * 16 + v);
            hi = -1;
        }
    }
    if (hi >= 0) {
        if (n >= max) return -1;
        out[n++] = (uint8_t)hi;
    }
    return n > 0 ? n : -1;
}

int cheat_parse_offset(const char *s, uint64_t *out) {
    if (!s || !out) return -1;
    while (*s == ' ' || *s == '\t') s++;
    if (s[0] == '0' && (s[1] == 'x' || s[1] == 'X')) s += 2;
    if (!*s) return -1;
    const char *p = s;
    while (hex_val(*p) >= 0) p++;
    while (*p == ' ' || *p == '\t' || *p == '\r' || *p == '\n') p++;
    if (*p) return -1;
    errno = 0;
    unsigned long long v = strtoull(s, NULL, 16);
    if (errno != 0) return -1;
    *out = (uint64_t)v;
    return 0;
}

int cheat_loc_state(const cheat_loc_t *l) {
    size_t n = (size_t)l->len;
    if (memcmp(l->cur, l->on, n) == 0) return CHEAT_LOC_ON;
    if (memcmp(l->cur, l->off, n) == 0) return CHEAT_LOC_OFF;
    return CHEAT_LOC_MISMATCH;
}

int cheat_loc_is_cave(const cheat_loc_t *l) {
    if (l->len <= 0) return 0;
    uint8_t first = l->off[0];
    if (first != 0xCC && first != 0x00) return 0;
    for (int i = 1; i < l->len; i++)
        if (l->off[i] != first) return 0;
    return 1;
}

void cheat_plan_order(const cheat_loc_t *locs, int n, int turn_on, int *order) {
    int k = 0;
    for (int i = 0; i < n; i++)
        if (cheat_loc_is_cave(&locs[i])) order[k++] = i;
    for (int i = 0; i < n; i++)
        if (!cheat_loc_is_cave(&locs[i])) order[k++] = i;
    if (!turn_on) {
        for (int i = 0, j = n - 1; i < j; i++, j--) {
            int t = order[i];
            order[i] = order[j];
            order[j] = t;
        }
    }
}

int cheat_addr_ok(intptr_t addr, int len) {
    if (len <= 0) return 0;
    uint64_t a = (uint64_t)addr;
    uint64_t end = a + (uint64_t)len;
    return a >= 0x10000ULL && end > a && end <= 0x800000000000ULL;
}

int cheat_module_is_game(uint64_t handle, const char *path) {
    if (!path) path = "";
    if (strstr(path, "/app0/") != NULL) return 1;
    if (handle == 0 && !path[0]) return 1;
    const char *leaf = strrchr(path, '/');
    leaf = leaf ? leaf + 1 : path;
    if (strcasecmp(leaf, "eboot.bin") != 0) return 0;
    /* Handle 0 named eboot.bin, as always. Under another handle (a PS4 game's eboot.bin that is
     * not handle 0, #946, from a sandbox path like /mnt/sandbox/pfsmnt/CUSA00284-app0/) only
     * outside the system's folders: a system app's eboot.bin is not the game's. */
    return handle == 0 || (strncmp(path, "/system", 7) != 0 && strncmp(path, "/preinst", 8) != 0);
}

int cheat_patch_base_ok(uint64_t handle, const char *path, char *why, size_t why_cap) {
    if (why && why_cap) why[0] = '\0';
    if (cheat_module_is_game(handle, path)) return 1;
    if (why && why_cap) {
        if (handle == 0)
            snprintf(why, why_cap,
                     "the game's executable is not where this patch looks for it (that module is %s); "
                     "nothing was written",
                     path);
        else
            snprintf(why, why_cap, "%s is not one of the game's own modules; nothing was written", path);
    }
    return 0;
}

uint64_t cheat_exe_base_pick(int ps4, const cheat_exe_seen_t *h0, const cheat_exe_seen_t *eboot,
                             int *how) {
    int found = h0 && h0->found;
    uint64_t b = found ? h0->base : 0;
    int usable = found && cheat_addr_ok((intptr_t)b, 1);
    if (ps4 && usable && cheat_module_is_game(0, h0->path)) {
        if (how) *how = CHEAT_EXE_HANDLE0;
        return b;
    }
    if (ps4 && eboot && eboot->found && cheat_addr_ok((intptr_t)eboot->base, 1) &&
        cheat_module_is_game(1 /* not handle 0 */, eboot->path)) {
        if (how) *how = CHEAT_EXE_NAMED;
        return eboot->base;
    }
    if (ps4) {
        if (how) *how = CHEAT_EXE_PS4_DEFAULT;
        return CHEAT_PS4_EXE_BASE;
    }
    if (how) *how = usable ? CHEAT_EXE_HANDLE0 : CHEAT_EXE_NONE;
    return usable ? b : 0;
}

void cheat_exe_describe(const cheat_exe_seen_t *h0, char *out, size_t cap) {
    if (!out || !cap) return;
    if (!h0 || !h0->found) {
        snprintf(out, cap, "not in the module list");
        return;
    }
    const char *leaf = strrchr(h0->path, '/');
    leaf = leaf ? leaf + 1 : h0->path;
    snprintf(out, cap, "%s at 0x%llx, 0x%llx bytes", leaf[0] ? leaf : "(no path)",
             (unsigned long long)h0->base, (unsigned long long)h0->size);
}

static int overlaps(intptr_t a, int alen, intptr_t b, int blen) {
    return a < b + (intptr_t)blen && b < a + (intptr_t)alen;
}

int cheat_plan_apply(cheat_loc_t *locs, int n, int turn_on,
                     const cheat_range_t *keep, int n_keep,
                     const cheat_mem_ops_t *ops, char *err, size_t err_cap) {
    if (err && err_cap) err[0] = '\0';
    if (n <= 0 || n > CHEAT_PLAN_MAX_APPLY) {
        if (err) snprintf(err, err_cap, "cheat has no usable locations");
        return -1;
    }

    for (int i = 0; i < n; i++) {
        cheat_loc_t *l = &locs[i];
        l->keep = 0;
        if (!turn_on && cheat_loc_is_cave(l)) {
            for (int k = 0; k < n_keep; k++)
                if (overlaps(l->addr, l->len, keep[k].addr, keep[k].len)) { l->keep = 1; break; }
        }
        if (l->keep) continue;
        if (cheat_loc_state(l) == CHEAT_LOC_MISMATCH) {
            if (err) snprintf(err, err_cap,
                              "cheat does not match this game build "
                              "(location %d at 0x%lx holds other bytes); nothing was written",
                              i + 1, (unsigned long)l->addr);
            return -1;
        }
    }

    int order_s[CHEAT_PLAN_MAX_LOCS], done_s[CHEAT_PLAN_MAX_LOCS];
    int *order = order_s, *done = done_s;
    if (n > CHEAT_PLAN_MAX_LOCS) {
        order = (int *)malloc((size_t)n * sizeof(int));
        done = (int *)malloc((size_t)n * sizeof(int));
        if (!order || !done) {
            free(order);
            free(done);
            if (err) snprintf(err, err_cap, "out of memory; nothing was written");
            return -1;
        }
    }
    int n_done = 0;
    cheat_plan_order(locs, n, turn_on, order);

    int failed = -1, why = 0;
    for (int k = 0; k < n; k++) {
        cheat_loc_t *l = &locs[order[k]];
        if (l->keep) continue;
        const uint8_t *want = turn_on ? l->on : l->off;
        if (memcmp(l->cur, want, (size_t)l->len) == 0) continue; /* already there */
        if (!ops->alive(ops->ctx)) { why = -100; failed = -2; break; }
        int wrc = ops->write(ops->ctx, l->addr, want, (size_t)l->len);
        if (wrc != 0) { failed = order[k]; why = wrc; break; }
        done[n_done++] = order[k];
    }
    if (failed == -1 || failed == -2) {
        if (order != order_s) {
            free(order);
            free(done);
        }
        /* -2: the game exited mid-apply, nothing left to put back. */
        if (failed == -2 && err) snprintf(err, err_cap, "no game is currently running");
        return failed == -1 ? 0 : -1;
    }

    /* The failing location may be half written (a split write that failed on its second page,
     * or a read-back mismatch): put it back too, then the earlier ones, newest first. */
    int undo_fail = 0;
    if (ops->write(ops->ctx, locs[failed].addr, locs[failed].cur, (size_t)locs[failed].len) != 0)
        undo_fail++;
    for (int k = n_done - 1; k >= 0; k--) {
        cheat_loc_t *l = &locs[done[k]];
        if (ops->write(ops->ctx, l->addr, l->cur, (size_t)l->len) != 0) undo_fail++;
    }
    if (err) {
        if (undo_fail)
            snprintf(err, err_cap,
                     "write failed at 0x%lx (rc=%d); %d of %d location(s) could not be put back "
                     "- restart the game",
                     (unsigned long)locs[failed].addr, why, undo_fail, n_done + 1);
        else
            snprintf(err, err_cap, "write failed at 0x%lx (rc=%d); the game was left as it was",
                     (unsigned long)locs[failed].addr, why);
    }
    if (order != order_s) {
        free(order);
        free(done);
    }
    return -1;
}
