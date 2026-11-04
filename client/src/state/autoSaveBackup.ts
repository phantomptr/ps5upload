import { create } from "zustand";

import { saveBackupApi, type SaveBackupSettings, type SaveBackupStatus } from "../api/saveBackup";
import { hostOf } from "../lib/addr";
import { safeGetItem, safeRemoveItem } from "../lib/safeStorage";
import { log } from "./logs";
import { useRosterStore } from "./roster";
import { NOOP_RELOAD, registerSettingReload } from "../lib/liveSettings";

/**
 * Automatic save backups: the engine copies saves that changed since their last backup into a
 * folder it can write, a few versions of each kept (engine save_backup.rs). The engine runs them,
 * so they carry on with no window open: around the clock in Docker, while the app runs on a
 * computer. This store mirrors the engine's settings and last result for the Saves screen, and
 * keeps the engine's list of consoles the same as the app's.
 */

export const DEFAULT_KEEP = 5;

/** Where versions before 7.4.4 kept the settings, in this browser or app. */
const LEGACY_KEY = "ps5upload.auto_save_backup";

export function clampKeep(n: unknown): number {
  const v = typeof n === "number" && Number.isFinite(n) ? Math.round(n) : DEFAULT_KEEP;
  return Math.max(1, Math.min(50, v));
}

/** The settings an older version kept here, if it had a folder set. */
export function legacySettings(raw: string | null): Pick<SaveBackupSettings, "enabled" | "dir" | "keep"> | null {
  try {
    const v = JSON.parse(raw ?? "null") as Partial<SaveBackupSettings> | null;
    if (!v || typeof v.dir !== "string" || !v.dir.trim()) return null;
    return { enabled: v.enabled === true, dir: v.dir, keep: clampKeep(v.keep) };
  } catch {
    return null;
  }
}

/** The consoles the engine backs up: every console in the app's list, by bare IP. */
export function backupHosts(profiles: readonly { host: string }[]): string[] {
  return [...new Set(profiles.map((p) => hostOf(p.host)).filter(Boolean))].sort();
}

interface AutoSaveBackupState {
  settings: SaveBackupSettings | null;
  status: SaveBackupStatus | null;
  /** The last error talking to the engine (a refused folder, an engine that is down). */
  error: string | null;
  refresh: () => Promise<void>;
  set: (patch: Partial<Pick<SaveBackupSettings, "enabled" | "dir" | "keep">>) => Promise<void>;
  runNow: () => Promise<void>;
}

export const useAutoSaveBackupStore = create<AutoSaveBackupState>((set) => ({
  settings: null,
  status: null,
  error: null,
  refresh: async () => {
    try {
      const v = await saveBackupApi.view();
      set({ settings: v.settings, status: v.status, error: null });
    } catch (e) {
      set({ error: e instanceof Error ? e.message : String(e) });
    }
  },
  set: async (patch) => {
    try {
      const v = await saveBackupApi.set(patch);
      set({ settings: v.settings, status: v.status, error: null });
    } catch (e) {
      set({ error: e instanceof Error ? e.message : String(e) });
    }
  },
  runNow: async () => {
    try {
      const v = await saveBackupApi.runNow();
      set({ settings: v.settings, status: { ...v.status, running: true }, error: null });
    } catch (e) {
      set({ error: e instanceof Error ? e.message : String(e) });
    }
  },
}));

/** The wait before sending the console list again after a failure, doubling up to the limit. */
const RETRY_FIRST_MS = 5_000;
const RETRY_MAX_MS = 5 * 60_000;

/**
 * Keeps the engine's console list the same as the app's, and hands over the settings an older
 * version kept in this app (once, when the engine has no folder yet). Returns a stop.
 */
export function startSaveBackupSync(): () => void {
  let sent = "";
  let stopped = false;
  let retry: ReturnType<typeof setTimeout> | null = null;
  let failures = 0;
  // What this client has told the engine about: only that can be taken off again. Several clients
  // can share one engine, and each has its own roster.
  let told: string[] = [];
  const push = (profiles: readonly { host: string }[]) => {
    const hosts = backupHosts(profiles);
    const key = hosts.join(",");
    if (key === sent) return;
    sent = key;
    if (retry) clearTimeout(retry);
    retry = null;
    const removed = told.filter((h) => !hosts.includes(h));
    void saveBackupApi.set({ add_hosts: hosts, remove_hosts: removed }).then(
      () => {
        told = hosts;
        failures = 0;
      },
      (e) => {
        // The engine may not be up yet, and the roster may never change again: try again.
        if (sent === key) sent = "";
        if (failures++ === 0) {
          log.warn("saves", "could not give the engine the console list for save backups", e);
        }
        if (stopped || retry) return;
        retry = setTimeout(() => {
          retry = null;
          push(useRosterStore.getState().profiles);
        }, Math.min(RETRY_MAX_MS, RETRY_FIRST_MS * 2 ** Math.min(failures, 8)));
      },
    );
  };
  void (async () => {
    try {
      const legacy = legacySettings(safeGetItem(LEGACY_KEY));
      if (legacy) {
        const v = await saveBackupApi.view();
        if (!v.settings.dir.trim()) await saveBackupApi.set(legacy);
        safeRemoveItem(LEGACY_KEY);
      }
    } catch (e) {
      log.warn("saves", "could not move the automatic backup settings to the engine", e);
    }
    push(useRosterStore.getState().profiles);
  })();
  const unsubscribe = useRosterStore.subscribe((s) => push(s.profiles));
  return () => {
    stopped = true;
    if (retry) clearTimeout(retry);
    unsubscribe();
  };
}

// Only a pre-engine leftover: the engine keeps the live setting (lib/liveSettings).
registerSettingReload([LEGACY_KEY], NOOP_RELOAD);
