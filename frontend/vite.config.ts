import { defineConfig } from 'vitest/config'
import react from '@vitejs/plugin-react'
import tailwindcss from '@tailwindcss/vite'
import { fileURLToPath, URL } from 'node:url'

export default defineConfig({
  plugins: [react(), tailwindcss()],
  resolve: {
    alias: {
      '@': fileURLToPath(new URL('./src', import.meta.url)),
    },
  },
  build: {
    outDir: 'dist',
    emptyOutDir: true,
  },
  server: {
    port: 5173,
    proxy: {
      '/api': 'http://localhost:8990',
      '/health': 'http://localhost:8990',
    },
  },
  test: {
    include: ['src/**/*.test.{ts,tsx}'],
    // jsdom globally rather than a per-file `@vitest-environment` pragma: every
    // existing suite passes under it, so the switch costs only time. Measured
    // at 13 files: 772ms -> 2.41s wall, ~1.25s of jsdom construction per file.
    // Negligible now and it buys one uniform environment, but it scales
    // linearly and the pure-logic src/lib suites pay it for nothing. If this
    // reaches ~100 files, split with vitest `projects` (node for src/lib, jsdom
    // for components) or move to happy-dom.
    environment: 'jsdom',
    setupFiles: ['./src/test/setup.ts'],
    // Component tests are the ones that spy on fetch/matchMedia/timers, and an
    // un-restored spy fails whichever test happens to run next. Free to set in
    // the change that defines the harness; an audit once there are 40 files.
    restoreMocks: true,
  },
})
