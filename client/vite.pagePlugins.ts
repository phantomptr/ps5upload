/**
 * What the engine-served pages' builds share (vite.join.config.ts, vite.pad.config.ts): each is one
 * file the engine embeds, so instead of every locale (megabytes) it carries only the strings its
 * own components use, in every language.
 */
import { readFileSync, readdirSync } from "node:fs";
import { fileURLToPath } from "node:url";
import path from "node:path";
import vm from "node:vm";
import postcss from "postcss";
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

/** The page's strings as it carries them (src/join/virtual.d.ts): the keys once, then
 *  each language's texts in that order, `0` where it says what English says (English is the
 *  fallback) or has nothing. The key names are not repeated per language: about a quarter of the
 *  table in 21 languages. */
export function packStrings(
  locales: Record<string, Record<string, unknown>>,
  keys: string[],
): { k: string[]; l: Record<string, (string | 0)[]> } {
  const en = locales.en ?? {};
  const l: Record<string, (string | 0)[]> = {};
  for (const [code, dict] of Object.entries(locales)) {
    l[code] = keys.map((k) => {
      const v = dict[k];
      return typeof v === "string" && (code === "en" || v !== en[k]) ? v : 0;
    });
  }
  return { k: keys, l };
}

/** `key` is still a string literal in the built code (quoted any of the three ways). */
export function stillNamed(code: string, key: string): boolean {
  return code.includes(`"${key}"`) || code.includes(`'${key}'`) || code.includes(`\`${key}\``);
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
      // A module's words that the bundle dropped with the code that said them (a shared module
      // the page uses one function of) stay out: only keys still named in the built code.
      const code = Object.values(bundle)
        .map((f) => (f.type === "chunk" ? f.code : ""))
        .join("\n");
      for (const k of [...keys]) if (!stillNamed(code, k)) keys.delete(k);
      const locales = loadLocales();
      const en = locales.en ?? {};
      const missing = [...keys].filter((k) => !(k in en));
      if (missing.length) this.warn(`${page}: keys not in en.ts: ${missing.join(", ")}`);
      const json = JSON.stringify(packStrings(locales, [...keys].sort()));
      let placed = false;
      for (const file of Object.values(bundle)) {
        if (file.type === "chunk" && file.code.includes(PLACEHOLDER)) {
          file.code = file.code.split(PLACEHOLDER).join(`(${json})`);
          placed = true;
        }
      }
      if (!placed) this.error(`${page}: the strings placeholder is not in the bundle`);
      this.info(`${page} strings: ${keys.size} keys × ${Object.keys(locales).length} languages`);
    },
  };
}

/** `cls` (a class from the stylesheet, unescaped) is named nowhere in the page's code: not whole,
 *  and not as a stem a class could be built from (`"anim-` / `` `anim-${…}` ``). */
export function classUnused(code: string, cls: string): boolean {
  if (code.includes(cls)) return false;
  const dash = cls.indexOf("-");
  if (dash < 1) return true;
  const stem = cls.slice(0, dash + 1);
  return !(code.includes(`"${stem}`) || code.includes(`'${stem}`) || code.includes(`\`${stem}`) || code.includes(` ${stem}\${`));
}

/** Rules of the shared stylesheet (src/index.css's own component classes, which every page gets)
 *  whose classes the page never names. A selector goes only when a class in it is named nowhere;
 *  `:not()`, `:is()`, `:where()` and `:has()` selectors always stay (a missing class can't make
 *  them match less). A rule goes when all its selectors went, an at-rule when it is left empty. */
export function pruneUnusedClasses(css: string, code: string): string {
  const root = postcss.parse(css);
  const CLASS = /\.((?:[\w-]|\\.)+)/g;
  root.walkRules((rule) => {
    if (rule.parent?.type === "atrule" && /keyframes$/i.test((rule.parent as postcss.AtRule).name)) return;
    const keep = rule.selectors.filter((sel) => {
      // (`:not(#\#)` is the cascade-layers polyfill's specificity, not a real condition)
      if (/:(?:not|is|where|has)\(/.test(sel.replace(/:not\(#\\#\)/g, ""))) return true;
      for (const m of sel.matchAll(CLASS)) {
        if (classUnused(code, m[1].replace(/\\/g, ""))) return false;
      }
      return true;
    });
    if (!keep.length) rule.remove();
    else if (keep.length !== rule.selectors.length) rule.selectors = keep;
  });
  let removed = true;
  while (removed) {
    removed = false;
    root.walkAtRules((at) => {
      if (/^(media|supports|layer|container)$/i.test(at.name) && at.nodes && at.nodes.length === 0) {
        at.remove();
        removed = true;
      }
    });
  }
  return root.toString();
}

/** The page's CSS without the shared stylesheet's rules for classes it never names
 *  ([`pruneUnusedClasses`]); runs on the bundle, before the single file is made. */
export function pruneCss(): Plugin {
  return {
    name: "ps5upload-prune-css",
    apply: "build",
    // after Vite's own CSS step has put the stylesheet in the bundle
    enforce: "post",
    generateBundle(_, bundle) {
      const code = Object.values(bundle)
        .map((f) => (f.type === "chunk" ? f.code : f.fileName.endsWith(".html") ? String(f.source) : ""))
        .join("\n");
      for (const file of Object.values(bundle)) {
        if (file.type === "asset" && file.fileName.endsWith(".css")) {
          const before = String(file.source);
          file.source = pruneUnusedClasses(before, code);
          this.info(`css: ${(before.length / 1024).toFixed(1)} → ${(file.source.length / 1024).toFixed(1)} KiB without classes the page never names`);
        }
      }
    },
  };
}

/** One language per `:lang()` in the built CSS (postcss/langLists.mjs says why), plus the
 * fallbacks old WebKit needs (postcss/oldWebkit.mjs). */
export function oneLangPerSelector(): Plugin {
  return {
    name: "ps5upload-one-lang-per-selector",
    apply: "build",
    // After Vite's own CSS step has put the stylesheet in the bundle: before that the guest pages'
    // CSS was not there yet, and their `:lang(a,b,…)` lists went out unsplit.
    enforce: "post",
    generateBundle(_, bundle) {
      for (const file of Object.values(bundle)) {
        if (file.type === "asset" && file.fileName.endsWith(".css") && typeof file.source === "string") {
          file.source = addOldWebkitFallbacks(splitLangLists(file.source));
        }
      }
    },
  };
}
