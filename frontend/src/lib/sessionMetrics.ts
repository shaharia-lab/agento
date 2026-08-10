import type { ClaudeSessionSummary } from '../types'

/**
 * The per-session figures the sessions list displays and filters on.
 *
 * Every one of these sums the main thread and its delegated sub-agents, which
 * is what the list's columns render. Filtering on a different basis than the
 * column shows is the bug this module exists to prevent: a row displaying
 * $36.30 must not be hidden by "cost at most $40".
 */

export function sessionInputTokens(s: ClaudeSessionSummary): number {
  return (s.usage?.input_tokens ?? 0) + (s.subagent_usage?.input_tokens ?? 0)
}

export function sessionOutputTokens(s: ClaudeSessionSummary): number {
  return (s.usage?.output_tokens ?? 0) + (s.subagent_usage?.output_tokens ?? 0)
}

/** Input + output tokens. Cache tokens are excluded, as in the list column. */
export function sessionTokens(s: ClaudeSessionSummary): number {
  return sessionInputTokens(s) + sessionOutputTokens(s)
}

/** Total cost in USD: main thread plus delegated sub-agents. */
export function sessionCost(s: ClaudeSessionSummary): number {
  return (s.cost?.total_usd ?? 0) + (s.subagent_cost?.total_usd ?? 0)
}

/**
 * Active duration in milliseconds: inter-event gaps capped at 10 minutes,
 * main thread plus delegated sub-agents — the backend-computed figure, not the
 * start/last span. The span of a resumable session counts every idle day
 * between sittings, which reported a 6-hour session as 678 hours.
 *
 * Missing values read as 0 rather than NaN: NaN would make every duration
 * comparison false, silently hiding the row from a filter it should simply not
 * have matched.
 */
export function sessionDurationMs(s: ClaudeSessionSummary): number {
  return (s.active_duration_ms ?? 0) + (s.subagent_active_duration_ms ?? 0)
}

/** The same span in minutes, the unit the duration filter is entered in. */
export function sessionDurationMinutes(s: ClaudeSessionSummary): number {
  return sessionDurationMs(s) / 60_000
}
