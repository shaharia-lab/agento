import { describe, it, expect } from 'vitest'
import {
  matchesFilters,
  permissionModesOf,
  modelsOf,
  hasPRs,
  hasFavorites,
  inRange,
  isBounded,
  UNBOUNDED,
  type SessionFilters,
} from './sessionFilters'
import type {
  ClaudeSessionSummary,
  ClaudeSessionCost,
  ClaudeSessionPR,
  ClaudeTokenUsage,
} from '../types'

const emptyUsage: ClaudeTokenUsage = {
  input_tokens: 0,
  output_tokens: 0,
  cache_creation_tokens: 0,
  cache_creation_5m_tokens: 0,
  cache_creation_1h_tokens: 0,
  cache_read_tokens: 0,
}

const zeroCost: ClaudeSessionCost = {
  input_usd: 0,
  output_usd: 0,
  cache_read_usd: 0,
  cache_write_usd: 0,
  total_usd: 0,
}

const samplePR: ClaudeSessionPR = {
  pr_number: 1,
  pr_url: 'https://github.com/shaharia-lab/agento/pull/1',
  pr_repository: 'shaharia-lab/agento',
  first_seen_at: '2026-08-07T10:30:00.000Z',
}

function makeSession(overrides: Partial<ClaudeSessionSummary> = {}): ClaudeSessionSummary {
  return {
    session_id: 'sess-1',
    project_path: '/home/dev/alpha',
    preview: 'add a login form',
    display_title: 'Login form',
    is_favorite: true,
    start_time: '2026-08-07T10:00:00.000Z',
    last_activity: '2026-08-07T11:00:00.000Z',
    message_count: 4,
    event_count: 9,
    usage: { ...emptyUsage },
    subagent_count: 0,
    subagent_usage: { ...emptyUsage },
    permission_mode: 'bypassPermissions',
    compaction_count: 0,
    dropped_tokens: 0,
    cost: { ...zeroCost },
    subagent_cost: { ...zeroCost },
    prs: [samplePR],
    ...overrides,
  }
}

/** Every filter set to its match-everything value. */
function noFilters(overrides: Partial<SessionFilters> = {}): SessionFilters {
  return {
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
    drilldownActive: false,
    drilldownWindows: [],
    ...overrides,
  }
}

/** A session whose in/out tokens and cost span the main thread and a sub-agent. */
function withMetrics(
  main: { in: number; out: number; usd: number },
  sub: { in: number; out: number; usd: number } = { in: 0, out: 0, usd: 0 },
): ClaudeSessionSummary {
  return makeSession({
    usage: { ...emptyUsage, input_tokens: main.in, output_tokens: main.out },
    subagent_usage: { ...emptyUsage, input_tokens: sub.in, output_tokens: sub.out },
    cost: { ...zeroCost, total_usd: main.usd },
    subagent_cost: { ...zeroCost, total_usd: sub.usd },
  })
}

