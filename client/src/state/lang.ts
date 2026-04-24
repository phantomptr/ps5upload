import { useCallback } from "react";
import { create } from "zustand";
import { t as translate, type LanguageCode } from "../i18n";

const STORAGE_KEY = "ps5upload.lang";

/**
 * The 18 languages we ship with. Mirrors `translations` in `src/i18n.ts`
 * and the label table from v1.5.4's `App.tsx`. RTL detection is handled
 * in `applyLang()` below; only Arabic triggers it today.
 */
export const LANGUAGES: { code: LanguageCode; label: string }[] = [
  { code: "en", label: "English" },
  { code: "zh-CN", label: "简体中文" },
  { code: "zh-TW", label: "繁體中文" },
  { code: "hi", label: "हिन्दी" },
  { code: "es", label: "Español" },
  { code: "ar", label: "العربية" },
  { code: "bn", label: "বাংলা" },
  { code: "pt-BR", label: "Português (Brasil)" },
  { code: "ru", label: "Русский" },
  { code: "ja", label: "日本語" },
  { code: "de", label: "Deutsch" },
  { code: "fr", label: "Français" },
  { code: "ko", label: "한국어" },
  { code: "tr", label: "Türkçe" },
  { code: "vi", label: "Tiếng Việt" },
  { code: "id", label: "Bahasa Indonesia" },
  { code: "it", label: "Italiano" },
  { code: "th", label: "ไทย" },
];

const RTL_LANGS: Set<LanguageCode> = new Set(["ar"]);

function initialLang(): LanguageCode {
  if (typeof window === "undefined") return "en";
  const stored = window.localStorage.getItem(STORAGE_KEY);
  if (stored && LANGUAGES.some((l) => l.code === stored)) {
    return stored as LanguageCode;
  }
  // Best-effort: try the browser/OS locale. `navigator.language` returns
  // BCP-47 (e.g. "zh-CN", "en-US"). Match exact then language-prefix.
  const nav = navigator.language;
  if (nav) {
    const exact = LANGUAGES.find((l) => l.code === nav);
    if (exact) return exact.code;
    const prefix = nav.split("-")[0];
    const partial = LANGUAGES.find((l) => l.code === prefix);
    if (partial) return partial.code;
  }
  return "en";
}

function applyLang(lang: LanguageCode) {
  if (typeof document === "undefined") return;
  document.documentElement.lang = lang;
  document.documentElement.dir = RTL_LANGS.has(lang) ? "rtl" : "ltr";
}

interface LangState {
  lang: LanguageCode;
  setLang: (lang: LanguageCode) => void;
}

export const useLangStore = create<LangState>((set) => ({
  lang: initialLang(),
  setLang: (lang) => {
    window.localStorage.setItem(STORAGE_KEY, lang);
    applyLang(lang);
    set({ lang });
  },
}));

// Apply the initial lang on module load so <html dir> is right for the
// first paint — avoids LTR-to-RTL flicker for Arabic users.
if (typeof document !== "undefined") {
  applyLang(initialLang());
}

/**
 * Hook-based translator. **Use this from React components**, not
 * `useLangStore((s) => s.tr)`.
 *
 * Why: the `tr` function stored in Zustand is a stable reference
 * captured over `get()` at store-creation time. A `useLangStore(s =>
 * s.tr)` selector returns the same function object on every render,
 * so switching language never triggers a re-render — components
 * using that pattern keep showing the old language until they
 * re-render for an unrelated reason (which is why v2.2.0's Settings
 * *looked* like the only screen respecting language changes: it
 * destructures the whole store and gets re-rendered every time any
 * field mutates).
 *
 * This hook subscribes to `lang` directly. When `lang` changes, the
 * memoized translator returns a fresh closure bound to the new lang,
 * so every component calling `useTr()` re-renders with the new
 * strings.
 */
export function useTr() {
  const lang = useLangStore((s) => s.lang);
  return useCallback(
    (
      key: string,
      vars?: Record<string, string | number>,
      fallback?: string,
    ) => {
      const result = translate(lang, key, vars);
      if (result === key && fallback !== undefined) return fallback;
      return result;
    },
    [lang],
  );
}
