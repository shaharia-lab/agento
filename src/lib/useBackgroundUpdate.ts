/* ============================================================================
   The launch-time update check.

   Runs once per app start, never on a timer: a desktop app that restarts
   itself mid-sentence because a release landed is worse than one that updates
   tomorrow.
   ========================================================================== */

import { useEffect, useState } from "react";
import { IS_TAURI } from "./tauri";
import { checkForUpdate, installUpdate, type AvailableUpdate } from "./updater";
import { UPDATE_PREF_KEY, loadUpdatePref } from "./updatePref";

export interface BackgroundUpdate {
  available: AvailableUpdate | null;
  /** True while an automatic install is running. */
  installing: boolean;
  dismiss(): void;
}

export function useBackgroundUpdate(canSelfUpdate: boolean | undefined): BackgroundUpdate {
  const [available, setAvailable] = useState<AvailableUpdate | null>(null);
  const [installing, setInstalling] = useState(false);
  const [dismissed, setDismissed] = useState(false);
  const [pref, setPref] = useState(loadUpdatePref);

  useEffect(() => {
    const onStorage = (e: StorageEvent) => {
      if (e.key === UPDATE_PREF_KEY) setPref(loadUpdatePref());
    };
    window.addEventListener("storage", onStorage);
    return () => window.removeEventListener("storage", onStorage);
  }, []);

  useEffect(() => {
    // Wait for the host probe: acting before `can_self_update` is known could
    // start an automatic install on a .deb, which must never happen.
    if (!IS_TAURI || canSelfUpdate === undefined || pref === "never") return;

    let cancelled = false;

    checkForUpdate()
      .then((update) => {
        if (cancelled || !update) return;
        setAvailable(update);

        if (pref === "auto" && canSelfUpdate) {
          setInstalling(true);
          // Progress is not surfaced here — the About pane owns that UI. This
          // path ends in a relaunch, so there is no success state to render.
          return installUpdate(() => {}).catch(() => {
            if (!cancelled) setInstalling(false);
          });
        }
      })
      .catch(() => {
        // A failed check is not worth interrupting anyone over; the About pane
        // reports it when they go looking.
      });

    return () => {
      cancelled = true;
    };
  }, [canSelfUpdate, pref]);

  return {
    available: dismissed ? null : available,
    installing,
    dismiss: () => setDismissed(true),
  };
}
