/* ============================================================================
   In-app updates.

   Signed with our own minisign key, which the app carries the public half of —
   nothing to do with Apple or Microsoft code signing. That is why updates work
   on unsigned macOS builds: Gatekeeper only gates the first launch of a
   browser-downloaded app, not a bundle the updater replaced.

   Installs from a .deb or .rpm are notify-only. dpkg and rpm own the files they
   installed, so overwriting them would leave the package database describing a
   version that is no longer on disk. Those users update through apt/dnf.
   ========================================================================== */

import { IS_TAURI } from "./tauri";

export interface AvailableUpdate {
  version: string;
  notes?: string;
  date?: string;
}

export type UpdateState =
  | { kind: "idle" }
  | { kind: "checking" }
  | { kind: "current" }
  | { kind: "available"; update: AvailableUpdate }
  | { kind: "downloading"; percent: number | null }
  | { kind: "installed" }
  | { kind: "error"; message: string };

/**
 * Where to send someone whose install cannot update itself.
 *
 * `/releases/latest`, not a filtered search: releases are tagged `v*` since
 * v1.0.0 and that tag is the newest in the repository, so the plain link is
 * both correct and the one a user would guess.
 */
export const RELEASES_URL =
  "https://github.com/shaharia-lab/agento/releases/latest";

/**
 * Ask the update server whether a newer version exists.
 * Returns null when up to date, and throws when the check itself failed.
 */
export async function checkForUpdate(): Promise<AvailableUpdate | null> {
  if (!IS_TAURI) return null;

  const { check } = await import("@tauri-apps/plugin-updater");
  const update = await check();
  if (!update) return null;

  return {
    version: update.version,
    notes: update.body,
    date: update.date,
  };
}

/**
 * Download and install, reporting progress, then relaunch.
 *
 * `check()` is called again rather than caching the handle from the earlier
 * check: the handle owns a download session, and reusing a stale one after the
 * user has left the dialog open for a while fails in a way that reads as a
 * corrupt download.
 */
export async function installUpdate(
  onProgress: (percent: number | null) => void
): Promise<void> {
  if (!IS_TAURI) throw new Error("updates are only available in the desktop app");

  const { check } = await import("@tauri-apps/plugin-updater");
  const update = await check();
  if (!update) throw new Error("no update is available any more");

  let total = 0;
  let received = 0;

  await update.downloadAndInstall((event) => {
    switch (event.event) {
      case "Started":
        total = event.data.contentLength ?? 0;
        onProgress(total > 0 ? 0 : null);
        break;
      case "Progress":
        received += event.data.chunkLength;
        // A server that sends no Content-Length leaves us unable to show a
        // percentage; report null so the UI shows an indeterminate bar rather
        // than a fake number.
        onProgress(total > 0 ? Math.min(100, (received / total) * 100) : null);
        break;
      case "Finished":
        onProgress(100);
        break;
    }
  });

  const { relaunch } = await import("@tauri-apps/plugin-process");
  await relaunch();
}
