import { describe, expect, it } from 'vitest'

import {
  avgSessionsPerDay,
  observedDaySpan,
  previousRange,
  withPreviousSeries,
} from './analyticsMetrics'
import type { TimeSeriesPoint } from '@/types'

/** A daily bucket carrying only the fields these metrics read. */
function bucket(date: string, sessions: number): TimeSeriesPoint {
  return {
    date,
    sessions,
    input_tokens: 0,
    output_tokens: 0,
    cache_read_tokens: 0,
    cache_creation_tokens: 0,
    total_tokens: 0,
  }
}

describe('avgSessionsPerDay', () => {
  it('divides by the observed extent, not the requested window', () => {
    // The "All time" preset asks for 2020-01-01 onward. Dividing 20 sessions by
    // that window is what pinned this tile at 0.00; the data spans three days.
    const series = [
      ...Array.from({ length: 2000 }, (_, i) => bucket(`2020-01-${String((i % 28) + 1)}`, 0)),
      bucket('2026-08-01', 10),
      bucket('2026-08-02', 4),
      bucket('2026-08-03', 6),
    ]
    expect(avgSessionsPerDay(20, series)).toBe('6.7')
  })

  it('counts idle days inside the extent', () => {
    const series = [
      bucket('2026-08-01', 4),
      bucket('2026-08-02', 0),
      bucket('2026-08-03', 0),
      bucket('2026-08-04', 4),
    ]
    expect(avgSessionsPerDay(8, series)).toBe('2.0')
  })

  it('reports an absence rather than a measurement when nothing is in range', () => {
    expect(avgSessionsPerDay(0, [bucket('2026-08-01', 0)])).toBe('—')
    expect(avgSessionsPerDay(0, [])).toBe('—')
  })

  it('keeps two decimals below one per day', () => {
    const series = [
      bucket('2026-08-01', 1),
      ...Array.from({ length: 9 }, (_, i) =>
        bucket(`2026-08-${String(i + 2).padStart(2, '0')}`, 0),
      ),
      bucket('2026-08-11', 1),
    ]
    expect(avgSessionsPerDay(2, series)).toBe('0.18')
  })

  it('handles a single populated day', () => {
    expect(avgSessionsPerDay(3, [bucket('2026-08-01', 3)])).toBe('3.0')
  })

  it('reads hourly bucket keys as their calendar day', () => {
    // A ≤7-day window buckets hourly ("2026-08-01T09"). Treating the whole key
    // as a date would fail to parse and lose the tile entirely.
    const series = [
      bucket('2026-08-01T09', 2),
      bucket('2026-08-01T13', 2),
      bucket('2026-08-02T10', 2),
    ]
    expect(avgSessionsPerDay(6, series)).toBe('3.0')
  })
})

describe('observedDaySpan', () => {
  it('is inclusive of both ends', () => {
    expect(observedDaySpan([bucket('2026-08-01', 1), bucket('2026-08-03', 1)])).toBe(3)
  })

  it('is zero when nothing is populated', () => {
    expect(observedDaySpan([bucket('2026-08-01', 0)])).toBe(0)
  })
})

describe('previousRange', () => {
  it('is the same length of time, immediately before', () => {
    // 10 days: Aug 1–10 → Jul 22–31, ending the day before and not overlapping.
    expect(previousRange('2026-08-01', '2026-08-10')).toEqual({
      from: '2026-07-22',
      to: '2026-07-31',
    })
  })

  it('handles a single-day window', () => {
    expect(previousRange('2026-08-05', '2026-08-05')).toEqual({
      from: '2026-08-04',
      to: '2026-08-04',
    })
  })

  it('crosses a year boundary', () => {
    expect(previousRange('2026-01-01', '2026-01-07')).toEqual({
      from: '2025-12-25',
      to: '2025-12-31',
    })
  })

  it('passes through an unparseable range rather than inventing one', () => {
    expect(previousRange('not-a-date', '2026-01-07')).toEqual({
      from: 'not-a-date',
      to: '2026-01-07',
    })
  })
})

describe('withPreviousSeries', () => {
  const rows = [bucket('2026-08-01', 3), bucket('2026-08-02', 5)]
  const prior = [bucket('2026-07-30', 1), bucket('2026-07-31', 9)]

  it('pairs buckets by position, not by date', () => {
    const merged = withPreviousSeries(
      rows,
      prior,
      p => ({ date: p.date, sessions: p.sessions }),
      p => p.sessions,
    )
    expect(merged).toEqual([
      { date: '2026-08-01', sessions: 3, previous: 1 },
      { date: '2026-08-02', sessions: 5, previous: 9 },
    ])
  })

  it('leaves buckets without a counterpart unpaired', () => {
    const merged = withPreviousSeries(
      rows,
      [prior[0]],
      p => ({ date: p.date }),
      p => p.sessions,
    )
    expect(merged[1].previous).toBeUndefined()
  })

  it('omits the ghost entirely when there is no previous series', () => {
    const merged = withPreviousSeries(
      rows,
      undefined,
      p => ({ date: p.date }),
      p => p.sessions,
    )
    expect(merged.every(r => r.previous === undefined)).toBe(true)
  })
})
