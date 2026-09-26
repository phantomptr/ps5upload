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
  // Strip the archive extension so `MyGame.rar` extracts to `<root>/MyGame`,
  // matching zip/7z. `.rar` is a first-class archive source (archiveFormat()
  // returns "rar", pickFile sets kind:"archive"), so it must be stripped too —
  // otherwise the contents land in a folder literally named `MyGame.rar`.
  let name = raw;
  if (isArchive) {
    // Peel the extension, then a trailing `.partN` multi-part marker.
    //
    // The `.partN` peel used to be rar-only, on the reasoning that zip/7z
    // "don't use that scheme". They do: sites routinely publish a game as
    // `Game.part01.zip` … `Game.part06.zip`, where each part is a SEPARATE
    // self-contained archive holding a different slice of the game's files.
    // Keeping the marker gave every part its own destination folder
    // (`…part01/`, `…part02/`), scattering one game across six directories
    // so it could not launch — the user had to notice and retarget each
    // upload by hand. Stripping it lands every part in one folder, which is
    // the only arrangement that works.
    //
    // Safe for a lone archive that merely has `.partN` in its name: the
    // only effect is a folder called `Game` instead of `Game.part1`.
    name = raw
      .replace(/\.(zip|7z|rar)$/i, "")
      .replace(/\.part\d+$/i, "");
  }
  const dest = name ? `${destRoot}/${name}` : destRoot;
  return { destRoot, dest };
}
