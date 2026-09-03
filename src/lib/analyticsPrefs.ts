/* ============================================================================
   What the analytics toolbar was last set to (#539).

   Same reasoning as `newChatPrefs.ts` and `inspectorPrefs.ts`: "compare this
   window against the one before it" is *how I like to read the dashboard*,
   which belongs to this install rather than to the account. There is no wire
   field for it, and adding one would make a presentation toggle a synced
   preference — and would put a second request behind a `user_settings` write.

   Two rules the shape depends on, both inherited:

   * **Every key is optional on read.** A blob written by an older build keeps
     the keys it has and takes the shipped default for the rest, so a second
     toggle added later needs no migration.
   * **The key and the shipped defaults are pinned** through `lib/typeAssert.ts`
     — the localStorage key is a string nothing else validates, so respelling it
     silently resets every user's saved state, and there is no TypeScript test
     harness here that would notice.
   ========================================================================== */

import type { Eq, Expect } from "./typeAssert";

/**
 * The toolbar toggles and what they ship as.
 *
 * `compare` ships **off**: it costs a second `/claude-analytics` request per
 * query change, and an install that never asks for a comparison must not pay
 * for one.
 */
export const DEFAULT_ANALYTICS_PREFS = {
  compare: false,
} as const;

/** The stored blob's key. */
const KEY = "agento.analytics";

/**
 * The key and the shipped defaults, pinned.
 *
 * `KEY` is what a stored blob is found under, so respelling it is
 * indistinguishable from a user who never set the toggle. `DEFAULT_ANALYTICS_PREFS`
 * is what an install ships with, and flipping it would turn the second request
 * on for everyone. Neither has a test to catch it, so the pin is the guard: an
 * edit to either now fails `tsc`, i.e. fails CI.
 */
export type PinAnalyticsPrefsKey = Expect<Eq<typeof KEY, "agento.analytics">>;
export type PinAnalyticsPrefs = Expect<
  Eq<typeof DEFAULT_ANALYTICS_PREFS, { readonly compare: false }>
>;

export type AnalyticsPrefs = { -readonly [K in keyof typeof DEFAULT_ANALYTICS_PREFS]: boolean };

/**
 * The stored toolbar state, defaulted per key.
 *
 * Reads defensively rather than trusting the blob — localStorage is shared with
 * anything else on this origin and survives every upgrade, so a `JSON.parse` of
 * a corrupted value would take the whole view down at first render. A key whose
 * value is not a boolean is treated as absent, which is the same fallback a
 * missing key gets.
 */
export function loadAnalyticsPrefs(): AnalyticsPrefs {
  const prefs: AnalyticsPrefs = { ...DEFAULT_ANALYTICS_PREFS };
  try {
    const raw = localStorage.getItem(KEY);
    if (!raw) return prefs;
    const parsed: unknown = JSON.parse(raw);
    if (!parsed || typeof parsed !== "object") return prefs;
    const stored = parsed as Record<string, unknown>;
    for (const id of Object.keys(prefs) as (keyof AnalyticsPrefs)[]) {
      const value = stored[id];
      if (typeof value === "boolean") prefs[id] = value;
    }
    return prefs;
  } catch {
    return { ...DEFAULT_ANALYTICS_PREFS };
  }
}

/** Remember the toolbar. A private-mode or quota failure costs the memory, not the view. */
export function saveAnalyticsPrefs(prefs: AnalyticsPrefs): void {
  try {
    localStorage.setItem(KEY, JSON.stringify(prefs));
  } catch {
    // Nothing to do: the toggle still worked for this session.
  }
}
