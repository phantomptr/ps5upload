#ifndef PS5UPLOAD2_PROFILE_H
#define PS5UPLOAD2_PROFILE_H

#include <stdint.h>
#include <stddef.h>

/*
 * ps5upload — PS5 profile operations: avatar image change and offline
 * account (offact) username change.
 *
 * The offline-account registry plumbing — the KEY_* slot layout, the
 * DEFAULT_FLAGS value, the FNV-1a id derivation, and the activate/clear
 * semantics — is vendored from ps5-payload-dev/offact (GPL-3.0), via the
 * Elf Arsenal project. ps5upload is itself GPL-3.0, so this reuse is
 * license-compatible; the attribution is preserved. The original ships an
 * SDL2 fullscreen UI; we keep only the sceRegMgr side-effects and drive
 * them from the desktop UI over the FTX2 protocol.
 *
 * Avatar apply is host-driven: the desktop decodes/resizes/DXT5-encodes
 * the image and stages the finished DDS + online.json files under
 * PROFILE_STAGE_ROOT/0x<UID>/ using the existing FS_WRITE_BYTES path. The
 * payload's only job is the privileged copy of that staging directory into
 * the live profile cache dir, which lives outside the normal writable
 * roots. See profile_apply_avatar().
 *
 * Privilege: same envelope as sys_registry.c / sys_time.c — the registry
 * writes and the /system_data write need the ucred elevation kstuff
 * installs. Without it the calls fail with a Sony err_code (surfaced to
 * the desktop), they don't crash the payload.
 */

#define PROFILE_SLOT_COUNT     16
#define PROFILE_NAME_MAX       32
#define PROFILE_TYPE_MAX       17
#define PROFILE_DEFAULT_FLAGS  0x1002

/* Staging root the host uploads generated avatar files to (a normal
 * writable root, reachable by the existing FS_WRITE_BYTES handler). */
#define PROFILE_STAGE_ROOT     "/data/ps5upload/profile"

/* ── Offline-account slot ops (offact) ──────────────────────────────────
 * `slot` is 1..PROFILE_SLOT_COUNT. All return 0 on success, -1 on failure.
 * On failure *out_err_code (when the variant has one) carries Sony's raw
 * rc or a SYS_REGISTRY_ERR_* sentinel. */
int profile_slot_get_name(int slot, char out[PROFILE_NAME_MAX], uint32_t *out_err_code);
int profile_slot_set_name(int slot, const char *name, uint32_t *out_err_code);
int profile_slot_get_id(int slot, uint64_t *out, uint32_t *out_err_code);
int profile_slot_set_id(int slot, uint64_t id, uint32_t *out_err_code);
int profile_slot_get_type(int slot, char out[PROFILE_TYPE_MAX], uint32_t *out_err_code);
int profile_slot_set_type(int slot, const char *type, uint32_t *out_err_code);
int profile_slot_get_flags(int slot, int *out, uint32_t *out_err_code);
int profile_slot_set_flags(int slot, int flags, uint32_t *out_err_code);

/* FNV-1a-style id derivation from the slot name. Verbatim from offact
 * (including the upstream `0x5EAF00D / 0xCA7F00D` seed, which integer-
 * divides to 0 — kept so derived ids match the reference exactly). */
uint64_t profile_gen_id(const char *name);

/* Activate a slot: derive an id from its name if `id` is 0, then set
 * id + type ("np") + DEFAULT_FLAGS. Fails if the slot has no name. */
int profile_slot_activate(int slot, uint64_t id);

/* Zero out a slot's id + flags (leaves name + type so it's still
 * recognisable but no longer "activated"). */
int profile_slot_clear(int slot);

/* ── Foreground user ────────────────────────────────────────────────────
 * Resolve the active (foreground) user id. Returns the id, or 0 on
 * failure. If name_out is non-NULL it is filled with the current display
 * name (always NUL-terminated; empty string if the name can't be read). */
uint32_t profile_foreground_user(char *name_out, size_t name_out_size);

/* Look up a user's display name by uid (best-effort; works for non-
 * foreground users on most firmwares). Returns 0 on success and fills
 * name_out (NUL-terminated), -1 on failure (name_out set to ""). */
int profile_user_name(uint32_t uid, char *name_out, size_t name_out_size);

/* Rename a local console user via sceUserServiceSetUserName. Returns 0 on
 * success, -1 on failure (incl. the symbol not being exported on this
 * firmware). This is the *active profile* display name, distinct from the
 * offline-account slots above. */
int profile_set_local_username(uint32_t uid, const char *name);

/* ── Avatar apply ───────────────────────────────────────────────────────
 * Copy every regular file from PROFILE_STAGE_ROOT/0x<UID>/ into
 * /system_data/priv/cache/profile/0x<UID>/ (privileged), creating the
 * target directory tree if needed. Returns 0 on success, -1 on failure.
 * *out_copied (when non-NULL) gets the number of files copied. */
int profile_apply_avatar(uint32_t uid, int *out_copied);

#endif /* PS5UPLOAD2_PROFILE_H */
