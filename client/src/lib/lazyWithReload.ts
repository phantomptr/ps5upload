// Screens are loaded on demand (App.tsx). When a screen's code can't be fetched, the browser
// rejects the import — "Importing a module script failed" — and the route showed the crash
// screen. That happens when the code the page was built against is gone: the web UI after an
// update (the open tab asks for chunk names the new build doesn't have), or a dev server whose
// dependency cache was rebuilt under it. A reload fetches the current build; once, so a real
// outage still shows its error instead of reloading forever.

import { lazy } from "react";

import { flushAppJournal, recordAppEvent } from "./appJournal";
import { safeGetItem, safeSetItem } from "./safeStorage";

const KEY = "ps5upload.chunk-reload-at";
/** Another failure within this long of a reload is shown, not reloaded again. */
const WINDOW_MS = 30_000;

/** The browsers' wordings for a dynamic import whose file could not be loaded. */
export function isChunkLoadError(e: unknown): boolean {
  const msg = e instanceof Error ? e.message : String(e);
  return /Importing a module script failed|Failed to fetch dynamically imported module|error loading dynamically imported module|Unable to preload CSS/i.test(
    msg,
  );
}

interface Deps {
  now: () => number;
  get: (key: string) => string | null;
  set: (key: string, value: string) => void;
  reload: () => void;
  /** Records why the page is about to reload: it is otherwise invisible (#418). */
  note: (message: string) => void | Promise<void>;
}

const realDeps: Deps = {
  now: () => Date.now(),
  get: safeGetItem,
  set: safeSetItem,
  reload: () => window.location.reload(),
  note: async (message) => {
    // The journal survives the reload (IndexedDB / the desktop file): write it out first, but
    // never hold the reload up for more than half a second.
    recordAppEvent({
      cat: "app",
      level: "warn",
      code: "chunk_reload",
      msg: `reloaded: a screen's code could not be loaded (${message})`,
    });
    await Promise.race([flushAppJournal().catch(() => {}), new Promise((r) => setTimeout(r, 500))]);
  },
};

export async function importWithReload<T>(load: () => Promise<T>, deps: Deps = realDeps): Promise<T> {
  try {
    return await load();
  } catch (e) {
    if (!isChunkLoadError(e)) throw e;
    const last = Number(deps.get(KEY) ?? 0);
    if (deps.now() - last < WINDOW_MS) throw e;
    deps.set(KEY, String(deps.now()));
    await deps.note(e instanceof Error ? e.message : String(e));
    deps.reload();
    // The page is going away; never settle into the error screen meanwhile.
    return new Promise<T>(() => {});
  }
}

/** `React.lazy`, reloading the page once when the screen's code can't be fetched. */
export const lazyWithReload: typeof lazy = (load) => lazy(() => importWithReload(load));