describe('matchesFilters — each predicate in isolation', () => {
  it('project matches "all" and the exact path only', () => {
    const s = makeSession({ project_path: '/home/dev/alpha' })
    expect(matchesFilters(s, noFilters())).toBe(true)
    expect(matchesFilters(s, noFilters({ project: '/home/dev/alpha' }))).toBe(true)
    expect(matchesFilters(s, noFilters({ project: '/home/dev/beta' }))).toBe(false)
    // Not a prefix or substring match.
    expect(matchesFilters(s, noFilters({ project: '/home/dev' }))).toBe(false)
  })

  it('search is match-all when empty', () => {
    expect(matchesFilters(makeSession(), noFilters({ search: '' }))).toBe(true)
  })

  it('search matches each of the four searched fields, case-insensitively', () => {
    const s = makeSession({
      session_id: 'ABC-123',
      display_title: 'Refactor Pricing',
      preview: 'Rename the CATALOG',
      project_path: '/home/dev/Gamma',
    })
    for (const q of ['abc-123', 'refactor pricing', 'catalog', 'gamma']) {
      expect(matchesFilters(s, noFilters({ search: q })), `query ${q}`).toBe(true)
    }
    // And uppercase queries against lowercase content.
    expect(matchesFilters(s, noFilters({ search: 'RENAME' }))).toBe(true)
    expect(matchesFilters(s, noFilters({ search: 'nothing here' }))).toBe(false)
  })

  it('search tolerates a missing display_title rather than matching on "undefined"', () => {
    const s = makeSession({ display_title: undefined })
    expect(matchesFilters(s, noFilters({ search: 'undefined' }))).toBe(false)
    expect(matchesFilters(s, noFilters({ search: 'login' }))).toBe(true)
  })

  it('favorites only excludes when enabled', () => {
    const fav = makeSession({ is_favorite: true })
    const notFav = makeSession({ is_favorite: false })
    const absent = makeSession({ is_favorite: undefined })
    expect(matchesFilters(notFav, noFilters({ favorites: false }))).toBe(true)
    expect(matchesFilters(fav, noFilters({ favorites: true }))).toBe(true)
    expect(matchesFilters(notFav, noFilters({ favorites: true }))).toBe(false)
    expect(matchesFilters(absent, noFilters({ favorites: true }))).toBe(false)
  })

  it('links treats a missing and an empty prs array alike', () => {
    const withPR = makeSession()
    const emptyPRs = makeSession({ prs: [] })
    const noPRs = makeSession({ prs: undefined })

    // 'all' is the match-everything value.
    for (const s of [withPR, emptyPRs, noPRs]) {
      expect(matchesFilters(s, noFilters({ links: 'all' }))).toBe(true)
    }
    expect(matchesFilters(withPR, noFilters({ links: 'with' }))).toBe(true)
    expect(matchesFilters(emptyPRs, noFilters({ links: 'with' }))).toBe(false)
    expect(matchesFilters(noPRs, noFilters({ links: 'with' }))).toBe(false)
    // 'without' is the exact complement of 'with', not merely "not with".
    expect(matchesFilters(withPR, noFilters({ links: 'without' }))).toBe(false)
    expect(matchesFilters(emptyPRs, noFilters({ links: 'without' }))).toBe(true)
    expect(matchesFilters(noPRs, noFilters({ links: 'without' }))).toBe(true)
  })

  it('message count honours a min, a max and both together', () => {
    const s = makeSession({ message_count: 10 })
    expect(matchesFilters(s, noFilters({ messages: { min: 10, max: null } }))).toBe(true)
    expect(matchesFilters(s, noFilters({ messages: { min: 11, max: null } }))).toBe(false)
    expect(matchesFilters(s, noFilters({ messages: { min: null, max: 10 } }))).toBe(true)
    expect(matchesFilters(s, noFilters({ messages: { min: null, max: 9 } }))).toBe(false)
    expect(matchesFilters(s, noFilters({ messages: { min: 5, max: 15 } }))).toBe(true)
    expect(matchesFilters(s, noFilters({ messages: { min: 11, max: 15 } }))).toBe(false)
  })

  it('model matches "all" and the exact id only', () => {
    const s = makeSession({ model: 'claude-opus-5' })
    expect(matchesFilters(s, noFilters({ model: 'all' }))).toBe(true)
    expect(matchesFilters(s, noFilters({ model: 'claude-opus-5' }))).toBe(true)
    expect(matchesFilters(s, noFilters({ model: 'claude-sonnet-5' }))).toBe(false)
    // Not a prefix match — "claude-opus-5" must not be matched by "claude-opus".
    expect(matchesFilters(s, noFilters({ model: 'claude-opus' }))).toBe(false)
    // A session with no recorded model is excluded by any specific model.
    const unset = makeSession({ model: undefined })
    expect(matchesFilters(unset, noFilters({ model: 'claude-opus-5' }))).toBe(false)
    expect(matchesFilters(unset, noFilters({ model: 'all' }))).toBe(true)
  })

  it('duration is measured in minutes from start to last activity', () => {
    // 10:00 → 11:00 is 60 minutes.
    const s = makeSession()
    expect(matchesFilters(s, noFilters({ durationMinutes: { min: 60, max: 60 } }))).toBe(true)
    expect(matchesFilters(s, noFilters({ durationMinutes: { min: 61, max: null } }))).toBe(false)
    expect(matchesFilters(s, noFilters({ durationMinutes: { min: null, max: 59 } }))).toBe(false)
    expect(matchesFilters(s, noFilters({ durationMinutes: { min: 30, max: 90 } }))).toBe(true)
  })

  it('a reversed timestamp pair reads as zero duration, not a negative', () => {
    // A negative would pass every "at most" bound, so a corrupt row would show
    // up in exactly the searches meant to find the short sessions.
    const reversed = makeSession({
      start_time: '2026-08-07T11:00:00.000Z',
      last_activity: '2026-08-07T10:00:00.000Z',
    })
    expect(matchesFilters(reversed, noFilters({ durationMinutes: { min: null, max: 5 } }))).toBe(
      true,
    )
    expect(matchesFilters(reversed, noFilters({ durationMinutes: { min: 1, max: null } }))).toBe(
      false,
    )
  })

  it('an unparseable timestamp is dropped by the time predicate, before duration', () => {
    // Documented rather than asserted on duration alone: overlapsRange already
    // rejects NaN timestamps outright, so such a row never reaches the list at
    // all — no duration bound can bring it back.
    const broken = makeSession({ start_time: 'nonsense', last_activity: 'nonsense' })
    expect(matchesFilters(broken, noFilters())).toBe(false)
  })

  it('token and cost ranges include sub-agent work, matching the columns shown', () => {
    // Main thread alone would fail every one of these; the displayed figure is
    // the total, so the filter must use the total too.
    const s = withMetrics({ in: 100, out: 20, usd: 1 }, { in: 400, out: 80, usd: 4 })
    expect(matchesFilters(s, noFilters({ tokensIn: { min: 500, max: null } }))).toBe(true)
    expect(matchesFilters(s, noFilters({ tokensIn: { min: 501, max: null } }))).toBe(false)
    expect(matchesFilters(s, noFilters({ tokensOut: { min: 100, max: null } }))).toBe(true)
    expect(matchesFilters(s, noFilters({ tokensOut: { min: null, max: 99 } }))).toBe(false)
    expect(matchesFilters(s, noFilters({ cost: { min: 5, max: 5 } }))).toBe(true)
    expect(matchesFilters(s, noFilters({ cost: { min: null, max: 4.99 } }))).toBe(false)
  })

  it('a zero bound filters, where an absent bound does not', () => {
    const zero = withMetrics({ in: 0, out: 0, usd: 0 })
    // max: 0 is a real constraint that this session satisfies...
    expect(matchesFilters(zero, noFilters({ cost: { min: null, max: 0 } }))).toBe(true)
    // ...and one that a paid session fails, rather than 0 reading as "unset".
    const paid = withMetrics({ in: 0, out: 0, usd: 2 })
    expect(matchesFilters(paid, noFilters({ cost: { min: null, max: 0 } }))).toBe(false)
    expect(matchesFilters(paid, noFilters({ cost: { min: 0, max: null } }))).toBe(true)
  })

  it('permission mode matches "all" and the exact mode only', () => {
    const s = makeSession({ permission_mode: 'plan' })
    expect(matchesFilters(s, noFilters({ permissionMode: 'all' }))).toBe(true)
    expect(matchesFilters(s, noFilters({ permissionMode: 'plan' }))).toBe(true)
    expect(matchesFilters(s, noFilters({ permissionMode: 'default' }))).toBe(false)
    // A session with no recorded mode is excluded by any specific mode.
    const unset = makeSession({ permission_mode: undefined })
    expect(matchesFilters(unset, noFilters({ permissionMode: 'plan' }))).toBe(false)
    expect(matchesFilters(unset, noFilters({ permissionMode: 'all' }))).toBe(true)
  })

  it('time range keeps sessions overlapping [from, to] and drops the rest', () => {
    const s = makeSession({
      start_time: '2026-08-07T10:00:00.000Z',
      last_activity: '2026-08-07T11:00:00.000Z',
    })
    // Fully inside.
    expect(
      matchesFilters(
        s,
        noFilters({ from: new Date('2026-08-07T09:00:00Z'), to: new Date('2026-08-07T12:00:00Z') }),
      ),
    ).toBe(true)
    // Overlapping the tail only — still a match.
    expect(matchesFilters(s, noFilters({ from: new Date('2026-08-07T10:30:00Z') }))).toBe(true)
    // Entirely before the window.
    expect(matchesFilters(s, noFilters({ from: new Date('2026-08-08T00:00:00Z') }))).toBe(false)
    // Entirely after it.
    expect(matchesFilters(s, noFilters({ to: new Date('2026-08-07T09:00:00Z') }))).toBe(false)
  })
})

