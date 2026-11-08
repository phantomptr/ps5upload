/**
 * The page replaces itself when the helper is newer than the page's build, with no button press.
 *
 * Why: after a helper update the PS5 browser kept showing its stored copy of the old page (the
 * appcache serves its copy first), and the "newer helper" banner (UpdateBanner) needs a press that
 * nobody can aim over Remote Play, where the browser shows as black frames.
 *
 * How: drop the service worker and its caches, then load "/?cw_fresh=<helper version>". That URL is
 * in no appcache (an appcache answers only for the URLs it stored), so the helper serves the page,
 * and the service worker is gone, so nothing answers in its place. location.reload() would not do:
 * with an appcache it reloads the stored copy.
 *
 * Loop guard: at most one such load per helper version, remembered in localStorage and, where
 * storage is unavailable, in the URL itself (the fresh load carries the version). After that the
 * banner is the fallback, as before.
 */
import { onTokenExpired, readHelperVersion } from "../api.ts";
import { pollWhileVisible } from "../ui/visiblePoll.ts";
import { markUpdateReady, isNewerHelper, PAGE_VERSION } from "./update.ts";

export const RELOAD_KEY = "ps5upload.console.reloadedFor";
export const RELOAD_PARAM = "cw_fresh";

/** The numeric parts of "11.6.1" / "v11.6.1-beta.2" (prerelease ignored here). */
function parts(v: string): number[] | null {
  const m = /^v?(\d+(?:\.\d+)*)(?:-([0-9A-Za-z.-]+))?$/.exec(v.trim());
  if (!m) return null;
  return m[1].split(".").map((n) => Number(n));
}

/**
 * -1 / 0 / 1 like a sort: a < b, equal, a > b. A prerelease is lower than its release
 * ("11.7.0-beta.1" < "11.7.0"). Null when either is not a version.
 */
export function compareVersions(a: string, b: string): number | null {
  const pa = parts(a);
  const pb = parts(b);
  if (!pa || !pb) return null;
  for (let i = 0; i < Math.max(pa.length, pb.length); i++) {
    const x = pa[i] ?? 0;
    const y = pb[i] ?? 0;
    if (x !== y) return x < y ? -1 : 1;
  }
  const preA = a.includes("-");
  const preB = b.includes("-");
  if (preA !== preB) return preA ? -1 : 1;
  if (preA && preB && a !== b) {
    // Dot-separated identifiers, numbers as numbers: "beta.10" is after "beta.9".
    const ia = a.slice(a.indexOf("-") + 1).split(".");
    const ib = b.slice(b.indexOf("-") + 1).split(".");
    for (let i = 0; i < Math.max(ia.length, ib.length); i++) {
      const x = ia[i];
      const y = ib[i];
      if (x === y) continue;
      if (x === undefined) return -1;
      if (y === undefined) return 1;
      const nx = /^\d+$/.test(x);
      const ny = /^\d+$/.test(y);
      if (nx && ny) return Number(x) < Number(y) ? -1 : 1;
      if (nx !== ny) return nx ? -1 : 1;
      return x < y ? -1 : 1;
    }
  }
  return 0;
}

/** The helper is a newer build than this page (an unknown or unversioned page never is). */
export function helperIsNewer(helper: unknown, page: string = PAGE_VERSION): boolean {
  if (!page || typeof helper !== "string" || !helper) return false;
  return compareVersions(helper, page) === 1;
}

interface StorageLike {
  getItem(key: string): string | null;
  setItem(key: string, value: string): void;
}

/** Whether a fresh load for `version` may still be tried (none tried yet for it). */
export function mayReloadFor(version: string, search: string, storage: StorageLike | null): boolean {
  try {
    if (new URLSearchParams(search).get(RELOAD_PARAM) === version) return false;
  } catch {
    /* no URLSearchParams: storage decides */
  }
  try {
    if (storage && storage.getItem(RELOAD_KEY) === version) return false;
  } catch {
    /* storage refused: the URL decides */
  }
  return true;
}

function remember(version: string, storage: StorageLike | null): void {
  try {
    storage?.setItem(RELOAD_KEY, version);
  } catch {
    /* the URL carries it */
  }
}

/** The URL the fresh load opens. */
export function freshUrl(version: string): string {
  return `/?${RELOAD_PARAM}=${encodeURIComponent(version)}`;
}

/** Gives up on a step that hangs (a browser whose service worker calls never settle). */
function within<T>(p: Promise<T>, ms: number): Promise<T | undefined> {
  return Promise.race([p, new Promise<undefined>((done) => setTimeout(() => done(undefined), ms))]);
}

