// What the desktop sidebar leaves out.
//
// The sidebar lists every screen, in its sections; people hide what they don't use, rather
// than having to discover and star what they do (the old Favorites model left a new user
// with Home and About, and everything else behind More). Section headers collapse too.
//
// Persisted per machine in localStorage via safeStorage, and mirrored to
// ~/.ps5upload/settings.json by userConfig, like the collapsed-rail flag.

import { create } from "zustand";

import { safeGetItem, safeSetItem } from "../lib/safeStorage";

const HIDDEN_KEY = "ps5upload.desktop-sidebar.hidden.v1";
const CLOSED_KEY = "ps5upload.desktop-sidebar.closed-sections.v1";

/** A stored list of strings, or empty: the value is hand-editable and outlives builds. */
function loadList(key: string): string[] {
  const raw = safeGetItem(key);
  if (!raw) return [];
  try {
    const parsed: unknown = JSON.parse(raw);
    return Array.isArray(parsed) ? parsed.filter((v): v is string => typeof v === "string") : [];
  } catch {
    return [];
  }
}

const strings = (v: readonly unknown[]) => v.filter((x): x is string => typeof x === "string");

function flip(list: string[], value: string): string[] {
  return list.includes(value) ? list.filter((x) => x !== value) : [...list, value];
}

interface NavSidebarState {
  /** Route paths left out of the sidebar. */
  hidden: string[];
  /** Section keys whose items are folded away. */
  closedSections: string[];
  toggleHidden: (to: string) => void;
  toggleSection: (key: string) => void;
  /** Replace both lists — hydrating from the settings-file mirror. */
  setAll: (hidden: readonly unknown[], closedSections: readonly unknown[]) => void;
}

export const useNavSidebarStore = create<NavSidebarState>((set) => ({
  hidden: loadList(HIDDEN_KEY),
  closedSections: loadList(CLOSED_KEY),
  toggleHidden: (to) =>
    set((s) => {
      const hidden = flip(s.hidden, to);
      safeSetItem(HIDDEN_KEY, JSON.stringify(hidden));
      return { hidden };
    }),
  toggleSection: (key) =>
    set((s) => {
      const closedSections = flip(s.closedSections, key);
      safeSetItem(CLOSED_KEY, JSON.stringify(closedSections));
      return { closedSections };
    }),
  setAll: (hiddenIn, closedIn) => {
    const hidden = strings(hiddenIn);
    const closedSections = strings(closedIn);
    safeSetItem(HIDDEN_KEY, JSON.stringify(hidden));
    safeSetItem(CLOSED_KEY, JSON.stringify(closedSections));
    set({ hidden, closedSections });
  },
}));
