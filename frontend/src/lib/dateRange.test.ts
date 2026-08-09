import { describe, it, expect } from 'vitest'
import { fmt, subDays, presetToRange } from '@/pages/analyticsShared'

// These helpers had no coverage at all, which is how `fmt` came to serialise a
// locally-built Date through a UTC formatter. Every assertion here is in local
// time; run the suite under a few TZ values to confirm it holds either side of
// the meridian.
describe('fmt', () => {
  it('formats a local date, not its UTC equivalent', () => {
    // Local midnight on the 1st. toISOString would render this as the previous
    // month's last day for anyone east of UTC.
    expect(fmt(new Date(2026, 7, 1))).toBe('2026-08-01')
  })

  it('formats late-evening local times as the same local day', () => {
    // 23:30 local on the 8th is the 9th in UTC east of the meridian; the range
    // the user picked is the local one.
    expect(fmt(new Date(2026, 7, 8, 23, 30))).toBe('2026-08-08')
  })

  it('formats early-morning local times as the same local day', () => {
    // 00:30 local is the previous day in UTC west of the meridian.
    expect(fmt(new Date(2026, 7, 8, 0, 30))).toBe('2026-08-08')
  })

  it('zero-pads month and day', () => {
    expect(fmt(new Date(2026, 0, 5))).toBe('2026-01-05')
  })
})

describe('subDays', () => {
  it('steps back across a month boundary', () => {
    expect(fmt(subDays(new Date(2026, 7, 3), 7))).toBe('2026-07-27')
  })

  it('does not mutate its argument', () => {
    const d = new Date(2026, 7, 3)
    subDays(d, 7)
    expect(fmt(d)).toBe('2026-08-03')
  })
})

describe('presetToRange', () => {
  // The bug: presetToRange builds its dates with local calendar arithmetic
  // (new Date(y, m, 1)) and then serialised them through a UTC formatter, so
  // the two halves disagreed at every boundary.
  it('starts "this month" on the local 1st', () => {
    const { from } = presetToRange('this-month')
    expect(from.endsWith('-01')).toBe(true)
    const now = new Date()
    expect(from).toBe(fmt(new Date(now.getFullYear(), now.getMonth(), 1)))
  })

  it('ends "last month" on the local last day of that month', () => {
    const { from, to } = presetToRange('last-month')
    const now = new Date()
    expect(from).toBe(fmt(new Date(now.getFullYear(), now.getMonth() - 1, 1)))
    expect(to).toBe(fmt(new Date(now.getFullYear(), now.getMonth(), 0)))
  })

  it('ends the rolling presets on the local today', () => {
    const today = fmt(new Date())
    for (const preset of ['7d', '30d', '90d', 'all-time'] as const) {
      expect(presetToRange(preset).to).toBe(today)
    }
  })

  it('spans the requested number of days', () => {
    const { from, to } = presetToRange('7d')
    expect(from).toBe(fmt(subDays(new Date(to + 'T00:00:00'), 7)))
  })
})
