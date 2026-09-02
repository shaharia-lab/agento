/* ============================================================================
   Which inspector groups are open, remembered across launches (#538).

   Same reasoning as `newChatPrefs.ts`: this is "how I like the pane", which
   belongs to this install rather than to the account, so it lives in
   localStorage and never in `user_settings` — there is no wire field for it and
   adding one would make a presentation toggle a synced preference.

   Two rules the shape depends on:

   * **The key is a stable slug, never the rendered title.** Two of the groups
     interpolate a count into their heading (`Sub-agents · 12`), so a
     title-keyed blob would store a new key per session and remember nothing.
   * **Every key is optional on read.** A blob written by an older build keeps
     the keys it has and takes the shipped default for the rest, so adding a
     seventh group later needs no migration.
   ========================================================================== */

import type { Eq, Expect } from "./typeAssert";

/**
 * The collapsible groups and what they ship as.
 *
 * `Activity` and `Cost` are the two figures the sessions list is read for, so
 * they are open; the rest start collapsed, which is what puts both above the
 * fold on the default window size. `Session` is not here — it is the pane's
 * identity rather than data, and does not collapse.
 */
export const DEFAULT_OPEN = {
  activity: true,
  tokens: false,
  subagents: false,
  cost: true,
  prs: false,
} as const;

/**
 * The ids and the shipped defaults, pinned.
 *
 * These strings are the localStorage keys, so respelling one silently resets
 * every user's saved state to the default. There is no TypeScript test harness
 * here (see `lib/typeAssert.ts`), so the pin is the regression guard: an edit
 * to either the id or the default now fails `tsc`, i.e. fails CI.
 */
export type PinDefaultOpen = Expect<
  Eq<
    typeof DEFAULT_OPEN,
    {
      readonly activity: true;
      readonly tokens: false;
      readonly subagents: false;
      readonly cost: true;
      readonly prs: false;
    }
  >
>;

/** A collapsible group's stable id. Derived from the defaults so the two cannot drift. */
export type InspectorGroupId = keyof typeof DEFAULT_OPEN;

export type PinGroupIds = Expect<
  Eq<InspectorGroupId, "activity" | "tokens" | "subagents" | "cost" | "prs">
>;

export type InspectorPrefs = Record<InspectorGroupId, boolean>;

const KEY = "agento.inspector";

/**
 * The stored open/closed state, defaulted per group.
 *
 * Reads defensively rather than trusting the blob — localStorage is shared with
 * anything else on this origin and survives every upgrade, so a `JSON.parse` of
 * a corrupted value would take the whole pane down at first render. A key whose
 * value is not a boolean is treated as absent, which is the same fallback a
 * missing key gets.
 */
export function loadInspectorPrefs(): InspectorPrefs {
  const prefs: InspectorPrefs = { ...DEFAULT_OPEN };
  try {
    const raw = localStorage.getItem(KEY);
    if (!raw) return prefs;
    const parsed: unknown = JSON.parse(raw);
    if (!parsed || typeof parsed !== "object") return prefs;
    const stored = parsed as Record<string, unknown>;
    for (const id of Object.keys(prefs) as InspectorGroupId[]) {
      const value = stored[id];
      if (typeof value === "boolean") prefs[id] = value;
    }
    return prefs;
  } catch {
    return { ...DEFAULT_OPEN };
  }
}

/** Remember the pane's shape. A private-mode or quota failure costs the memory, not the view. */
export function saveInspectorPrefs(prefs: InspectorPrefs): void {
  try {
    localStorage.setItem(KEY, JSON.stringify(prefs));
  } catch {
    // Nothing to do: the toggle still worked for this session.
  }
}
