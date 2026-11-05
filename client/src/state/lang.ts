import { useCallback, useEffect, useState } from "react";
import { create } from "zustand";
import {
  t as translate,
  ensureLocaleWithRetry,
  interpolate,
  pickLanguage,
  subscribeLocaleLoaded,
  type LanguageCode,
} from "../i18n";
import { safeGetItem, safeSetItem } from "../lib/safeStorage";

const STORAGE_KEY = "ps5upload.lang";

/**
 * The 21 languages we ship with, one per file in `src/i18n/locales/`.
 * RTL detection is handled in `applyLang()` below; Arabic and Persian
 * trigger it (`RTL_LANGS`).
 */
export const LANGUAGES: { code: LanguageCode; label: string }[] = [
  { code: "en", label: "English" },
  { code: "zh-CN", label: "简体中文" },
  { code: "zh-TW", label: "繁體中文" },
  { code: "hi", label: "हिन्दी" },
  { code: "es", label: "Español" },
  { code: "ar", label: "العربية" },
  { code: "bn", label: "বাংলা" },
  { code: "pl", label: "Polski" },
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
  { code: "hu", label: "Magyar" },
  { code: "fa", label: "فارسی" },
];

export const RTL_LANGS: Set<LanguageCode> = new Set(["ar", "fa"]);

function initialLang(): LanguageCode {
  if (typeof window === "undefined") return "en";
  const stored = safeGetItem(STORAGE_KEY);
  if (stored && LANGUAGES.some((l) => l.code === stored)) {
    return stored as LanguageCode;
  }
  // Best-effort: the browser/OS preference list, in BCP-47 ("zh-HK", "pt-PT", "en-US").
  const wanted = navigator.languages?.length ? navigator.languages : [navigator.language];
  return pickLanguage(wanted, LANGUAGES.map((l) => l.code));
}

/** Load a language's strings. If they cannot be loaded even on a retry, the app stays in English
 *  and says so once. */
function loadLocale(lang: LanguageCode) {
  void ensureLocaleWithRetry(lang, (err) => {
    console.warn(`[i18n] could not load the ${lang} strings`, err);
    void import("./toasts").then(({ useToastStore }) =>
      useToastStore.getState().push({ tone: "warn", message: translate(lang, "lang_load_failed") }),
    );
  });
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
    safeSetItem(STORAGE_KEY, lang);
    applyLang(lang);
    set({ lang });
    // Kick off the lazy chunk fetch for the newly selected locale.
    // `t()` falls back to English until the import resolves; the
    // load-listener subscribed in `useTr()` re-renders consumers as
    // soon as the dict is in the cache.
    if (lang !== "en") loadLocale(lang);
  },
}));

// Apply the initial lang on module load so <html dir> is right for the
// first paint — avoids LTR-to-RTL flicker for Arabic users.
if (typeof document !== "undefined") {
  applyLang(initialLang());
}
// Trigger the initial locale's chunk fetch as soon as the module loads
// (most users boot into a locale they've used before, stored in
// localStorage — kicking off the import here means it's racing the
// React render rather than waiting for the first `setLang()` call).
{
  const lang0 = initialLang();
  if (lang0 !== "en") loadLocale(lang0);
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
/**
 * Translator function returned by `useTr()`. Three call shapes are
 * supported so components can pick the form that reads cleanly:
 *
 *   tr("foo")                        — plain lookup, returns the key
 *                                     itself if unknown.
 *   tr("foo", "Foo fallback")        — lookup with English fallback if
 *                                     key is missing in the active
 *                                     locale AND in English. Avoids
 *                                     the `tr(key, {}, fallback)` boilerplate
 *                                     for variable-free strings.
 *   tr("foo {x}", { x: 1 })          — lookup with `{name}` interpolation.
 *   tr("foo {x}", { x: 1 }, "Foo {x}") — lookup + interpolation + fallback.
 */
export interface Translator {
  (key: string): string;
  (key: string, fallback: string): string;
  (
    key: string,
    vars: Record<string, string | number> | undefined,
    fallback?: string,
  ): string;
}

export function useTr(): Translator {
  const lang = useLangStore((s) => s.lang);
  // `bump` increments every time *any* locale finishes its lazy load.
  // Components using `useTr()` need to re-render at that moment so the
  // freshly-cached dictionary replaces the temporary English fallback
  // they've been showing during the import. Without this hook, a user
  // switching to Japanese would stay on English until something else
  // unrelated triggered their next render.
  const [bump, setBump] = useState(0);
  useEffect(() => subscribeLocaleLoaded(() => setBump((n) => n + 1)), []);
  return useCallback<Translator>(
    (
      key: string,
      varsOrFallback?: Record<string, string | number> | string,
      fallback?: string,
    ): string => {
      let vars: Record<string, string | number> | undefined;
      let fb: string | undefined;
      if (typeof varsOrFallback === "string") {
        // 2-arg form: tr(key, fallback)
        fb = varsOrFallback;
      } else {
        // 1- or 3-arg form: tr(key) / tr(key, vars) / tr(key, vars, fallback)
        vars = varsOrFallback;
        fb = fallback;
      }
      const result = translate(lang, key, vars);
      // The fallback takes the same `{name}` placeholders as the catalogue
      // text (see the Translator doc), so a missing key still reads whole.
      if (result === key && fb !== undefined) return vars ? interpolate(fb, vars) : fb;
      return result;
    },
    // `bump` is intentionally part of the dep list so the closure is
    // re-created when a lazy locale resolves; without it, components
    // that ran `useTr` once would keep the stale closure with the old
    // lookup table. exhaustive-deps thinks the dep is unused because
    // it's not referenced in the body — it's the *signal*, not the
    // input.
    // eslint-disable-next-line react-hooks/exhaustive-deps
    [lang, bump],
  );
}
