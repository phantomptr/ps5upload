#ifndef PS5UPLOAD_CHEATS_H
#define PS5UPLOAD_CHEATS_H

#include <stddef.h>
#include <stdint.h>

/*
 * Cheat engine — applies byte-stream memory patches to running games.
 *
 * Supports three file formats (JSON, SHN-XML, MC4-encrypted-XML) stored
 * under /data/ps5upload/cheats/{json,shn,mc4}/. Each file is named
 * <TITLEID>_* and contains one or more "mods" with memory entries
 * (offset + on/off hex byte patterns).
 *
 * The engine uses ptrace (PT_ATTACH + PT_IO) to write bytes into the
 * game process, with kernel mprotect to temporarily flip execute-only
 * pages to RWX for the write, then restore.
 *
 * A background watcher thread (3s poll) auto-applies "patches" (always-on
 * cheats from /data/ps5upload/patches/) and re-applies user-toggled
 * cheats when a new game process appears.
 */

/* Initialize the cheat engine: create directories, start watcher thread. */
void cheats_init(void);

/* ── Engine master flag ──────────────────────────────────────────── */

int  cheats_engine_enabled(void);
void cheats_engine_set_enabled(int on);

/* ── Watcher / auto-patch state ──────────────────────────────────── */

int cheats_patches_last_mod_count(void);
int cheats_patches_total_writes(void);

/* ── JSON builders (fill buf, return 0 on success) ───────────────── */

/* List all titles that have cheat files, plus running-game info. */
int cheats_list_titles(char *buf, size_t cap, size_t *written);

/* List all mods for a title (union across all format files). */
int cheats_list_mods(const char *title_id, char *buf, size_t cap,
                     size_t *written);

/* Toggle a mod by flat index. turn_on=1 to enable, 0 to disable.
 * Returns 0 on success, -1 on error (err_out gets a diagnostic). */
int cheats_toggle(const char *title_id, int mod_index, int turn_on,
                  char *err, size_t err_cap);

/* Delete all cheat files for a title. Returns 0 on success. */
int cheats_delete(const char *title_id, char *err, size_t err_cap);

/* Force re-apply of patches + enabled cheats for the running game. */
int cheats_reload(char *err, size_t err_cap);

/* Engine status JSON: {"enabled":bool,"patches_last":N,"patches_total":N,
 *   "game_running":bool,"game_title":"...","game_pid":N} */
int cheats_status_json(char *buf, size_t cap, size_t *written);

#endif /* PS5UPLOAD_CHEATS_H */
