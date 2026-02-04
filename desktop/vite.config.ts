import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";

export default defineConfig({
  plugins: [react()],
  clearScreen: false,
  base: './',
  server: {
    port: 1420,
    strictPort: true
  },
  build: {
    outDir: "dist",
    emptyOutDir: true,
    chunkSizeWarningLimit: 900,
    rollupOptions: {
      output: {
        manualChunks(id) {
          if (!id.includes("node_modules")) return;
          if (id.includes("@svar-ui/react-filemanager")) return "vendor-filemanager";
          if (id.includes("react")) return "vendor-react";
          return "vendor";
        }
      }
    }
  }
});
