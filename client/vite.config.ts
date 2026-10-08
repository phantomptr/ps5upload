import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { defineConfig, type Plugin } from "vite";
import react from "@vitejs/plugin-react";
import tailwindcss from "@tailwindcss/vite";

/**
 * The FAQ and What's New screens show the repo-root FAQ.md and CHANGELOG.md (lib/bundledDoc.ts).
 * Imported as `../../../FAQ.md?raw`, the dev server served them as raw markdown instead of
 * modules (they sit outside the client folder), so both screens failed there. As virtual modules
 * they go through the normal pipeline in dev and in builds alike.
 */
function bundledDocs(): Plugin {
  const files: Record<string, string> = {
    "virtual:doc/faq": "../FAQ.md",
    "virtual:doc/changelog": "../CHANGELOG.md",
  };
  return {
    name: "ps5upload-bundled-docs",
    resolveId(id) {
      return id in files ? `\0${id}` : undefined;
    },
    load(id) {
      if (!id.startsWith("\0virtual:doc/")) return undefined;
      const file = fileURLToPath(new URL(files[id.slice(1)], import.meta.url));
      this.addWatchFile(file);
      return `export default ${JSON.stringify(readFileSync(file, "utf8"))};`;
    },
  };
}

// @ts-expect-error process is a nodejs global
const host = process.env.TAURI_DEV_HOST;
// Dev server port (live reload uses the next one up). Desktop dev uses 1420;
// `make run-android` picks its own so both can run at once.
// @ts-expect-error process is a nodejs global
const port = Number(process.env.PS5UPLOAD_VITE_PORT) || 1420;

export default defineConfig({
  plugins: [react(), tailwindcss(), bundledDocs()],
  clearScreen: false,
  base: process.env.VITE_BASE_URL || "./",
  server: {
    port,
    strictPort: true,
    // An explicit IPv4 address, not "localhost": Vite 8 binds the first address localhost
    // resolves to, which on macOS is ::1 alone. The desktop WebView then could not load
    // devUrl and `make run-client` opened a blank dark window (Chromium fell back to ::1,
    // so the page looked fine in a browser). devUrl and playwright.config use 127.0.0.1 too.
    host: host || "127.0.0.1",
    hmr: host
      ? {
          protocol: "ws",
          host,
          port: port + 1,
        }
      : undefined,
  },
  build: {
    outDir: "dist",
    emptyOutDir: true,
    // Pin a conservative JS/CSS target. Tauri renders in a fixed WebView
    // per platform, and the Android System WebView in particular can be
    // an old Chromium. Without this, Vite ships its modern default
    // (ES2020+ optional-chaining / nullish / logical-assignment that the
    // v8/rolldown upgrade left un-lowered) and an older Android WebView
    // renders the first screen, then crashes on `??=` / `?.` → the app
    // "opens then terminates". safari13 (~ES2019) down-levels all of it
    // and is safe for every WebView we ship to (Android Chromium,
    // WebView2, WKWebView, WebKitGTK). Mirrors Tauri's recommended config.
    target:
      process.env.TAURI_ENV_PLATFORM === "windows" ? "chrome105" : "safari13",
    chunkSizeWarningLimit: 900,
    rollupOptions: {
      // Silence rolldown's informational "[PLUGIN_TIMINGS] … significant time
      // in @tailwindcss/vite:generate:build" note. Tailwind v4 scans the whole
      // source tree to generate utilities, so its codegen is legitimately the
      // slowest plugin — that's expected, not a problem to fix, and the note is
      // just build noise. (rolldown input option, passed through by
      // rolldown-vite; not in Vite's RollupOptions types yet.)
      // @ts-expect-error checks is a rolldown passthrough not typed by Vite
      checks: { pluginTimings: false },
      output: {
        manualChunks(id) {
          if (!id.includes("node_modules")) return;
          if (id.includes("react")) return "vendor-react";
          return "vendor";
        },
      },
    },
  },
  test: {
    // Fail any test that reaches the network. See src/test-setup.ts for
    // why: unmocked API wrappers silently hit a running dev engine.
    setupFiles: ["./src/test-setup.ts"],
    // Playwright owns real-browser journeys; Vitest must not import those
    // files into its jsdom/node runner.
    exclude: ["e2e/**", "**/node_modules/**", "**/.git/**"],
  },
});
