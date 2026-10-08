import { volumeAllocatableBytes, volumeLikelyFitsBytes, type Volume } from "../api/ps5";
import { safeGetItem, safeSetItem } from "./safeStorage";

/** The volume a PS5 path lives on: the longest mount path that contains it. */
export function volumeOf(path: string, volumes: Volume[]): Volume | null {
  let best: Volume | null = null;
  for (const v of volumes) {
    const root = v.path.endsWith("/") ? v.path.slice(0, -1) : v.path;
    if (path === root || path.startsWith(`${root}/`)) {
      if (!best || root.length > best.path.length) best = v;
    }
  }
  return best;
}

export interface MovePlan {
  /** A rename on one drive (instant), or a copy to another drive and then removing the originals. */
  crossDrive: boolean;
  /** Bytes that will be copied (only meaningful across drives). */
  bytes: number;
  /** "no" only when it certainly won't fit; "tight" when the PS5 may hold back more as it writes. */
  fits: "yes" | "tight" | "no";
  /** A destination inside one of the items being moved. */
  intoItself: boolean;
  /** Everything already lives in the destination. */
  alreadyThere: boolean;
}

export function planMove(
  items: { path: string; size: number }[],
  dest: string,
  volumes: Volume[],
): MovePlan {
  const destVol = volumeOf(dest, volumes);
  const crossDrive = items.some((i) => volumeOf(i.path, volumes)?.path !== destVol?.path);
  const bytes = crossDrive ? items.reduce((n, i) => n + (i.size > 0 ? i.size : 0), 0) : 0;
  let fits: MovePlan["fits"] = "yes";
  if (crossDrive && destVol) {
    // Only a certain shortfall refuses (a guessed reserve once blocked uploads that fit);
    // the PS5's write-time reserve only warns.
    if (bytes > volumeAllocatableBytes(destVol)) fits = "no";
    else if (bytes > volumeLikelyFitsBytes(destVol)) fits = "tight";
  }
  const intoItself = items.some((i) => dest === i.path || dest.startsWith(`${i.path}/`));
  const parentOf = (p: string) => p.slice(0, Math.max(1, p.lastIndexOf("/"))) || "/";
  const alreadyThere = items.length > 0 && items.every((i) => parentOf(i.path) === dest);
  return { crossDrive, bytes, fits, intoItself, alreadyThere };
}

/** The usual places games and images live, one per drive. */
export function quickDestinations(volumes: Volume[]): { path: string; volume: Volume }[] {
  return volumes.map((v) => ({
    path: `${v.path.endsWith("/") ? v.path.slice(0, -1) : v.path}/homebrew`,
    volume: v,
  }));
}

const RECENT_MAX = 5;
const recentKey = (host: string) => `ps5upload.moveTo.recent.${host}`;

/** Folders recently moved into on this console, newest first. */
export function recentDestinations(host: string): string[] {
  try {
    const raw = safeGetItem(recentKey(host));
    const list = raw ? (JSON.parse(raw) as unknown) : [];
    return Array.isArray(list) ? list.filter((x): x is string => typeof x === "string") : [];
  } catch {
    return [];
  }
}

export function rememberDestination(host: string, dest: string): void {
  const next = [dest, ...recentDestinations(host).filter((d) => d !== dest)].slice(0, RECENT_MAX);
  try {
    safeSetItem(recentKey(host), JSON.stringify(next));
  } catch {
    // A convenience only.
  }
}
