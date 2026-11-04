// The renderer's half of background work (#457).
//
// The desktop, Android and iPhone hosts watch the engine from Rust (src-tauri/src/bg_activity.rs):
// it keeps the app running while jobs run, shows their progress (Android) and posts
// complete / failed notifications. This module gives it what only the page knows:
//
//   * a readable name for each job the page starts (`labelEngineJob`), sent to the
//     engine so the host's notification says "Uploading Astro Bot", not a job id;
//   * whether the page is showing, the person's notification setting, and the
//     notification texts in their language (`bg_configure`);
//   * and it passes the host's `bg-tick` / `bg-job-finished` events to the queue's
//     sleeps (lib/wakeableSleep), so the queue moves on while the window is hidden.
//
// Outside Tauri (the self-hosted web UI) there is no host: everything here no-ops.

import { listen } from "@tauri-apps/api/event";

import { invoke } from "./invokeLogged";
import { isTauriEnv } from "./tauriEnv";
import { trStatic } from "./trStatic";
import { wakeSleepers } from "./wakeableSleep";
import { getEngineUrl } from "../state/engine";
import { useLangStore } from "../state/lang";
import { subscribeLocaleLoaded } from "../i18n";
import { useNotificationsStore } from "../state/notifications";

export type BgJobKind = "upload" | "download" | "install";

/** Name an engine job for the host's notifications. Fire-and-forget: a job without
 *  a name still runs; it just goes unannounced. */
export function labelEngineJob(jobId: string | null | undefined, kind: BgJobKind, label: string): void {
  if (!isTauriEnv() || !jobId || !label.trim()) return;
  void fetch(`${getEngineUrl()}/api/activity/label`, {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({ job_id: jobId, kind, label }),
  }).catch(() => {
    /* an older or unreachable engine: no name, nothing else lost */
  });
}

/** The notification texts, keyed as bg_activity.rs's `BgStrings`. */
export function bgStrings(): Record<string, string> {
  return {
    uploading: trStatic("bg_uploading", "Uploading {name}"),
    downloading: trStatic("bg_downloading", "Downloading {name}"),
    installing: trStatic("bg_installing", "Installing {name}"),
    working: trStatic("bg_working", "Working on {name}"),
    jobs_running: trStatic("bg_jobs_running", "{n} jobs running"),
    waiting: trStatic("bg_waiting", "Waiting for the next item"),
    progress_of: trStatic("bg_progress_of", "{done} of {total}"),
    done_upload: trStatic("bg_done_upload", "Upload complete"),
    done_download: trStatic("bg_done_download", "Download complete"),
    done_install: trStatic("bg_done_install", "Installed"),
    done_job: trStatic("bg_done_job", "Finished"),
    failed_upload: trStatic("bg_failed_upload", "Upload failed"),
    failed_download: trStatic("bg_failed_download", "Download failed"),
    failed_install: trStatic("bg_failed_install", "Install failed"),
    failed_job: trStatic("bg_failed_job", "Failed"),
    channel_progress: trStatic("bg_channel_progress", "Transfers in progress"),
    channel_results: trStatic("bg_channel_results", "Finished transfers"),
  };
}

function configure(config: { visible?: boolean; notify?: boolean; strings?: Record<string, string> }): void {
  invoke("bg_configure", { config }).catch(() => {
    /* older host without the command */
  });
}

let installed = false;

/** Wire the page to the host's background watcher. Idempotent. */
export function installBgWork(): void {
  if (installed || !isTauriEnv()) return;
  installed = true;

  void listen("bg-tick", () => wakeSleepers()).catch(() => {});
  void listen("bg-job-finished", () => wakeSleepers({ polls: true })).catch(() => {});

  const visible = () => document.visibilityState !== "hidden";
  configure({
    visible: visible(),
    notify: useNotificationsStore.getState().osNotifyEnabled,
    strings: bgStrings(),
  });
  document.addEventListener("visibilitychange", () => configure({ visible: visible() }));
  useNotificationsStore.subscribe((s, prev) => {
    if (s.osNotifyEnabled !== prev.osNotifyEnabled) configure({ notify: s.osNotifyEnabled });
  });
  // A language's texts arrive after its chunk loads, not when it is picked.
  useLangStore.subscribe(() => configure({ strings: bgStrings() }));
  subscribeLocaleLoaded(() => configure({ strings: bgStrings() }));
}