describe('matchesFilters — the && chain', () => {
  /**
   * One case per filter: the session fails that filter and passes the other
   * five. A dropped clause makes exactly one of these return true, which no
   * single-filter test can catch.
   */
  const allFilters = noFilters({
    project: '/home/dev/alpha',
    search: 'login',
    favorites: true,
    links: 'with',
    permissionMode: 'bypassPermissions',
    model: 'claude-opus-5',
    messages: { min: 2, max: 8 },
    durationMinutes: { min: 30, max: 90 },
    tokensIn: { min: 10, max: 1000 },
    tokensOut: { min: 5, max: 500 },
    cost: { min: 0.5, max: 50 },
    from: new Date('2026-08-07T09:00:00Z'),
    to: new Date('2026-08-07T12:00:00Z'),
  })

  /** Satisfies every clause of `allFilters`. */
  const passing = (): ClaudeSessionSummary =>
    makeSession({
      model: 'claude-opus-5',
      usage: { ...emptyUsage, input_tokens: 100, output_tokens: 50 },
      cost: { ...zeroCost, total_usd: 5 },
    })

  it('passes a session satisfying every filter', () => {
    expect(matchesFilters(passing(), allFilters)).toBe(true)
  })

  it.each([
    ['project', { project_path: '/home/dev/beta' }],
    ['search', { preview: 'unrelated', display_title: 'unrelated', session_id: 'zzz' }],
    ['favorites', { is_favorite: false }],
    ['links', { prs: [] }],
    ['permission mode', { permission_mode: 'plan' }],
    ['messages', { message_count: 99 }],
    ['model', { model: 'claude-sonnet-5' }],
    ['duration', { last_activity: '2026-08-07T10:05:00.000Z' }],
    ['tokens in', { usage: { ...emptyUsage, input_tokens: 5, output_tokens: 50 } }],
    ['tokens out', { usage: { ...emptyUsage, input_tokens: 100, output_tokens: 1 } }],
    ['cost', { cost: { ...zeroCost, total_usd: 0.1 } }],
    [
      'time range',
      { start_time: '2026-08-01T00:00:00.000Z', last_activity: '2026-08-01T01:00:00.000Z' },
    ],
  ] as [string, Partial<ClaudeSessionSummary>][])(
    'rejects a session failing only the %s filter',
    (_name, overrides) => {
      expect(matchesFilters({ ...passing(), ...overrides }, allFilters)).toBe(false)
    },
  )
})

