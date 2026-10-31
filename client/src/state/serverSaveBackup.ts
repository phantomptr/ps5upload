import { create } from "zustand";

import {
  serverBackupApi,
  type ServerBackupSettings,
  type ServerBackupStatus,
} from "../api/saveBackup";

/**
 * Save backups to a network share (#592): the engine backs up saves to a folder on a saved
 * server every few hours or when a game is closed, verifies each copy by reading it back, and
 * restores from it (engine save_backup_server.rs). This store mirrors its settings and last
 * result for the Saves screen.
 */

/** A choice where an empty list means "all of them": whether `item` is chosen. */
export function isChosen<T>(chosen: readonly T[], item: T): boolean {
  return chosen.length === 0 || chosen.includes(item);
}

/** Ticks or unticks `item` among `all`, keeping "every one ticked" as the empty list (so a
 *  console or user added later is included too). Never leaves nothing ticked: unticking the
 *  last one keeps it. */
export function toggleChosen<T>(chosen: readonly T[], item: T, on: boolean, all: readonly T[]): T[] {
  const current = chosen.length === 0 ? [...all] : chosen.filter((x) => all.includes(x));
  const next = on ? [...new Set([...current, item])] : current.filter((x) => x !== item);
  if (next.length === 0) return [...chosen];
  return all.every((x) => next.includes(x)) ? [] : next;
}

export const MIN_HOURS = 1;
export const MAX_HOURS = 168;

export function clampHours(n: unknown): number {
  const v = typeof n === "number" && Number.isFinite(n) ? Math.round(n) : 6;
  return Math.max(MIN_HOURS, Math.min(MAX_HOURS, v));
}

interface ServerBackupState {
  settings: ServerBackupSettings | null;
  status: ServerBackupStatus | null;
  nextAt: number | null;
  error: string | null;
  /** A settings change is being checked by the engine (a new folder is test-written). */
  saving: boolean;
  refresh: () => Promise<void>;
  set: (patch: Partial<ServerBackupSettings>) => Promise<boolean>;
  runNow: () => Promise<void>;
}

const message = (e: unknown) => (e instanceof Error ? e.message : String(e));

export const useServerBackupStore = create<ServerBackupState>((set) => ({
  settings: null,
  status: null,
  nextAt: null,
  error: null,
  saving: false,
  refresh: async () => {
    try {
      const v = await serverBackupApi.view();
      set({ settings: v.settings, status: v.status, nextAt: v.next_at, error: null });
    } catch (e) {
      set({ error: message(e) });
    }
  },
  set: async (patch) => {
    set({ saving: true });
    try {
      const v = await serverBackupApi.set(patch);
      set({ settings: v.settings, status: v.status, nextAt: v.next_at, error: null, saving: false });
      return true;
    } catch (e) {
      set({ error: message(e), saving: false });
      return false;
    }
  },
  runNow: async () => {
    try {
      const v = await serverBackupApi.runNow();
      set({ settings: v.settings, status: { ...v.status, running: true }, nextAt: v.next_at, error: null });
    } catch (e) {
      set({ error: message(e) });
    }
  },
}));
