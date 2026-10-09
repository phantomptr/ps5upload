import { openUrl } from "@tauri-apps/plugin-opener";

import { log } from "../state/logs";
import { isTauriEnv } from "./tauriEnv";

/**
 * Open an external URL in the system browser / default handler.
 *
 * Uses the Tauri `opener` plugin, which works on EVERY platform — including
 * Android, where it fires an `Intent.ACTION_VIEW`. The old approach
 * (`@tauri-apps/plugin-shell` `open`) tried to spawn a system-opener process
 * and failed on Android with "Scoped shell IO error: No such file or directory
 * (os error 2)", which is why in-app links and the self-update browser fallback
 * never worked there.
 *
 * Logs failures (category "ui") so a "links don't open" report leaves a trace,
 * and returns `false` instead of throwing so fire-and-forget callers stay
 * simple while callers that care (the updater) can branch on the result.
 */
export async function openExternalUrl(url: string): Promise<boolean> {
  // The self-hosted web UI has no Tauri opener: a new tab is the same thing there.
  if (!isTauriEnv()) {
    if (typeof window === "undefined") return false;
    // With "noopener" the browser returns null even when the tab opened, so there is nothing
    // to check: asking is all a page can do.
    window.open(url, "_blank", "noopener");
    return true;
  }
  try {
    await openUrl(url);
    return true;
  } catch (e) {
    log.warn(
      "ui",
      `failed to open external url ${url}: ${
        e instanceof Error ? e.message : String(e)
      }`,
    );
    return false;
  }
}
