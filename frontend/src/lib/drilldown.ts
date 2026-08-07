import { DAY_NAMES } from '@/pages/analyticsShared'

const DAY_NAMES_PLURAL = ['Sundays', 'Mondays', 'Tuesdays', 'Wednesdays', 'Thursdays', 'Fridays', 'Saturdays']

/**
 * Drill-down helpers for the analytics → Claude Sessions navigation.
 *
 * The backend buckets sessions by the UTC weekday/hour of their last activity
 * (session timestamps are parsed from JSONL `Z` timestamps and never converted,
 * and the analytics `from`/`to` params are UTC midnights). So a click must
 * expand back into every matching concrete UTC hour window inside the range.
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
 * Parses the analytics date range ("YYYY-MM-DD" strings, inclusive) into UTC
 * millisecond bounds, matching how the backend parses the same params
 * (`time.Parse("2006-01-02", …)` → UTC midnight). Returns null when either
 * bound is invalid.
 */
export function parseRangeBounds(fromDate: string, toDate: string): { from: number; to: number } | null {
  const DATE_RE = /^\d{4}-\d{2}-\d{2}$/
  if (!DATE_RE.test(fromDate) || !DATE_RE.test(toDate)) return null
  const from = Date.parse(`${fromDate}T00:00:00Z`)
  const to = Date.parse(`${toDate}T00:00:00Z`) + 24 * 60 * 60 * 1000 // `to` date is inclusive
  if (Number.isNaN(from) || Number.isNaN(to) || from >= to) return null
  return { from, to }
}

function collectWindows(
  bounds: { from: number; to: number },
  matches: (start: Date) => boolean,
): DrilldownWindow[] {
  const windows: DrilldownWindow[] = []
  // Walk hour-by-hour from the range start; ranges are at most ~90 days so this
  // stays cheap (≤ ~2200 iterations) and avoids day/hour juggling edge cases.
  const cursor = new Date(bounds.from)
  while (cursor.getTime() < bounds.to) {
    if (matches(cursor)) {
      windows.push({ from: cursor.getTime(), to: cursor.getTime() + 60 * 60 * 1000 })
    }
    cursor.setUTCHours(cursor.getUTCHours() + 1)
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
    windows: collectWindows(bounds, d => d.getUTCDay() === dayOfWeek && d.getUTCHours() === hour),
    label: `${DAY_NAMES_PLURAL[dayOfWeek] ?? DAY_NAMES[dayOfWeek]} ${padHour(hour)}:00–${padHour((hour + 1) % 24)}:00 UTC`,
    rangeLabel: `${fromDate} → ${toDate}`,
  }
}

/** Windows for an hourly bar: the given UTC hour of every day in the range. */
export function hourlyBarTarget(
  fromDate: string,
  toDate: string,
  hour: number,
): DrilldownTarget | null {
  const bounds = parseRangeBounds(fromDate, toDate)
  if (!bounds) return null
  return {
    windows: collectWindows(bounds, d => d.getUTCHours() === hour),
    label: `every day ${padHour(hour)}:00–${padHour((hour + 1) % 24)}:00 UTC`,
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
