/**
 * The Play Together guest page (src/join/, #641): `npm run build:join` writes ONE file, script and
 * styles inlined, to engine/crates/ps5upload-engine/src/join_page.html, which the engine embeds
 * and serves at GET /join to a friend who opened the host's link or QR code (the same way /pad is
 * served, vite.pad.config.ts). Commit the result.
 *
 * - Localized, unlike /pad: state/lang is swapped for src/join/joinLang.ts, whose strings come
 *   from `virtual:join-strings`: every locale, but only the keys the page's own modules use
 *   (`tr("key"` in their sources), found after bundling and written into the page.
 * - Budget: the build fails over 900 KiB raw.
 */
import { readFileSync, readdirSync } from "node:fs";
import { fileURLToPath } from "node:url";
import path from "node:path";
import vm from "node:vm";
import { defineConfig, type Plugin } from "vite";
import react from "@vitejs/plugin-react";
import tailwindcss from "@tailwindcss/vite";
import { pageCss } from "./vite.pageCss";

const RAW_BUDGET = 900 * 1024;
const OUT = fileURLToPath(new URL("../engine/crates/ps5upload-engine/src/join_page.html", import.meta.url));
const SRC = fileURLToPath(new URL("./src/", import.meta.url));
const LOCALES = fileURLToPath(new URL("./src/i18n/locales/", import.meta.url));
const LANG = fileURLToPath(new URL("./src/state/lang.ts", import.meta.url));
const JOIN_LANG = fileURLToPath(new URL("./src/join/joinLang.ts", import.meta.url));
const TR_STATIC = fileURLToPath(new URL("./src/lib/trStatic.ts", import.meta.url));
const TR_STATIC_JOIN = fileURLToPath(new URL("./src/join/trStaticJoin.ts", import.meta.url));
const VIRTUAL = "virtual:join-strings";
const PLACEHOLDER = "__PS5UPLOAD_JOIN_STRINGS__";

const escapeRe = (s: string) => s.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");

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

/** state/lang and lib/trStatic → the page's own; the strings as a placeholder, filled after
 *  bundling with exactly the keys the bundled modules use. */
function joinStrings(): Plugin {
  return {
    name: "ps5upload-join-strings",
    enforce: "pre",
    async resolveId(source, importer, opts) {
      if (source === VIRTUAL) return `\0${VIRTUAL}`;
      const r = await this.resolve(source, importer, { ...opts, skipSelf: true });
      if (r?.id === LANG) return JOIN_LANG;
      return r?.id === TR_STATIC ? TR_STATIC_JOIN : null;
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
      if (missing.length) this.warn(`join: keys not in en.ts: ${missing.join(", ")}`);
      const json = JSON.stringify(strings);
      let placed = false;
      for (const file of Object.values(bundle)) {
        if (file.type === "chunk" && file.code.includes(PLACEHOLDER)) {
          file.code = file.code.split(PLACEHOLDER).join(`(${json})`);
          placed = true;
        }
      }
      if (!placed) this.error("join: the strings placeholder is not in the bundle");
      this.info(`join strings: ${keys.size} keys × ${Object.keys(strings).length} languages`);
    },
  };
}

/** Everything into index.html, written to the engine's join_page.html. */
function singleFile(): Plugin {
  return {
    name: "ps5upload-join-single-file",
    apply: "build",
    enforce: "post",
    async generateBundle(_, bundle) {
      const html = Object.values(bundle).find((f) => f.type === "asset" && f.fileName.endsWith("index.html"));
      if (!html || html.type !== "asset") return this.error("join: no index.html in the bundle");
      let page = String(html.source);
      for (const [name, file] of Object.entries(bundle)) {
        if (file.type === "chunk") {
          const re = new RegExp(`<script[^>]*src="[^"]*${escapeRe(name)}"[^>]*></script>`);
          if (!re.test(page)) return this.error(`join: ${name} is not referenced by index.html`);
          const code = file.code.replace(/<\/script/gi, "<\\/script");
          page = page.replace(re, () => `<script type="module">${code}</script>`);
        } else if (file.fileName.endsWith(".css")) {
          const re = new RegExp(`<link[^>]*href="[^"]*${escapeRe(name)}"[^>]*>`);
          if (!re.test(page)) return this.error(`join: ${name} is not referenced by index.html`);
          page = page.replace(re, () => `<style>${String(file.source).replace(/<\/style/gi, "<\\/style")}</style>`);
        }
      }
      const raw = Buffer.byteLength(page);
      this.info(`join page: ${(raw / 1024).toFixed(1)} KiB (budget ${RAW_BUDGET / 1024} KiB)`);
      if (raw > RAW_BUDGET) this.error(`join page over budget: ${(raw / 1024).toFixed(1)} KiB`);
      const { writeFileSync } = await import("node:fs");
      writeFileSync(OUT, page);
    },
  };
}

export default defineConfig({
  base: "./",
  plugins: [
    joinStrings(),
    pageCss(fileURLToPath(new URL("./src/join/main.tsx", import.meta.url)), (f) =>
      f === LANG ? JOIN_LANG : f === TR_STATIC ? TR_STATIC_JOIN : f,
    ),
    react(),
    tailwindcss(),
    singleFile(),
  ],
  build: {
    outDir: fileURLToPath(new URL("./dist-join", import.meta.url)),
    emptyOutDir: true,
    target: "safari14",
    cssCodeSplit: false,
    assetsInlineLimit: Number.MAX_SAFE_INTEGER,
    modulePreload: false,
    rollupOptions: {
      input: fileURLToPath(new URL("./src/join/index.html", import.meta.url)),
      output: { inlineDynamicImports: true },
    },
  },
});
