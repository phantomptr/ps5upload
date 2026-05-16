// Helpers for naming the on-PS5 staging file used by the legacy
// "stage" install method (upload .pkg → /user/data/ps5upload/pkg_temp/
// → ShellUI-RPC install from that path).
//
// Sonicloader's homebrew.c:436 (canonicalise_pkg_filename) discovered
// empirically that some Sony installer code paths key on the basename
// of the .pkg, preferring `<ContentID>.pkg`. Naming the staging file
// after the ContentID (when we have it) sidesteps a class of "PKG
// installer rejected the file" errors that look like the SDK rejecting
// our payload but are actually a basename mismatch.
//
// When the ContentID is missing or has unsafe characters (corrupt
// header, weird homebrew tooling) we fall back to the legacy
// `<queueId>_<timestamp>.pkg` shape so the install still gets a shot.

/** Maximum length of a PS5 PKG ContentID — fixed by Sony's header
 *  format (offset 0x40, 36 bytes). */
const CONTENT_ID_MAX_LEN = 36;

/** ContentID alphabet. Real ContentIDs look like
 *  `IV9999-PSPS69691_00-SONICLOADER00001` — alphanumeric plus dash
 *  and underscore. Period is included defensively so non-Sony homebrew
 *  PKGs with looser headers still pass the safety gate. Slash,
 *  backslash, NUL etc. are excluded so a tampered header can't write
 *  outside the staging dir. */
const CONTENT_ID_SAFE_RE = /^[A-Za-z0-9._-]+$/;

/** True when `contentId` is safe to use as the basename of a staging
 *  file. False for empty strings, oversized strings, anything with
 *  path-traversal characters, or anything containing `..`. */
export function isSafeContentId(contentId: string | null | undefined): boolean {
  if (!contentId) return false;
  if (contentId.length === 0 || contentId.length > CONTENT_ID_MAX_LEN) return false;
  if (contentId.includes("..")) return false;
  return CONTENT_ID_SAFE_RE.test(contentId);
}

/** Derive the basename for the staging file. Returns `<ContentID>.pkg`
 *  when `contentId` is safe; otherwise the legacy
 *  `<queueId>_<nowMs>.pkg` shape. Pure function — `nowMs` is passed
 *  in so tests don't depend on Date.now(). */
export function stagingBasename(
  contentId: string | null | undefined,
  queueId: string,
  nowMs: number,
): string {
  if (isSafeContentId(contentId)) return `${contentId}.pkg`;
  return `${queueId}_${nowMs}.pkg`;
}
