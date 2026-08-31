/* ============================================================================
   The words on a form's two action buttons (#516).

   Agento had three dialects for one gesture. Tasks said `Discard` + `Create`,
   Agents said `Discard` + `Create agent`, the gateway said `Cancel` + `Save`,
   and Integrations said `+ Create` with no partner at all — the plus icon that
   was reported as confusing, because everywhere else in the app a `+` opens a
   blank *new thing* rather than committing a form.

   Two rules, and they are the whole module:

   * **The submit follows existence.** `Create` while the record does not exist
     yet, `Save` once it does — and the in-flight label follows it, so a form
     never says "Saving…" for something that was never stored.
   * **Its partner follows the same split.** `Discard` throws away a record
     that was never stored; `Revert` restores one that was. Two words because
     they undo two different things, which is why the gateway's single `Cancel`
     went rather than spreading: it claimed to undo a save.

   Why a module rather than a paragraph: the four views that speak this grammar
   share no component, so the only thing that can hold them together is one
   spelling of each literal plus a compile-time pin on it. There is no
   TypeScript test harness here (`npm run build` is `tsc --noEmit && vite
   build`, and that is the whole frontend gate), so `lib/typeAssert.ts`'s idiom
   is what stands in for the test — respell a literal below and CI fails.

   The `+` icon rule cannot be expressed here, because it is the *absence* of an
   `<Icon name="plus" />` beside these labels. It is written down in `CLAUDE.md`
   under *Conventions*, with the rest of the grammar.
   ========================================================================== */

import type { Eq, Expect } from "./typeAssert";

/** The submit, while the record does not exist yet. */
export const SUBMIT_CREATE = "Create";
/** The submit, once the record exists. */
export const SUBMIT_SAVE = "Save";
/** The submit while a create is in flight. */
export const SUBMIT_CREATING = "Creating…";
/** The submit while a save is in flight. */
export const SUBMIT_SAVING = "Saving…";
/** The partner, while the record does not exist yet: throw the draft away. */
export const PARTNER_DISCARD = "Discard";
/** The partner, once the record exists: restore what is stored. */
export const PARTNER_REVERT = "Revert";

/**
 * What the primary button reads, given the two states that decide it.
 *
 * `busy` is checked first deliberately: a form mid-request is neither
 * saveable nor discardable, and the in-flight word is the only honest label
 * for a button that is doing something.
 */
export function submitLabel(creating: boolean, busy: boolean): string {
  if (busy) return creating ? SUBMIT_CREATING : SUBMIT_SAVING;
  return creating ? SUBMIT_CREATE : SUBMIT_SAVE;
}

/** What the partner button reads. */
export function partnerLabel(creating: boolean): string {
  return creating ? PARTNER_DISCARD : PARTNER_REVERT;
}

/* --- The compile-time pin ------------------------------------------------- */

/**
 * Respell any literal above and these stop compiling, which fails `npm run
 * build` and therefore CI. Exported so `noUnusedLocals` does not delete the
 * guard for being unused — see `lib/typeAssert.ts` for why `Eq` and not
 * `extends`.
 */
export type PinSubmitCreate = Expect<Eq<typeof SUBMIT_CREATE, "Create">>;
export type PinSubmitSave = Expect<Eq<typeof SUBMIT_SAVE, "Save">>;
export type PinSubmitCreating = Expect<Eq<typeof SUBMIT_CREATING, "Creating…">>;
export type PinSubmitSaving = Expect<Eq<typeof SUBMIT_SAVING, "Saving…">>;
export type PinPartnerDiscard = Expect<Eq<typeof PARTNER_DISCARD, "Discard">>;
export type PinPartnerRevert = Expect<Eq<typeof PARTNER_REVERT, "Revert">>;
