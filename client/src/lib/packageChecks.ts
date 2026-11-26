// Checks the package viewer shows against the selected console: can it run
// this package, is it already installed, and does it fit. Pure, so the panel
// stays a view and these stay tested.

import { volumeAllocatableBytes, volumeLikelyFitsBytes, type Volume } from "../api/ps5";

export type Verdict = "ok" | "warn" | "bad" | "unknown";

/** "01.002.000" / "1.2" → [1, 2, 0] for comparison. */
function parts(v: string): number[] {
  return v.split(".").map((p) => parseInt(p, 10) || 0);
}

/** Numeric, part-by-part; missing parts count as 0 ("01.002" = "01.002.000"). */
export function versionCompare(a: string, b: string): number {
  const x = parts(a);
  const y = parts(b);
  for (let i = 0; i < Math.max(x.length, y.length); i++) {
    const d = (x[i] ?? 0) - (y[i] ?? 0);
    if (d !== 0) return d;
  }
  return 0;
}

/** Whether the console's firmware meets the package's minimum. */
export function firmwareCheck(minFw: string | null, consoleFw: string | null) {
  const verdict: Verdict =
    !minFw || !consoleFw
      ? "unknown"
      : versionCompare(consoleFw, minFw) >= 0
        ? "ok"
        : "bad";
  return { verdict, minFw, consoleFw };
}

/** This package against the version installed on the console. */
export function installedCheck(pkgVer: string, installedVer: string | null) {
  if (!installedVer) {
    return { verdict: "ok" as Verdict, relation: "not-installed" as const };
  }
  const c = versionCompare(pkgVer, installedVer);
  if (c > 0) return { verdict: "ok" as Verdict, relation: "newer" as const };
  if (c < 0) return { verdict: "warn" as Verdict, relation: "older" as const };
  return { verdict: "warn" as Verdict, relation: "same" as const };
}

/** Whether `size` fits in `roomBytes`; unknown when free space can't be read. With
 *  `likelyBytes` (what is likely to fit, less than the room on internal storage, where the PS5
 *  holds back more as it writes), a package between the two is "may not fit", not "fits". */
export function spaceCheck(size: number, roomBytes: number | null, likelyBytes: number | null = null) {
  if (roomBytes == null) return { verdict: "unknown" as Verdict, shortBy: 0 };
  if (size > roomBytes) return { verdict: "bad" as Verdict, shortBy: size - roomBytes };
  if (likelyBytes != null && size > likelyBytes) return { verdict: "warn" as Verdict, shortBy: 0 };
  return { verdict: "ok" as Verdict, shortBy: 0 };
}

/** Firmware check that only compares like with like: a PS4 package declares a
 *  PS4 firmware, which says nothing about which PS5 firmware runs it. */
export function platformFirmwareCheck(
  platform: string,
  minFw: string | null,
  consoleFw: string | null,
) {
  return firmwareCheck(platform === "ps5" ? minFw : null, consoleFw);
}

export type InstalledRelation =
  | "not-installed"
  | "newer"
  | "older"
  | "same"
  | "installed"
  | "base-missing";

/** The installed check from the engine's preflight answer. No answer, or an
 *  answer the engine couldn't determine, is no verdict — never "not installed". */
export function installedFromPreflight(
  pkgVer: string,
  pre: { state: string; installedVersion: string | null } | null,
  platform?: string,
): { verdict: Verdict; relation: InstalledRelation; installedVer: string | null } | null {
  if (!pre || pre.state === "unknown") return null;
  const ver = pre.installedVersion;
  if (pre.state === "base_missing") {
    // A PS5 update can carry the whole app (a YouTube update installed on a console without
    // YouTube), so the PS5 decides and this is a warning; a PS4 one is known not to apply.
    return {
      verdict: platform === "ps5" ? "warn" : "bad",
      relation: "base-missing",
      installedVer: null,
    };
  }
  if (pre.state === "not_installed") {
    return { verdict: "ok", relation: "not-installed", installedVer: null };
  }
  // Installed, but one side's version is unknown: say installed, don't compare.
  if (!ver || !pkgVer) {
    return { verdict: "warn", relation: "installed", installedVer: ver };
  }
  return { ...installedCheck(pkgVer, ver), installedVer: ver };
}

/** The room on the roomiest single drive: a package lands on one drive, so the sum across
 *  drives would over-promise. `room` is what a copy may take (free less a small working
 *  margin), `likely` what is likely to fit. Not the raw free space: on internal storage that
 *  said "Fits" for a package the PS5 would run out of room for (#944). Null when nothing is
 *  readable. */
export function largestRoom(volumes: readonly Volume[]): { room: number; likely: number } | null {
  const real = volumes.filter((v) => !v.is_placeholder && v.writable !== false);
  if (real.length === 0) return null;
  let best: { room: number; likely: number } | null = null;
  for (const v of real) {
    const room = volumeAllocatableBytes(v);
    if (!best || room > best.room) best = { room, likely: volumeLikelyFitsBytes(v) };
  }
  return best;
}
