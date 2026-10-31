import { create } from "zustand";
import { listen } from "@tauri-apps/api/event";

import { updateDownloadApk, type UpdateCheck } from "../api/ps5";
import { openExternalUrl } from "../lib/openExternalUrl";
import { t as translate } from "../i18n";
import { useLangStore } from "./lang";

// Android: download the update APK inside the app and hand it to Android's
// installer, so the person sees the system "Update?" dialog instead of
// downloading in the browser and installing by hand.
//
// The download (with progress) and its checks run in Rust
// (commands/update_android.rs). The install runs in AppUpdater.kt, written by
// scripts/release/android-postinit-patch.sh, through `window.PS5UploadUpdater`.
// Where the bridge is absent (desktop, iOS, browser build, an Android build
// without it) the store's mobile branch keeps opening the browser.

/** `window.PS5UploadUpdater` (MainActivity, see AppUpdater.kt). */
export interface UpdaterBridge {
  canInstall(): boolean;
  openInstallSettings(): void;
  /** "" when the install started, "needs-permission", "signed-differently", or an error. */
  install(path: string): string;
  /** JSON `{ state, message }`; state: idle | writing | confirm | done | cancelled | failed. */
  status(): string;
}

type WindowWithUpdater = Window & { PS5UploadUpdater?: UpdaterBridge };

export function updaterBridge(
  w: Window | undefined = typeof window !== "undefined" ? window : undefined,
): UpdaterBridge | undefined {
  return w ? (w as WindowWithUpdater).PS5UploadUpdater : undefined;
}

/** This copy can install its own update (the Android bridge is there). */
export function apkUpdaterAvailable(w?: Window): boolean {
  return !!updaterBridge(w ?? (typeof window !== "undefined" ? window : undefined));
}

export type ApkStep =
  | { kind: "idle" }
  | { kind: "downloading"; received: number; total: number }
  /** "Install unknown apps" is off; the setting was opened. Continues on return. */
  | { kind: "needs-permission" }
  /** Handing over to Android, or its dialog is up. */
  | { kind: "installing" }
  /** `during`: the download, or Android's install (a refusal, a different key…). */
  | { kind: "failed"; message: string; during?: "download" | "install" };

export interface InstallStatus {
  state: "idle" | "writing" | "confirm" | "done" | "cancelled" | "failed";
  message: string;
}

const STATES = new Set<InstallStatus["state"]>([
  "idle",
  "writing",
  "confirm",
  "done",
  "cancelled",
  "failed",
]);

/** Parse `PS5UploadUpdater.status()`. Anything unreadable is a failure. */
export function parseInstallStatus(raw: string): InstallStatus {
  try {
    const v = JSON.parse(raw) as { state?: unknown; message?: unknown };
    const state = typeof v.state === "string" ? v.state : "";
    if (STATES.has(state as InstallStatus["state"])) {
      return {
        state: state as InstallStatus["state"],
        message: typeof v.message === "string" ? v.message : "",
      };
    }
  } catch {
    /* fall through */
  }
  return { state: "failed", message: `unexpected installer status: ${raw}` };
}

/** Whole percent of a download, or null while the size is unknown. */
export function progressPercent(received: number, total: number): number | null {
  if (!(total > 0) || !(received >= 0)) return null;
  return Math.min(100, Math.floor((received / total) * 100));
}

/** The release page to fall back to when the in-app install cannot finish. */
export function releasePageFor(version: string): string {
  const v = version.replace(/^v/, "").trim();
  return v
    ? `https://github.com/phantomptr/ps5upload/releases/tag/v${v}`
    : "https://github.com/phantomptr/ps5upload/releases/latest";
}

/** Open the download page for `result` in the browser (the manual way). */
export function openApkDownloadPage(result: UpdateCheck): Promise<boolean> {
  return openExternalUrl(releasePageFor(result.latest_version));
}

function trFb(key: string, fallback: string): string {
  const out = translate(useLangStore.getState().lang, key, undefined);
  return out === key ? fallback : out;
}

/** What the person reads for an `install()` refusal. */
export function installRefusalMessage(code: string): string {
  if (code === "signed-differently") {
    return trFb(
      "update_apk_signed_differently",
      "This update is signed with a different key than the installed app. Uninstall PS5Upload, then install the new APK from the download page.",
    );
  }
  if (code === "needs-permission") {
    return trFb(
      "update_apk_permission_off",
      "Install unknown apps is still off for PS5Upload. Turn it on, then try again.",
    );
  }
  return code;
}

