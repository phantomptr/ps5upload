// The screen last used with each console (#518): switching consoles goes back to where that
// console was left, as browser tabs do. Kept on this device (not a shared setting): it is about
// this window. Keyed by the console's host, at most a few dozen consoles.

import { hostOf } from "./addr";
import { safeGetItem, safeSetItem } from "./safeStorage";

const KEY = "ps5upload.console_routes.v1";
const MAX = 32;

type Routes = Record<string, { route: string; at: number }>;

function load(): Routes {
  try {
    const v = JSON.parse(safeGetItem(KEY) ?? "{}") as unknown;
    return v && typeof v === "object" ? (v as Routes) : {};
  } catch {
    return {};
  }
}

const keyOf = (host: string) => hostOf(host) || host;

/** Remembers `route` (path and query) as where `host` was left. */
export function rememberRoute(host: string, route: string, now = Date.now()): void {
  if (!host) return;
  const routes = load();
  routes[keyOf(host)] = { route, at: now };
  // Oldest first out, so a long list of consoles that come and go stays small.
  const kept = Object.entries(routes)
    .sort((a, b) => b[1].at - a[1].at)
    .slice(0, MAX);
  safeSetItem(KEY, JSON.stringify(Object.fromEntries(kept)));
}

/** Where `host` was left, or null the first time it is used here. */
export function recallRoute(host: string): string | null {
  if (!host) return null;
  return load()[keyOf(host)]?.route ?? null;
}

/** Whether a switch of console keeps the screen on show instead of going back to where the new
 *  console was left: the Consoles page (Connection's first tab) shows the console picked at the
 *  top, so picking another chip there is how another console's card is opened. */
export function staysOnConsoleSwitch(route: string): boolean {
  const q = route.indexOf("?");
  const path = q < 0 ? route : route.slice(0, q);
  if (path !== "/connection") return false;
  const tab = new URLSearchParams(q < 0 ? "" : route.slice(q + 1)).get("tab");
  return tab !== "devices" && tab !== "health";
}
