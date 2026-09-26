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

/** Sony's older AppInstUtil builds reject otherwise-valid local package paths
 *  once the full path grows past roughly 127 bytes (hardware-observed on FW
 *  9.60 as SCE_APP_INSTALLER_ERROR_PARAM). A 32-hex BLAKE3 prefix still gives
 *  128 bits of collision resistance while keeping the longest canonical
 *  staging path comfortably below that limit. The full 256-bit fingerprint is
 *  retained in metadata and re-sampled from the staged file after a cold
 *  refresh. */
export const PACKAGE_FINGERPRINT_DIR_HEX_LEN = 32;

/** True when `contentId` is safe to use as the basename of a staging
 *  file. False for empty strings, oversized strings, anything with
 *  path-traversal characters, or anything containing `..`. */
export function isSafeContentId(contentId: string | null | undefined): boolean {
  if (!contentId) return false;
  if (contentId.length === 0 || contentId.length > CONTENT_ID_MAX_LEN)
    return false;
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

// A PS5 base game (PARAM.SFO CATEGORY "gd") and its update ("gp") and DLC
// ("ac") all SHARE the same ContentID. Since the staging basename must stay
// `<ContentID>.pkg` (Sony's installer keys on the basename — see the header
// comment), the only way base/update/DLC can coexist in the library without
// overwriting each other is to put them in DIFFERENT directories. The
// basename is unchanged, only the parent dir differs, so the installer's
// basename cross-check still passes.

/** Staging sub-directory (relative to the library root) for a package of the
 *  given PARAM.SFO category. Base + unknown stay at the root (unchanged
 *  behaviour, back-compat with already-staged files); updates and DLC get
 *  their own dir so they don't collide with the base's `<ContentID>.pkg`. */
export function stagingSubdirForCategory(
  category: string | null | undefined,
): string {
  switch (category) {
    case "gp":
      return "updates";
    case "ac":
      return "dlc";
    default:
      return "";
  }
}

/** Directory relative to the package-library root for one exact package.
 *
 * Updates and DLC keep the Sony-required `<ContentID>.pkg` basename, but each
 * exact artifact gets a fingerprint directory. This lets two same-version
 * alternatives (optional fix/backport) coexist instead of silently replacing
 * one another. A missing/malformed fingerprint falls back to the legacy
 * category directory so already-staged and headerless packages remain usable.
 * Base packages intentionally retain the historical root path: one ContentID
 * identifies one base, while variants are an update/DLC concern. */
export function stagingDirectoryForPackage(
  category: string | null | undefined,
  fingerprint: string | null | undefined,
): string {
  const categoryDir = stagingSubdirForCategory(category);
  if (!categoryDir) return "";
  if (!fingerprint || !/^[a-f0-9]{64}$/i.test(fingerprint)) {
    return categoryDir;
  }
  return `${categoryDir}/${fingerprint
    .slice(0, PACKAGE_FINGERPRINT_DIR_HEX_LEN)
    .toLowerCase()}`;
}

/** Recognise both the current bounded 128-bit directory token and the original
 *  256-bit layout written by early multi-variant builds. */
export function fingerprintFromStagingSubdir(
  subdir: string,
): string | undefined {
  const token = subdir.split("/")[1];
  if (!token || !/^(?:[a-f0-9]{32}|[a-f0-9]{64})$/i.test(token)) {
    return undefined;
  }
  return token.toLowerCase();
}

/** Reverse of `stagingSubdirForCategory`: the category a library file's parent
 *  sub-directory implies. Used by refresh to badge files listed from disk
 *  (the filename alone can't distinguish base from update — they share a
 *  ContentID — but the directory can). Root → undefined (base or unknown;
 *  we don't claim "Base" for a file that might just be an oddly-categorised
 *  pkg). */
export function categoryForSubdir(subdir: string): string | undefined {
  // New layouts are `updates/<fingerprint>` / `dlc/<fingerprint>`; legacy
  // layouts contain the pkg directly in the category directory. The first
  // component identifies the category in both forms.
  switch (subdir.split("/")[0]) {
    case "updates":
      return "gp";
    case "dlc":
      return "ac";
    default:
      return undefined;
  }
}

/** Whether a package of this category is an "add-on" that needs its base game
 *  already installed on the console — updates ("gp") and DLC ("ac"). Sony's
 *  installer accepts an add-on with no base (err_code 0) but installs nothing
 *  (hardware-confirmed: the patch lands in /user/patch/<id>/ only when
 *  /user/app/<id>/ exists), so the UI warns before installing one whose base
 *  isn't present. Base ("gd") and unknown categories return false. */
export function isAddonCategory(category: string | null | undefined): boolean {
  return category === "gp" || category === "ac";
}

/** Short human label for a package category, or null when it shouldn't get a
 *  badge. Drives the "Update" / "DLC" chip in the library UI. */
export function pkgCategoryLabel(
  category: string | null | undefined,
): "Base" | "Update" | "DLC" | null {
  switch (category) {
    case "gd":
      return "Base";
    case "gp":
      return "Update";
    case "ac":
      return "DLC";
    default:
      return null;
  }
}
