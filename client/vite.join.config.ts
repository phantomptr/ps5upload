/**
 * The Play Together guest page (src/join/, #641): `npm run build:join` writes ONE file, script and
 * styles inlined, to engine/crates/ps5upload-engine/src/join_page.html, which the engine embeds
 * and serves at GET /join to a friend who opened the host's link or QR code (the same way /pad is
 * served, vite.pad.config.ts). Commit the result.
 *
 * - Localized, unlike /pad: state/lang is swapped for src/join/joinLang.ts, whose strings come
 *   from `virtual:join-strings`: every locale, but only the keys the page's own modules use
 *   (`tr("key"` in their sources), found after bundling and written into the page (shared with
 *   /pad, vite.pagePlugins.ts).
 * - The built CSS goes through what the app's does (oneLangPerSelector): older Android WebViews
 *   and Safari 14.0 are friends' phones too.
 * - Budget: the build fails over 900 KiB raw.
 */
import { fileURLToPath } from "node:url";
import { defineConfig, type Plugin } from "vite";
import react from "@vitejs/plugin-react";
import tailwindcss from "@tailwindcss/vite";
import { oneLangPerSelector, pageModuleSwap, pageStrings } from "./vite.pagePlugins";
import { pageCss } from "./vite.pageCss";

const RAW_BUDGET = 900 * 1024;
const OUT = fileURLToPath(new URL("../engine/crates/ps5upload-engine/src/join_page.html", import.meta.url));

const escapeRe = (s: string) => s.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");

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
    pageStrings("join"),
    pageCss(fileURLToPath(new URL("./src/join/main.tsx", import.meta.url)), pageModuleSwap),
    react(),
    tailwindcss(),
    oneLangPerSelector(),
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
