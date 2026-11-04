/** A console whose address changed (a new DHCP lease): when the saved address stops answering,
 *  scan the network and find the same paired console somewhere else. The engine proves it is
 *  the same one by its key (`sameConsole`), so the app never offers a different PS5. Only a
 *  console already running our helper can prove it; one that rebooted without it is left to
 *  the Connection screen's Find PS5s. */

import type { DiscoveredHost } from "../api/ps5";
import { hostOf } from "./addr";

/** A down console is looked for at most this often. */
export const LOOK_EVERY_MS = 5 * 60_000;

/** At most this many addresses are asked about per look. */
const MAX_CHECKS = 4;

/** Time to look again? Only while the console is down and not known to be asleep (its address is
 *  fine then, and the scan is a sweep of the network), and not more often than `LOOK_EVERY_MS`. */
export function shouldLook(
  status: "up" | "down" | "unknown",
  lastLookAt: number | undefined,
  now: number,
  power?: string,
): boolean {
  if (status !== "down" || power === "rest") return false;
  return lastLookAt === undefined || now - lastLookAt >= LOOK_EVERY_MS;
}

/** The scan results worth asking about: a PS5 running our helper (only it can prove its key),
 *  at an address that is neither the old one nor another console in the roster. */
export function movedCandidates(
  oldHost: string,
  found: DiscoveredHost[],
  rosterHosts: string[],
): string[] {
  const skip = new Set([hostOf(oldHost), ...rosterHosts.map(hostOf)]);
  const out: string[] = [];
  for (const h of found) {
    if (!h.payload_port_open || skip.has(h.ip) || out.includes(h.ip)) continue;
    out.push(h.ip);
    if (out.length >= MAX_CHECKS) break;
  }
  return out;
}

export interface MovedConsoleDeps {
  discover: () => Promise<DiscoveredHost[]>;
  sameConsole: (oldHost: string, newHost: string) => Promise<"same" | "different" | "unknown">;
}

/** The new address of the console that was at `oldHost`, or null. */
export async function findMovedConsole(
  oldHost: string,
  rosterHosts: string[],
  deps: MovedConsoleDeps,
): Promise<string | null> {
  let found: DiscoveredHost[];
  try {
    found = await deps.discover();
  } catch {
    return null;
  }
  for (const ip of movedCandidates(oldHost, found, rosterHosts)) {
    if ((await deps.sameConsole(oldHost, ip)) === "same") return ip;
  }
  return null;
}
