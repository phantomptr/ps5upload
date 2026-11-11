import { create } from "zustand";
import { useConnectionStore } from "./connection";
import { hostOf } from "../lib/addr";

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
  /** A cut that has been pasted: clear the clipboard of the console it ran on. That console
   *  may be hidden by now (its clipboard is the stashed one), and its clipboard may hold
   *  something newer: with `items`, it is cleared only while it still holds exactly those. */
  clearFor: (host: string | null | undefined, items?: ClipboardItem[]) => void;
  /** Console switch: stash THIS console's clipboard and restore the target
   *  console's, so a cut/copy survives a round-trip switch (each PS5 keeps its
   *  own clipboard). The clipboard holds PS5-side source paths, so it's only
   *  ever pasted on the console it was cut from — preserving it per-console is
   *  safe. Call BEFORE connection.setHost (the connection store must still hold
   *  the OLD host the current clipboard belongs to). */
  switchToHost: (newHost: string) => void;
}

interface ClipboardSnapshot {
  op: ClipboardOp | null;
  items: ClipboardItem[];
  sourceLabel: string | null;
}
const EMPTY_CLIPBOARD: ClipboardSnapshot = {
  op: null,
  items: [],
  sourceLabel: null,
};
/** One stashed clipboard per console (port-stripped host key). Module-level. */
const clipboardStash = new Map<string, ClipboardSnapshot>();
const clipKey = (host: string | null | undefined): string =>
  hostOf(host ?? "") || "_unset_";

/** Forget a console's stashed clipboard — called when its roster profile is
 *  removed/re-pointed. */
export function evictFsClipboard(host: string): void {
  clipboardStash.delete(clipKey(host));
}

export const useFsClipboardStore = create<FsClipboardState>((set, get) => ({
  op: null,
  items: [],
  sourceLabel: null,

  set: (op, items, sourceLabel) => set({ op, items, sourceLabel }),
  clear: () => set({ op: null, items: [], sourceLabel: null }),
  clearFor: (host, items) => {
    const key = clipKey(host);
    if (key === clipKey(useConnectionStore.getState().host)) {
      if (!items || get().items === items) get().clear();
      return;
    }
    const stashed = clipboardStash.get(key);
    if (stashed && (!items || stashed.items === items)) clipboardStash.delete(key);
  },
  switchToHost: (newHost) => {
    const oldKey = clipKey(useConnectionStore.getState().host);
    const newKey = clipKey(newHost);
    if (oldKey === newKey) return;
    const s = get();
    clipboardStash.set(oldKey, {
      op: s.op,
      items: s.items,
      sourceLabel: s.sourceLabel,
    });
    set(clipboardStash.get(newKey) ?? EMPTY_CLIPBOARD);
  },
}));
