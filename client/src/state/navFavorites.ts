// Which screens the user has pinned to the desktop sidebar.
//
// The sidebar used to be a hardcoded five-item list. It now holds Home
// (permanent) plus whatever the user has starred in More, so nobody is
// handed an assumption about which four screens matter to them.
//
// Persisted per-machine in localStorage via safeStorage — the same
// storage the sidebar's collapsed state uses, so a browser that blocks
// site data degrades to "no favorites" instead of throwing.

import { create } from "zustand";

import { safeGetItem, safeSetItem } from "../lib/safeStorage";

const FAVORITES_KEY = "ps5upload.desktop-sidebar.favorites.v1";
const HINT_DISMISSED_KEY = "ps5upload.desktop-sidebar.favorites-hint.v1";

/** Parse the stored list defensively: anything that is not an array of
 *  strings is treated as absent rather than crashing the sidebar. The
 *  value is user-editable on disk and survives downgrades. */
function loadFavorites(): string[] {
  const raw = safeGetItem(FAVORITES_KEY);
  if (!raw) return [];
  try {
    const parsed: unknown = JSON.parse(raw);
    if (!Array.isArray(parsed)) return [];
    return parsed.filter((v): v is string => typeof v === "string");
  } catch {
    return [];
  }
}

interface NavFavoritesState {
  /** Route paths, in the order the user starred them. Never contains Home. */
  favorites: string[];
  /** True once the user dismisses the "star screens in More" hint. */
  hintDismissed: boolean;
  isFavorite: (to: string) => boolean;
  toggle: (to: string) => void;
  dismissHint: () => void;
  /** Replace the whole list — used when hydrating from the
   *  `~/.ps5upload/settings.json` mirror at startup. Non-string entries are
   *  dropped for the same reason `loadFavorites` filters them: the value is
   *  hand-editable on disk. */
  setFavorites: (paths: string[], hintDismissed?: boolean) => void;
}

export const useNavFavoritesStore = create<NavFavoritesState>((set, get) => ({
  favorites: loadFavorites(),
  hintDismissed: safeGetItem(HINT_DISMISSED_KEY) === "1",

  isFavorite: (to) => get().favorites.includes(to),

  toggle: (to) =>
    set((s) => {
      const next = s.favorites.includes(to)
        ? s.favorites.filter((p) => p !== to)
        : [...s.favorites, to];
      safeSetItem(FAVORITES_KEY, JSON.stringify(next));
      // Adding a first favorite answers the hint's question, so retire it
      // without making the user also dismiss it by hand.
      const hintDismissed = s.hintDismissed || next.length > 0;
      if (hintDismissed && !s.hintDismissed) {
        safeSetItem(HINT_DISMISSED_KEY, "1");
      }
      return { favorites: next, hintDismissed };
    }),

  dismissHint: () => {
    safeSetItem(HINT_DISMISSED_KEY, "1");
    set({ hintDismissed: true });
  },

  setFavorites: (paths, hintDismissed) =>
    set((s) => {
      const next = paths.filter((p): p is string => typeof p === "string");
      safeSetItem(FAVORITES_KEY, JSON.stringify(next));
      const nextHint = hintDismissed ?? (s.hintDismissed || next.length > 0);
      safeSetItem(HINT_DISMISSED_KEY, nextHint ? "1" : "0");
      return { favorites: next, hintDismissed: nextHint };
    }),
}));
