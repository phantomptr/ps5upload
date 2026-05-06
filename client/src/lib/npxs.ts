/**
 * NPXS-prefix system-package detection.
 *
 * NPXS pkgs are Sony's system applications: Store updates, Settings
 * patches, built-in apps. They share the `<TWO-LETTER>0000-NPXS<digits>`
 * content_id shape (UP0000-NPXS21001, EP9000-NPXS40012, etc.).
 *
 * We isolate the detection here because two call sites depend on it:
 *
 *   - `state/installQueue.ts` — NPXS rows take the fire-and-forget
 *     register-accepted-equals-success path; status polling for them
 *     fails because Sony's mgmt service freezes mid-flight, and
 *     pollErrors >= 5 would surface as a misleading "failed" even
 *     though the install completed on-console.
 *
 *   - `lib/humanizeError.ts` — When mgmt-disconnect is observed
 *     mid-install AND the content_id is NPXS, the humanizer routes
 *     to the actual root-cause copy ("system-pkg destabilisation,
 *     use Settings → Debug Settings → Package Installer") instead
 *     of the misleading "send the payload again" hint.
 *
 * The two had to stay in sync. Centralising the regex (a) eliminates
 * drift, (b) makes the rule unit-testable in isolation, and (c) keeps
 * one canonical comment about *why* NPXS pkgs are special.
 */
export const NPXS_CONTENT_ID_RE = /^[A-Z]{2}\d{4}-NPXS\d+/i;

/**
 * `true` if `contentId` looks like an NPXS-prefix system pkg.
 * Empty / undefined / malformed strings return `false`.
 */
export function isNpxsContentId(contentId: string | undefined | null): boolean {
  if (!contentId) return false;
  return NPXS_CONTENT_ID_RE.test(contentId);
}
