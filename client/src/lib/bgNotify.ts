// Whether the host posts job complete / failed notifications itself (#457).
//
// The desktop, Android and iPhone hosts watch the engine from Rust (bg_activity.rs) and
// notify when a job ends, whatever the WebView is doing. When they do, the
// renderer must not mirror its own notification for the same ending, or the
// person gets two. The browser build has no host: false, the old behaviour.
//
// Kept apart from lib/bgWork.ts so state/notifications.ts can use it without an
// import cycle; uses the raw Tauri invoke for the same reason (invokeLogged pulls
// in the log and session stores).

import { invoke } from "@tauri-apps/api/core";
import { isTauriEnv } from "./tauriEnv";

let known: boolean | null = null;
let pending: Promise<boolean> | null = null;

/** True once the host reports its watcher running. A false answer is asked again
 *  next time: the watcher starts after the engine, which can be after the first ask. */
export function nativeJobNotify(): Promise<boolean> {
  if (known) return Promise.resolve(true);
  if (!isTauriEnv()) return Promise.resolve(false);
  pending ??= invoke<{ native_job_notify?: boolean }>("bg_capabilities")
    .then((c) => {
      known = c?.native_job_notify === true;
      return known;
    })
    .catch(() => false)
    .finally(() => {
      pending = null;
    });
  return pending;
}