interface ApkUpdateStore {
  step: ApkStep;
}

export const useApkUpdateStore = create<ApkUpdateStore>(() => ({ step: { kind: "idle" } }));

const setStep = (step: ApkStep) => useApkUpdateStore.setState({ step });

const POLL_MS = 500;
/** Give up watching a dialog nobody answers (the app keeps working either way). */
const WATCH_LIMIT_MS = 30 * 60 * 1000;

const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms));

/** Downloaded, waiting for "Install unknown apps" to be turned on. */
let pendingPath: string | null = null;

/** Hand `path` to Android. Resolves with an error, or null once Android has
 *  taken it (its dialog is up, or the install already ended). Watches the
 *  dialog afterwards so a cancel or a late failure shows. */
async function startInstall(bridge: UpdaterBridge, path: string): Promise<string | null> {
  let code: string;
  try {
    code = bridge.install(path);
  } catch (e) {
    code = e instanceof Error ? e.message : String(e);
  }
  if (code === "needs-permission") {
    pendingPath = path;
    setStep({ kind: "needs-permission" });
    bridge.openInstallSettings();
    return null;
  }
  if (code) {
    const message = installRefusalMessage(code);
    setStep({ kind: "failed", message });
    return message;
  }
  setStep({ kind: "installing" });
  const started = Date.now();
  // Wait until Android has the APK (its dialog is up) or the install ended.
  for (;;) {
    const s = parseInstallStatus(bridge.status());
    if (s.state === "failed") {
      const message = s.message || "install failed";
      setStep({ kind: "failed", message });
      return message;
    }
    if (s.state === "cancelled" || s.state === "done") {
      setStep({ kind: "idle" });
      return null;
    }
    if (s.state === "confirm") break;
    if (Date.now() - started > WATCH_LIMIT_MS) return null;
    await sleep(POLL_MS);
  }
  void (async () => {
    while (Date.now() - started < WATCH_LIMIT_MS) {
      await sleep(POLL_MS);
      const s = parseInstallStatus(bridge.status());
      if (s.state === "confirm" || s.state === "writing") continue;
      setStep(
        s.state === "failed"
          ? { kind: "failed", message: s.message || "install failed" }
          : { kind: "idle" },
      );
      return;
    }
    setStep({ kind: "idle" });
  })();
  return null;
}

// Back from "Install unknown apps": carry on with the pending APK.
if (typeof document !== "undefined") {
  document.addEventListener("visibilitychange", () => {
    if (document.visibilityState !== "visible" || !pendingPath) return;
    const bridge = updaterBridge();
    if (!bridge) return;
    let allowed: boolean;
    try {
      allowed = bridge.canInstall();
    } catch {
      allowed = false;
    }
    const path = pendingPath;
    if (!allowed) {
      setStep({ kind: "failed", message: installRefusalMessage("needs-permission") });
      pendingPath = null;
      return;
    }
    pendingPath = null;
    void startInstall(bridge, path);
  });
}

/** Re-open "Install unknown apps" (the needs-permission notice's button). */
export function openInstallSettings(): void {
  try {
    updaterBridge()?.openInstallSettings();
  } catch {
    /* the notice stays; the download page is the fallback */
  }
}

/**
 * Download the APK for this device with progress, check it, and hand it to
 * Android. Resolves with an error message, or null when Android has it (or is
 * waiting for "Install unknown apps", which continues by itself).
 */
export async function downloadAndInstallApk(result: UpdateCheck): Promise<string | null> {
  const bridge = updaterBridge();
  if (!bridge) return "this copy cannot install updates itself";
  if (!result.download_url || !result.download_filename) {
    return "no APK for this device in this release";
  }
  pendingPath = null;
  setStep({ kind: "downloading", received: 0, total: 0 });
  let unlisten: (() => void) | undefined;
  try {
    unlisten = await listen<{ received: number; total: number }>(
      "update-apk-progress",
      (e) => {
        if (useApkUpdateStore.getState().step.kind !== "downloading") return;
        setStep({ kind: "downloading", received: e.payload.received, total: e.payload.total });
      },
    );
  } catch {
    // No progress events: the download still runs, the bar just stays empty.
  }
  let path: string;
  try {
    const dl = await updateDownloadApk(
      result.download_url,
      result.download_filename,
      result.sha256,
    );
    path = dl.path;
  } catch (e) {
    const message = e instanceof Error ? e.message : String(e);
    setStep({ kind: "failed", message, during: "download" });
    return message;
  } finally {
    unlisten?.();
  }
  return startInstall(bridge, path);
}
