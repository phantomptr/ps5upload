// Before the app deletes a package on its own (Clear finished, delete after install, a space plan),
// it asks whether Kraken Mount uses that file (#894): a package Kraken Mount mounts is the game.

import { smpStatus, type KrakenStatus, type SmpStatus } from "../api/ps5";
import { hostOf, mgmtAddr } from "./addr";
import { krakenKeepsPath, useMounterStore } from "./mounter";

/** A snapshot read this recently is used again (a bulk delete asks once per file). */
const FRESH_MS = 15_000;
const cache = new Map<string, { at: number; kraken: KrakenStatus | undefined }>();

export type KrakenKeeps = (path: string, isDir?: boolean) => boolean;

/** Kraken Mount on `host`, as the engine reports it. `undefined` when that can't be read (no
 *  answer, an engine older than the field): callers then keep everything in its default roots. */
export async function krakenSnapshot(
  host: string,
  read?: (addr: string) => Promise<SmpStatus>,
): Promise<KrakenStatus | undefined> {
  const key = hostOf(host);
  const hit = cache.get(key);
  if (hit && Date.now() - hit.at < FRESH_MS) return hit.kraken;
  let kraken: KrakenStatus | undefined;
  try {
    const s = await (read ?? smpStatus)(mgmtAddr(host));
    kraken = s.kraken;
    useMounterStore.getState().put(host, s);
  } catch {
    kraken = undefined;
  }
  cache.set(key, { at: Date.now(), kraken });
  return kraken;
}

/** Answers "does Kraken Mount use this path on `host`?" for many paths after one read. */
export async function krakenGuard(
  host: string,
  read?: (addr: string) => Promise<SmpStatus>,
): Promise<KrakenKeeps> {
  const kraken = await krakenSnapshot(host, read);
  return (path, isDir = false) => krakenKeepsPath(path, kraken, isDir);
}

/** The paths of `items` Kraken Mount uses, for a question the user is waiting on (Files' delete
 *  confirmation): the last snapshot of the console when one was read, else a read only when a path
 *  could sit in one of its default folders, so a plain delete is never held up. */
export async function krakenKeptPaths(
  host: string,
  items: { path: string; isDir: boolean }[],
  read?: (addr: string) => Promise<SmpStatus>,
): Promise<string[]> {
  const known = useMounterStore.getState().byHost[hostOf(host)];
  let kraken: KrakenStatus | undefined;
  if (known) {
    kraken = known.kraken;
  } else {
    if (!items.some((i) => krakenKeepsPath(i.path, undefined, i.isDir))) return [];
    kraken = await krakenSnapshot(host, read);
  }
  return items.filter((i) => krakenKeepsPath(i.path, kraken, i.isDir)).map((i) => i.path);
}

/** For tests. */
export function resetKrakenGuardCache(): void {
  cache.clear();
}
