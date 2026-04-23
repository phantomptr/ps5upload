import { create } from "zustand";

export type Theme = "dark" | "light";

const STORAGE_KEY = "ps5upload.theme";

/** Read the persisted theme synchronously so the first paint is correct.
 *  Returning "dark" as the fallback keeps parity with the app's historical
 *  look for users who've never toggled. */
function initialTheme(): Theme {
  if (typeof window === "undefined") return "dark";
  const stored = window.localStorage.getItem(STORAGE_KEY);
  return stored === "light" ? "light" : "dark";
}

/** Write the theme attribute onto <html>. Our `index.css` keys the light
 *  override off `:root[data-theme="light"]`; the dark theme is the
 *  attribute-less default so we remove the attr rather than set it. */
function applyTheme(theme: Theme) {
  if (typeof document === "undefined") return;
  if (theme === "light") {
    document.documentElement.dataset.theme = "light";
  } else {
    delete document.documentElement.dataset.theme;
  }
}

interface ThemeState {
  theme: Theme;
  setTheme: (theme: Theme) => void;
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
    const next: Theme = get().theme === "dark" ? "light" : "dark";
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
