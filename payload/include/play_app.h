/* PS5Upload Play (FAKE99261) on this console: where it is installed, its versions, and the install /
 * update the Minion page runs (webui_api.c /api/play-app).
 *
 * It installs EXACTLY the way the homebrew store app installs it, so the two never fight over the
 * folder (the store's src/install/transaction.cpp, files.cpp, archive.cpp). The engine's twin is
 * engine/crates/ps5upload-engine/src/play_app_install.rs, which holds the same description:
 *
 *   Kept copy   The engine leaves the bundled app at PLAY_APP_KEPT_DIR (sce_sys/param.json written
 *               last: with it the copy is complete) and the release zip's SHA-256 in
 *               PLAY_APP_SHA_FILE (64 hex, no newline).
 *   Target      If PLAY_APP_STORE_RECEIPT is a valid receipt (titleId FAKE99261, "location" a clean
 *               absolute path on a known drive) and <location>/FAKE99261/sce_sys/param.json exists
 *               with titleId FAKE99261, the app lives at <location>/FAKE99261 and is updated there.
 *               Otherwise a new install goes to PLAY_APP_DEFAULT_LOCATION/FAKE99261.
 *   Work folder <drive root of the location>/prosperostore (/data for anything under /data, else
 *               /mnt/ext0, /mnt/ext1, /mnt/usb0..7). Refused (store_busy) while
 *               PLAY_APP_STORE_JOURNAL exists, for any title: the store must recover first.
 *   Staging     <work>/staging/FAKE99261 is removed if present, the kept copy is copied there, and
 *               every folder (the top one too) and file in it is chmod 0777.
 *   Fresh       (target absent) ONE rename(2) of the staging folder to the target (same drive by
 *               construction; the devices are compared first anyway: a rename across devices
 *               panics the PS5's kernel). A target that exists but is not a folder holding
 *               FAKE99261's param.json is refused (unexpected_target).
 *   Update      the store's "Quick update", an overlay: every folder of the staged tree is made in
 *               the target and chmod 0777; every staged file is renamed over its target path one by
 *               one, sce_sys/param.json LAST (a target path that exists and is not a regular file is
 *               refused); nothing is set aside; files the new version does not bring stay; the
 *               staging folder is removed. Refused while <work>/backup/FAKE99261 exists (a clean
 *               update of the store's own was cut short).
 *   Receipt     PLAY_APP_STORE_RECEIPT is written (the folder made when missing, a tmp name then a
 *               rename) as one line plus "\n":
 *               {"schema":1,"titleId":"FAKE99261","location":"<location>","contentVersion":"13.000.000",
 *                "releaseTag":"v13.0.0","sha256":"<64 lowercase hex>","installedAt":"YYYY-MM-DDTHH:MM:SSZ"}
 *               sha256 from PLAY_APP_SHA_FILE, or 64 '0' when it is missing (the store's own "adopt").
 *   Refresh     (update only, best effort) every regular file of <target>/sce_sys is copied into
 *               /user/appmeta/FAKE99261/ and /user/app/FAKE99261/sce_sys/ when those folders exist,
 *               and sce_sys/icon0.png to /user/app/FAKE99261/icon0.png when that file exists.
 * The app is closed first if it runs, and registration (the store never registers: ShadowMount+
 * scans /data/homebrew) is decided by the caller: webui_api.c, which records the outcome in
 * PLAY_APP_INSTALL_JSON (play_app_write_install_json).
 *
 * File work only, no Sony calls. Every path is taken under env->root ("" on the console, a temp
 * folder in payload/tests/play_app_selftest.c). */
#ifndef PS5UPLOAD_PLAY_APP_H
#define PS5UPLOAD_PLAY_APP_H

#include <stddef.h>
#include <stdint.h>
#include <time.h>

#include "config.h"

