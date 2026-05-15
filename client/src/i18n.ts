// i18n translations for PS5 Upload webapp
//
// The 18 locale dictionaries each live in `src/i18n/locales/<code>.ts`
// and are imported here via per-locale dynamic `import()` so Vite ships
// each as its own chunk. English is loaded eagerly because it's the
// universal fallback and every screen's inline `tr(key, "English…")`
// default needs it ready on first paint.
//
// Non-English locales load on demand: `state/lang.ts::useLangStore`
// kicks off `ensureLocale()` on init (if the user's stored or browser-
// preferred locale isn't English) and on every `setLang()` call. Until
// the chunk resolves, lookups fall back to English (so the UI shows
// English briefly during a locale switch, then re-renders once the
// chunk is loaded). The whole monolithic file used to be 996 KB / 15k
// lines and bundled into the main entry chunk; this split drops the
// main chunk from ~1 MB to ~95 KB.
//
// Mirrors core/src/i18n.rs.

import type { Translations } from "./i18n/types";
import enTranslations from "./i18n/locales/en";

export type LanguageCode =
  | "en"
  | "vi"
  | "hi"
  | "bn"
  | "pt-BR"
  | "ru"
  | "ja"
  | "tr"
  | "id"
  | "th"
  | "ko"
  | "de"
  | "it"
  | "zh-CN"
  | "zh-TW"
  | "fr"
  | "es"
  | "ar";

/** Cache of loaded locale dictionaries. English is pre-populated; the
 *  other 17 land here once their dynamic import resolves. */
const loaded: Partial<Record<LanguageCode, Translations>> = {
  en: enTranslations,
};

/** In-flight load Promises by code — second concurrent caller awaits
 *  the same Promise instead of triggering a redundant import. */
const pending: Partial<Record<LanguageCode, Promise<void>>> = {};

/** Observers re-rendered when a locale finishes loading. `state/lang.ts`
 *  subscribes one entry so React components using `useTr()` re-render
 *  the moment a freshly-selected locale's chunk arrives, swapping out
 *  the temporary English fallback for the real translations. */
const loadListeners = new Set<() => void>();

export function subscribeLocaleLoaded(fn: () => void): () => void {
  loadListeners.add(fn);
  return () => loadListeners.delete(fn);
}

/** Dynamic-import dispatch. Each case is a literal string so Vite can
 *  static-analyze the import and emit a separate chunk per locale. */
function importLocale(code: LanguageCode): Promise<{ default: Translations }> {
  switch (code) {
    case "en":
      // Already loaded eagerly above; the resolver never reaches here in
      // practice but keeps the union type total.
      return Promise.resolve({ default: enTranslations });
    case "vi":
      return import("./i18n/locales/vi");
    case "hi":
      return import("./i18n/locales/hi");
    case "bn":
      return import("./i18n/locales/bn");
    case "pt-BR":
      return import("./i18n/locales/pt-BR");
    case "ru":
      return import("./i18n/locales/ru");
    case "ja":
      return import("./i18n/locales/ja");
    case "tr":
      return import("./i18n/locales/tr");
    case "id":
      return import("./i18n/locales/id");
    case "th":
      return import("./i18n/locales/th");
    case "ko":
      return import("./i18n/locales/ko");
    case "de":
      return import("./i18n/locales/de");
    case "it":
      return import("./i18n/locales/it");
    case "zh-CN":
      return import("./i18n/locales/zh-CN");
    case "zh-TW":
      return import("./i18n/locales/zh-TW");
    case "fr":
      return import("./i18n/locales/fr");
    case "es":
      return import("./i18n/locales/es");
    case "ar":
      return import("./i18n/locales/ar");
  }
}

/** Idempotent loader: kicks off the dynamic import for `code` if it
 *  isn't already cached or in flight, returns the Promise either way.
 *  Notifies `subscribeLocaleLoaded` observers when the chunk lands. */
export function ensureLocale(code: LanguageCode): Promise<void> {
  if (loaded[code]) return Promise.resolve();
  const inflight = pending[code];
  if (inflight) return inflight;
  const p = (async () => {
    try {
      const mod = await importLocale(code);
      loaded[code] = mod.default;
      for (const fn of loadListeners) fn();
    } finally {
      delete pending[code];
    }
  })();
  pending[code] = p;
  return p;
}

/** Synchronously look up `key` in `lang`'s dictionary. Three-level
 *  fallback chain: active locale → English → the key itself. The "raw
 *  key" return is the signal `useTr()` uses to substitute inline
 *  fallbacks (`tr(key, "English default")`).
 *
 *  Vars interpolation: `{name}` placeholders are replaced from `vars`.
 *  Unknown placeholders stay verbatim (signal to the developer that a
 *  call site is using a key that doesn't have that placeholder, or
 *  vice versa). */
/** Dev-mode safety net for the "key in code but not in any dictionary"
 *  class of i18n gap that bypasses both the ESLint rule and the
 *  phantom extractor. Triggered when a `tr(key, ...)` call hits a key
 *  that isn't in the active locale AND isn't in English — the caller
 *  is about to fall back to the inline `fallback` string passed at
 *  the call site (via `useTr()`), which means non-English users will
 *  see English no matter what. Common cause: data-driven UIs that
 *  invoke `tr(item.key, item.fallback)` over an array of `{key,
 *  fallback}` objects (Sidebar nav, command palette items, table
 *  configs); the extractor needs literal `tr("x","y")` pairs to
 *  harvest, so those keys never land in en.ts.
 *
 *  Warnings are deduped per key so a 100-item nav doesn't flood the
 *  console. Production builds (import.meta.env.DEV false) skip the
 *  whole check — zero runtime cost. */
const warnedMissingKeys = new Set<string>();

export function t(
  lang: string,
  key: string,
  vars?: Record<string, string | number>,
): string {
  const langCode = (lang in loaded ? lang : "en") as LanguageCode;
  const dict = loaded[langCode] ?? loaded.en!;
  const raw = dict[key] ?? loaded.en![key] ?? key;
  if (import.meta.env?.DEV) {
    if (
      raw === key &&
      !(key in dict) &&
      !(key in (loaded.en ?? {})) &&
      !warnedMissingKeys.has(key)
    ) {
      warnedMissingKeys.add(key);
      console.warn(
        `[i18n] missing key "${key}" — not in ${langCode} or en. ` +
          "The call site's fallback string will render; non-English " +
          "users will see English. Add this key to client/src/i18n/locales/en.ts.",
      );
    }
  }
  if (!vars) return raw;
  return raw.replace(/\{(\w+)\}/g, (match, name) =>
    Object.prototype.hasOwnProperty.call(vars, name)
      ? String(vars[name])
      : match,
  );
}

/** Test-only escape hatch: synchronously install a locale dict (used
 *  by `i18n.test.ts` to pre-populate all 18 locales before running
 *  fallback-chain assertions, since the test runner can't await between
 *  setup and `expect`). Never called from production code paths. */
export function __testInstallLocale(code: LanguageCode, dict: Translations) {
  loaded[code] = dict;
}
