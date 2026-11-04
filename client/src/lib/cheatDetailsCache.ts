// Game details by host + title id, for a few minutes: a version doesn't change while you browse
// Cheats, and each is a round trip to the console, but a patch install does change it. A read that
// failed is not remembered.

import type { AppInfoDetails } from "../api/ps5";

const DETAILS_FOR_MS = 5 * 60 * 1000;
const detailsCache = new Map<string, { details: AppInfoDetails; at: number }>();

export function cachedDetails(key: string, now: number): AppInfoDetails | null {
  const hit = detailsCache.get(key);
  return hit && now - hit.at < DETAILS_FOR_MS ? hit.details : null;
}

export function cacheDetails(key: string, details: AppInfoDetails | null, now: number): void {
  if (details) detailsCache.set(key, { details, at: now });
  else detailsCache.delete(key);
}

if (typeof window !== "undefined") {
  // An install, uninstall or patch changed what a title's version is.
  window.addEventListener("ps5upload:library:invalidate", () => detailsCache.clear());
}
