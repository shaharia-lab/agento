/**
 * The option lists an **agent-shaped** run is configured with, and the helper
 * that keeps a stored value the lists do not enumerate.
 *
 * Lifted out of `views/AgentsView.tsx` by #569, which needed the same three
 * pickers on a Slack trigger rule. `AgentsView` had already grown a private
 * copy of `MODELS`/`withCurrent` beside `views/chat/NewChatBar.tsx`'s, and a
 * third copy in `IntegrationsView` is how "the agent editor and a trigger rule
 * offer the same models" quietly stops being true.
 *
 * **`NewChatBar` deliberately does not import this.** Its permission list is
 * chat-shaped — it carries an explicit "agent default" entry and labels every
 * option `Permissions: …` because the control sits in a composer strip — and
 * its `withCurrent` early-returns on an empty value rather than naming it. A
 * trigger run is unattended, so it wants the agent form's words, not a
 * conversation's.
 */

import type { Eq, Expect } from "./typeAssert";

export interface Option {
  value: string;
  label: string;
}

export const MODELS: Option[] = [
  { value: "sonnet", label: "Sonnet" },
  { value: "opus", label: "Opus" },
  { value: "haiku", label: "Haiku" },
];

/**
 * The four values `permission_mode` may carry, spelled once.
 *
 * They are the *wire's* values, not the UI's: the trigger-rule write validates
 * against exactly this set and answers 422 for anything else, and the chat
 * runner routes every mode it does not recognise into bypass — so a typo here
 * is not a broken control, it is an unattended run with no permission gate at
 * all. `npm run build` is the whole frontend gate, so the literals are pinned
 * below rather than tested.
 */
const PERMISSION_VALUES = ["bypass", "default", "plan", "dontAsk"] as const;

export type PermissionValue = (typeof PERMISSION_VALUES)[number];

export type PinPermissionValues = Expect<
  Eq<PermissionValue, "bypass" | "default" | "plan" | "dontAsk">
>;

/**
 * The label each mode is *offered* under. A `Record` over the pinned union, so
 * adding a value without wording it — or wording one that is not a value —
 * fails `tsc`.
 */
const PERMISSION_OFFERED: Record<PermissionValue, string> = {
  bypass: "Bypass — never ask",
  default: "Default — ask before acting",
  plan: "Plan — propose, do not act",
  dontAsk: "Don't ask",
};

/** The same modes without the sentence, for anywhere that *reports* one. */
export const PERMISSION_LABELS: Record<PermissionValue, string> = {
  bypass: "Bypass",
  default: "Default",
  plan: "Plan",
  dontAsk: "Don't ask",
};

export const PERMISSION_MODES: Option[] = PERMISSION_VALUES.map((value) => ({
  value,
  label: PERMISSION_OFFERED[value],
}));

/** The stored mode, in the words this module reports one in. */
export function permissionLabel(mode: string): string {
  return PERMISSION_LABELS[mode as PermissionValue] ?? (mode || "Default");
}

/**
 * Keep a value the server sent but the UI does not enumerate selectable.
 *
 * Without it a `Dropdown` handed a stored `claude-opus-4-1-20250805` shows an
 * empty trigger, and the first save rewrites the field to whichever option the
 * user then picks — a value they never chose to change.
 */
export function withCurrent(options: Option[], value: string): Option[] {
  if (options.some((o) => o.value === value)) return options;
  return [
    { value, label: value === "" ? "Unset — server default" : value },
    ...options,
  ];
}