#define PLAY_APP_KEPT_DIR "/data/ps5upload/play/" PLAY_APP_TITLE_ID
#define PLAY_APP_SHA_FILE "/data/ps5upload/play/" PLAY_APP_TITLE_ID ".sha256"
#define PLAY_APP_INSTALL_JSON "/data/ps5upload/play/install.json"
#define PLAY_APP_STORE_RECEIPTS "/data/prosperostore/receipts"
#define PLAY_APP_STORE_RECEIPT PLAY_APP_STORE_RECEIPTS "/" PLAY_APP_TITLE_ID ".json"
#define PLAY_APP_STORE_JOURNAL "/data/prosperostore/journal.json"
#define PLAY_APP_DEFAULT_LOCATION "/data/homebrew"
#define PLAY_APP_APPMETA_DIR "/user/appmeta/" PLAY_APP_TITLE_ID
#define PLAY_APP_USER_APP_DIR "/user/app/" PLAY_APP_TITLE_ID

#define PLAY_APP_VERSION_MAX 32
#define PLAY_APP_LOCATION_MAX 256
#define PLAY_APP_FILES_MAX 2048 /* files in the app's folder */
#define PLAY_APP_DEPTH_MAX 8
#define PLAY_APP_RECEIPT_MAX 512

typedef void (*play_app_progress_fn)(void *ctx, uint64_t done, uint64_t total);

typedef struct {
    const char *root;  /* prefix for every path: "" on the console */
    time_t now;        /* installedAt */
    play_app_progress_fn progress;
    void *ctx;
} play_app_env_t;

/* contentVersion of <root><dir>/sce_sys/param.json: 1 (out set), 0 no param.json or no such field,
 * -1 unreadable / not JSON / too long. */
int play_app_version(const char *root, const char *dir, char *out, size_t cap);

/* "13.000.000" -> "13.0.0" (each part as a number); anything else as it is. */
void play_app_human_version(const char *cv, char *out, size_t cap);

/* The drive root of an absolute location: "/data" for /data and below, "/mnt/ext0", "/mnt/ext1",
 * "/mnt/usb0".."/mnt/usb7" for those and below. 0, or -1 for any other place. */
int play_app_drive_root(const char *location, char *out, size_t cap);

/* A clean absolute path: starts with '/', no "." or ".." part, no empty part, no trailing '/',
 * no control character or backslash, on a known drive (play_app_drive_root). 1 / 0. */
int play_app_location_ok(const char *location);

/* Where the app lives (see Target above): 1 the receipt names it (location from the receipt),
 * 0 the default location. location is the folder that holds FAKE99261, never FAKE99261 itself. */
int play_app_target(const char *root, char *location, size_t cap);

/* The receipt's one line (with its "\n"). Its length, or -1 when out is too small or an input is
 * unusable (sha256 is used only when it is 64 hex characters; else 64 '0'). */
int play_app_receipt_text(char *out, size_t cap, const char *location, const char *content_version,
                          const char *sha256, time_t now);

/* The other app's receipt names this install: location `location` and contentVersion `installed`.
 * 1 / 0. Informational: our own install writes the same receipt. */
int play_app_store_managed(const char *root, const char *location, const char *installed);

/* PLAY_APP_STORE_JOURNAL exists (a store transaction, any title). 1 / 0. */
int play_app_store_busy(const char *root);

/* The order files are put in place: by path, sce_sys/param.json last (rel are paths relative to
 * the app's folder). */
void play_app_order(char **rel, int n);

/* What an install did. */
typedef struct {
    char location[PLAY_APP_LOCATION_MAX]; /* the folder that holds FAKE99261 */
    char version[PLAY_APP_VERSION_MAX];   /* the contentVersion installed */
    int update;                           /* 1 an update in place, 0 a fresh install */
} play_app_result_t;

/* Install or update from the kept copy, write the receipt, and (update) refresh the console's
 * sce_sys copies. 0, or -1 with *err one of:
 *   store_busy, not_staged (no complete kept copy), store_backup_present, unexpected_target,
 *   staging_failed, cross_device, place_failed, receipt_failed, too_many_files */
int play_app_install(const play_app_env_t *env, play_app_result_t *res, const char **err);

/* PLAY_APP_INSTALL_JSON: {"version","location","registered_by","installed_at"}. 0 / -1. */
int play_app_write_install_json(const char *root, const char *version, const char *location,
                                const char *registered_by, time_t now);

/* Is `needle` (lowercase ASCII) in s[0..n), ignoring ASCII case? 1 / 0. */
int play_app_has_nocase(const char *s, size_t n, const char *needle);

#endif
