/**
 * The phone controller page (src/pad/): `npm run build:pad` writes ONE file, script and styles
 * inlined, to engine/crates/ps5upload-engine/src/pad_page.html, which the engine embeds and serves
 * at GET /pad to a phone that scanned the computer's QR code (pad_link.rs). Commit the result.
 *
 * - In the phone's language, as /join is: src/join/joinLang.ts with only the keys the page's own
 *   modules use, in every locale (vite.pagePlugins.ts).
 * - The built CSS goes through what the app's does (oneLangPerSelector).
 * - Budget: the build fails over 800 KiB raw (660 before the strings, 640 before the "Open in the
 *   PS5Upload app?" question, OpenInAppGate, 600 before the Touchpad & keyboard tab).
 */
import { fileURLToPath } from "node:url";
import { defineConfig, type Plugin } from "vite";
import react from "@vitejs/plugin-react";
import tailwindcss from "@tailwindcss/vite";
import { oneLangPerSelector, pageModuleSwap, pageStrings } from "./vite.pagePlugins";
import { pageCss } from "./vite.pageCss";

const RAW_BUDGET = 800 * 1024;
const OUT = fileURLToPath(new URL("../engine/crates/ps5upload-engine/src/pad_page.html", import.meta.url));
const escapeRe = (s: string) => s.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");

/** Everything into index.html, written to the engine's pad_page.html. */
function singleFile(): Plugin {
  return {
    name: "ps5upload-pad-single-file",
    apply: "build",
    enforce: "post",
    async generateBundle(_, bundle) {
      const html = Object.values(bundle).find((f) => f.type === "asset" && f.fileName.endsWith("index.html"));
      if (!html || html.type !== "asset") return this.error("pad: no index.html in the bundle");
      let page = String(html.source);
      for (const [name, file] of Object.entries(bundle)) {
        if (file.type === "chunk") {
          const re = new RegExp(`<script[^>]*src="[^"]*${escapeRe(name)}"[^>]*></script>`);
          if (!re.test(page)) return this.error(`pad: ${name} is not referenced by index.html`);
          const code = file.code.replace(/<\/script/gi, "<\\/script");
          page = page.replace(re, () => `<script type="module">${code}</script>`);
        } else if (file.fileName.endsWith(".css")) {
          const re = new RegExp(`<link[^>]*href="[^"]*${escapeRe(name)}"[^>]*>`);
          if (!re.test(page)) return this.error(`pad: ${name} is not referenced by index.html`);
          page = page.replace(re, () => `<style>${String(file.source).replace(/<\/style/gi, "<\\/style")}</style>`);
        }
      }
      const raw = Buffer.byteLength(page);
      this.info(`pad page: ${(raw / 1024).toFixed(1)} KiB (budget ${RAW_BUDGET / 1024} KiB)`);
      if (raw > RAW_BUDGET) this.error(`pad page over budget: ${(raw / 1024).toFixed(1)} KiB`);
      const { writeFileSync } = await import("node:fs");
      writeFileSync(OUT, page);
    },
  };
}

export default defineConfig({
  base: "./",
  plugins: [
    pageStrings("pad"),
    pageCss(fileURLToPath(new URL("./src/pad/main.tsx", import.meta.url)), pageModuleSwap),
    react(),
    tailwindcss(),
    oneLangPerSelector(),
    singleFile(),
  ],
  build: {
    outDir: fileURLToPath(new URL("./dist-pad", import.meta.url)),
    emptyOutDir: true,
    target: "safari14",
    cssCodeSplit: false,
    assetsInlineLimit: Number.MAX_SAFE_INTEGER,
    modulePreload: false,
    rollupOptions: {
      input: fileURLToPath(new URL("./src/pad/index.html", import.meta.url)),
      output: { inlineDynamicImports: true },
    },
  },
});
