/**
 * What the engine-served pages' builds share (vite.join.config.ts, vite.pad.config.ts): each is one
 * file the engine embeds, so instead of every locale (megabytes) it carries only the strings its
 * own components use, in every language.
 */
import { readFileSync, readdirSync } from "node:fs";
import { fileURLToPath } from "node:url";
import path from "node:path";
import vm from "node:vm";
import type { Plugin } from "vite";
// @ts-expect-error plain .mjs helper shared with its vitest test, no type declarations
import { splitLangLists } from "./postcss/langLists.mjs";
// @ts-expect-error plain .mjs helper shared with its vitest test, no type declarations
import { addOldWebkitFallbacks } from "./postcss/oldWebkit.mjs";

const SRC = fileURLToPath(new URL("./src/", import.meta.url));
const LOCALES = fileURLToPath(new URL("./src/i18n/locales/", import.meta.url));
const LANG = fileURLToPath(new URL("./src/state/lang.ts", import.meta.url));
const PAGE_LANG = fileURLToPath(new URL("./src/join/joinLang.ts", import.meta.url));
const TR_STATIC = fileURLToPath(new URL("./src/lib/trStatic.ts", import.meta.url));
const TR_STATIC_PAGE = fileURLToPath(new URL("./src/join/trStaticJoin.ts", import.meta.url));
const VIRTUAL = "virtual:join-strings";
const PLACEHOLDER = "__PS5UPLOAD_PAGE_STRINGS__";

/** Every locale file's object literal, evaluated the way scripts/i18n-coverage.mjs does (the
 *  files mix quoting styles, so no regex over their lines). */
export function loadLocales(dir = LOCALES): Record<string, Record<string, string>> {
  const out: Record<string, Record<string, string>> = {};
  for (const f of readdirSync(dir)) {
    if (!f.endsWith(".ts") || f.endsWith(".d.ts")) continue;
    const src = readFileSync(path.join(dir, f), "utf8");
    const start = src.match(/^const\s+[A-Za-z_][A-Za-z0-9_]*\s*:\s*Translations\s*=\s*/m);
    if (!start || start.index === undefined) throw new Error(`${f}: no 'const <name>: Translations ='`);
    const from = start.index + start[0].length;
    const end = src.slice(from).match(/\n};\s*\n\s*export default/);
    if (!end || end.index === undefined) throw new Error(`${f}: no end of the literal`);
    const literal = src.slice(from, from + end.index + 2);
    out[f.replace(/\.ts$/, "")] = vm.runInNewContext(`(${literal})`, {}) as Record<string, string>;
  }
  return out;
}

/** The keys a source file looks up (`tr("key"` / `trStatic("key"`, across lines too). */
export function keysIn(source: string): string[] {
  return [...source.matchAll(/\b(?:tr|trStatic)\(\s*"([\w.]+)"/g)].map((m) => m[1]);
}

/** The modules a page swaps (see `pageStrings`), for what walks its files without the bundler. */
export const pageModuleSwap = (file: string): string =>
  file === LANG ? PAGE_LANG : file === TR_STATIC ? TR_STATIC_PAGE : file;

/** state/lang and lib/trStatic → the page's own (src/join/joinLang.ts); the strings as a
 *  placeholder, filled after bundling with exactly the keys the bundled modules use. */
export function pageStrings(page: string): Plugin {
  return {
    name: `ps5upload-${page}-strings`,
    enforce: "pre",
    async resolveId(source, importer, opts) {
      if (source === VIRTUAL) return `\0${VIRTUAL}`;
      const r = await this.resolve(source, importer, { ...opts, skipSelf: true });
      if (r?.id === LANG) return PAGE_LANG;
      return r?.id === TR_STATIC ? TR_STATIC_PAGE : null;
    },
    load(id) {
      return id === `\0${VIRTUAL}` ? `export default ${PLACEHOLDER};` : null;
    },
    generateBundle(_, bundle) {
      const keys = new Set<string>();
      for (const file of Object.values(bundle)) {
        if (file.type !== "chunk") continue;
        for (const id of Object.keys(file.modules)) {
          const clean = id.split("?")[0];
          if (!clean.startsWith(SRC) || !/\.(tsx?|jsx?)$/.test(clean)) continue;
          for (const k of keysIn(readFileSync(clean, "utf8"))) keys.add(k);
        }
      }
      const locales = loadLocales();
      const en = locales.en ?? {};
      const strings: Record<string, Record<string, string>> = {};
      for (const [code, dict] of Object.entries(locales)) {
        const picked: Record<string, string> = {};
        for (const k of keys) {
          const v = dict[k];
          // Other languages carry only what differs from English (English is the fallback).
          if (typeof v === "string" && (code === "en" || v !== en[k])) picked[k] = v;
        }
        strings[code] = picked;
      }
      const missing = [...keys].filter((k) => !(k in en));
      if (missing.length) this.warn(`${page}: keys not in en.ts: ${missing.join(", ")}`);
      const json = JSON.stringify(strings);
      let placed = false;
      for (const file of Object.values(bundle)) {
        if (file.type === "chunk" && file.code.includes(PLACEHOLDER)) {
          file.code = file.code.split(PLACEHOLDER).join(`(${json})`);
          placed = true;
        }
      }
      if (!placed) this.error(`${page}: the strings placeholder is not in the bundle`);
      this.info(`${page} strings: ${keys.size} keys × ${Object.keys(strings).length} languages`);
    },
  };
}

/** One language per `:lang()` in the built CSS (postcss/langLists.mjs says why), plus the
 * fallbacks old WebKit needs (postcss/oldWebkit.mjs). */
export function oneLangPerSelector(): Plugin {
  return {
    name: "ps5upload-one-lang-per-selector",
    apply: "build",
    generateBundle(_, bundle) {
      for (const file of Object.values(bundle)) {
        if (file.type === "asset" && file.fileName.endsWith(".css") && typeof file.source === "string") {
          file.source = addOldWebkitFallbacks(splitLangLists(file.source));
        }
      }
    },
  };
}
