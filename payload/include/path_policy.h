/* Symlink-safe path policy (shared by runtime.c's is_path_allowed and the host tests). */
#ifndef PS5UPLOAD_PATH_POLICY_H
#define PS5UPLOAD_PATH_POLICY_H

/* Does `p` resolve to somewhere `lexical_ok` accepts?
 *
 * `lexical_ok` is the pure string rule (the allowed roots, no `..`). It is applied to `p` AND to
 * the canonical form of `p`, so a symlink that leads out of the allowed roots is refused.
 *
 * A path that does not exist yet (a write, a mkdir, a rename's destination) cannot be
 * realpath()ed, and falling back to the lexical rule alone let a NONEXISTENT LEAF UNDER A
 * SYMLINKED PARENT through (`/data/link/new` with `/data/link -> /system_ex`). So the deepest
 * EXISTING ancestor is resolved and the policy is re-run on that canonical ancestor joined with
 * the components that do not exist yet. A final component that is a symlink whose target is
 * missing (a dangling link) is refused outright: creating through it would write wherever it points.
 * Returns 1 allowed, 0 refused. */
int path_resolve_allowed(const char *p, int (*lexical_ok)(const char *));

/* The trust store. `/data/ps5upload/ava` holds this console's AVA1 identity and its list of paired peers;
 * a paired peer that could overwrite, delete or read them could hijack the console's trust. So the
 * directory and everything under it is denied to EVERY path policy that goes through this file
 * (runtime.c's is_path_allowed, so the management handlers and AVA1's fs.* / job ops). The saved
 * network shares' secrets, `/data/ps5upload/smb` (the encrypted share file and its key), are
 * denied the same way by path_in_protected / path_contains_protected: readable, they would give
 * every saved share password away. path_protected_forms covers only the trust store: a walker whose
 * root could reach the SMB directory is an ancestor of the trust store too, so it is refused before
 * it starts.
 *
 * path_in_protected: `p` is the directory or below it, judged on three forms of the path: as written,
 * lexically normalised (`//`, `.`, `..` collapsed), and canonical (symlinks resolved, with the deepest
 * existing ancestor resolved for a path that does not exist yet). Names compare case-insensitively
 * (a case-insensitive filesystem would otherwise open a hole; denying a harmless extra spelling on a
 * case-sensitive one costs nothing).
 * path_contains_protected: `p` IS the directory or one of its ANCESTORS (renaming or deleting
 * /data/ps5upload takes the trust store with it): destructive operations on a source path refuse these too. */
int path_in_protected(const char *p);
/* The rule for every read: never the protected folders, by any of their names, whatever else the
 * caller's own rules (`others`: its allowlist and read-only exceptions) would let through. */
int path_read_allowed(const char *p, int unsafe_read, int (*others)(const char *, int));
int path_contains_protected(const char *p);
/* The one rule for every operation that walks a tree or moves/replaces one (copy, recursive chmod and
 * delete, move, an upload/download/copy root of a data-plane job): refuse a path that IS the trust store,
 * is below it, or is an ANCESTOR of it, because a walk from an ancestor reaches the store and an
 * overwrite of an ancestor replaces it. 1 = refuse. Callers apply it to every path they are handed
 * (source and destination), after their own allowlist. Trade-off, deliberate: a whole-/data or / root
 * is refused; per-entry filtering inside the walkers would be needed to allow it. */
int path_tree_op_refused(const char *p);
/* For a walker that tests many entries of one canonical root (no symlink below it is followed): the
 * trust store's lexical and canonical forms, resolved once (PATH_MAX buffers), then
 * path_under_forms(entry, lex, canon) per entry: no realpath per entry. 1 = the entry is the store or
 * below it (case-insensitive, component boundary). */
void path_protected_forms(char *lex, char *canon);
int path_under_forms(const char *p, const char *lex, const char *canon);
/* Test hook: the protected directory (NULL restores /data/ps5upload/ava). */
void path_policy_set_protected(const char *dir);

#endif
