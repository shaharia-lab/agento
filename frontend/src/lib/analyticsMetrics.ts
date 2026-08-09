/**
 * Derived analytics figures the dashboards compute client-side.
 *
 * They live here rather than inside a page component so they can be tested:
 * every function in this file was, at some point, a KPI tile that read wrong.
 */
import type { TimeSeriesPoint } from '@/types'

/** A bucket key is "YYYY-MM-DD" or "YYYY-MM-DDTHH", already in the browser's zone. */
function bucketDay(key: string): string {
  return key.split('T')[0]
}

/**
 * Parses a "YYYY-MM-DD" bucket day as a **local** midnight.
 *
 * `new Date("2026-08-08")` parses as UTC midnight, which is the previous local
 * day for anyone west of UTC — and the backend produced these keys in the
 * browser's own timezone, so reading them back as UTC reintroduces exactly the
 * offset the `tz` parameter exists to remove.
 */
function parseLocalDay(day: string): Date | null {
  const parts = day.split('-').map(Number)
  if (parts.length !== 3 || parts.some(Number.isNaN)) return null
  return new Date(parts[0], parts[1] - 1, parts[2])
}

/** Whole calendar days from a to b, inclusive of both ends. */
function inclusiveDaySpan(a: Date, b: Date): number {
  // Rounding rather than flooring, because a span crossing a DST boundary is
  // 23 or 25 hours and would otherwise lose or gain a day.
  return Math.round((b.getTime() - a.getTime()) / 86_400_000) + 1
}

/**
 * Average sessions per day across the days the data actually spans.
 *
 * The denominator is the observed extent — first to last bucket that contains a
 * session — not the requested window. Dividing by the requested window made
 * "All time" meaningless: that preset asks for 2020-01-01 onward, so a corpus
 * of 781 sessions over two years was divided by ~2,400 days and the tile read
 * 0.00 forever, on the one range where a user most wants the number.
 *
 * Idle days *inside* the observed extent still count, because a quiet Sunday is
 * a real part of "per day"; only the empty runway before the first session and
 * after the last is excluded.
 *
 * Returns "—" when the range holds no sessions at all, rather than 0.00, which
 * reads as a measurement rather than an absence.
 */
export function avgSessionsPerDay(totalSessions: number, series: TimeSeriesPoint[]): string {
  const populated = series.filter(p => p.sessions > 0).map(p => bucketDay(p.date))
  if (totalSessions === 0 || populated.length === 0) return '—'

  const first = parseLocalDay(populated[0])
  const last = parseLocalDay(populated[populated.length - 1])
  if (!first || !last) return '—'

  const days = Math.max(1, inclusiveDaySpan(first, last))
  const avg = totalSessions / days
  return avg < 1 ? avg.toFixed(2) : avg.toFixed(1)
}

/**
 * The equally-sized window immediately before [from, to].
 *
 * Both bounds are YYYY-MM-DD local days, the form the analytics API takes, and
 * the previous window ends the day before this one starts — so the two never
 * overlap and "the same length of time, just before" is literally true.
 */
export function previousRange(from: string, to: string): { from: string; to: string } {
  const start = parseLocalDay(from)
  const end = parseLocalDay(to)
  if (!start || !end) return { from, to }

  const days = inclusiveDaySpan(start, end)
  const prevEnd = new Date(start)
  prevEnd.setDate(prevEnd.getDate() - 1)
  const prevStart = new Date(prevEnd)
  prevStart.setDate(prevStart.getDate() - (days - 1))

  return { from: formatLocalDay(prevStart), to: formatLocalDay(prevEnd) }
}

/** Formats a Date as the YYYY-MM-DD the analytics API expects, in local time. */
function formatLocalDay(d: Date): string {
  const pad = (n: number) => String(n).padStart(2, '0')
  return `${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())}`
}

/**
 * Pairs each bucket of the current series with the same-numbered bucket of the
 * previous one, for a ghost overlay.
 *
 * Aligned by position rather than by date, because that is the comparison being
 * made: the first day of this window against the first day of the last one. The
 * two series have the same length whenever the windows do; a previous series
 * that is shorter simply leaves later buckets without a ghost value rather than
 * wrapping around.
 */
export function withPreviousSeries<T, R extends Record<string, unknown>>(
  current: T[],
  previous: T[] | undefined,
  project: (point: T) => R,
  value: (point: T) => number,
): (R & { previous?: number })[] {
  return current.map((point, i) => {
    const row = project(point) as R & { previous?: number }
    const prior = previous?.[i]
    if (prior !== undefined) row.previous = value(prior)
    return row
  })
}

/**
 * The observed extent as a human-readable hint for the tile that uses it, so a
 * reader can see which denominator produced the average.
 */
export function observedDaySpan(series: TimeSeriesPoint[]): number {
  const populated = series.filter(p => p.sessions > 0).map(p => bucketDay(p.date))
  if (populated.length === 0) return 0
  const first = parseLocalDay(populated[0])
  const last = parseLocalDay(populated[populated.length - 1])
  if (!first || !last) return 0
  return Math.max(1, inclusiveDaySpan(first, last))
}
