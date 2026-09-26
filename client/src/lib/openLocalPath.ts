import { openPath, revealItemInDir } from "@tauri-apps/plugin-opener";

import { log } from "../state/logs";
import { isTauriEnv } from "./tauriEnv";

/**
 * Open a local folder in the system file manager.
 *
 * NOT `openUrl("file://…")`: the opener plugin scopes `openUrl` to
 * `mailto:`, `tel:`, `http://` and `https://`, so a `file://` URL is
 * rejected before it reaches the OS. That failure is silent from the
 * user's side — the Edit Game Image screen's "Show files" button looked
 * like it did nothing at all, which is how it shipped broken.
 *
 * `openPath` opens the folder itself, so the user lands among the files
 * rather than beside the volume. If that is refused we fall back to
 * revealing the item, which the default permission set always allows.
 *
 * Returns false rather than throwing, but callers should surface that:
 * a button that quietly does nothing is worse than one that says why.
 */
export async function openLocalPath(path: string): Promise<boolean> {
  if (!path) return false;
  if (!isTauriEnv()) {
    // No file manager to open in a browser/Docker session.
    return false;
  }
  try {
    await openPath(path);
    return true;
  } catch (e) {
    log.warn("ui", `openPath failed for ${path}: ${String(e)}`);
  }
  try {
    await revealItemInDir(path);
    return true;
  } catch (e) {
    log.warn("ui", `revealItemInDir failed for ${path}: ${String(e)}`);
    return false;
  }
}
