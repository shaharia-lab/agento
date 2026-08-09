import { describe, it, expect } from 'vitest'
import {
  matchesFilters,
  permissionModesOf,
  hasPRs,
  hasFavorites,
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
    hasPR: false,
    permissionMode: 'all',
    from: null,
    to: null,
    drilldownActive: false,
    drilldownWindows: [],
    ...overrides,
  }
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

  it('has-PR treats a missing and an empty prs array alike', () => {
    const withPR = makeSession()
    const emptyPRs = makeSession({ prs: [] })
    const noPRs = makeSession({ prs: undefined })
    expect(matchesFilters(emptyPRs, noFilters({ hasPR: false }))).toBe(true)
    expect(matchesFilters(withPR, noFilters({ hasPR: true }))).toBe(true)
    expect(matchesFilters(emptyPRs, noFilters({ hasPR: true }))).toBe(false)
    expect(matchesFilters(noPRs, noFilters({ hasPR: true }))).toBe(false)
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
  const allSix = noFilters({
    project: '/home/dev/alpha',
    search: 'login',
    favorites: true,
    hasPR: true,
    permissionMode: 'bypassPermissions',
    from: new Date('2026-08-07T09:00:00Z'),
    to: new Date('2026-08-07T12:00:00Z'),
  })

  it('passes a session satisfying every filter', () => {
    expect(matchesFilters(makeSession(), allSix)).toBe(true)
  })

  it.each([
    ['project', { project_path: '/home/dev/beta' }],
    ['search', { preview: 'unrelated', display_title: 'unrelated', session_id: 'zzz' }],
    ['favorites', { is_favorite: false }],
    ['has-PR', { prs: [] }],
    ['permission mode', { permission_mode: 'plan' }],
    [
      'time range',
      { start_time: '2026-08-01T00:00:00.000Z', last_activity: '2026-08-01T01:00:00.000Z' },
    ],
  ] as [string, Partial<ClaudeSessionSummary>][])(
    'rejects a session failing only the %s filter',
    (_name, overrides) => {
      expect(matchesFilters(makeSession(overrides), allSix)).toBe(false)
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
