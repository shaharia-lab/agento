import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";

// Tauri expects a fixed port and never obscures Rust errors on the terminal.
export default defineConfig({
  plugins: [react()],
  clearScreen: false,
  server: {
    port: 1420,
    strictPort: true,
    watch: { ignored: ["**/src-tauri/**"] },
    // In development the page is served by Vite, so /api would resolve to Vite
    // itself. Forward it to the Rust proxy (fixed port in debug builds), which
    // fronts the Go sidecar. Proxying server-side also keeps the browser out of
    // CORS entirely and leaves SSE intact.
    proxy: {
      "/api": {
        target: "http://127.0.0.1:8991",
        changeOrigin: false,
        ws: false,
      },
      "/health": { target: "http://127.0.0.1:8991", changeOrigin: false },
    },
  },
  build: {
    target: "es2021",
    minify: "esbuild",
    sourcemap: false,
  },
});
