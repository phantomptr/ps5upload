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
): { destRoot: string; dest: string } {
  const vol = volume ?? "/data";
  const sub = subpath.replace(/^\/+|\/+$/g, "");
  const destRoot = sub ? `${vol}/${sub}` : vol;
  const name = basename(sourcePath);
  const dest = name ? `${destRoot}/${name}` : destRoot;
  return { destRoot, dest };
}
