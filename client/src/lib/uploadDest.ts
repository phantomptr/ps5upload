// Pure path resolution for the Upload screen's destination preview.
//
// Extracted so the rule (`final dest always suffixes the source basename,
// for both folders and files`) can be unit-tested without having to
// render the React screen. The rule is how "one subfolder per title"
// ends up on disk — a folder `/Users/.../my-folder` uploaded to
// `/data/homebrew` lands at `/data/homebrew/my-folder`, not spilled
// directly into `/data/homebrew`. Third-party PS5 managers assume
// this layout; merging contents would clobber other directories
// living under the same subpath.

/** Last path component of `p`, tolerant of both POSIX and Windows
 *  separators. `C:\\foo\\bar` → `bar`; `/Users/me/thing/` → `thing`. */
export function basename(p: string): string {
  // Strip trailing separators first so a path with a trailing slash
  // like "/data/foo/" resolves to "foo", not "".
  const trimmed = p.replace(/[\\/]+$/g, "");
  const norm = trimmed.replace(/\\/g, "/");
  const i = norm.lastIndexOf("/");
  return i === -1 ? norm : norm.slice(i + 1);
}

/** Resolve the pair of PS5 paths derived from a user's destination
 *  selection and chosen source. `destRoot` is the parent directory
 *  (volume + subpath); `dest` is the final on-PS5 target and always
 *  includes the source's basename as the trailing segment.
 *
 *  `volume = null` is a "not yet chosen" state — falls back to `/data`
 *  to match the handler's default. `subpath` is normalized by stripping
 *  surrounding slashes so users can paste either `homebrew` or
 *  `/homebrew/` and get the same result. */
export function resolveUploadDest(
  volume: string | null,
  subpath: string,
  sourcePath: string,
  /** When the source is a `.zip` archive, the trailing `.zip` is stripped
   *  from the folder name so the extracted game lands at
   *  `<destRoot>/MyGame`, not `<destRoot>/MyGame.zip`. (The archive is
   *  decompressed host-side; only its contents reach the PS5.) */
  isArchive = false,
  /** For an archive, whether to wrap its contents in a folder named after
   *  the zip (`true`, the default — `<destRoot>/MyGame/...`) or extract
   *  them straight into the destination (`false` — `<destRoot>/...`).
   *  Ignored for non-archive sources, which always suffix their basename.
   *  The "flat" option is for zips that already wrap the game in its own
   *  top-level folder, where the default would double-nest. */
  archiveIntoSubfolder = true,
): { destRoot: string; dest: string } {
  const vol = volume ?? "/data";
  const sub = subpath.replace(/^\/+|\/+$/g, "");
  const destRoot = sub ? `${vol}/${sub}` : vol;
  // Flat-extract: the archive's contents land directly in destRoot with no
  // wrapper folder, so the final target IS the destination root.
  if (isArchive && !archiveIntoSubfolder) {
    return { destRoot, dest: destRoot };
  }
  const raw = basename(sourcePath);
  const name = isArchive ? raw.replace(/\.(zip|7z)$/i, "") : raw;
  const dest = name ? `${destRoot}/${name}` : destRoot;
  return { destRoot, dest };
}
