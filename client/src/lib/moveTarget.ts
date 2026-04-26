// Pure helpers for the Library "Move" modal.
//
// Extracted so the rules (volume detection, default subpath, no-op
// detection) can be unit-tested without rendering the React modal.

/** Pick the writable PS5 volume whose path is a prefix of `entryPath`.
 *  Picks the longest match — `/mnt/ext1/games/X` matches `/mnt/ext1`
 *  even if `/mnt` were a volume. Falls back to the first volume in
 *  `volumes` when nothing matches (e.g. entry lives somewhere weird
 *  the volumes probe didn't surface). Returns null only when the
 *  volumes list itself is empty. */
export function detectSourceVolume(
  entryPath: string,
  volumePaths: string[],
): string | null {
  if (volumePaths.length === 0) return null;
  let best: string | null = null;
  for (const v of volumePaths) {
    const v2 = v.endsWith("/") ? v.slice(0, -1) : v;
    if (entryPath === v2 || entryPath.startsWith(`${v2}/`)) {
      if (best === null || v2.length > best.length) best = v2;
    }
  }
  return best ?? volumePaths[0];
}

/** Return the source's parent directory relative to `volume`. Used as
 *  the move-modal's default destination subpath so the user sees their
 *  current parent folder pre-filled. Volume itself has no leading slash
 *  in the result (the dropdown supplies it). Empty string when the
 *  source lives directly under the volume root. */
export function defaultMoveSubpath(entryPath: string, volume: string): string {
  const v2 = volume.endsWith("/") ? volume.slice(0, -1) : volume;
  if (!entryPath.startsWith(`${v2}/`)) return "";
  const rest = entryPath.slice(v2.length + 1);
  // strip the entry's own basename — we want the parent dir
  const i = rest.lastIndexOf("/");
  return i === -1 ? "" : rest.slice(0, i);
}

/** The trailing basename of `sourcePath` — the default "name" the
 *  Move modal proposes for the destination. Exported so the modal
 *  can prefill its rename input with exactly the same rule the
 *  resolver applies when no override is supplied. */
export function sourceBasename(sourcePath: string): string {
  return sourcePath.replace(/[\\/]+$/, "").split(/[\\/]/).pop() ?? "";
}

/** Compose the final destination path the move will produce.
 *
 *  By default appends the source basename so the entry keeps its
 *  name in the new location (same "one subfolder per title" rule
 *  the upload screen uses). When `customName` is a non-empty string,
 *  it replaces the basename — letting the user rename during the
 *  move (`/data/foo` → `/mnt/ext1/games/Foo Renamed`). The override
 *  is normalised: leading/trailing slashes stripped, embedded `/`
 *  rejected at the modal level so a single-level rename can't
 *  accidentally jump dirs. */
export function resolveMoveDestination(
  volume: string,
  subpath: string,
  sourcePath: string,
  customName?: string,
): string {
  const v = volume.replace(/\/+$/, "");
  const sub = subpath.replace(/^\/+|\/+$/g, "");
  const root = sub ? `${v}/${sub}` : v;
  const overridden = (customName ?? "").trim().replace(/^\/+|\/+$/g, "");
  const name = overridden.length > 0 ? overridden : sourceBasename(sourcePath);
  return name ? `${root}/${name}` : root;
}

/** True when the resolved destination equals the source — moving to
 *  itself is a no-op the modal should refuse. */
export function isMoveNoop(sourcePath: string, destination: string): boolean {
  const norm = (p: string) => p.replace(/\/+$/, "");
  return norm(sourcePath) === norm(destination);
}

/** True when a user-typed name is invalid for use as a single
 *  destination folder/file segment. Surface as a modal warning so
 *  users don't accidentally try to embed a slash and end up with
 *  weird paths or a payload-side rejection. */
export function isInvalidName(name: string): boolean {
  if (name.length === 0) return false; // empty is OK; falls back to source basename
  if (/[\\/]/.test(name)) return true;
  if (name === "." || name === "..") return true;
  return false;
}
