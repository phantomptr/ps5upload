/**
 * Build of the console web page (src/console/) that the helper serves on the
 * PS5 itself: `npm run build:console` → dist-console/index.html, ONE file with
 * the script and styles inlined (payload/Makefile gzips it and embeds it in
 * ps5upload.elf; the helper serves it at GET /).
 *
 * - Target safari13 like the PC app (the PS5 browser is WebKit). BigInt cannot
 *   be down-levelled and is rejected by the client lint rule.
 * - The PC app's theme tokens (dark, light, peach, OLED, high contrast) come from src/index.css itself (pcTokens),
 *   not a copy, so the page follows the app's colours.
 * - Size budget: the build FAILS over 768 KiB raw or 220 KiB gzip. Ruling R19 raised the gzip
 *   cap from 150 KiB when all 21 locales were inlined; since then only English is (consoleStrings).
 *   The page is served gzipped, so the gzip cap is the one that matters; the raw cap went from 640 to
 *   768 KiB when the Games, Fan, Processes and Clean up sections landed together (7.1.18).
 * - Strings: every other locale is emitted next to the page as strings/<code>.json (minified, cw_*
 *   keys only); payload/Makefile gzips each into the ELF (build/webui_strings.c) and the helper
 *   serves them at GET /strings/<code>.json. The page fetches them with ?v=<hash of these files>
 *   (__CW_STRINGS_V__), so the helper's immutable caching never mixes builds.
 * - Offline copy: sw.js and cache.appcache next to the page (offlineCopy.ts), with the page's
 *   hash in them; payload/Makefile embeds both (build/webui_offline.c), served at /sw.js and
 *   /cache.appcache.
 * - Right-to-left: console.css is written with logical properties. At this target the margins,
 *   paddings and borders stay as written (Safari 12.1 has them); `inset-inline-*` (Safari 14.1) is
 *   rewritten by the minifier into left/right pairs picked by `:lang(…)`, which strings.ts keeps in
 *   step with `dir`. splitLangLists then gives each pair one language per `:lang()`: a list makes
 *   Chromium before 141 (a phone opening the page over the LAN) drop both rules.
 */
