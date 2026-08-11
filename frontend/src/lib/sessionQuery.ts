import type { DrilldownWindow } from './drilldown'
import { encodeWindows } from './drilldown'

/**
 * The sessions list's filter state, and how it is sent to the server.
 *
 * Until this module existed the whole corpus was shipped to the browser and
 * every predicate ran here, over every session, on every keystroke. The
 * predicates now live in SQL (internal/claudesessions/session_query.go); what
 * is left on this side is the state the controls bind to and one function that
 * serializes it. Keeping that serialization in one place is what stops the list
 * and the facet aggregate — two endpoints that must describe the same set —
 * from being narrowed by two slightly different query strings.
 */

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

/** Whether a session must have linked PRs, must have none, or either. */
export type LinkFilter = 'all' | 'with' | 'without'

/** The column the list is ordered by. Mirrors claudesessions.SessionSort. */
export type SessionSort = 'recent' | 'cost' | 'tokens' | 'duration' | 'messages'

/** Labels for the sort control, in the order it offers them. */
export const SORT_LABELS: Record<SessionSort, string> = {
  recent: 'Most recent',
  cost: 'Highest cost',
  tokens: 'Most tokens',
  duration: 'Longest active',
  messages: 'Most messages',
}

/**
 * Day grouping is only meaningful under the recency sort: under any other, two
 * adjacent rows can be weeks apart and a day header would be a heading over
 * nothing. The list falls back to a flat table for those.
 */
export function groupsByDay(sort: SessionSort): boolean {
  return sort === 'recent'
}

/** Everything the sessions list can narrow by. */
export interface SessionFilters {
  /** `'all'` matches every project. */
  project: string
  /** Empty matches everything; matched case-insensitively as a substring. */
  search: string
  favorites: boolean
  links: LinkFilter
  /** `'all'` matches every mode. */
  permissionMode: string
  /** `'all'` matches every model. */
  model: string
  messages: NumericRange
  /** Active duration in minutes — never the wall-clock span. */
  durationMinutes: NumericRange
  tokensIn: NumericRange
  tokensOut: NumericRange
  /** USD. */
  cost: NumericRange
  from: Date | null
  to: Date | null
  /** When non-empty the drill-down windows replace the from/to range entirely. */
  drilldownWindows: DrilldownWindow[]
}

/** The filter state that narrows nothing. */
export const NO_FILTERS: SessionFilters = {
  project: 'all',
  search: '',
  favorites: false,
  links: 'all',
  permissionMode: 'all',
  model: 'all',
  messages: UNBOUNDED,
  durationMinutes: UNBOUNDED,
  tokensIn: UNBOUNDED,
  tokensOut: UNBOUNDED,
  cost: UNBOUNDED,
  from: null,
  to: null,
  drilldownWindows: [],
}

/**
 * Serializes the filter state into the query the list and facet endpoints take.
 *
 * `'all'` and `null` are omitted rather than sent as sentinels: an absent
 * parameter is what the server reads as "unconstrained", and sending
 * `model=all` would filter for a model literally named "all".
 */
export function toQueryParams(f: SessionFilters): URLSearchParams {
  const qs = new URLSearchParams()
  if (f.project !== 'all') qs.set('project', f.project)
  if (f.search.trim()) qs.set('q', f.search.trim())
  if (f.favorites) qs.set('favorites', 'true')
  if (f.links !== 'all') qs.set('links', f.links)
  if (f.permissionMode !== 'all') qs.set('permission_mode', f.permissionMode)
  if (f.model !== 'all') qs.set('model', f.model)

  setRange(qs, 'messages', f.messages)
  setRange(qs, 'duration', f.durationMinutes)
  setRange(qs, 'tokens_in', f.tokensIn)
  setRange(qs, 'tokens_out', f.tokensOut)
  setRange(qs, 'cost', f.cost)

  // A drill-down is not an extra narrowing on top of the range — it replaces
  // it, exactly as the UI does by disabling the preset control while one is
  // active. Sending both would intersect two independent time filters.
  if (f.drilldownWindows.length > 0) {
    qs.set('windows', encodeWindows(f.drilldownWindows))
  } else {
    if (f.from) qs.set('from', f.from.toISOString())
    if (f.to) qs.set('to', f.to.toISOString())
  }
  return qs
}

function setRange(qs: URLSearchParams, name: string, r: NumericRange): void {
  if (r.min !== null) qs.set(`${name}_min`, String(r.min))
  if (r.max !== null) qs.set(`${name}_max`, String(r.max))
}

/**
 * Whether anything is narrowing the list.
 *
 * Distinguishes the two empty states that used to be one: with the corpus in
 * the browser, "no rows" and "no rows matching" were both derivable from the
 * loaded array. With a paged list an empty page means nothing on its own, so
 * the message has to be chosen from the filter rather than from the result.
 */
export function filterActive(f: SessionFilters): boolean {
  return (
    f.project !== 'all' ||
    f.search.trim() !== '' ||
    f.favorites ||
    f.from !== null ||
    f.to !== null ||
    f.drilldownWindows.length > 0 ||
    countActiveFilters(f) > 0
  )
}

/** How many filters are narrowing the list, for the toolbar's badge. */
export function countActiveFilters(f: SessionFilters): number {
  return (
    (f.permissionMode === 'all' ? 0 : 1) +
    (f.model === 'all' ? 0 : 1) +
    (f.links === 'all' ? 0 : 1) +
    [f.messages, f.durationMinutes, f.tokensIn, f.tokensOut, f.cost].filter(isBounded).length
  )
}
