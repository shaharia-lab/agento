/* ============================================================================
   Which Tasks-form sections are open, remembered across launches (#631).

   The form-pane sibling of `inspectorPrefs.ts`, and the same shape for the same
   reasons: it is "how I like the form", so it lives in localStorage and never in
   `user_settings`; the key is a stable slug, never the rendered title; and every
   key is optional on read, so a blob from an older build keeps what it has.

   It is its own module rather than two more keys in `inspectorPrefs.ts`: those
   live under `agento.inspector` and widen `InspectorGroupId`, which the session
   inspector keys on, so a form section would become an inspector group.
   ========================================================================== */

import type { Eq, Expect } from "./typeAssert";

/**
 * The collapsible sections and what they ship as.
 *
 * Both start collapsed — most tasks never change either, and the collapsed
 * header summarises the values so a non-default one is never hidden. Task,
 * Schedule and Instruction are not here: they do not collapse.
 */
export const DEFAULT_OPEN = {
  execution: false,
  limits: false,
} as const;

/**
 * The ids and the shipped defaults, pinned — these strings are the
 * localStorage keys, so a respelling silently resets everyone's saved state.
 * See `lib/typeAssert.ts`.
 */
export type PinDefaultOpen = Expect<
  Eq<typeof DEFAULT_OPEN, { readonly execution: false; readonly limits: false }>
>;

/** A collapsible section's stable id. Derived from the defaults so the two cannot drift. */
export type TaskFormSectionId = keyof typeof DEFAULT_OPEN;

export type PinSectionIds = Expect<Eq<TaskFormSectionId, "execution" | "limits">>;

export type TaskFormPrefs = Record<TaskFormSectionId, boolean>;

const KEY = "agento.taskForm";

/**
 * The stored open/closed state, defaulted per section. A corrupt blob, or a key
 * whose value is not a boolean, falls back to the default rather than throwing.
 */
export function loadTaskFormPrefs(): TaskFormPrefs {
  const prefs: TaskFormPrefs = { ...DEFAULT_OPEN };
  try {
    const raw = localStorage.getItem(KEY);
    if (!raw) return prefs;
    const parsed: unknown = JSON.parse(raw);
    if (!parsed || typeof parsed !== "object") return prefs;
    const stored = parsed as Record<string, unknown>;
    for (const id of Object.keys(prefs) as TaskFormSectionId[]) {
      const value = stored[id];
      if (typeof value === "boolean") prefs[id] = value;
    }
    return prefs;
  } catch {
    return { ...DEFAULT_OPEN };
  }
}

/** Remember the form's shape. A private-mode or quota failure costs the memory, not the view. */
export function saveTaskFormPrefs(prefs: TaskFormPrefs): void {
  try {
    localStorage.setItem(KEY, JSON.stringify(prefs));
  } catch {
    // Nothing to do: the toggle still worked for this session.
  }
}
