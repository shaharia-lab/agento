import { describe, it, expect } from 'vitest'
import {
  heatmapCellTarget,
  hourlyBarTarget,
  encodeWindows,
  decodeWindows,
  drilldownUrl,
  overlapsAnyWindow,
  parseRangeBounds,
} from './drilldown'

// 2026-08-03 is a Monday, 2026-08-09 a Sunday (local time).
const FROM = '2026-08-03'
const TO = '2026-08-09'

describe('parseRangeBounds', () => {
  it('spans the inclusive `to` date, anchored to UTC', () => {
    const bounds = parseRangeBounds(FROM, TO)!
    expect(bounds.from).toBe(Date.parse('2026-08-03T00:00:00Z'))
    expect(bounds.to).toBe(Date.parse('2026-08-10T00:00:00Z')) // one day past `to`
  })

  it('rejects invalid dates', () => {
    expect(parseRangeBounds('nope', TO)).toBeNull()
    expect(parseRangeBounds(FROM, 'nope')).toBeNull()
    expect(parseRangeBounds('08/03/2026', TO)).toBeNull()
  })

  it('rejects inverted ranges', () => {
    expect(parseRangeBounds(TO, FROM)).toBeNull()
  })
})

describe('heatmapCellTarget', () => {
  it('produces one window per occurrence of the weekday+hour in range (UTC)', () => {
    // Monday (1) 14:00 UTC within Mon 3rd – Sun 9th → exactly one occurrence.
    const target = heatmapCellTarget(FROM, TO, 1, 14)!
    expect(target.windows).toHaveLength(1)
    const w = target.windows[0]
    expect(w.from).toBe(Date.parse('2026-08-03T14:00:00Z'))
    expect(new Date(w.from).getUTCDay()).toBe(1)
    expect(new Date(w.from).getUTCHours()).toBe(14)
    expect(w.to - w.from).toBe(60 * 60 * 1000)
  })

  it('covers every matching weekday across multi-week ranges', () => {
    // Three Mondays in Aug 3–23 2026 (3rd, 10th, 17th).
    const target = heatmapCellTarget('2026-08-03', '2026-08-23', 1, 9)!
    expect(target.windows).toHaveLength(3)
    for (const w of target.windows) {
      expect(new Date(w.from).getUTCDay()).toBe(1)
      expect(new Date(w.from).getUTCHours()).toBe(9)
    }
  })

  it('describes the bucket in the label', () => {
    expect(heatmapCellTarget(FROM, TO, 2, 5)!.label).toBe('Tuesdays 05:00–06:00 UTC')
    expect(heatmapCellTarget(FROM, TO, 6, 23)!.label).toBe('Saturdays 23:00–00:00 UTC')
  })

  it('returns null for an invalid range', () => {
    expect(heatmapCellTarget('bad', TO, 1, 14)).toBeNull()
  })
})

describe('hourlyBarTarget', () => {
  it('produces one window per day in range (UTC)', () => {
    const target = hourlyBarTarget(FROM, TO, 8)! // 7 days
    expect(target.windows).toHaveLength(7)
    expect(target.windows[0].from).toBe(Date.parse('2026-08-03T08:00:00Z'))
    for (const w of target.windows) {
      expect(new Date(w.from).getUTCHours()).toBe(8)
      expect(w.to - w.from).toBe(60 * 60 * 1000)
    }
  })

  it('describes the bucket in the label', () => {
    expect(hourlyBarTarget(FROM, TO, 14)!.label).toBe('every day 14:00–15:00 UTC')
  })
})

describe('encode/decodeWindows round-trip', () => {
  it('round-trips windows', () => {
    const windows = [
      { from: 1_000, to: 2_000 },
      { from: 3_000, to: 4_000 },
    ]
    expect(decodeWindows(encodeWindows(windows))).toEqual(windows)
  })

  it('drops malformed entries', () => {
    expect(decodeWindows('1000-2000,garbage,5000-4000')).toEqual([{ from: 1000, to: 2000 }])
  })

  it('returns empty for missing param', () => {
    expect(decodeWindows(null)).toEqual([])
    expect(decodeWindows('')).toEqual([])
  })
})

describe('drilldownUrl', () => {
  it('builds a /claude-sessions URL with windows and label', () => {
    const target = heatmapCellTarget(FROM, TO, 1, 14)!
    const url = drilldownUrl(target)
    expect(url.startsWith('/claude-sessions?')).toBe(true)
    const params = new URLSearchParams(url.split('?')[1])
    expect(decodeWindows(params.get('windows'))).toEqual(target.windows)
    expect(params.get('label')).toBe('Mondays 14:00–15:00 UTC · 2026-08-03 → 2026-08-09')
    expect(params.get('project')).toBeNull()
  })

  it('carries the analytics project filter when given', () => {
    const target = hourlyBarTarget(FROM, TO, 8)!
    const params = new URLSearchParams(drilldownUrl(target, '/home/user/proj').split('?')[1])
    expect(params.get('project')).toBe('/home/user/proj')
  })
})

describe('overlapsAnyWindow', () => {
  const windows = [
    { from: 1_000, to: 2_000 },
    { from: 5_000, to: 6_000 },
  ]
  const iso = (ms: number) => new Date(ms).toISOString()

  it('matches a session fully inside a window', () => {
    expect(overlapsAnyWindow(iso(1_100), iso(1_900), windows)).toBe(true)
  })

  it('matches a session spanning window boundaries', () => {
    expect(overlapsAnyWindow(iso(500), iso(1_500), windows)).toBe(true)
    expect(overlapsAnyWindow(iso(1_500), iso(5_500), windows)).toBe(true)
  })

  it('matches a session touching a window edge (last activity at window start)', () => {
    expect(overlapsAnyWindow(iso(0), iso(1_000), windows)).toBe(true)
  })

  it('rejects sessions outside all windows', () => {
    expect(overlapsAnyWindow(iso(2_000), iso(3_000), windows)).toBe(false)
    expect(overlapsAnyWindow(iso(6_000), iso(7_000), windows)).toBe(false)
  })

  it('rejects unparseable timestamps', () => {
    expect(overlapsAnyWindow('bad', iso(1_500), windows)).toBe(false)
  })
})
