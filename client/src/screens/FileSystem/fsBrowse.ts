// FileZilla-style browsing helpers for the Files screen: column sorting and the keyboard map.
// Pure, so the screen stays a thin wiring layer and these stay tested.

export type SortKey = "name" | "size" | "mtime";
export interface SortState {
  key: SortKey;
  desc: boolean;
}

interface Sortable {
  name: string;
  kind: string;
  size: number;
  mtime?: number;
}

/** Folders first (as every file manager does), then by the chosen column; ties by name. */
export function sortEntries<T extends Sortable>(entries: T[], sort: SortState): T[] {
  const dir = sort.desc ? -1 : 1;
  const byName = (a: T, b: T) =>
    a.name.localeCompare(b.name, undefined, { numeric: true, sensitivity: "base" });
  return [...entries].sort((a, b) => {
    const ad = a.kind === "dir" ? 0 : 1;
    const bd = b.kind === "dir" ? 0 : 1;
    if (ad !== bd) return ad - bd;
    const c =
      sort.key === "size"
        ? a.size - b.size
        : sort.key === "mtime"
          ? (a.mtime ?? 0) - (b.mtime ?? 0)
          : byName(a, b);
    // Equal on the column (two folders by size): by name A to Z whichever way the column runs.
    return c === 0 ? byName(a, b) : c * dir;
  });
}

/** Clicking a column: the same column flips the order, a new one starts ascending. */
export function nextSort(cur: SortState, key: SortKey): SortState {
  return cur.key === key ? { key, desc: !cur.desc } : { key, desc: false };
}

export type FsKeyAction =
  | "select-all"
  | "copy"
  | "cut"
  | "paste"
  | "delete"
  | "rename"
  | "refresh"
  | "up"
  | "open"
  | "clear";

/** The browser's key event, trimmed to what the map reads. */
export interface KeyLike {
  key: string;
  ctrlKey: boolean;
  metaKey: boolean;
  altKey: boolean;
  shiftKey: boolean;
}

/**
 * The Files screen's keyboard map. Ctrl on Windows/Linux, ⌘ on macOS. Plain Backspace goes up
 * a folder (as FileZilla's remote pane does); Delete and ⌘⌫ delete the selection.
 */
export function fsKeyAction(e: KeyLike): FsKeyAction | null {
  const mod = e.ctrlKey || e.metaKey;
  const k = e.key.length === 1 ? e.key.toLowerCase() : e.key;
  if (mod && !e.altKey) {
    if (k === "a") return "select-all";
    if (k === "c") return "copy";
    if (k === "x") return "cut";
    if (k === "v") return "paste";
    if (k === "r") return "refresh";
    if (e.metaKey && k === "Backspace") return "delete";
    return null;
  }
  if (e.altKey) return null;
  switch (k) {
    case "Delete":
      return "delete";
    case "F2":
      return "rename";
    case "F5":
      return "refresh";
    case "Backspace":
      return "up";
    case "Enter":
      return "open";
    case "Escape":
      return "clear";
    default:
      return null;
  }
}

/** True when keystrokes belong to a text field or an open dialog, not the file list. */
export function keysBelongElsewhere(target: EventTarget | null): boolean {
  const el = target as HTMLElement | null;
  if (!el || typeof el.closest !== "function") return false;
  const tag = el.tagName;
  if (tag === "INPUT" || tag === "TEXTAREA" || tag === "SELECT" || el.isContentEditable) return true;
  return !!el.closest('[role="dialog"],[role="menu"]');
}

/** Normalizes a typed path: absolute, single slashes, no trailing slash (except "/"). */
export function normalizeTypedPath(raw: string): string | null {
  const t = raw.trim();
  if (!t.startsWith("/")) return null;
  const collapsed = t.replace(/\/+/g, "/");
  return collapsed.length > 1 ? collapsed.replace(/\/$/, "") : "/";
}
