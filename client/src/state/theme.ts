import { create } from "zustand";
import { safeGetItem, safeSetItem } from "../lib/safeStorage";

/** Two modes: light (the warm daytime atmosphere) and dark (the same
 *  language at night). Earlier versions also had "oled" and "rose"; a stored
 *  value from those is read as its nearest mode (see `normalizeTheme`).
 *
 *  Storage key kept at the original "ps5upload.theme" so users don't
 *  lose their setting across the upgrade. */
export type Theme = "dark" | "light";

const STORAGE_KEY = "ps5upload.theme";

/** A stored or mirrored theme value → one of the two modes. "oled" was a
 *  darker dark and "rose" a warmer light, so each keeps the user on the side
 *  they picked. Anything unknown is null (the caller picks the default). */
export function normalizeTheme(value: unknown): Theme | null {
  switch (value) {
    case "dark":
    case "oled":
      return "dark";
    case "light":
    case "rose":
      return "light";
    default:
      return null;
  }
}

/** The other mode: the toggle flips between the two. */
export function nextTheme(current: Theme): Theme {
  return current === "light" ? "dark" : "light";
}

/** Read the persisted theme synchronously so the first paint is correct.
 *  Returning "dark" as the fallback keeps parity with the app's historical
 *  look for users who've never toggled. A legacy value is rewritten in place
 *  so the stored key only ever holds one of the two modes from here on. */
function initialTheme(): Theme {
  if (typeof window === "undefined") return "dark";
  const stored = safeGetItem(STORAGE_KEY);
  const theme = normalizeTheme(stored) ?? "dark";
  if (stored !== null && stored !== theme) safeSetItem(STORAGE_KEY, theme);
  return theme;
}

/** Write the theme attribute onto <html>. `index.css` keys the light tokens
 *  off `:root[data-theme="light"]`; dark is the attribute-less default, so
 *  we remove the attr rather than set it. */
function applyTheme(theme: Theme) {
  if (typeof document === "undefined") return;
  if (theme === "dark") {
    delete document.documentElement.dataset.theme;
  } else {
    document.documentElement.dataset.theme = theme;
  }
}

interface ThemeState {
  theme: Theme;
  setTheme: (theme: Theme) => void;
  /** Flips light ↔ dark. */
  toggleTheme: () => void;
}

export const useThemeStore = create<ThemeState>((set, get) => ({
  theme: initialTheme(),
  setTheme: (theme) => {
    safeSetItem(STORAGE_KEY, theme);
    applyTheme(theme);
    set({ theme });
  },
  toggleTheme: () => {
    const next = nextTheme(get().theme);
    safeSetItem(STORAGE_KEY, next);
    applyTheme(next);
    set({ theme: next });
  },
}));

// Apply the initial theme on module load so the very first paint is
// right — beats waiting for React to mount.
if (typeof document !== "undefined") {
  applyTheme(initialTheme());
}
