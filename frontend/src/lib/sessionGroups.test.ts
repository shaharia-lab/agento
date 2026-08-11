import { describe, it, expect } from 'vitest'
import type { ClaudeSessionSummary } from '../types'
import { dayLabel, groupSessionsByDay } from './sessionGroups'

const zeroUsage = {
  input_tokens: 0,
  output_tokens: 0,
  cache_read_tokens: 0,
  cache_creation_tokens: 0,
  cache_creation_5m_tokens: 0,
  cache_creation_1h_tokens: 0,
}
const zeroCost = {
  input_usd: 0,
  output_usd: 0,
  cache_read_usd: 0,
  cache_write_usd: 0,
  total_usd: 0,
}

function session(over: Partial<ClaudeSessionSummary> = {}): ClaudeSessionSummary {
  return {
    session_id: 'id',
    project_path: '/home/u/p',
    preview: '',
    start_time: '2026-08-09T10:00:00Z',
    last_activity: '2026-08-09T11:00:00Z',
    message_count: 0,
    event_count: 0,
    usage: { ...zeroUsage },
    subagent_count: 0,
    subagent_usage: { ...zeroUsage },
    compaction_count: 0,
    dropped_tokens: 0,
    cost: { ...zeroCost },
    subagent_cost: { ...zeroCost },
    ...over,
  } as ClaudeSessionSummary
}

/** Local noon, so the assertions never straddle a UTC day boundary. */
const at = (y: number, m: number, d: number, h = 12) => new Date(y, m - 1, d, h).toISOString()

describe('dayLabel', () => {
  const now = new Date(2026, 7, 9, 15) // Sun 9 Aug 2026, local

  it('names today and yesterday relatively', () => {
    expect(dayLabel(new Date(2026, 7, 9, 1), now)).toBe('Today')
    expect(dayLabel(new Date(2026, 7, 8, 23), now)).toBe('Yesterday')
  })

  it('falls back to an absolute date beyond that', () => {
    expect(dayLabel(new Date(2026, 7, 7, 9), now)).toMatch(/7 Aug$/)
  })

  it('appends the year only when it differs from now', () => {
    expect(dayLabel(new Date(2025, 7, 7, 9), now)).toMatch(/2025$/)
  })
})

describe('groupSessionsByDay', () => {
  const now = new Date(2026, 7, 9, 15)

  it('buckets by the local day of last_activity, newest first', () => {
    const groups = groupSessionsByDay(
      [
        session({ session_id: 'older', last_activity: at(2026, 8, 7) }),
        session({ session_id: 'newest', last_activity: at(2026, 8, 9, 14) }),
        session({ session_id: 'mid', last_activity: at(2026, 8, 9, 9) }),
      ],
      now,
    )
    expect(groups.map(g => g.label)).toEqual(['Today', 'Fri 7 Aug'])
    expect(groups[0].sessions.map(s => s.session_id)).toEqual(['newest', 'mid'])
  })

  it('rolls up messages, tokens and cost per day', () => {
    const groups = groupSessionsByDay(
      [
        session({
          last_activity: at(2026, 8, 9, 14),
          message_count: 3,
          usage: { ...zeroUsage, input_tokens: 10, output_tokens: 2 },
          cost: { ...zeroCost, total_usd: 1 },
        }),
        session({
          last_activity: at(2026, 8, 9, 9),
          message_count: 4,
          subagent_usage: { ...zeroUsage, input_tokens: 8, output_tokens: 0 },
          subagent_cost: { ...zeroCost, total_usd: 0.5 },
        }),
      ],
      now,
    )
    expect(groups).toHaveLength(1)
    expect(groups[0].messageCount).toBe(7)
    expect(groups[0].tokens).toBe(20)
    expect(groups[0].cost).toBeCloseTo(1.5)
  })

  it('keeps a session with an unparseable timestamp visible', () => {
    const groups = groupSessionsByDay([session({ last_activity: 'not-a-date' })], now)
    expect(groups.map(g => g.label)).toEqual(['Unknown date'])
  })

  it('returns nothing for an empty list', () => {
    expect(groupSessionsByDay([], now)).toEqual([])
  })
})
