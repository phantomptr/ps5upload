import { create } from "zustand";

import {
  saveArchiveBackupFinalize,
  saveArchiveCleanupTemp,
  saveArchiveMakeTemp,
  saveArchiveZip,
  saveAutoBackupSlot,
  savesList,
  startTransferDownload,
  waitForJob,
  type SaveEntry,
} from "../api/ps5";
import { consoleAddr, mgmtAddr } from "../lib/addr";
import { backupTimestamp } from "../lib/backupTimestamp";
import { safeGetItem, safeSetItem } from "../lib/safeStorage";
import { log } from "./logs";

/**
 * Automatic save backups: while the app is open and a console is connected, saves that
 * changed since their last backup are copied to a folder on this computer, a few versions
 * of each kept. The same download-clean-zip steps as the Saves screen's Backup button, run
 * on a timer. Off until a folder is chosen and the switch is turned on.
 */

const KEY = "ps5upload.auto_save_backup";
const KEY_DONE = "ps5upload.auto_save_backup.done";

/** A save written to in the last few minutes is probably still being written: wait. */
export const SETTLE_SECS = 180;
export const DEFAULT_KEEP = 5;

export interface AutoSaveBackupSettings {
  enabled: boolean;
  /** A folder on this computer. */
  dir: string;
  /** Versions of each save to keep (1 to 50). */
  keep: number;
}

function loadSettings(): AutoSaveBackupSettings {
  const fallback = { enabled: false, dir: "", keep: DEFAULT_KEEP };
  try {
    const v = JSON.parse(safeGetItem(KEY) ?? "null") as Partial<AutoSaveBackupSettings> | null;
    if (!v) return fallback;
    return {
      enabled: v.enabled === true,
      dir: typeof v.dir === "string" ? v.dir : "",
      keep: clampKeep(v.keep),
    };
  } catch {
    return fallback;
  }
}

export function clampKeep(n: unknown): number {
  const v = typeof n === "number" && Number.isFinite(n) ? Math.round(n) : DEFAULT_KEEP;
  return Math.max(1, Math.min(50, v));
}

interface AutoSaveBackupState extends AutoSaveBackupSettings {
  /** What the last pass did, for the Saves screen. */
  last: { at: number; backedUp: number; failed: number } | null;
  running: boolean;
  set: (patch: Partial<AutoSaveBackupSettings>) => void;
}

export const useAutoSaveBackupStore = create<AutoSaveBackupState>((set, get) => ({
  ...loadSettings(),
  last: null,
  running: false,
  set: (patch) => {
    const next = {
      enabled: patch.enabled ?? get().enabled,
      dir: patch.dir ?? get().dir,
      keep: clampKeep(patch.keep ?? get().keep),
    };
    safeSetItem(KEY, JSON.stringify(next));
    set(next);
  },
}));

/** What was backed up last, per console and save: the save's modification time then. */
type Done = Record<string, number>;

function loadDone(): Done {
  try {
    const v = JSON.parse(safeGetItem(KEY_DONE) ?? "{}") as unknown;
    return v && typeof v === "object" ? (v as Done) : {};
  } catch {
    return {};
  }
}

const doneKey = (host: string, e: SaveEntry) => `${host}|${e.path}`;

/** The saves that need a backup: changed since the last one, and not written to in the last
 *  few minutes. A save never backed up counts as changed. */
export function savesNeedingBackup(
  host: string,
  saves: readonly SaveEntry[],
  done: Readonly<Done>,
  nowSecs: number,
): SaveEntry[] {
  return saves.filter((e) => {
    if (e.mtime <= 0 || nowSecs - e.mtime < SETTLE_SECS) return false;
    return e.mtime > (done[doneKey(host, e)] ?? 0);
  });
}

async function backUpOne(host: string, entry: SaveEntry, dir: string, keep: number): Promise<void> {
  let tempDir: string | null = null;
  try {
    tempDir = await saveArchiveMakeTemp(entry.title_id);
    const jobId = await startTransferDownload(entry.path, tempDir, consoleAddr(host), "folder");
    await waitForJob(jobId);
    await saveArchiveBackupFinalize(tempDir, entry.title_id);
    const dest = await saveAutoBackupSlot(dir, entry.title_id, entry.user_id, backupTimestamp(), keep);
    await saveArchiveZip(tempDir, entry.title_id, dest, `${entry.title_id}.zip`);
  } finally {
    if (tempDir) await saveArchiveCleanupTemp(tempDir).catch(() => {});
  }
}

/** One pass for `host`: back up every save that changed. Does nothing when the feature is
 *  off, no folder is chosen, or a pass is already running. */
export async function runAutoSaveBackup(host: string): Promise<void> {
  const st = useAutoSaveBackupStore.getState();
  if (!st.enabled || !st.dir.trim() || st.running || !host.trim()) return;
  useAutoSaveBackupStore.setState({ running: true });
  let backedUp = 0;
  let failed = 0;
  try {
    const list = await savesList(mgmtAddr(host));
    const done = loadDone();
    const due = savesNeedingBackup(host, list.saves, done, Math.floor(Date.now() / 1000));
    for (const entry of due) {
      // The switch may be turned off part-way through a long pass.
      if (!useAutoSaveBackupStore.getState().enabled) break;
      try {
        await backUpOne(host, entry, st.dir, st.keep);
        done[doneKey(host, entry)] = entry.mtime;
        safeSetItem(KEY_DONE, JSON.stringify(done));
        backedUp += 1;
      } catch (e) {
        failed += 1;
        log.warn("saves", `automatic backup of ${entry.title_id} failed`, e);
      }
    }
    if (backedUp > 0 || failed > 0) {
      log.info("saves", `automatic backup: ${backedUp} saved, ${failed} failed (${st.dir})`);
    }
  } catch (e) {
    log.warn("saves", "automatic backup could not list saves", e);
  } finally {
    useAutoSaveBackupStore.setState({
      running: false,
      last: { at: Date.now(), backedUp, failed },
    });
  }
}
