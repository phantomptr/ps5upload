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
  /** Translate a key under the current lang, with optional `{name}` vars.
   *  If `fallback` is provided and neither the current lang nor English
   *  has the key, returns `fallback` instead of the raw key. Useful for
   *  2.1-era strings that aren't yet in the dictionary — EN users see
   *  the fallback, other-lang users see the EN fallback (standard i18n
   *  degradation). */
  tr: (
    key: string,
    vars?: Record<string, string | number>,
    fallback?: string
  ) => string;
}

export const useLangStore = create<LangState>((set, get) => ({
  lang: initialLang(),
  setLang: (lang) => {
    window.localStorage.setItem(STORAGE_KEY, lang);
    applyLang(lang);
    set({ lang });
  },
  tr: (key, vars, fallback) => {
    const result = translate(get().lang, key, vars);
    // The underlying `t()` returns the raw key when neither the current
    // lang nor English has it. That's our "missing" sentinel — swap in
    // the fallback if provided.
    if (result === key && fallback !== undefined) return fallback;
    return result;
  },
}));

// Apply the initial lang on module load so <html dir> is right for the
// first paint — avoids LTR-to-RTL flicker for Arabic users.
if (typeof document !== "undefined") {
  applyLang(initialLang());
}
