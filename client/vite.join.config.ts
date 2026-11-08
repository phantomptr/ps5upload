/**
 * The Play Together guest page (src/join/, #641): `npm run build:join` writes ONE file, script and
 * styles inlined, to engine/crates/ps5upload-engine/src/join_page.html, which the engine embeds
 * and serves at GET /join to a friend who opened the host's link or QR code (the same way /pad is
 * served, vite.pad.config.ts). The same file goes to site/join/index.html, the website's copy that
 * internet invites open (WebRTC, src/join/rtcTransport.ts). Commit both.
 *
 * - Localized, unlike /pad: state/lang is swapped for src/join/joinLang.ts, whose strings come
 *   from `virtual:join-strings`: every locale, but only the keys the page's own modules use
 *   (`tr("key"` in their sources), found after bundling and written into the page (shared with
 *   /pad, vite.pagePlugins.ts).
 * - The built CSS goes through what the app's does (oneLangPerSelector): older Android WebViews
 *   and Safari 14.0 are friends' phones too.
 * - Budget: the build fails over 906 KiB raw (900 until the picture as a video track,
 *   join/trackPlayer.ts, took 2.4 KiB; nothing it replaces can go: the data channel's picture
 *   stays as its fallback. 904 until "Open on this Wi-Fi", join/lanHint.ts and OpenOnWifi.tsx,
 *   took 1.7 KiB: the page was at 903.3 before it).
 */
import { fileURLToPath } from "node:url";
import { defineConfig, type Plugin } from "vite";
import react from "@vitejs/plugin-react";
import tailwindcss from "@tailwindcss/vite";
import { oneLangPerSelector, pageModuleSwap, pageStrings } from "./vite.pagePlugins";
import { pageCss } from "./vite.pageCss";

// 768 KiB in 12.1.0, once the page stopped importing the host's whole Remote Play store for rpInput
// (state/rpInput.ts): kept close, so that does not creep back in.
const RAW_BUDGET = 800 * 1024;
const OUT = fileURLToPath(new URL("../engine/crates/ps5upload-engine/src/join_page.html", import.meta.url));
/** The same page on the website (https://ps5upload.phantomptr.com/join/, .github/workflows/
 *  pages.yml), where internet invites open (#room=…&k=…&fp=…&hub=…, join/rtcTransport.ts). */
const SITE_OUT = fileURLToPath(new URL("../site/join/index.html", import.meta.url));

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
      const { mkdirSync, writeFileSync } = await import("node:fs");
      const { dirname } = await import("node:path");
      writeFileSync(OUT, page);
      mkdirSync(dirname(SITE_OUT), { recursive: true });
      writeFileSync(SITE_OUT, page);
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
