import { safeGetItem, safeSetItem } from "./safeStorage";

/** The screen the user was last on, so the app reopens there.
 *
 *  Restored by the landing redirect ("/"), not by a separate effect: when the
 *  landing moved from What's New to Home, the landing's own redirect started
 *  winning the race against the old restore effect, and "reopen where you
 *  left off" silently stopped working — while the leftover "/whats-new"
 *  trigger hijacked anyone opening What's New by its address. */
const LAST_ROUTE_KEY = "ps5upload.last_route";

/** Never saved, so never reopened: the landing itself, the setup wizard
 *  (re-entered through Settings), and release notes (a place you visit, not
 *  work in). */
const NOT_SAVED = new Set(["/", "/first-run", "/whats-new"]);

export function isSavableRoute(pathname: string): boolean {
  return pathname.startsWith("/") && !pathname.startsWith("//") && !NOT_SAVED.has(pathname);
}

/** The saved route (path plus query, e.g. `/games?tab=files`), or null. */
export function readLastRoute(): string | null {
  const stored = safeGetItem(LAST_ROUTE_KEY);
  if (!stored) return null;
  const pathname = stored.split(/[?#]/)[0];
  return isSavableRoute(pathname) ? stored : null;
}

export function saveLastRoute(pathname: string, search: string): void {
  if (!isSavableRoute(pathname)) return;
  safeSetItem(LAST_ROUTE_KEY, `${pathname}${search}`);
}

/** Where the app opens. A fresh install (no console yet) goes to Connection: nothing works
 *  before a console is set up. A returning user goes back to the screen they were last on, or
 *  Home; but never back to Connection by itself. Connection is where someone goes when a
 *  console looked down, so reopening there on the next start (while the first check still
 *  runs) read as "the app sent me to Connection" with the helper running fine. */
export function landingRoute(hasConsole: boolean, saved: string | null = readLastRoute()): string {
  if (!hasConsole) return "/connection";
  if (!saved || saved.split(/[?#]/)[0] === "/connection") return "/home";
  return saved;
}
