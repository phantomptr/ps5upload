/**
 * Translations for the console page: the `cw_*` keys of the PC app's 21
 * locale files, extracted by scripts/build-console-strings.mjs into
 * locales/<code>.json. Only English is part of the page; every other
 * language is fetched from the helper (GET /strings/<code>.json?v=<hash>,
 * gzip'd into the ELF) when it is chosen, at start or in Settings.
 *
 * Until a language's strings arrive the page stays in the language it shows
 * (English at start; the previous choice when Settings switches: never a flash
 * of English in between), keys a language lacks fall back to English, and a
 * failed or timed-out load (STRINGS_TIMEOUT_MS) leaves the page as it was
 * (logged once). The language follows the PS5's
 * system language (navigator.language) until setLanguage overrides it.
 */
import { LANGUAGE_NAMES } from "./languages";
import en from "./locales/en.json";

export type StringMaps = Record<string, Record<string, string>>;
export type Vars = Record<string, string | number>;
/** Fetches one language's strings (never English). Rejects on any failure. */
export type StringsLoader = (code: string) => Promise<Record<string, string>>;

const EN: Record<string, string> = en;

/** `{name}` placeholders replaced from vars; unknown ones stay as written. */
function interpolate(raw: string, vars?: Vars): string {
  if (!vars) return raw;
  return raw.replace(/\{(\w+)\}/g, (match, name: string) =>
    Object.prototype.hasOwnProperty.call(vars, name) ? String(vars[name]) : match,
  );
}

/**
 * In Arabic or Persian a value written left to right (a path, a size with its unit, a file name)
 * is kept whole and in its own order: "/data" read "data/" and "163 GiB" "GiB 163". It goes between
 * LEFT-TO-RIGHT ISOLATE and POP DIRECTIONAL ISOLATE (drawn as nothing), as the app's tr() does.
 */
function isolated(vars: Vars): Vars {
  const out: Vars = {};
  for (const k of Object.keys(vars)) {
    const s = String(vars[k]);
    out[k] = /[A-Za-z/\\]/.test(s) && !/[֐-ࣿיִ-ﻼ]/.test(s) ? `⁦${s}⁩` : vars[k];
  }
  return out;
}

/** A translator over `maps` for `lang`, falling back to English, then to the key. */
export function makeT(maps: StringMaps, lang: string): (key: string, vars?: Vars) => string {
  const own = maps[lang] ?? {};
  const fallback = maps.en ?? {};
  const rtl = lang === "ar" || lang === "fa";
  return (key, vars) => interpolate(own[key] ?? fallback[key] ?? key, vars && rtl ? isolated(vars) : vars);
}

/**
 * The shipped language closest to a BCP-47 tag: an exact match (any case),
 * then Chinese by script/region (Hant, TW, HK, MO → zh-TW; other zh → zh-CN),
 * then the first shipped code with the same primary language, else English.
 */
export function pickLanguage(tag: string | undefined | null, have: string[]): string {
  if (!tag) return "en";
  const lower = tag.toLowerCase();
  const exact = have.find((c) => c.toLowerCase() === lower);
  if (exact) return exact;
  const parts = lower.split(/[-_]/);
  const primary = parts[0];
  if (primary === "zh") {
    const traditional = parts.some((p) => p === "hant" || p === "tw" || p === "hk" || p === "mo");
    const want = traditional ? "zh-TW" : "zh-CN";
    if (have.includes(want)) return want;
  }
  const same = have.find((c) => c.toLowerCase().split("-")[0] === primary);
  return same ?? "en";
}

/** Every shipped language code, sorted (the locale files' order). */
export const LANGUAGES: string[] = Object.keys(LANGUAGE_NAMES).sort();

/**
 * The strings' build hash (vite.console.config.ts defines it from the emitted files), so the
 * helper's year-long immutable cache never serves another build's strings.
 */
export const STRINGS_VERSION: string = typeof __CW_STRINGS_V__ === "string" ? __CW_STRINGS_V__ : "dev";

/** The URL of one language's strings on the helper. */
export function stringsUrl(code: string): string {
  return `/strings/${encodeURIComponent(code)}.json?v=${encodeURIComponent(STRINGS_VERSION)}`;
}

/** Only string values of `cw_*` keys survive: a damaged file cannot inject anything else. */
export function cleanStrings(raw: unknown): Record<string, string> {
  const out: Record<string, string> = {};
  if (!raw || typeof raw !== "object" || Array.isArray(raw)) return out;
  for (const [k, v] of Object.entries(raw as Record<string, unknown>)) {
    if (k.startsWith("cw_") && typeof v === "string") out[k] = v;
  }
  return out;
}

