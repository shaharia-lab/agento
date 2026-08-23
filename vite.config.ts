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
 * Add the bearer token to a proxied request **that does not already carry one**.
 *
 * This is what keeps the two workflows in `.claude/skills/local-verify/`
 * alive now that `/api` authenticates: Chrome on `localhost:1420` has no Tauri
 * IPC, so the page itself can never hold a token, and without this every request
 * it makes would 401.
 *
 * **The "does not already carry one" half is load-bearing since #405**, and this
 * used to overwrite unconditionally. That was harmless while the token was an
 * opaque string fixed for the life of the launch: the page's header and the
 * file's held the identical value, so replacing one with the other changed
 * nothing. A signed token is not fixed — `host_info` mints a fresh one per
 * invocation, and regenerating the keypair from Settings → Security invalidates
 * every token issued before it, this file's included.
 *
 * So an unconditional overwrite replaced the webview's *valid* credential with a
 * *stale* one and turned `api.ts`'s 401-retry into a loop that could never
 * succeed — a dev-only failure, and the shape that costs most to diagnose,
 * because the app is correct and the harness is what refuses it. Measured
 * directly: after a regenerate the page's own token answered 200 against `:8991`
 * and 401 through this proxy.
 *
 * Leaving the header alone when it is present also makes dev match release,
 * where there is no proxy and the page's header is what arrives.
 */
function authenticate(): ProxyOptions["configure"] {
  return (proxy) => {
    proxy.on("proxyReq", (proxyReq) => {
      if (proxyReq.getHeader("Authorization")) return;
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
