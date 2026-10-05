#ifndef PS5UPLOAD_STATE_MIGRATE_H
#define PS5UPLOAD_STATE_MIGRATE_H

/*
 * First start of the AVA1-only payload: the folders the retired transfer protocol kept its
 * transaction journal and its shard spool in are removed. AVA1's own state (<root>/ava), the
 * runtime record (<root>/runtime) and everything else under <root> are left alone.
 *
 * Symlinks are unlinked, never followed, and the walk does not cross onto another device, so a
 * link or a mount inside a retired folder cannot make this delete anything outside it.
 * Idempotent: a folder that is already gone is not an error.
 *
 * Returns 0, or -1 when a retired folder could not be removed (the payload starts anyway: the
 * folders are only wasted space).
 */
int payload_remove_retired_dirs(const char *root);

#endif
