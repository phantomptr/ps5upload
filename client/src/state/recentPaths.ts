import { create } from "zustand";

/**
 * MRU list of remote PS5 filesystem paths the user has navigated to
 * in the FileSystem screen.
 *
 * Capped at 8 entries — beyond that the dropdown becomes scroll-y
 * and the value of "recent" diminishes (older entries are stale by
 * the time the user looks at them).
 *
 * Stored in localStorage so the list survives across launches. The
 * keying is global (not per-PS5): users tend to navigate to the same
 * filesystem layout (`/data/`, `/user/`, `/mnt/usb0/`) regardless of
 * which console they're on.
 */

const STORAGE_KEY = "ps5upload.recent_paths.v1";
const MAX_ENTRIES = 8;

interface RecentPathsState {
  paths: string[];
  push: (p: string) => void;
  remove: (p: string) => void;
  clear: () => void;
}

function loadStored(): string[] {
  if (typeof window === "undefined") return [];
  try {
    const raw = window.localStorage.getItem(STORAGE_KEY);
    if (!raw) return [];
    const parsed = JSON.parse(raw);
    if (!Array.isArray(parsed)) return [];
    return parsed.filter((p): p is string => typeof p === "string").slice(0, MAX_ENTRIES);
  } catch {
    return [];
  }
}

function persist(paths: string[]) {
  if (typeof window === "undefined") return;
  try {
    window.localStorage.setItem(STORAGE_KEY, JSON.stringify(paths));
  } catch {
    // best-effort
  }
}

export const useRecentPathsStore = create<RecentPathsState>((set, get) => ({
  paths: loadStored(),
  push: (p) => {
    const trimmed = p.trim();
    if (!trimmed || trimmed === "/") return;
    // Move-to-front semantics: dedupe + prepend.
    const next = [trimmed, ...get().paths.filter((x) => x !== trimmed)].slice(
      0,
      MAX_ENTRIES,
    );
    set({ paths: next });
    persist(next);
  },
  remove: (p) => {
    const next = get().paths.filter((x) => x !== p);
    set({ paths: next });
    persist(next);
  },
  clear: () => {
    set({ paths: [] });
    persist([]);
  },
}));