describe('matchesFilters — drill-down branch', () => {
  const s = makeSession({
    start_time: '2026-08-07T10:00:00.000Z',
    last_activity: '2026-08-07T11:00:00.000Z',
  })

  it('uses the windows and ignores from/to when a drill-down is active', () => {
    const overlapping = {
      from: Date.parse('2026-08-07T10:30:00Z'),
      to: Date.parse('2026-08-07T12:00:00Z'),
    }
    // from/to here would exclude the session outright; the drill-down wins.
    expect(
      matchesFilters(
        s,
        noFilters({
          drilldownActive: true,
          drilldownWindows: [overlapping],
          from: new Date('2020-01-01T00:00:00Z'),
          to: new Date('2020-01-02T00:00:00Z'),
        }),
      ),
    ).toBe(true)
  })

  it('rejects a session outside every window', () => {
    const elsewhere = {
      from: Date.parse('2026-08-09T00:00:00Z'),
      to: Date.parse('2026-08-10T00:00:00Z'),
    }
    expect(
      matchesFilters(s, noFilters({ drilldownActive: true, drilldownWindows: [elsewhere] })),
    ).toBe(false)
  })

  it('matches when any one of several windows overlaps', () => {
    const miss = {
      from: Date.parse('2026-08-01T00:00:00Z'),
      to: Date.parse('2026-08-02T00:00:00Z'),
    }
    const hit = {
      from: Date.parse('2026-08-07T09:00:00Z'),
      to: Date.parse('2026-08-07T10:30:00Z'),
    }
    expect(
      matchesFilters(s, noFilters({ drilldownActive: true, drilldownWindows: [miss, hit] })),
    ).toBe(true)
  })

  it('matches nothing when active with no windows', () => {
    expect(matchesFilters(s, noFilters({ drilldownActive: true, drilldownWindows: [] }))).toBe(
      false,
    )
  })

  it('ignores the windows entirely when not active', () => {
    const elsewhere = {
      from: Date.parse('2026-08-09T00:00:00Z'),
      to: Date.parse('2026-08-10T00:00:00Z'),
    }
    expect(
      matchesFilters(s, noFilters({ drilldownActive: false, drilldownWindows: [elsewhere] })),
    ).toBe(true)
  })
})

