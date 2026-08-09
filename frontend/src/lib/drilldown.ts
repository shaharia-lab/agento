import { DAY_NAMES } from '@/pages/analyticsShared'

const DAY_NAMES_PLURAL = [
  'Sundays',
  'Mondays',
  'Tuesdays',
  'Wednesdays',
  'Thursdays',
  'Fridays',
  'Saturdays',
]

/**
 * Drill-down helpers for the analytics → Claude Sessions navigation.
 *
 * The backend buckets sessions by the weekday/hour of their last activity in
 * the browser's timezone (it is sent as `tz` and applied before bucketing; the
 * `from`/`to` params are local day boundaries). So a click must expand back
 * into every matching concrete local hour window inside the range.
 * The sessions page then keeps the sessions whose activity window overlaps any
 * of those windows.
 */

export interface DrilldownWindow {
  /** Millisecond timestamp, inclusive. */
  from: number
  /** Millisecond timestamp, exclusive. */
  to: number
}

export interface DrilldownTarget {
  /** All hour windows inside the analytics range matching the clicked bucket. */
  windows: DrilldownWindow[]
  /** Human-readable description of the clicked bucket, e.g. "Tuesdays 14:00–15:00". */
  label: string
  /** Human-readable description of the analytics range, e.g. "2026-07-08 → 2026-08-07". */
  rangeLabel: string
}

/**
 * Parses the analytics date range ("YYYY-MM-DD" strings, inclusive) into
 * millisecond bounds at local midnight, matching how the backend now parses the
 * same params (`time.ParseInLocation("2006-01-02", …, tz)`). Returns null when either
 * bound is invalid, or when the range exceeds MAX_DRILLDOWN_DAYS — the
 * serialized window list would otherwise exceed URL length limits.
 */
export function parseRangeBounds(
  fromDate: string,
  toDate: string,
): { from: number; to: number } | null {
  const DATE_RE = /^\d{4}-\d{2}-\d{2}$/
  if (!DATE_RE.test(fromDate) || !DATE_RE.test(toDate)) return null
  // No `Z`: a bare date-time string is parsed in the browser's zone, which is
  // the zone the backend was asked to bucket in.
  const from = Date.parse(`${fromDate}T00:00:00`)
  const to = Date.parse(`${toDate}T00:00:00`) + 24 * 60 * 60 * 1000 // `to` date is inclusive
  if (Number.isNaN(from) || Number.isNaN(to) || from >= to) return null
  if (to - from > MAX_DRILLDOWN_DAYS * 24 * 60 * 60 * 1000) return null
  return { from, to }
}

/**
 * Ranges longer than this produce window lists too large for a URL (all-time
 * would be ~2400 windows ≈ 67 KB of query string), so drill-down is disabled
 * beyond it.
 */
export const MAX_DRILLDOWN_DAYS = 180

function collectWindows(
  bounds: { from: number; to: number },
  matches: (start: Date) => boolean,
): DrilldownWindow[] {
  const HOUR_MS = 60 * 60 * 1000
  const windows: DrilldownWindow[] = []
  // Walk hour-by-hour from the range start; ranges are capped at
  // MAX_DRILLDOWN_DAYS so this stays cheap (≤ ~4400 iterations) and avoids
  // day/hour juggling edge cases.
  //
  // Stepping by a real hour rather than setHours(getHours() + 1) keeps this
  // correct across DST: the wall clock skips an hour in spring and repeats one
  // in autumn, so incrementing the local hour field can jump two hours or stall.
  // Walking absolute time visits every hour that actually elapsed exactly once,
  // and `matches` reads the local fields off each one — a repeated 02:00 legitimately
  // yields two windows, and a skipped one yields none.
  for (let ms = bounds.from; ms < bounds.to; ms += HOUR_MS) {
    const start = new Date(ms)
    if (matches(start)) {
      windows.push({ from: ms, to: ms + HOUR_MS })
    }
  }
  return windows
}

/** Windows for a heatmap cell: the given weekday + hour across the range. */
export function heatmapCellTarget(
  fromDate: string,
  toDate: string,
  dayOfWeek: number,
  hour: number,
): DrilldownTarget | null {
  const bounds = parseRangeBounds(fromDate, toDate)
  if (!bounds) return null
  return {
    windows: collectWindows(bounds, d => d.getDay() === dayOfWeek && d.getHours() === hour),
    label: `${DAY_NAMES_PLURAL[dayOfWeek] ?? DAY_NAMES[dayOfWeek]} ${padHour(hour)}:00–${padHour((hour + 1) % 24)}:00`,
    rangeLabel: `${fromDate} → ${toDate}`,
  }
}

/** Windows for an hourly bar: the given local hour of every day in the range. */
export function hourlyBarTarget(
  fromDate: string,
  toDate: string,
  hour: number,
): DrilldownTarget | null {
  const bounds = parseRangeBounds(fromDate, toDate)
  if (!bounds) return null
  return {
    windows: collectWindows(bounds, d => d.getHours() === hour),
    label: `every day ${padHour(hour)}:00–${padHour((hour + 1) % 24)}:00`,
    rangeLabel: `${fromDate} → ${toDate}`,
  }
}

function padHour(h: number): string {
  return String(h).padStart(2, '0')
}

/**
 * Serializes windows into a compact `from-to,from-to,…` string for the
 * `windows` query param.
 */
export function encodeWindows(windows: DrilldownWindow[]): string {
  return windows.map(w => `${w.from}-${w.to}`).join(',')
}

/** Parses the `windows` query param back into windows; invalid entries are dropped. */
export function decodeWindows(value: string | null): DrilldownWindow[] {
  if (!value) return []
  return value
    .split(',')
    .map(part => {
      const [from, to] = part.split('-').map(Number)
      if (!Number.isFinite(from) || !Number.isFinite(to) || from >= to) return null
      return { from, to }
    })
    .filter((w): w is DrilldownWindow => w !== null)
}

/** Builds the `/claude-sessions` URL for a drill-down target. */
export function drilldownUrl(target: DrilldownTarget, project?: string): string {
  const qs = new URLSearchParams()
  qs.set('windows', encodeWindows(target.windows))
  qs.set('label', `${target.label} · ${target.rangeLabel}`)
  if (project) qs.set('project', project)
  return `/claude-sessions?${qs.toString()}`
}

/**
 * Returns true when a session's activity window [startISO, lastActivityISO]
 * overlaps any of the given windows. Used on the sessions page instead of the
 * preset-based range filter when a drill-down is active.
 */
export function overlapsAnyWindow(
  startISO: string,
  lastActivityISO: string,
  windows: DrilldownWindow[],
): boolean {
  const start = new Date(startISO).getTime()
  const lastActivity = new Date(lastActivityISO).getTime()
  if (Number.isNaN(start) || Number.isNaN(lastActivity)) return false
  return windows.some(w => start < w.to && lastActivity >= w.from)
}
