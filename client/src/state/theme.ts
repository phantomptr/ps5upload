import { create } from "zustand";

/** Three-way theme:
 *    dark — original default; balanced colours, slight elevation
 *    light — daytime colours, high readability under strong ambient
 *    oled — pure-#000 background variant of dark; mitigates OLED
 *           burn-in for users running this app on an OLED panel
 *           (it's small but persistent UI, so risk is real)
 *
 *  Storage key kept at the original "ps5upload.theme" so users don't
 *  lose their setting across the upgrade. */
export type Theme = "dark" | "light" | "oled";

const STORAGE_KEY = "ps5upload.theme";

const VALID_THEMES: Theme[] = ["dark", "light", "oled"];

/** Read the persisted theme synchronously so the first paint is correct.
 *  Returning "dark" as the fallback keeps parity with the app's historical
 *  look for users who've never toggled. */
function initialTheme(): Theme {
  if (typeof window === "undefined") return "dark";
  const stored = window.localStorage.getItem(STORAGE_KEY) as Theme | null;
  return stored && VALID_THEMES.includes(stored) ? stored : "dark";
}

/** Write the theme attribute onto <html>. Our `index.css` keys both
 *  the light and oled overrides off `:root[data-theme="<name>"]`; the
 *  dark theme is the attribute-less default so we remove the attr
 *  rather than set it. */
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
  /** Cycles dark → light → oled → dark. Three clicks returns to
   *  the starting theme. The picker in Settings (when one exists)
   *  can call setTheme directly for a non-cyclic UX. */
  toggleTheme: () => void;
}

export const useThemeStore = create<ThemeState>((set, get) => ({
  theme: initialTheme(),
  setTheme: (theme) => {
    window.localStorage.setItem(STORAGE_KEY, theme);
    applyTheme(theme);
    set({ theme });
  },
  toggleTheme: () => {
    const order: Theme[] = ["dark", "light", "oled"];
    const idx = order.indexOf(get().theme);
    const next: Theme = order[(idx + 1) % order.length];
    window.localStorage.setItem(STORAGE_KEY, next);
    applyTheme(next);
    set({ theme: next });
  },
}));

// Apply the initial theme on module load so the very first paint is
// right — beats waiting for React to mount.
if (typeof document !== "undefined") {
  applyTheme(initialTheme());
}
