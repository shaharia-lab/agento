/* ============================================================================
   The session totals and the permission-mode tables.

   Lifted out of `views/SessionsView.tsx` by #538, when the inspector became its
   own component (`SessionInspector.tsx`) and these turned out to have callers on
   both sides of the split — the table's Cost column and its row badges as much
   as the inspector's groups. Exporting them from the view instead would have
   made `SessionsView` and `SessionInspector` import each other, so they live
   here, where neither owns the other.

   Nothing here renders. It is arithmetic over a `ClaudeSessionSummary` plus the
   two lookup tables, so a `SessionDetail` — which extends the summary — can use
   every one of them unchanged.
   ========================================================================== */

import type {
  ClaudeSessionSummary,
  SessionCost,
  TokenUsage,
} from "../../lib/types";

/* --- Totals -------------------------------------------------------------- */

const ZERO_USAGE: TokenUsage = {
  input_tokens: 0,
  output_tokens: 0,
  cache_creation_tokens: 0,
  cache_creation_5m_tokens: 0,
  cache_creation_1h_tokens: 0,
  cache_read_tokens: 0,
};

const ZERO_COST: SessionCost = {
  input_usd: 0,
  output_usd: 0,
  cache_read_usd: 0,
  cache_write_usd: 0,
  total_usd: 0,
};

export function usageOf(u: TokenUsage | undefined | null): TokenUsage {
  return u ?? ZERO_USAGE;
}

export function costOf(c: SessionCost | undefined | null): SessionCost {
  return c ?? ZERO_COST;
}

/**
 * Billable input/output, main thread plus sub-agents. Cache tokens are billed
 * separately and are deliberately not folded in here — `facets.total_tokens`
 * is exactly the sum of these two numbers over the filtered set.
 */
export function tokensIn(s: ClaudeSessionSummary): number {
  return usageOf(s.usage).input_tokens + usageOf(s.subagent_usage).input_tokens;
}

export function tokensOut(s: ClaudeSessionSummary): number {
  return (
    usageOf(s.usage).output_tokens + usageOf(s.subagent_usage).output_tokens
  );
}

export function totalCost(s: ClaudeSessionSummary): number {
  return costOf(s.cost).total_usd + costOf(s.subagent_cost).total_usd;
}

export function totalDuration(s: ClaudeSessionSummary): number {
  return (s.active_duration_ms ?? 0) + (s.subagent_active_duration_ms ?? 0);
}

/* --- Presentation helpers ------------------------------------------------- */

/**
 * The permission modes Claude Code writes, and how each reads.
 *
 * One table for the row badge and the Mode filter, so the option a user picks
 * is spelled exactly as the column they picked it from. An unknown value keeps
 * its raw text in both places rather than being dropped — the corpus predates
 * some of these names.
 */
const MODE_LABELS: Record<string, string> = {
  bypassPermissions: "Bypass",
  plan: "Plan",
  acceptEdits: "Accept",
  dontAsk: "Don't ask",
  default: "Default",
};

const MODE_TONES: Record<string, string> = {
  bypassPermissions: "badge--amber",
  plan: "badge--purple",
  acceptEdits: "badge--teal",
  dontAsk: "badge--teal",
};

/**
 * `permission_mode` is a free-form column — the scanner copies whatever the
 * transcript's `permission-mode` event carried, with no enum in between — so
 * every read of these tables goes through a `typeof` check rather than `??`.
 * A plain object inherits `toString`, `constructor` and `valueOf`, and `??`
 * does not catch a function: the value would reach JSX as a React child and as
 * a `className`. The `switch` these tables replaced was immune by construction.
 */
function lookup(table: Record<string, string>, key: string): string | undefined {
  const v = table[key];
  return typeof v === "string" ? v : undefined;
}

export function modeLabel(mode: string): string {
  return lookup(MODE_LABELS, mode) ?? mode;
}

export function modeBadge(
  s: ClaudeSessionSummary,
): { label: string; tone: string } | null {
  // `omitempty` on the Go side, so the field is absent on a row that recorded
  // no mode — which is not the same as one whose mode this table does not know.
  const mode = s.permission_mode ?? "";
  const label = lookup(MODE_LABELS, mode);
  if (label) return { label, tone: lookup(MODE_TONES, mode) ?? "" };
  return s.mode ? { label: s.mode, tone: "" } : null;
}
