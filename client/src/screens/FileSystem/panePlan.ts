// Files' two panes (#952): the PS5 on one side, another place on the other, and Copy / Move
// between them. Every transfer here is one the app already had (upload, download, download to a
// saved server, send to another console); this file only says which one a pair of places uses,
// and runs a move as copy, check, then delete. Pure, so it is tested without a screen.

import type { Place } from "../../lib/placeList";

/** Whether the two panes are on, per device. */
export const TWO_PANES_KEY = "ps5upload.files.twoPanes";
/** The other pane's place and folder, per device. */
export const OTHER_PANE_KEY = "ps5upload.files.otherPane";

/** Wide enough for two lists side by side, with a mouse: the desktop app and the web UI on a
 *  computer. Phones and tablets keep the one list. */
export const TWO_PANES_QUERY = "(min-width: 1100px) and (pointer: fine)";

export function readTwoPanes(): boolean {
  try {
    return window.localStorage.getItem(TWO_PANES_KEY) === "1";
  } catch {
    return false;
  }
}

export function writeTwoPanes(on: boolean): void {
  try {
    if (on) window.localStorage.setItem(TWO_PANES_KEY, "1");
    else window.localStorage.removeItem(TWO_PANES_KEY);
  } catch {
    /* remembered as a convenience */
  }
}

export interface OtherPaneMemory {
  place: Place;
  path: string;
}

export function readOtherPane(): OtherPaneMemory | null {
  try {
    const raw = window.localStorage.getItem(OTHER_PANE_KEY);
    if (!raw) return null;
    const v = JSON.parse(raw) as OtherPaneMemory;
    const k = v?.place?.kind;
    if (typeof v.path !== "string") return null;
    if (k === "computer") return { place: { kind: "computer" }, path: v.path };
    if (k === "server" && typeof (v.place as { id?: unknown }).id === "string") return v;
    if (k === "console" && typeof (v.place as { host?: unknown }).host === "string") return v;
    return null;
  } catch {
    return null;
  }
}

export function writeOtherPane(m: OtherPaneMemory): void {
  try {
    window.localStorage.setItem(OTHER_PANE_KEY, JSON.stringify(m));
  } catch {
    /* remembered as a convenience */
  }
}

/** Which way a transfer goes: from the PS5 to the other pane, or into the PS5. */
export type Toward = "other" | "ps5";
export type PaneOp = "copy" | "move";

/** The existing operation a pair of places maps to. */
export type PaneVia =
  /** PS5 → this computer: Files' Download. */
  | "download"
  /** PS5 → a saved server: Files' "Download to {server}". */
  | "download-server"
  /** This computer or a saved server → PS5: Files' Add files. */
  | "upload"
  /** Console ↔ console: "Send to another console", queued. */
  | "console-send";

/** Why an action is not offered for a pair of places. */
export type PaneBlock =
  /** Nothing in the app deletes files on this computer, so it can't move from it. */
  | "no-delete-computer"
  /** Nothing in the app deletes files on a saved server, so it can't move from one. */
  | "no-delete-server";

export type PaneRoute = { ok: true; via: PaneVia } | { ok: false; block: PaneBlock };

/** The operation `op` toward `toward` uses with the other pane on `place`, or why there is none.
 *  A move is a copy and then the source deleted, so it is offered only where the app can delete
 *  the source: the PS5 and another console, never this computer or a saved server. */
export function paneRoute(place: Place, toward: Toward, op: PaneOp): PaneRoute {
  if (place.kind === "console") return { ok: true, via: "console-send" };
  if (toward === "other") return { ok: true, via: place.kind === "server" ? "download-server" : "download" };
  if (op === "move") return { ok: false, block: place.kind === "server" ? "no-delete-server" : "no-delete-computer" };
  return { ok: true, via: "upload" };
}

/** How one copy ended: landed, failed, or stopped by the user (nothing after it runs). */
export type CopyOutcome = "done" | "failed" | "stopped";

export interface PaneTransferDeps<T> {
  /** Copies one item to the other side. Resolves when that copy has ended. */
  copy: (item: T) => Promise<CopyOutcome>;
  /** After a copy reported done: the copy is there, and whole (a file has its size). */
  verify: (item: T) => Promise<boolean>;
  /** Deletes the item where it came from. Only ever after `verify` said yes. */
  removeSource: (item: T) => Promise<void>;
}

export interface PaneTransferResult<T> {
  /** Copied, and for a move also deleted where it came from. */
  finished: T[];
  failed: T[];
  /** A move whose copy could not be checked: the source is kept. */
  unverified: T[];
  /** A move whose copy is there but whose source could not be deleted. */
  notRemoved: T[];
  /** The user stopped a copy: it and the items after it were left alone. */
  stopped: boolean;
}

/**
 * Copies each item in turn; a move then checks the copy and deletes the source, item by item.
 * The source is never touched before its own copy has finished and been checked: a failed or
 * stopped copy, or a copy that can't be found whole, keeps it. A stop ends the run.
 */
export async function runPaneTransfer<T>(
  items: T[],
  op: PaneOp,
  deps: PaneTransferDeps<T>,
): Promise<PaneTransferResult<T>> {
  const out: PaneTransferResult<T> = { finished: [], failed: [], unverified: [], notRemoved: [], stopped: false };
  for (const item of items) {
    let outcome: CopyOutcome;
    try {
      outcome = await deps.copy(item);
    } catch {
      outcome = "failed";
    }
    if (outcome === "stopped") {
      out.stopped = true;
      break;
    }
    if (outcome === "failed") {
      out.failed.push(item);
      continue;
    }
    if (op === "copy") {
      out.finished.push(item);
      continue;
    }
    let whole: boolean;
    try {
      whole = await deps.verify(item);
    } catch {
      whole = false;
    }
    if (!whole) {
      out.unverified.push(item);
      continue;
    }
    try {
      await deps.removeSource(item);
      out.finished.push(item);
    } catch {
      out.notRemoved.push(item);
    }
  }
  return out;
}

/** A copy found where it should be: a folder by its name, a file by its name and size. */
export function copyLooksWhole(
  source: { isDir: boolean; size: number },
  found: { is_dir: boolean; size: number } | null,
): boolean {
  if (!found || found.is_dir !== source.isDir) return false;
  return source.isDir || found.size === source.size;
}

/** The names among `names` already in the destination folder: a copy there replaces them. */
export function namesAlreadyThere(names: string[], destNames: Iterable<string>): string[] {
  const there = new Set(destNames);
  return names.filter((n) => there.has(n));
}
