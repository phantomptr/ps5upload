import type { PayloadReleaseInfo } from "../api/ps5";

// Helpers for the Payloads version picker. Pure — unit-tested without a
// PS5 or a network round-trip. GitHub/Gitea return releases newest-first;
// these helpers assume that order.

/** Releases that actually carry a downloadable asset for this payload.
 *  A release whose `picked_asset_url` is empty matched no asset for the
 *  catalogue's hint (source-only tag, wrong file naming, draft) and can't
 *  be sent — so the picker hides it rather than offering a dead option. */
export function downloadableReleases(
  releases: PayloadReleaseInfo[],
): PayloadReleaseInfo[] {
  return releases.filter((r) => r.picked_asset_url.length > 0);
}

/** The release to pre-select in the picker: the newest STABLE
 *  (non-pre-release) downloadable release, falling back to the newest
 *  downloadable one when every release is a pre-release. Returns null
 *  when nothing is downloadable. Biasing to stable-by-default is the
 *  point — the latest tag is often a fast-moving, possibly-unstable
 *  pre-release. */
export function defaultRelease(
  releases: PayloadReleaseInfo[],
): PayloadReleaseInfo | null {
  const usable = downloadableReleases(releases);
  if (usable.length === 0) return null;
  return usable.find((r) => !r.prerelease) ?? usable[0];
}

/** True when `tag` is the newest tag overall (index 0 of the raw,
 *  newest-first list) — drives the "latest" chip in the picker. */
export function isLatestTag(
  releases: PayloadReleaseInfo[],
  tag: string,
): boolean {
  return releases.length > 0 && releases[0].tag === tag;
}
