// lib/updateNotice wired to the stores: the update (desktop and phone app) or the engine's update
// (web UI), the selected console's helper, and what the person dismissed.

import { useEffect, useState } from "react";
import { create } from "zustand";

import { getAppVersion } from "../lib/appVersion";
import { safeGetItem, safeSetItem } from "../lib/safeStorage";
import { isMobile } from "../lib/platform";
import { isTauriEnv } from "../lib/tauriEnv";
import { bannerId, updateNotice, type UpdateNotice } from "../lib/updateNotice";
import { useWebUpdateStore, webUpdateWaiting } from "../layout/WebUpdateToast";
import { apkUpdaterAvailable, useApkUpdateStore } from "./apkUpdate";
import { useConnectionStore } from "./connection";
import { failedInstall, useUpdateStore, type UpdatePhase } from "./update";

const DISMISSED_KEY = "ps5upload.update-banner.dismissed";
/** Where the newer-helper banner kept its dismissal before 12.6.15: the version pair. */
const OLD_HELPER_DISMISSED_KEY = "ps5upload.helper-mismatch.dismissed";
/** Enough to cover a few releases; the oldest go first. */
const KEEP_DISMISSED = 12;

function loadDismissed(): string[] {
  let list: string[] = [];
  try {
    const v = JSON.parse(safeGetItem(DISMISSED_KEY) ?? "[]") as unknown;
    if (Array.isArray(v)) list = v.filter((x): x is string => typeof x === "string");
  } catch {
    // A damaged value: start over.
  }
  const old = safeGetItem(OLD_HELPER_DISMISSED_KEY);
  if (old) list.push(bannerId("helper-newer", old));
  return list;
}

export const useBannerDismissStore = create<{ dismissed: string[]; dismiss: (id: string) => void }>((set, get) => ({
  dismissed: loadDismissed(),
  dismiss(id) {
    const next = [...get().dismissed.filter((x) => x !== id), id].slice(-KEEP_DISMISSED);
    safeSetItem(DISMISSED_KEY, JSON.stringify(next));
    set({ dismissed: next });
  },
}));

/** An update waiting to be installed (found, downloading, downloaded, or its download failed). */
export function updateWaitingVersion(phase: UpdatePhase): string | null {
  return phase.kind === "available" ||
    phase.kind === "downloading" ||
    phase.kind === "downloaded" ||
    phase.kind === "download-failed"
    ? phase.result.latest_version
    : null;
}

let appVersionMemo: Promise<string | null> | null = null;

/** This app's version (null until read, or when it can't be). Read once per page. */
export function useAppVersionValue(): string | null {
  const [v, setV] = useState<string | null>(null);
  useEffect(() => {
    let live = true;
    appVersionMemo ??= getAppVersion().catch(() => null);
    void appVersionMemo.then((x) => {
      if (live) setV(x);
    });
    return () => {
      live = false;
    };
  }, []);
  return v;
}

/** The desktop/phone update not skipped, and whether its install failed. */
function useAppUpdate(): { waiting: string | null; failed: boolean } {
  const waiting = useUpdateStore((s) => {
    const v = updateWaitingVersion(s.phase);
    return v && v !== s.skippedVersion ? v : null;
  });
  const lastFailed = useUpdateStore((s) => failedInstall(s, updateWaitingVersion(s.phase)) != null);
  // Android refused the APK after its dialog (Play Protect, a conflict, no space).
  const apkFailed = useApkUpdateStore((s) => s.step.kind === "failed");
  const apk = isMobile() && apkUpdaterAvailable();
  return { waiting, failed: !!waiting && (lastFailed || (apk && apkFailed)) };
}

/** Everything the update surfaces show (lib/updateNotice). */
export function useUpdateNotice(): UpdateNotice {
  const tauri = isTauriEnv();
  const app = useAppUpdate();
  const webWaiting = useWebUpdateStore(webUpdateWaiting);
  const appVersion = useAppVersionValue();
  // The selected console's helper, while it answers (a pairing ask is not an answer to trust).
  const helperVersion = useConnectionStore((s) =>
    s.payloadStatus === "up" && s.session !== "needs_pairing" ? s.payloadVersion : null,
  );
  const dismissed = useBannerDismissStore((s) => s.dismissed);
  return updateNotice({
    appVersion,
    helperVersion,
    waitingVersion: tauri ? app.waiting : webWaiting,
    installFailed: tauri ? app.failed : false,
    dismissed,
  });
}

/** The dot on Settings (sidebar row, More tab and its Settings row). */
export function useSettingsDot(): boolean {
  return useUpdateNotice().dot;
}
