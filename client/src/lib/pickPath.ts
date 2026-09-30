// Cross-platform file/folder picker that returns a REAL filesystem path.
//
// Desktop uses the native dialog (plugin-dialog), which already returns
// real paths. Android's scoped storage doesn't — its directory picker is
// a no-op and its file picker returns content:// URIs the engine can't
// read — so on Android we route to the in-app real-path browser
// (LocalPathPicker via the localPicker store) instead. Callers don't
// branch; they just `await pickPath({ mode })`.

import { open as openDialog } from "@tauri-apps/plugin-dialog";

import { isAndroid } from "./platform";
import { engineIsOnThisDevice } from "../state/engine";
import { pickLocalPath, pickLocalPaths } from "../state/localPicker";

export interface PickPathOptions {
  /** "any" (in-app browser only): a file, or the open folder. */
  mode: "file" | "folder" | "any";
  title?: string;
  /** File-type filters (the system dialog's, and the in-app browser's). */
  filters?: { name: string; extensions: string[] }[];
  /** A saved server to browse instead of this computer (resolves with a `remote://` path), or
   *  the console (a `ps5://` path). */
  source?: { connectionId: string } | { console: string };
}

/** Pick a single real path, or null if cancelled. */
export async function pickPath(opts: PickPathOptions): Promise<string | null> {
  // A server, Android, the web build and a remote engine all browse in-app: the web build has
  // no system dialog, and the system dialog would browse the wrong machine (this one, not the
  // engine's) whenever the engine runs elsewhere.
  if (opts.source || opts.mode === "any" || isAndroid() || !engineIsOnThisDevice()) {
    return pickLocalPath({
      mode: opts.mode,
      title: opts.title,
      filters: opts.filters,
      source: opts.source,
    });
  }
  const sel = await openDialog({
    directory: opts.mode === "folder",
    multiple: false,
    // (mode "any" never reaches the system dialog: it browses in-app, above.)
    title: opts.title,
    filters: opts.filters,
  });
  return typeof sel === "string" ? sel : null;
}

/**
 * Pick one OR MORE real file paths. Lets a user build a list in one
 * gesture — e.g. select several payloads to seed a playlist. The system
 * dialog multi-selects on desktop; Android, the web build and a saved
 * server tick rows in the in-app browser. Returns [] if cancelled. Files
 * only — multi-folder select isn't a use case here.
 */
export async function pickPaths(
  opts: Omit<PickPathOptions, "mode"> = {},
): Promise<string[]> {
  if (opts.source || isAndroid() || !engineIsOnThisDevice()) {
    return pickLocalPaths({ mode: "file", title: opts.title, filters: opts.filters, source: opts.source });
  }
  const sel = await openDialog({
    directory: false,
    multiple: true,
    title: opts.title,
    filters: opts.filters,
  });
  if (Array.isArray(sel)) {
    return sel.filter((s): s is string => typeof s === "string");
  }
  return typeof sel === "string" ? [sel] : [];
}
