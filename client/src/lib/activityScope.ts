import { hostOf } from "./addr";

/** The activity entries a console's workspace shows: that console's, plus local-only work
 *  (no console). No console selected shows everything. */
export function activityForHost<T extends { addr?: string | null }>(
  entries: T[],
  host: string | null | undefined,
): T[] {
  if (!host?.trim()) return entries;
  const h = hostOf(host);
  return entries.filter((e) => !e.addr || hostOf(e.addr) === h);
}
