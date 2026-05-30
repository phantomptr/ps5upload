import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";
import tailwindcss from "@tailwindcss/vite";

// @ts-expect-error process is a nodejs global
const host = process.env.TAURI_DEV_HOST;

export default defineConfig({
  plugins: [react(), tailwindcss()],
  clearScreen: false,
  base: "./",
  server: {
    port: 1420,
    strictPort: true,
    host: host || false,
    hmr: host
      ? {
          protocol: "ws",
          host,
          port: 1421,
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
      output: {
        manualChunks(id) {
          if (!id.includes("node_modules")) return;
          if (id.includes("react")) return "vendor-react";
          return "vendor";
        },
      },
    },
  },
});
