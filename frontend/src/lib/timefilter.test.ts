import { describe, it, expect } from 'vitest'
import { resolvePresetRange, overlapsRange } from './timefilter'

describe('resolvePresetRange', () => {
  const now = new Date('2026-08-07T12:00:00')

  it('returns open range for "all"', () => {
    expect(resolvePresetRange('all', undefined, undefined, now)).toEqual({ from: null, to: null })
  })

  it('computes preset ranges relative to now with open end', () => {
    expect(resolvePresetRange('1h', undefined, undefined, now)).toEqual({
      from: new Date('2026-08-07T11:00:00'),
      to: null,
    })
    expect(resolvePresetRange('24h', undefined, undefined, now).from).toEqual(
      new Date('2026-08-06T12:00:00'),
    )
    expect(resolvePresetRange('7d', undefined, undefined, now).from).toEqual(
      new Date('2026-07-31T12:00:00'),
    )
    expect(resolvePresetRange('30d', undefined, undefined, now).from).toEqual(
      new Date('2026-07-08T12:00:00'),
    )
  })

  it('parses custom bounds and supports open-ended ranges', () => {
    expect(resolvePresetRange('custom', '2026-08-01T09:00', '2026-08-02T17:30', now)).toEqual({
      from: new Date('2026-08-01T09:00'),
      to: new Date('2026-08-02T17:30'),
    })
    expect(resolvePresetRange('custom', '2026-08-01T09:00', undefined, now)).toEqual({
      from: new Date('2026-08-01T09:00'),
      to: null,
    })
    expect(resolvePresetRange('custom', undefined, '2026-08-02T17:30', now)).toEqual({
      from: null,
      to: new Date('2026-08-02T17:30'),
    })
    expect(resolvePresetRange('custom', undefined, undefined, now)).toEqual({
      from: null,
      to: null,
    })
  })
})

describe('overlapsRange', () => {
  const from = new Date('2026-08-05T10:00:00')
  const to = new Date('2026-08-05T14:00:00')

  it('matches a session fully inside the window', () => {
    expect(overlapsRange('2026-08-05T10:30:00', '2026-08-05T11:00:00', from, to)).toBe(true)
  })

  it('matches a session started before but active inside the window', () => {
    expect(overlapsRange('2026-08-03T09:00:00', '2026-08-05T11:00:00', from, to)).toBe(true)
  })

  it('matches a session spanning the entire window', () => {
    expect(overlapsRange('2026-08-01T00:00:00', '2026-08-10T00:00:00', from, to)).toBe(true)
  })

  it('matches sessions touching the window boundaries', () => {
    expect(overlapsRange('2026-08-05T14:00:00', '2026-08-05T15:00:00', from, to)).toBe(true)
    expect(overlapsRange('2026-08-05T08:00:00', '2026-08-05T10:00:00', from, to)).toBe(true)
  })

  it('rejects sessions entirely before or after the window', () => {
    expect(overlapsRange('2026-08-01T00:00:00', '2026-08-05T09:59:59', from, to)).toBe(false)
    expect(overlapsRange('2026-08-05T14:00:01', '2026-08-06T00:00:00', from, to)).toBe(false)
  })

  it('supports open-ended ranges', () => {
    // only from: any activity after from
    expect(overlapsRange('2026-08-01T00:00:00', '2026-08-05T12:00:00', from, null)).toBe(true)
    expect(overlapsRange('2026-08-01T00:00:00', '2026-08-04T12:00:00', from, null)).toBe(false)
    // only to: any activity before to
    expect(overlapsRange('2026-08-01T00:00:00', '2026-08-05T12:00:00', null, to)).toBe(true)
    expect(overlapsRange('2026-08-05T15:00:00', '2026-08-06T00:00:00', null, to)).toBe(false)
    // fully open: everything matches
    expect(overlapsRange('2020-01-01T00:00:00', '2020-01-02T00:00:00', null, null)).toBe(true)
  })

  it('rejects invalid timestamps', () => {
    expect(overlapsRange('not-a-date', '2026-08-05T11:00:00', from, to)).toBe(false)
    expect(overlapsRange('2026-08-05T10:30:00', '', from, to)).toBe(false)
  })
})
