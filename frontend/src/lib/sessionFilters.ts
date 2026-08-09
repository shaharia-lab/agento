import type { ClaudeSessionSummary } from '../types'
import type { DrilldownWindow } from './drilldown'
import { overlapsAnyWindow } from './drilldown'
import { overlapsRange } from './timefilter'

/**
 * The six sessions-list predicates, resolved to plain values. The time range is
 * already resolved (`resolvePresetRange` runs once per render, not per session),
 * so this stays a pure function of its arguments.
 */
export interface SessionFilters {
  /** `'all'` matches every project. */
  project: string
  /** Empty matches everything; matched case-insensitively. */
  search: string
  favorites: boolean
  hasPR: boolean
  /** `'all'` matches every mode. */
  permissionMode: string
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
  const matchesHasPR = !f.hasPR || (s.prs?.length ?? 0) > 0
  const matchesPermissionMode = f.permissionMode === 'all' || s.permission_mode === f.permissionMode
  const matchesTime = f.drilldownActive
    ? overlapsAnyWindow(s.start_time, s.last_activity, f.drilldownWindows)
    : overlapsRange(s.start_time, s.last_activity, f.from, f.to)
  return (
    matchesProject &&
    matchesSearch &&
    matchesFavorites &&
    matchesHasPR &&
    matchesPermissionMode &&
    matchesTime
  )
}

/**
 * The distinct permission modes present, sorted. The page only offers the
 * filter once this has more than one entry — a single-value dropdown filters
 * nothing — so dropping a mode here makes the control vanish.
 */
export function permissionModesOf(sessions: ClaudeSessionSummary[]): string[] {
  return [...new Set(sessions.map(s => s.permission_mode).filter((m): m is string => !!m))].sort()
}

/** Whether any session is linked to a PR, gating the has-PR toggle. */
export function hasPRs(sessions: ClaudeSessionSummary[]): boolean {
  return sessions.some(s => (s.prs?.length ?? 0) > 0)
}

/** Whether any session is favorited, gating the favorites toggle. */
export function hasFavorites(sessions: ClaudeSessionSummary[]): boolean {
  return sessions.some(s => s.is_favorite)
}