/** A strings file that has not arrived by then is given up (the page stays as it is). */
export const STRINGS_TIMEOUT_MS = 10_000;

const fetchStrings: StringsLoader = async (code) => {
  const stop = typeof AbortController === "function" ? new AbortController() : null;
  const timer = setTimeout(() => stop?.abort(), STRINGS_TIMEOUT_MS);
  try {
    const res = await fetch(stringsUrl(code), { credentials: "same-origin", signal: stop?.signal });
    if (!res.ok) throw new Error(`HTTP ${res.status}`);
    return cleanStrings(await res.json());
  } finally {
    clearTimeout(timer);
  }
};

const STORAGE_KEY = "ps5upload.console.lang";

/** The language Settings stored, or "" for the system default (also when storage fails). */
export function storedLanguage(): string {
  try {
    const v = localStorage.getItem(STORAGE_KEY);
    return v && LANGUAGES.includes(v) ? v : "";
  } catch {
    return "";
  }
}

function systemLanguage(): string {
  return pickLanguage(typeof navigator !== "undefined" ? navigator.language : "en", LANGUAGES);
}

let loader: StringsLoader = fetchStrings;
/** Languages whose strings arrived (English is always there). */
const loaded: StringMaps = { en: EN };
/** In-flight loads, one per language. */
const pending = new Map<string, Promise<void>>();
let loggedFailure = false;
/** The chosen language (what Settings shows), and the one the page currently speaks. */
let current = storedLanguage() || systemLanguage();
let shown = "en";
let translate = makeT(loaded, shown);
let version = 0;
const listeners = new Set<() => void>();

/** Called after the shown strings change (the page re-renders). Returns the unsubscribe function. */
export function onLanguageChange(fn: () => void): () => void {
  listeners.add(fn);
  return () => {
    listeners.delete(fn);
  };
}

/** Bumped on every change of the shown strings (for useSyncExternalStore). */
export function stringsVersion(): number {
  return version;
}

/** The chosen language (it may still be loading: the page shows English meanwhile). */
export function getLanguage(): string {
  return current;
}

/** Speak `code` now (its strings are loaded) and tell the listeners. */
function show(code: string): void {
  shown = code;
  translate = makeT(loaded, code);
  if (typeof document !== "undefined") {
    document.documentElement.lang = code;
    document.documentElement.dir = code === "ar" || code === "fa" ? "rtl" : "ltr";
  }
  version++;
  listeners.forEach((fn) => fn());
}

/** Fetch `code` once at a time; a failure is forgotten so a later choice can retry it. */
function load(code: string): Promise<void> {
  const inflight = pending.get(code);
  if (inflight) return inflight;
  const p = loader(code)
    .then(
      (map) => {
        loaded[code] = cleanStrings(map);
      },
      (err: unknown) => {
        if (!loggedFailure) {
          loggedFailure = true;
          console.warn(`console page: ${code} strings did not load, staying in ${shown}`, err);
        }
      },
    )
    .then(() => {
      pending.delete(code);
      if (current === code && loaded[code] && shown !== code) show(code);
    });
  pending.set(code, p);
  return p;
}

/**
 * Switch the page's language (Settings' manual override, and main.tsx at start). A language
 * already loaded applies at once; another one is fetched and applies when it arrives, the page
 * keeping the language it shows until then. Re-renders through the listeners whenever the shown
 * strings change.
 */
export function setLanguage(code: string): void {
  current = pickLanguage(code, LANGUAGES);
  if (loaded[current]) {
    show(current);
    return;
  }
  void load(current);
}

/** Resolves when the chosen language's load (if any) has settled. */
export function stringsReady(): Promise<void> {
  return pending.get(current) ?? Promise.resolve();
}

/**
 * Settings' choice: a shipped code, or "" for the system default. Applies (or starts loading) at
 * once and remembers it; a storage failure only loses the memory.
 */
export function chooseLanguage(code: string): void {
  try {
    if (code) localStorage.setItem(STORAGE_KEY, code);
    else localStorage.removeItem(STORAGE_KEY);
  } catch {
    /* private mode or blocked storage: the choice holds until the page closes */
  }
  setLanguage(code || systemLanguage());
  version++;
  listeners.forEach((fn) => fn());
}

/** The translation of `key` in the page's language. */
export function t(key: string, vars?: Vars): string {
  return translate(key, vars);
}

/** Tests: replace the network loader and forget every loaded language but English. */
export function _setStringsLoaderForTests(next: StringsLoader | null): void {
  loader = next ?? fetchStrings;
  for (const k of Object.keys(loaded)) if (k !== "en") delete loaded[k];
  pending.clear();
  loggedFailure = false;
}
