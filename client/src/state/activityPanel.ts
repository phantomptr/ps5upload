// Whether the activity panel is open, and which failed jobs the user has already seen there (so
// the strip's "N failed" badge clears once they have looked). Session-only: a restart starts clean.

import { create } from "zustand";

import { useTaskStore } from "./tasks";

export interface ActivityPanelState {
  open: boolean;
  seen: ReadonlySet<string>;
  /** When this session began; the panel lists only jobs that finished since. */
  sessionStart: number;
  toggle: () => void;
  close: () => void;
  markSeen: (ids: string[]) => void;
}

export const useActivityPanel = create<ActivityPanelState>((set, get) => ({
  open: false,
  seen: new Set(),
  sessionStart: Date.now(),
  toggle: () => {
    const open = !get().open;
    set({ open });
    // Opening shows every failure, so none of them is news any more.
    if (open)
      get().markSeen(
        useTaskStore
          .getState()
          .tasks.filter((t) => t.status === "failed")
          .map((t) => t.id),
      );
  },
  close: () => set({ open: false }),
  markSeen: (ids) => {
    const seen = get().seen;
    if (ids.every((id) => seen.has(id))) return;
    set({ seen: new Set([...seen, ...ids]) });
  },
}));
