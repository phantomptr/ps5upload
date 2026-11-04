import { t } from "../i18n";
import { useLangStore, type Translator } from "../state/lang";

/**
 * Translate from a plain module (no React hook available).
 *
 * `useTr()` is a hook, so any string produced outside a component — job error
 * messages, install failure hints — was hardcoded English while the rest of
 * the app shipped in 19 languages. These are the strings a user reads at the
 * moment something fails, which is the worst place to fall back to a language
 * they may not read.
 *
 * Reads the active language at call time (so a language switch is picked up)
 * and falls back to the English text passed at the call site, so a key missing
 * from a locale degrades to the previous behaviour rather than showing a raw
 * key. Never throws: a translation lookup must not turn an error message into
 * an exception.
 */
export function trStatic(key: string, fallback: string): string {
  try {
    const lang = useLangStore.getState().lang;
    const out = t(lang, key);
    return out === key ? fallback : out;
  } catch {
    return fallback;
  }
}

/** `trStatic` in the shape of `useTr()`'s translator, for code shared with components. */
export const staticTranslator: Translator = ((
  key: string,
  a?: Record<string, string | number> | string,
  b?: string,
) => {
  const fallback = typeof a === "string" ? a : (b ?? key);
  const text = trStatic(key, fallback);
  return typeof a === "object"
    ? Object.entries(a).reduce((out, [k, v]) => out.split(`{${k}}`).join(String(v)), text)
    : text;
}) as Translator;