import { createHash } from "node:crypto";
import { readdirSync, readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { gzipSync } from "node:zlib";
import { defineConfig, type Plugin } from "vite";
import react from "@vitejs/plugin-react";
// @ts-expect-error plain .mjs build helper, no type declarations
import { splitLangLists } from "./postcss/langLists.mjs";
import { appcacheManifest, serviceWorkerSource } from "./src/console/offline/offlineCopy.ts";

const RAW_BUDGET = 768 * 1024;
const GZIP_BUDGET = 220 * 1024; // ruling R19 (was 150 KiB)
const TOKENS_ID = "virtual:pc-tokens.css";

/** The `{…}` body that starts at `open` (the index of a "{"), braces balanced. */
function blockBody(css: string, open: number): string {
  let depth = 0;
  for (let i = open; i < css.length; i++) {
    if (css[i] === "{") depth++;
    else if (css[i] === "}" && --depth === 0) return css.slice(open + 1, i);
  }
  throw new Error("pcTokens: unbalanced braces in src/index.css");
}

/**
 * Every top-level `:root…` rule of index.css that is the dark default or a
 * theme / contrast variant (`[data-theme=…]`, `[data-contrast=…]`, and their
 * combinations), in source order, so later rules keep winning as in the app.
 * Comments are stripped first; @media rules and the other `:root[data-…]`
 * switches (motion, transparency, dyslexia) are left out.
 */
function themeRules(css: string): string[] {
  const src = css.replace(/\/\*[\s\S]*?\*\//g, "");
  const out: string[] = [];
  let depth = 0;
  let start = 0;
  let open = -1;
  for (let i = 0; i < src.length; i++) {
    const c = src[i];
    if (c === "{") {
      if (depth === 0) open = i;
      depth++;
    } else if (c === "}") {
      depth--;
      if (depth === 0) {
        const sel = src.slice(start, open).trim();
        const isRoot = sel === ":root";
        const isVariant = /^:root(\[[^\]]+\]|:is\([^)]*\))+$/.test(sel) && /\[data-(theme|contrast)=/.test(sel) && !/data-(transparency|motion|dyslexia)/.test(sel);
        if (isRoot || isVariant) out.push(`${sel} ${src.slice(open, i + 1)}`);
        start = i + 1;
      }
    } else if (c === ";" && depth === 0) {
      start = i + 1; // an @import line
    }
  }
  if (depth !== 0) throw new Error("pcTokens: unbalanced braces in src/index.css");
  return out;
}

/**
 * virtual:pc-tokens.css = the PC app's font stacks (its Tailwind @theme block)
 * and all of its theme token blocks (dark :root, light, peach, OLED, high
 * contrast), read from src/index.css at build time. Importing index.css
 * itself would pull in Tailwind and every app rule.
 */
function pcTokens(): Plugin {
  const file = fileURLToPath(new URL("./src/index.css", import.meta.url));
  return {
    name: "ps5upload-console-pc-tokens",
    resolveId(id) {
      return id === TOKENS_ID ? `\0${TOKENS_ID}` : undefined;
    },
    load(id) {
      if (id !== `\0${TOKENS_ID}`) return undefined;
      this.addWatchFile(file);
      const css = readFileSync(file, "utf8");
      const theme = css.indexOf("@theme {");
      if (theme < 0) this.error("pcTokens: @theme block not found in src/index.css");
      const rules = themeRules(css);
      // Every theme block and every high-contrast combination must be found, so a rename in
      // index.css fails the build instead of silently shipping a half-themed page. The
      // transparency/backdrop blocks are left out on purpose: console.css uses no backdrop-filter.
      const need = [
        /^:root\[data-theme="light"\]|^:root:is\(\[data-theme="light"\], \[data-theme="peach"\]\)\s*\{/,
        /^:root\[data-theme="peach"\]\s*\{/,
        /^:root\[data-theme="oled"\]\s*\{/,
        /^:root\[data-contrast="high"\]\s*\{/,
        /^:root:is\(\[data-theme="light"\], \[data-theme="peach"\]\)\[data-contrast="high"\]\s*\{/,
        /^:root\[data-theme="oled"\]\[data-contrast="high"\]\s*\{/,
      ];
      for (const re of need) {
        if (!rules.some((r) => re.test(r))) this.error(`pcTokens: no block matching ${re} in src/index.css`);
      }
      const themeBody = blockBody(css, css.indexOf("{", theme));
      return `:root {${themeBody}}\n${rules.join("\n")}\n`;
    },
  };
}

const LOCALES_DIR = fileURLToPath(new URL("./src/console/locales", import.meta.url));

/** Every committed locale but English (inlined), minified: { code: json text }. */
function otherLocales(): Record<string, string> {
  const out: Record<string, string> = {};
  for (const f of readdirSync(LOCALES_DIR).filter((n) => n.endsWith(".json")).sort()) {
    const code = f.slice(0, -".json".length);
    if (code === "en") continue;
    if (!/^[a-z]{2}(-[A-Z]{2})?$/.test(code)) throw new Error(`consoleStrings: locale file name ${f} is not <ll> or <ll>-<RR>`);
    out[code] = JSON.stringify(JSON.parse(readFileSync(`${LOCALES_DIR}/${f}`, "utf8")));
  }
  return out;
}

/** Defines __CW_STRINGS_V__ and emits strings/<code>.json for every locale but English. */
function consoleStrings(): Plugin {
  const files = otherLocales();
  const hash = createHash("sha256");
  for (const [code, json] of Object.entries(files)) hash.update(`${code}\0${json}\0`);
  const version = hash.digest("hex").slice(0, 12);
  return {
    name: "ps5upload-console-strings",
    config: () => ({ define: { __CW_STRINGS_V__: JSON.stringify(version) } }),
    generateBundle() {
      if (Object.keys(files).length === 0) this.error("consoleStrings: no locales besides English in src/console/locales");
      let gz = 0;
      for (const [code, json] of Object.entries(files)) {
        this.emitFile({ type: "asset", fileName: `strings/${code}.json`, source: json });
        gz += gzipSync(json, { level: 9 }).length;
      }
      this.info(`console strings: ${Object.keys(files).length} locales, ${(gz / 1024).toFixed(1)} KiB gzip together, v=${version}`);
    },
  };
}

/** Move every emitted script and stylesheet into index.html, then enforce the budget. */
function singleFile(): Plugin {
  return {
    name: "ps5upload-console-single-file",
    apply: "build",
    enforce: "post",
    generateBundle(_, bundle) {
      const html = Object.values(bundle).find((f) => f.type === "asset" && f.fileName === "index.html");
      if (!html || html.type !== "asset") return this.error("singleFile: no index.html in the bundle");
      let page = String(html.source);
      for (const [name, file] of Object.entries(bundle)) {
        if (file.type === "chunk") {
          const re = new RegExp(`<script[^>]*src="[^"]*${escapeRe(name)}"[^>]*></script>`);
          if (!re.test(page)) return this.error(`singleFile: ${name} is not referenced by index.html`);
          // Escape "</script" so the code cannot close its own tag.
          const code = file.code.replace(/<\/script/gi, "<\\/script");
          page = page.replace(re, () => `<script type="module">${code}</script>`);
          delete bundle[name];
        } else if (file.fileName.endsWith(".css")) {
          const re = new RegExp(`<link[^>]*href="[^"]*${escapeRe(name)}"[^>]*>`);
          if (!re.test(page)) return this.error(`singleFile: ${name} is not referenced by index.html`);
          // One language per `:lang()` (postcss/langLists.mjs says why). Here because the one CSS asset
          // of cssCodeSplit:false is only emitted in Vite's own generateBundle, after normal plugins.
          const css = splitLangLists(String(file.source)).replace(/<\/style/gi, "<\\/style");
          page = page.replace(re, () => `<style>${css}</style>`);
          delete bundle[name];
        }
      }
      // consoleStrings' strings/<code>.json stay separate files (the helper serves them).
      const left = Object.keys(bundle).filter((n) => n !== "index.html" && !/^strings\/[^/]+\.json$/.test(n));
      if (left.length) return this.error(`singleFile: files left outside index.html: ${left.join(", ")}`);
      html.source = page;

      const raw = Buffer.byteLength(page);
      const gz = gzipSync(page, { level: 9 }).length;
      const kib = (n: number) => `${(n / 1024).toFixed(1)} KiB`;
      this.info(`console page: ${kib(raw)} raw, ${kib(gz)} gzip (budget ${kib(RAW_BUDGET)} / ${kib(GZIP_BUDGET)})`);
      if (raw > RAW_BUDGET || gz > GZIP_BUDGET) {
        this.error(`console page over budget: ${kib(raw)} raw (max ${kib(RAW_BUDGET)}), ${kib(gz)} gzip (max ${kib(GZIP_BUDGET)})`);
      }

      // The offline copy (src/console/offline/offlineCopy.ts): a service worker and an appcache
      // manifest named after this page's hash, so a new build replaces the copy.
      const version = createHash("sha256").update(page).digest("hex").slice(0, 12);
      this.emitFile({ type: "asset", fileName: "sw.js", source: serviceWorkerSource(version) });
      this.emitFile({ type: "asset", fileName: "cache.appcache", source: appcacheManifest(version) });
    },
  };
}

function escapeRe(s: string): string {
  return s.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
}

/** The app version the page was built with (package.json; the helper's /api/settings version is the same). */
const APP_VERSION = (JSON.parse(readFileSync(fileURLToPath(new URL("./package.json", import.meta.url)), "utf8")) as { version: string }).version;

export default defineConfig({
  define: { __CW_APP_V__: JSON.stringify(APP_VERSION) },
  root: fileURLToPath(new URL("./src/console", import.meta.url)),
  base: "./",
  publicDir: false,
  plugins: [react(), pcTokens(), consoleStrings(), singleFile()],
  clearScreen: false,
  build: {
    outDir: fileURLToPath(new URL("./dist-console", import.meta.url)),
    emptyOutDir: true,
    target: "safari13",
    cssTarget: "safari13",
    // One chunk, no preload helper: everything ends up inline.
    modulePreload: false,
    cssCodeSplit: false,
    assetsInlineLimit: Number.MAX_SAFE_INTEGER,
    reportCompressedSize: false,
    rollupOptions: {
      // @ts-expect-error checks is a rolldown passthrough not typed by Vite (see vite.config.ts)
      checks: { pluginTimings: false },
    },
  },
});