export const STEP_TIMEOUT_MS = 2000;

interface WinLike {
  navigator: { serviceWorker?: { getRegistrations?: () => Promise<ReadonlyArray<{ unregister(): Promise<boolean> }>> } };
  caches?: { keys(): Promise<string[]>; delete(key: string): Promise<boolean> };
  location: { replace(url: string): void };
}

/**
 * Unregister the service worker, delete its caches ("cw-…"), then open freshUrl(version). Never
 * throws; the load happens even when a step fails.
 */
export async function reloadFresh(version: string, win: WinLike = window as unknown as WinLike): Promise<void> {
  try {
    const regs = await within(win.navigator.serviceWorker?.getRegistrations?.() ?? Promise.resolve([]), STEP_TIMEOUT_MS);
    await within(Promise.all((regs ?? []).map((r) => r.unregister().catch(() => false))), STEP_TIMEOUT_MS);
  } catch {
    /* no service worker */
  }
  try {
    const c = win.caches;
    if (c) {
      const keys = (await within(c.keys(), STEP_TIMEOUT_MS)) ?? [];
      await within(Promise.all(keys.filter((k) => k.startsWith("cw-")).map((k) => c.delete(k).catch(() => false))), STEP_TIMEOUT_MS);
    }
  } catch {
    /* no Cache Storage */
  }
  win.location.replace(freshUrl(version));
}

function storageOf(): StorageLike | null {
  try {
    return window.localStorage;
  } catch {
    return null;
  }
}

/**
 * What the page does with the helper's version: "reload" (newer helper, not tried yet: the page
 * replaces itself), "banner" (a different helper, or the one fresh load already tried: the
 * one-press banner), or "none".
 */
export function onHelperVersion(
  version: unknown,
  opts: {
    page?: string;
    search?: string;
    storage?: StorageLike | null;
    reload?: (version: string) => void;
    banner?: () => void;
  } = {},
): "reload" | "banner" | "none" {
  const page = opts.page ?? PAGE_VERSION;
  const banner = opts.banner ?? markUpdateReady;
  if (helperIsNewer(version, page)) {
    const v = version as string;
    const storage = opts.storage === undefined ? storageOf() : opts.storage;
    const search = opts.search ?? (typeof location === "undefined" ? "" : location.search);
    if (mayReloadFor(v, search, storage)) {
      remember(v, storage);
      (opts.reload ?? ((x: string) => void reloadFresh(x)))(v);
      return "reload";
    }
  }
  if (isNewerHelper(version, page)) {
    banner();
    return "banner";
  }
  return "none";
}

/** While the page is shown, the helper's version is read again this often (one small GET). */
export const VERSION_CHECK_MS = 60_000;

interface WatchDeps {
  read?: () => Promise<string | null>;
  decide?: (version: string) => "reload" | "banner" | "none";
  poll?: (fn: () => unknown, ms: number) => () => void;
  onExpired?: (fn: () => void) => () => void;
  target?: Pick<Window, "addEventListener" | "removeEventListener">;
}

/**
 * Keeps checking after the first look: when the page shows again (the PS5 browser brought back to
 * the front, a page restored from the back-forward cache), when the helper restarted under it (a
 * 401: a takeover, maybe a newer helper), and every VERSION_CHECK_MS while shown. Only ever
 * reloads; the banner is the first look's job (a dismissed one stays dismissed). Returns stop.
 */
export function watchHelperVersion(deps: WatchDeps = {}): () => void {
  const read = deps.read ?? readHelperVersion;
  const decide = deps.decide ?? ((v: string) => onHelperVersion(v, { banner: () => undefined }));
  let done = false;
  const check = async () => {
    if (done) return;
    const v = await read();
    if (!done && v && decide(v) === "reload") done = true;
  };
  let first = true;
  const stopPoll = (deps.poll ?? pollWhileVisible)(() => {
    if (first) {
      first = false; // the first look ran already
      return;
    }
    return check();
  }, VERSION_CHECK_MS);
  const stopExpired = (deps.onExpired ?? onTokenExpired)(() => void check());
  const target = deps.target ?? (typeof window === "undefined" ? undefined : window);
  const onShow = (e: Event) => {
    if ((e as PageTransitionEvent).persisted) void check();
  };
  target?.addEventListener("pageshow", onShow);
  return () => {
    done = true;
    stopPoll();
    stopExpired();
    target?.removeEventListener("pageshow", onShow);
  };
}
