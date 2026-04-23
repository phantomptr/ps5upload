import { create } from "zustand";

/**
 * Shared cut/copy clipboard for PS5 filesystem operations.
 *
 * The File System screen owns the paste target (the currently-browsed
 * directory); the Library screen can seed the clipboard with one or
 * more entries via Cut or Copy. Persisting this in a zustand store
 * (not component state) is what lets the user click "Cut" in Library,
 * tab over to File System, navigate, and click "Paste" — the staged
 * items survive the navigation.
 *
 * We don't persist this to disk: the clipboard is a session-level
 * intent, not a setting. If the app restarts mid-paste, it's safer to
 * forget than to replay.
 */

export type ClipboardOp = "cut" | "copy";

export interface ClipboardItem {
  /** Absolute PS5 path to the source. */
  path: string;
  /** Display name — usually the final path component, but the source
   *  screen may override (e.g. a game's localized title). */
  name: string;
  /** Rough byte count for paste-time progress/warnings. 0 if unknown. */
  size: number;
  /** Optional: so the UI can render an icon hint next to the item. */
  kind?: "file" | "dir" | "game" | "image" | "folder";
}

interface FsClipboardState {
  op: ClipboardOp | null;
  items: ClipboardItem[];
  /** Where the items came from — shown in the paste banner so users
   *  can confirm they're pasting what they meant. Not used for any
   *  logical decision; purely informational. */
  sourceLabel: string | null;

  /** Stage items for a future paste. Replaces any existing clipboard. */
  set: (op: ClipboardOp, items: ClipboardItem[], sourceLabel: string) => void;
  /** Clear after paste succeeds (for "cut") or user dismisses. */
  clear: () => void;
}

export const useFsClipboardStore = create<FsClipboardState>((set) => ({
  op: null,
  items: [],
  sourceLabel: null,

  set: (op, items, sourceLabel) => set({ op, items, sourceLabel }),
  clear: () => set({ op: null, items: [], sourceLabel: null }),
}));
