// Safe wrapper around Tauri's convertFileSrc.
//
// convertFileSrc reads window.__TAURI_INTERNALS__, which does not exist
// in the browser / self-hosted web UI. Calling it there throws
// "Cannot read properties of undefined (reading 'convertFileSrc')" and
// takes down whatever screen touched it — in #271 that was the entire
// upload flow, the instant a folder was picked.
//
// There is no browser equivalent: the path is local to the machine
// running the engine, not to the browser. So the honest answer outside
// Tauri is "no URL", and callers render their missing-image fallback.
import { convertFileSrc } from "@tauri-apps/api/core";
import { isTauriEnv } from "./tauriEnv";

/** A displayable URL for a local file, or null when that is not
 *  possible in this environment (browser / self-hosted web UI). */
export function localFileSrc(path: string | null | undefined): string | null {
  if (!path) return null;
  if (!isTauriEnv()) return null;
  return convertFileSrc(path);
}
