import fs from 'fs'
import path from 'path'

const DATA_DIR = '/tmp/agento-e2e-test'

/**
 * Runs once before any test worker starts.
 * Wipes the e2e data directory so every test run begins with a fresh
 * SQLite database (no stale chats, settings, or sessions from prior runs).
 */
export default async function globalSetup() {
  if (fs.existsSync(DATA_DIR)) {
    fs.rmSync(DATA_DIR, { recursive: true, force: true })
    console.log(`[global-setup] Cleaned data dir: ${DATA_DIR}`)
  }
  fs.mkdirSync(DATA_DIR, { recursive: true })
  console.log(`[global-setup] Created fresh data dir: ${DATA_DIR}`)

  // Also clean any files Claude may have left behind from previous runs
  const tmpArtifacts = [
    '/tmp/hello-world.txt',
    '/tmp/agento-test-output.txt',
  ]
  for (const f of tmpArtifacts) {
    try {
      if (fs.existsSync(f)) {
        fs.rmSync(f)
        console.log(`[global-setup] Removed leftover artifact: ${path.basename(f)}`)
      }
    } catch {
      // best-effort
    }
  }
}
