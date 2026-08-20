/* ============================================================================
   How the app should behave when a new version exists.

   Stored locally rather than in the backend's user_settings: it describes this
   *install*, not this user. The same account may run a .deb on one machine and
   an AppImage on another, and only one of those can update itself.
   ========================================================================== */

export type UpdatePref = "auto" | "notify" | "never";

export const UPDATE_PREF_KEY = "agento.updates";

export const UPDATE_PREF_OPTIONS: { value: UpdatePref; label: string; help: string }[] = [
  {
    value: "auto",
    label: "Download and install automatically",
    help: "Agento restarts itself once the update is in place.",
  },
  {
    value: "notify",
    label: "Notify me",
    help: "Check on launch and show a badge; install when you choose to.",
  },
  {
    value: "never",
    label: "Never check",
    help: "No update requests are made. You check the releases page yourself.",
  },
];

export function loadUpdatePref(): UpdatePref {
  const raw = localStorage.getItem(UPDATE_PREF_KEY);
  return raw === "auto" || raw === "notify" || raw === "never" ? raw : "notify";
}

export function saveUpdatePref(pref: UpdatePref): void {
  localStorage.setItem(UPDATE_PREF_KEY, pref);
  // localStorage's own event does not fire in the tab that wrote it, so the
  // other panes listening for this key need it dispatched explicitly.
  window.dispatchEvent(
    new StorageEvent("storage", { key: UPDATE_PREF_KEY, newValue: pref })
  );
}