describe('numeric range helpers', () => {
  it('inRange is inclusive on both ends', () => {
    expect(inRange(5, { min: 5, max: 5 })).toBe(true)
    expect(inRange(4, { min: 5, max: 10 })).toBe(false)
    expect(inRange(11, { min: 5, max: 10 })).toBe(false)
  })

  it('a null side is unbounded, not zero', () => {
    expect(inRange(-100, UNBOUNDED)).toBe(true)
    expect(inRange(1e9, UNBOUNDED)).toBe(true)
    expect(inRange(1e9, { min: 5, max: null })).toBe(true)
    expect(inRange(-100, { min: null, max: 5 })).toBe(true)
  })

  it('an inverted range matches nothing rather than silently swapping', () => {
    expect(inRange(7, { min: 10, max: 5 })).toBe(false)
  })

  it('isBounded drives the active-filter count', () => {
    expect(isBounded(UNBOUNDED)).toBe(false)
    expect(isBounded({ min: 0, max: null })).toBe(true)
    expect(isBounded({ min: null, max: 0 })).toBe(true)
  })
})

describe('control-visibility gates', () => {
  it('permissionModesOf de-duplicates, drops empty and missing, and sorts', () => {
    const sessions = [
      makeSession({ permission_mode: 'plan' }),
      makeSession({ permission_mode: 'bypassPermissions' }),
      makeSession({ permission_mode: 'plan' }),
      makeSession({ permission_mode: '' }),
      makeSession({ permission_mode: undefined }),
    ]
    expect(permissionModesOf(sessions)).toEqual(['bypassPermissions', 'plan'])
  })

  it('modelsOf de-duplicates, drops empty and missing, and sorts', () => {
    const sessions = [
      makeSession({ model: 'claude-sonnet-5' }),
      makeSession({ model: 'claude-opus-5' }),
      makeSession({ model: 'claude-sonnet-5' }),
      makeSession({ model: '' }),
      makeSession({ model: undefined }),
    ]
    expect(modelsOf(sessions)).toEqual(['claude-opus-5', 'claude-sonnet-5'])
    expect(modelsOf([])).toEqual([])
  })

  it('permissionModesOf returns empty for no sessions, so the control stays hidden', () => {
    expect(permissionModesOf([])).toEqual([])
    expect(permissionModesOf([makeSession({ permission_mode: undefined })])).toEqual([])
  })

  it('hasPRs is false for both a missing and an empty prs array', () => {
    expect(hasPRs([])).toBe(false)
    expect(hasPRs([makeSession({ prs: undefined })])).toBe(false)
    expect(hasPRs([makeSession({ prs: [] })])).toBe(false)
    expect(hasPRs([makeSession({ prs: [] }), makeSession()])).toBe(true)
  })

  it('hasFavorites distinguishes absent, false and true', () => {
    expect(hasFavorites([])).toBe(false)
    expect(hasFavorites([makeSession({ is_favorite: undefined })])).toBe(false)
    expect(hasFavorites([makeSession({ is_favorite: false })])).toBe(false)
    expect(hasFavorites([makeSession({ is_favorite: false }), makeSession()])).toBe(true)
  })
})
