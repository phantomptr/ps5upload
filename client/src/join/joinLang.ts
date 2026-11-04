// state/lang for the /join page (vite.join.config.ts swaps it in): the page is one file the
// host's engine serves, so instead of every locale (megabytes) it carries only the strings its
// own components use, in every language, picked at build time from src/i18n/locales. The
// friend's browser language chooses (or the app's saved choice, on a device that has it).

import strings from "virtual:join-strings";

type Vars = Record<string, string | number>;

const RTL = new Set(["ar", "fa"]);

function pickLang(): string {
  const have = Object.keys(strings);
  try {
    const saved = localStorage.getItem("ps5upload.lang");
    if (saved && have.includes(saved)) return saved;
  } catch {
    // No storage: the browser's language.
  }
  const wanted = typeof navigator === "undefined" ? [] : [...(navigator.languages ?? []), navigator.language];
  for (const w of wanted) {
    if (!w) continue;
    if (have.includes(w)) return w;
    const prefix = w.split("-")[0];
    const exactPrefix = have.find((c) => c === prefix);
    if (exactPrefix) return exactPrefix;
    // zh-HK → zh-TW, zh → zh-CN, pt-PT → pt-BR.
    if (prefix === "zh") return /TW|HK|MO|Hant/i.test(w) ? "zh-TW" : "zh-CN";
    const near = have.find((c) => c.split("-")[0] === prefix);
    if (near) return near;
  }
  return "en";
}

const lang = pickLang();
if (typeof document !== "undefined") {
  document.documentElement.lang = lang;
  document.documentElement.dir = RTL.has(lang) ? "rtl" : "ltr";
}
const dict = strings[lang] ?? {};
const en = strings.en ?? {};

const fill = (s: string, vars?: Vars) => (vars ? s.replace(/\{(\w+)\}/g, (m, k) => (k in vars ? String(vars[k]) : m)) : s);

export interface Translator {
  (key: string): string;
  (key: string, fallback: string): string;
  (key: string, vars: Vars | undefined, fallback?: string): string;
}

const tr: Translator = ((key: string, a?: Vars | string, b?: string) => {
  const vars = typeof a === "string" ? undefined : a;
  const fallback = typeof a === "string" ? a : b;
  const raw = dict[key] ?? en[key] ?? fallback ?? key;
  return fill(raw, vars);
}) as Translator;

export function useTr(): Translator {
  return tr;
}

/** lib/trStatic for the page. */
export function trStatic(
  key: string,
  fallback: string,
  vars?: Record<string, string | number>,
): string {
  return fill(dict[key] ?? en[key] ?? fallback, vars);
}
