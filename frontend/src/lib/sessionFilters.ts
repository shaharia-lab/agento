import type { ClaudeSessionSummary } from '../types'
import type { DrilldownWindow } from './drilldown'
import { overlapsAnyWindow } from './drilldown'
import { overlapsRange } from './timefilter'
import {
  sessionCost,
  sessionDurationMinutes,
  sessionInputTokens,
  sessionOutputTokens,
} from './sessionMetrics'

/**
 * An inclusive numeric bound where `null` means unbounded on that side.
 *
 * One min/max pair expresses all three comparisons the UI needs — min alone is
 * "at least", max alone is "at most", both is "between" — so no operator
 * selector is needed beside each field.
 */
export interface NumericRange {
  min: number | null
  max: number | null
}

/** A range that matches every value. */
export const UNBOUNDED: NumericRange = { min: null, max: null }

/** Whether a range constrains anything, i.e. counts as an active filter. */
export function isBounded(r: NumericRange): boolean {
  return r.min !== null || r.max !== null
}

/** Inclusive on both ends: `min: 5` keeps a session with exactly 5. */
export function inRange(value: number, r: NumericRange): boolean {
  if (r.min !== null && value < r.min) return false
  if (r.max !== null && value > r.max) return false
  return true
}

/** Whether a session must have linked PRs, must have none, or either. */
export type LinkFilter = 'all' | 'with' | 'without'

/**
 * The sessions-list predicates, resolved to plain values. The time range is
 * already resolved (`resolvePresetRange` runs once per render, not per session),
 * so this stays a pure function of its arguments.
 *
 * The numeric ranges compare against the same main-thread-plus-sub-agent totals
 * the list's columns render (see `sessionMetrics`), so a visible figure and the
 * filter that hides its row can never disagree.
 */
export interface SessionFilters {
  /** `'all'` matches every project. */
  project: string
  /** Empty matches everything; matched case-insensitively. */
  search: string
  favorites: boolean
  links: LinkFilter
  /** `'all'` matches every mode. */
  permissionMode: string
  /** `'all'` matches every model. */
  model: string
  messages: NumericRange
  /** Wall-clock span in minutes. */
  durationMinutes: NumericRange
  tokensIn: NumericRange
  tokensOut: NumericRange
  /** USD. */
  cost: NumericRange
  from: Date | null
  to: Date | null
  /** When true the drill-down windows replace the preset range entirely. */
  drilldownActive: boolean
  drilldownWindows: DrilldownWindow[]
}

/**
 * Returns true when a session passes every filter. Extracted from the sessions
 * page so the `&&` chain can be tested directly: a dropped clause returns a
 * plausible-looking subset rather than throwing, which is invisible in the UI.
 */
export function matchesFilters(s: ClaudeSessionSummary, f: SessionFilters): boolean {
  const matchesProject = f.project === 'all' || s.project_path === f.project
  const q = f.search.toLowerCase()
  const matchesSearch =
    !q ||
    s.session_id.toLowerCase().includes(q) ||
    (s.display_title ?? '').toLowerCase().includes(q) ||
    s.preview.toLowerCase().includes(q) ||
    s.project_path.toLowerCase().includes(q)
  const matchesFavorites = !f.favorites || !!s.is_favorite
  const matchesPermissionMode = f.permissionMode === 'all' || s.permission_mode === f.permissionMode
  const matchesTime = f.drilldownActive
    ? overlapsAnyWindow(s.start_time, s.last_activity, f.drilldownWindows)
    : overlapsRange(s.start_time, s.last_activity, f.from, f.to)
  return (
    matchesProject &&
    matchesSearch &&
    matchesFavorites &&
    matchesLinks(s, f.links) &&
    matchesPermissionMode &&
    (f.model === 'all' || s.model === f.model) &&
    inRange(s.message_count ?? 0, f.messages) &&
    inRange(sessionDurationMinutes(s), f.durationMinutes) &&
    inRange(sessionInputTokens(s), f.tokensIn) &&
    inRange(sessionOutputTokens(s), f.tokensOut) &&
    inRange(sessionCost(s), f.cost) &&
    matchesTime
  )
}

/** A missing `prs` array and an empty one both mean "no linked PRs". */
function matchesLinks(s: ClaudeSessionSummary, filter: LinkFilter): boolean {
  if (filter === 'all') return true
  const linked = (s.prs?.length ?? 0) > 0
  return filter === 'with' ? linked : !linked
}

/**
 * The distinct permission modes present, sorted. The page only offers the
 * filter once this has more than one entry — a single-value dropdown filters
 * nothing — so dropping a mode here makes the control vanish.
 */
export function permissionModesOf(sessions: ClaudeSessionSummary[]): string[] {
  return [...new Set(sessions.map(s => s.permission_mode).filter((m): m is string => !!m))].sort()
}

/**
 * The distinct models present, sorted — the options for the model dropdown.
 *
 * Aggregated from the sessions on hand rather than the pricing catalog: the
 * catalog lists models this machine may never have run, and a dropdown of
 * options that match nothing is worse than a short one.
 */
export function modelsOf(sessions: ClaudeSessionSummary[]): string[] {
  return [...new Set(sessions.map(s => s.model).filter((m): m is string => !!m))].sort()
}

/** Whether any session is linked to a PR, gating the has-PR toggle. */
export function hasPRs(sessions: ClaudeSessionSummary[]): boolean {
  return sessions.some(s => (s.prs?.length ?? 0) > 0)
}

/** Whether any session is favorited, gating the favorites toggle. */
export function hasFavorites(sessions: ClaudeSessionSummary[]): boolean {
  return sessions.some(s => s.is_favorite)
}
