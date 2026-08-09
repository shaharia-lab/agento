import { describe, expect, it } from 'vitest'

import { avgSessionsPerDay, observedDaySpan } from './analyticsMetrics'
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
