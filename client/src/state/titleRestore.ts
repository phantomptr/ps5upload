import { create } from "zustand";

import { restoreStart, restoreStatus, type RestoreRun } from "../api/titleRestore";
import { hostOf } from "../lib/addr";
import { restoreEnded, restoreErrorText, restoreOutcome } from "../lib/titleRestore";
import { pushNotification } from "./notifications";
import { withConsolePrefix } from "./roster";

/**
 * Restores to the home screen (#961) this app started, per console and title, so the game page and
 * the Games list show the same progress, and the result is told once even after the page that
 * started it was closed. In memory only: the engine keeps the run, and the console keeps an
 * unfinished one (the game page's check then says `pending`).
 */

export interface RestoreItem {
  titleId: string;
  name: string;
}

interface Entry extends RestoreItem {
  host: string;
  run: RestoreRun | null;
}

interface TitleRestoreState {
  /** `<host>/<TITLE>` → the latest known run. */
  entries: Record<string, Entry>;
  /** Start restoring `items` on `host` (one install after the other) and follow them. Rejects with
   *  the start's error (nothing was changed on the console then). */
  start: (host: string, items: RestoreItem[]) => Promise<void>;
}

const POLL_MS = 3_000;
/** A restore of a big game is a whole install: follow it for up to a day. */
const FOLLOW_MAX_MS = 24 * 60 * 60 * 1000;

export function restoreKey(host: string, titleId: string): string {
  return `${hostOf(host)}/${titleId.toUpperCase()}`;
}

export const useTitleRestoreStore = create<TitleRestoreState>((set) => {
  const put = (host: string, item: RestoreItem, run: RestoreRun | null) =>
    set((s) => ({
      entries: { ...s.entries, [restoreKey(host, item.titleId)]: { ...item, host, run } },
    }));

  const tell = (host: string, item: RestoreItem, run: RestoreRun) => {
    const o = restoreOutcome(run, item.name);
    pushNotification(o.level, withConsolePrefix(host, item.name), { body: o.body });
  };

  async function follow(host: string, items: RestoreItem[]) {
    const open = new Map(items.map((i) => [i.titleId.toUpperCase(), i]));
    const until = Date.now() + FOLLOW_MAX_MS;
    let misses = 0;
    while (open.size > 0 && Date.now() < until) {
      await new Promise((r) => setTimeout(r, POLL_MS));
      for (const [id, item] of [...open]) {
        try {
          const run = await restoreStatus(host, id);
          misses = 0;
          put(host, item, run);
          if (restoreEnded(run.phase)) {
            open.delete(id);
            tell(host, item, run);
          }
        } catch {
          // The engine restarted or is busy: its runs are gone after a restart, and the game's
          // page then reads where the console stands. Give up after a while.
          if (++misses > 40) return;
        }
      }
    }
  }

  return {
    entries: {},
    start: async (host, items) => {
      if (items.length === 0) return;
      for (const item of items) {
        put(host, item, { title_id: item.titleId, phase: "queued" });
      }
      try {
        const r = await restoreStart(
          host,
          items.map((i) => i.titleId),
        );
        put(host, items[0], { title_id: items[0].titleId, phase: "installing", job: r.job, prepared: r.prepared });
      } catch (e) {
        const raw = e instanceof Error ? e.message : String(e);
        for (const item of items) {
          put(host, item, { title_id: item.titleId, phase: "failed", error: raw });
        }
        const wrapped = new Error(restoreErrorText(raw, items[0].name));
        // The target is ES2020 (no Error cause option); attach it by hand (preserve-caught-error).
        (wrapped as Error & { cause?: unknown }).cause = e;
        throw wrapped;
      }
      void follow(host, items);
    },
  };
});

/** The run this app knows for a title on a console, or null. */
export function useRestoreRun(host: string, titleId: string): RestoreRun | null {
  return useTitleRestoreStore((s) => s.entries[restoreKey(host, titleId)]?.run ?? null);
}

/** Whether a restore this app started is still going on the console. */
export function useRestoreBusy(host: string): boolean {
  return useTitleRestoreStore((s) =>
    Object.values(s.entries).some((e) => hostOf(e.host) === hostOf(host) && e.run && !restoreEnded(e.run.phase)),
  );
}
