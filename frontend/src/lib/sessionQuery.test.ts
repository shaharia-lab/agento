import { describe, it, expect } from 'vitest'
import {
  NO_FILTERS,
  countActiveFilters,
  filterActive,
  groupsByDay,
  isBounded,
  toQueryParams,
  type SessionFilters,
} from './sessionQuery'

const filters = (over: Partial<SessionFilters> = {}): SessionFilters => ({ ...NO_FILTERS, ...over })

/** The serialized form, as a plain object, for readable assertions. */
const params = (f: SessionFilters) => Object.fromEntries(toQueryParams(f))

describe('toQueryParams', () => {
  it('sends nothing when nothing is narrowed', () => {
    // An absent parameter is what the server reads as "unconstrained".
    expect(params(filters())).toEqual({})
  })

  it('omits the "all" sentinels rather than sending them', () => {
    // `model=all` would filter for a model literally named "all".
    expect(params(filters({ project: 'all', model: 'all', permissionMode: 'all' }))).toEqual({})
  })

  it('sends the exact-match filters', () => {
    expect(
      params(
        filters({
          project: '/home/dev/repo',
          model: 'claude-opus-5',
          permissionMode: 'plan',
          links: 'with',
          favorites: true,
        }),
      ),
    ).toEqual({
      project: '/home/dev/repo',
      model: 'claude-opus-5',
      permission_mode: 'plan',
      links: 'with',
      favorites: 'true',
    })
  })

  it('trims the search and drops it when it is only whitespace', () => {
    expect(params(filters({ search: '  parser  ' }))).toEqual({ q: 'parser' })
    expect(params(filters({ search: '   ' }))).toEqual({})
  })

  it('sends each bound independently, and zero as a real bound', () => {
    expect(params(filters({ cost: { min: null, max: 0 } }))).toEqual({ cost_max: '0' })
    expect(params(filters({ messages: { min: 10, max: null } }))).toEqual({ messages_min: '10' })
    expect(params(filters({ durationMinutes: { min: 5, max: 60 } }))).toEqual({
      duration_min: '5',
      duration_max: '60',
    })
  })

  it('sends the from/to range as RFC3339 instants', () => {
    const from = new Date('2026-08-01T00:00:00Z')
    const to = new Date('2026-08-07T23:59:59Z')
    expect(params(filters({ from, to }))).toEqual({
      from: '2026-08-01T00:00:00.000Z',
      to: '2026-08-07T23:59:59.000Z',
    })
  })

  it('lets a drill-down replace the range rather than intersect with it', () => {
    // The UI disables the preset control while a drill-down is active; sending
    // both would apply two independent time filters at once.
    const out = params(
      filters({
        from: new Date('2026-08-01T00:00:00Z'),
        to: new Date('2026-08-07T00:00:00Z'),
        drilldownWindows: [{ from: 1000, to: 2000 }],
      }),
    )
    expect(out).toEqual({ windows: '1000-2000' })
  })
})

describe('countActiveFilters', () => {
  it('counts nothing for the unfiltered state', () => {
    expect(countActiveFilters(NO_FILTERS)).toBe(0)
  })

  it('counts each dropdown and each bounded range once', () => {
    expect(
      countActiveFilters(
        filters({
          model: 'claude-opus-5',
          links: 'without',
          cost: { min: 1, max: 2 },
          tokensIn: { min: null, max: 5 },
        }),
      ),
    ).toBe(4)
  })
})

describe('filterActive', () => {
  it('is false only when nothing narrows the list', () => {
    expect(filterActive(NO_FILTERS)).toBe(false)
  })

  it.each([
    ['a project', { project: '/home/dev/repo' }],
    ['a search', { search: 'parser' }],
    ['favorites', { favorites: true }],
    ['a range start', { from: new Date() }],
    ['a drill-down', { drilldownWindows: [{ from: 1, to: 2 }] }],
    ['an advanced range', { cost: { min: 5, max: null } }],
  ] as [string, Partial<SessionFilters>][])('is true for %s', (_label, over) => {
    expect(filterActive(filters(over))).toBe(true)
  })
})

describe('isBounded', () => {
  it('treats zero as a bound but null as none', () => {
    expect(isBounded({ min: null, max: null })).toBe(false)
    expect(isBounded({ min: 0, max: null })).toBe(true)
    expect(isBounded({ min: null, max: 0 })).toBe(true)
  })
})

describe('groupsByDay', () => {
  it('groups only under the recency sort', () => {
    // Under any other order two adjacent rows can be weeks apart, so a day
    // header would be a heading over nothing.
    expect(groupsByDay('recent')).toBe(true)
    for (const sort of ['cost', 'tokens', 'duration', 'messages'] as const) {
      expect(groupsByDay(sort), sort).toBe(false)
    }
  })
})
