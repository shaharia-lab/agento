import { defineConfig, devices } from '@playwright/test';
import path from 'path';

/**
 * E2E tests for Agento.
 *
 * Prerequisites:
 *   1. Build the binary first: `make build` (from repo root)
 *   2. Install Playwright browsers: `cd e2e && npm ci && npx playwright install chromium`
 *
 * Run tests:
 *   cd e2e && npm test
 *
 * The test runner starts the Agento server automatically using a clean data
 * directory at /tmp/agento-e2e-test and tears it down after the suite.
 */

const AGENTO_BINARY = path.resolve(__dirname, '../agento');
const DATA_DIR = '/tmp/agento-e2e-test';
const PORT = 8990;
const BASE_URL = `http://localhost:${PORT}`;

export default defineConfig({
  globalSetup: './global-setup',
  testDir: './tests',
  timeout: 120_000,       // 2 min per test (Claude may take time to respond)
  expect: { timeout: 30_000 },
  fullyParallel: false,   // run sequentially — each test starts/stops the server
  retries: 0,
  workers: 1,
  reporter: [
    ['list'],
    ['html', { open: 'never', outputFolder: 'playwright-report' }],
  ],

  use: {
    baseURL: BASE_URL,
    trace: 'on-first-retry',
    screenshot: 'only-on-failure',
    video: 'on-first-retry',
  },

  projects: [
    {
      name: 'chromium',
      use: { ...devices['Desktop Chrome'] },
    },
  ],

  webServer: {
    command: `AGENTO_DATA_DIR=${DATA_DIR} ${AGENTO_BINARY} web --no-browser`,
    url: BASE_URL,
    reuseExistingServer: false,
    timeout: 30_000,
    stdout: 'pipe',
    stderr: 'pipe',
    env: {
      AGENTO_DATA_DIR: DATA_DIR,
    },
  },
});
