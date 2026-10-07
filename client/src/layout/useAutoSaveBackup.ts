import { useEffect } from "react";

import { transferScreenBusy } from "../lib/ps5Transfers";
import { isTauriEnv } from "../lib/tauriEnv";
import { runAutoSaveBackup, useAutoSaveBackupStore } from "../state/autoSaveBackup";
import { useConnectionStore } from "../state/connection";

/** How often changed saves are looked for. */
const EVERY_MS = 10 * 60_000;
/** The first look after connecting or switching the feature on. */
const FIRST_MS = 60_000;

/** Runs the automatic save backup for the selected console while its helper is up. Desktop
 *  only: the zip and the folder are written by the desktop app. Skipped while an upload or
 *  download to that console is running, so it never competes with a transfer. */
export function useAutoSaveBackup(): void {
  const host = useConnectionStore((s) => s.host);
  const up = useConnectionStore((s) => s.payloadStatus === "up");
  const enabled = useAutoSaveBackupStore((s) => s.enabled && s.dir.trim() !== "");

  useEffect(() => {
    if (!isTauriEnv() || !enabled || !up || !host?.trim()) return;
    const tick = () => {
      if (!transferScreenBusy(host)) void runAutoSaveBackup(host);
    };
    const first = setTimeout(tick, FIRST_MS);
    const every = setInterval(tick, EVERY_MS);
    return () => {
      clearTimeout(first);
      clearInterval(every);
    };
  }, [enabled, up, host]);
}
