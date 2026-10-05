#ifndef PS5UPLOAD_CHEATS_LIST_H
#define PS5UPLOAD_CHEATS_LIST_H

#include <stddef.h>

/* The Cheats screen's title list (final review #8).
 *
 * One pass over the three cheat folders builds a title -> files map (the old code re-scanned every
 * folder per title and parsed every file on every call). Each file's game name and mod count are
 * cached keyed on (path, mtime, size), so a repeat call costs one stat per file. Names are made
 * valid UTF-8 (a Latin-1 or Shift-JIS file used to make the whole reply unparseable) and the reply
 * stops adding titles before it would overflow, ending in a valid document with "truncated":true.
 *
 * Kept free of the ptrace / Sony code in cheats.c so it builds and tests on the host. */

#define CHEATS_LIST_TITLE_MAX 16 /* MAX_TITLE_ID in cheats.c */

typedef struct {
    /* json, shn, mc4 folders (format 1, 2, 3) */
    const char *dirs[3];
    /* sidecar folder holding <TITLE>.json enabled-state files */
    const char *state_dir;
    /* Parses one cheat file: raw game name and mod count. 0 on success. */
    int (*load)(const char *path, int format, char *name, size_t name_cap, int *mod_count);
    /* Whether `name` is worth showing instead of the title id. NULL: any non-empty name. */
    int (*usable_name)(const char *name, const char *title_id);
    /* Running game (empty / 0 when none). */
    const char *running_title;
    int running;
} cheats_list_cfg_t;

/* Writes {"titles":[...],"truncated":bool,"game_running":bool,"game_title_id":"..."} into buf
 * (always valid JSON when cap >= 256). Returns 0, or -1 for a buffer too small to hold even an
 * empty document. */
int cheats_list_build(const cheats_list_cfg_t *cfg, char *buf, size_t cap, size_t *written);

/* Length (1-4) of the valid UTF-8 character at s, or 0 when the bytes there are not valid UTF-8
 * (overlong, surrogate, beyond U+10FFFF, truncated). A NUL returns 0. */
int cheats_utf8_char_len(const char *s);

/* Copies in to out (NUL-terminated, at most cap-1 bytes), replacing each invalid byte with '?' and
 * never ending in a partial character. Returns the length written. */
size_t cheats_utf8_sanitize(const char *in, char *out, size_t cap);

/* Number of cache misses (load() calls) since the last reset; a test hook. */
unsigned long cheats_list_cache_loads(void);
void cheats_list_cache_reset(void);

#endif
