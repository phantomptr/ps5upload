/**
 * Deleting captures from the Captures screen.
 *
 * A screenshot can live in two trees: the full-size file under
 * /user/av_contents/photo and a thumbnail under
 * /user/av_contents/thumbnails/photo (`<name>.jxr` → `<name>.jxr.jxr`). The
 * payload lists the full-size one and falls back to the thumbnail when the
 * original is gone, so deleting only the listed file would leave the shot in
 * the list as its thumbnail. Both go; a missing thumbnail is not a failure.
 */

const PHOTO_ROOT = "/user/av_contents/photo/";
const THUMB_ROOT = "/user/av_contents/thumbnails/photo/";

/** The listed path first, then the thumbnail that would replace it, if any. */
export function captureDeletePaths(path: string): string[] {
  if (path.startsWith(PHOTO_ROOT) && path.toLowerCase().endsWith(".jxr")) {
    return [path, `${THUMB_ROOT}${path.slice(PHOTO_ROOT.length)}.jxr`];
  }
  return [path];
}

export interface CaptureDeleteResult {
  /** Listed paths that are gone now. */
  deleted: string[];
  /** Listed paths that could not be deleted, with the reason. */
  failed: { path: string; error: string }[];
}

/**
 * Delete each listed capture (and its thumbnail), one after another. A failure
 * of one never stops the rest; the caller keeps the failed ones selected.
 */
export async function deleteCaptures(
  paths: readonly string[],
  deleter: (path: string) => Promise<void>,
): Promise<CaptureDeleteResult> {
  const out: CaptureDeleteResult = { deleted: [], failed: [] };
  for (const listed of paths) {
    const [main, ...extra] = captureDeletePaths(listed);
    try {
      await deleter(main);
    } catch (e) {
      out.failed.push({ path: listed, error: e instanceof Error ? e.message : String(e) });
      continue;
    }
    for (const p of extra) {
      // Best effort: the thumbnail may never have been written.
      await deleter(p).catch(() => undefined);
    }
    out.deleted.push(listed);
  }
  return out;
}
