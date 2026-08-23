import { readFileSync } from "node:fs";
import { homedir } from "node:os";
import { join } from "node:path";

import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";
import type { ProxyOptions } from "vite";

/**
 * Where a debug build leaves its `/api` bearer token (#400).
 *
 * `paths::data_dir()` in a debug build is `~/.agento-desktop-dev`, deliberately
 * not `~/.agento`. This mirrors that one constant; a release build writes no
 * such file.
 */
const DEV_TOKEN_FILE = join(homedir(), ".agento-desktop-dev", "api-token");

/**
 * Read the token **per request**, never once at startup.
 *
 * `npm run app` starts Vite before the Tauri app, so at Vite's startup the file
 * either does not exist yet or still holds the *previous* launch's token — and
 * the token changes on every launch. Reading it per request costs a stat of a
 * ~32-byte file on the dev machine only, and is the difference between the
 * proxy working after an app restart and 401ing until Vite is restarted too.
 */
function devToken(): string {
  try {
    return readFileSync(DEV_TOKEN_FILE, "utf8").trim();
  } catch {
    return "";
  }
}

/**
 * Add the bearer token to a proxied request.
 *
 * This is what keeps the two workflows in `.claude/skills/local-verify/`
 * alive now that `/api` authenticates: Chrome on `localhost:1420` has no Tauri
 * IPC, so the page itself can never hold a token, and without this every request
 * it makes would 401. Inside the Tauri dev webview the page *does* send its own
 * header — this overwrites it with the identical value.
 */
function authenticate(): ProxyOptions["configure"] {
  return (proxy) => {
    proxy.on("proxyReq", (proxyReq) => {
      const token = devToken();
      if (token) proxyReq.setHeader("Authorization", `Bearer ${token}`);
    });
  };
}

// Tauri expects a fixed port and never obscures Rust errors on the terminal.
export default defineConfig({
  plugins: [react()],
  clearScreen: false,
  server: {
    port: 1420,
    strictPort: true,
    watch: { ignored: ["**/src-tauri/**"] },
    // In development the page is served by Vite, so /api would resolve to Vite
    // itself. Forward it to the Rust API server (fixed port in debug builds).
    // Proxying server-side also keeps the browser out of CORS entirely and
    // leaves SSE intact.
    proxy: {
      "/api": {
        target: "http://127.0.0.1:8991",
        changeOrigin: false,
        ws: false,
        configure: authenticate(),
      },
      // /health is not guarded, so it needs no token — but it costs nothing to
      // send one and keeps the two entries from diverging on the next edit.
      "/health": {
        target: "http://127.0.0.1:8991",
        changeOrigin: false,
        configure: authenticate(),
      },
    },
  },
  build: {
    target: "es2021",
    minify: "esbuild",
    sourcemap: false,
  },
});
