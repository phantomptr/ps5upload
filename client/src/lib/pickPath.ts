// Cross-platform file/folder picker that returns a REAL filesystem path.
//
// Desktop uses the native dialog (plugin-dialog), which already returns
// real paths. Android's scoped storage doesn't — its directory picker is
// a no-op and its file picker returns content:// URIs the engine can't
// read — so on Android we route to the in-app real-path browser
// (LocalPathPicker via the localPicker store) instead. Callers don't
// branch; they just `await pickPath({ mode })`.
//
// iOS picks files with the system Files sheet (#644: iCloud Drive, On My iPhone, Chrome's
// downloads, other apps' folders), which copies each one into the app's own temporary folder
// and hands back that copy: a real path the engine, running in the app, can read. The in-app
// browser only ever saw the app's own folder. Folders (which the sheet cannot copy) and an
// engine on another computer still browse in-app.

import { open as openDialog } from "@tauri-apps/plugin-dialog";

import { isIOS, isMobile } from "./platform";
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
  /** The file is read here in the app, not by the engine (a settings backup, say), so a remote
   *  engine must not change which machine is browsed. */
  thisDevice?: boolean;
}

/** The system file sheet is used for files on iOS while the engine runs in the app. */
function iosSystemFiles(mode: string, source: PickPathOptions["source"], thisDevice?: boolean): boolean {
  return isIOS() && mode === "file" && !source && (!!thisDevice || engineIsOnThisDevice());
}

/** A picked file as a path: iOS hands back `file://` URLs (percent-encoded). */
export function pickedPath(s: string): string {
  if (!s.startsWith("file://")) return s;
  try {
    return decodeURIComponent(new URL(s).pathname);
  } catch {
    return s;
  }
}

/** iOS: the system Files sheet, or null when it could not open (the in-app browser then). */
async function pickIosFiles(multiple: boolean, opts: { title?: string; filters?: PickPathOptions["filters"] }): Promise<string[] | null> {
  try {
    const sel = await openDialog({ multiple, title: opts.title, filters: opts.filters });
    const list = Array.isArray(sel) ? sel : typeof sel === "string" ? [sel] : [];
    return list.filter((x): x is string => typeof x === "string").map(pickedPath);
  } catch {
    return null;
  }
}

/** Pick a single real path, or null if cancelled. */
export async function pickPath(opts: PickPathOptions): Promise<string | null> {
  // A server, Android, the web build and a remote engine all browse in-app: the web build has
  // no system dialog, and the system dialog would browse the wrong machine (this one, not the
  // engine's) whenever the engine runs elsewhere.
  if (iosSystemFiles(opts.mode, opts.source, opts.thisDevice)) {
    const got = await pickIosFiles(false, opts);
    if (got) return got[0] ?? null;
  }
  if (
    opts.source ||
    opts.mode === "any" ||
    isMobile() ||
    (!opts.thisDevice && !engineIsOnThisDevice())
  ) {
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
 * Pick one OR MORE real paths: files (the default), or folders with `mode: "folder"`. Lets a
 * user build a list in one gesture — e.g. several payloads for a playlist, or several game
 * folders to convert. The system dialog multi-selects on desktop; Android, the web build and a
 * saved server tick rows in the in-app browser. Returns [] if cancelled.
 */
export async function pickPaths(
  opts: Omit<PickPathOptions, "mode"> & { mode?: "file" | "folder" } = {},
): Promise<string[]> {
  const mode = opts.mode ?? "file";
  if (iosSystemFiles(mode, opts.source)) {
    const got = await pickIosFiles(true, opts);
    if (got) return got;
  }
  if (opts.source || isMobile() || !engineIsOnThisDevice()) {
    return pickLocalPaths({ mode, title: opts.title, filters: opts.filters, source: opts.source });
  }
  const sel = await openDialog({
    directory: mode === "folder",
    multiple: true,
    title: opts.title,
    filters: opts.filters,
  });
  if (Array.isArray(sel)) {
    return sel.filter((s): s is string => typeof s === "string");
  }
  return typeof sel === "string" ? [sel] : [];
}
