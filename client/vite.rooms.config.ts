/**
 * The website's Rooms page (src/rooms/, Play Rooms 13.0.0): `npm run build:rooms` writes ONE file,
 * script and styles inlined, to site/rooms/index.html (https://ps5upload.phantomptr.com/rooms/,
 * GitHub Pages: no server of ours besides the hub's cached GET /rooms, which answers our origin's
 * CORS). Localized like /join (vite.pagePlugins.ts: only the keys its modules use, in every
 * language). Joining hands over to the same site's /join/ page with the room in the fragment, the
 * same join flow as an invite. Not indexed (noindex): the list is people's rooms.
 * Budget: 500 KiB raw (478.9 measured in 13.0.0: a list, cards, Details, the notice and their words
 * in 21 languages; no player).
 */
import { fileURLToPath } from "node:url";
import { defineConfig, type Plugin } from "vite";
import react from "@vitejs/plugin-react";
import tailwindcss from "@tailwindcss/vite";
import { oneLangPerSelector, pageModuleSwap, pageStrings } from "./vite.pagePlugins";
import { pageCss } from "./vite.pageCss";

const RAW_BUDGET = 500 * 1024;
const SITE_OUT = fileURLToPath(new URL("../site/rooms/index.html", import.meta.url));

const escapeRe = (s: string) => s.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");

function singleFile(): Plugin {
  return {
    name: "ps5upload-rooms-single-file",
    apply: "build",
    enforce: "post",
    async generateBundle(_, bundle) {
      const html = Object.values(bundle).find((f) => f.type === "asset" && f.fileName.endsWith("index.html"));
      if (!html || html.type !== "asset") return this.error("rooms: no index.html in the bundle");
      let page = String(html.source);
      for (const [name, file] of Object.entries(bundle)) {
        if (file.type === "chunk") {
          const re = new RegExp(`<script[^>]*src="[^"]*${escapeRe(name)}"[^>]*></script>`);
          if (!re.test(page)) return this.error(`rooms: ${name} is not referenced by index.html`);
          const code = file.code.replace(/<\/script/gi, "<\\/script");
          page = page.replace(re, () => `<script type="module">${code}</script>`);
        } else if (file.fileName.endsWith(".css")) {
          const re = new RegExp(`<link[^>]*href="[^"]*${escapeRe(name)}"[^>]*>`);
          if (!re.test(page)) return this.error(`rooms: ${name} is not referenced by index.html`);
          page = page.replace(re, () => `<style>${String(file.source).replace(/<\/style/gi, "<\\/style")}</style>`);
        }
      }
      const raw = Buffer.byteLength(page);
      this.info(`rooms page: ${(raw / 1024).toFixed(1)} KiB (budget ${RAW_BUDGET / 1024} KiB)`);
      if (raw > RAW_BUDGET) this.error(`rooms page over budget: ${(raw / 1024).toFixed(1)} KiB`);
      const { mkdirSync, writeFileSync } = await import("node:fs");
      const { dirname } = await import("node:path");
      mkdirSync(dirname(SITE_OUT), { recursive: true });
      writeFileSync(SITE_OUT, page);
    },
  };
}

export default defineConfig({
  base: "./",
  plugins: [
    pageStrings("rooms"),
    pageCss(fileURLToPath(new URL("./src/rooms/main.tsx", import.meta.url)), pageModuleSwap),
    react(),
    tailwindcss(),
    oneLangPerSelector(),
    singleFile(),
  ],
  build: {
    outDir: fileURLToPath(new URL("./dist-rooms", import.meta.url)),
    emptyOutDir: true,
    target: "safari14",
    cssCodeSplit: false,
    assetsInlineLimit: Number.MAX_SAFE_INTEGER,
    modulePreload: false,
    rollupOptions: {
      input: fileURLToPath(new URL("./src/rooms/index.html", import.meta.url)),
      output: { inlineDynamicImports: true },
    },
  },
});
