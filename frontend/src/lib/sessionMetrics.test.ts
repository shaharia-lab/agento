import { describe, it, expect } from 'vitest'
import type { ClaudeSessionSummary } from '../types'
import {
  sessionCost,
  sessionDurationMinutes,
  sessionDurationMs,
  sessionInputTokens,
  sessionOutputTokens,
  sessionTokens,
} from './sessionMetrics'

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

describe('session metrics', () => {
  const busy = session({
    usage: { ...zeroUsage, input_tokens: 100, output_tokens: 10 },
    subagent_usage: { ...zeroUsage, input_tokens: 5, output_tokens: 1 },
    cost: { ...zeroCost, total_usd: 1.5 },
    subagent_cost: { ...zeroCost, total_usd: 0.25 },
  })

  it('adds delegated sub-agent work to the main thread', () => {
    expect(sessionInputTokens(busy)).toBe(105)
    expect(sessionOutputTokens(busy)).toBe(11)
    expect(sessionTokens(busy)).toBe(116)
    expect(sessionCost(busy)).toBeCloseTo(1.75)
  })

  it('ignores cache tokens, which are not input+output', () => {
    const s = session({ usage: { ...zeroUsage, cache_read_tokens: 900_000 } })
    expect(sessionTokens(s)).toBe(0)
  })

  it('measures duration from start to last activity', () => {
    const s = session({
      start_time: '2026-08-09T10:00:00Z',
      last_activity: '2026-08-09T11:30:00Z',
    })
    expect(sessionDurationMs(s)).toBe(90 * 60_000)
    expect(sessionDurationMinutes(s)).toBe(90)
  })

  it('clamps an unparseable or reversed pair to zero, never NaN or negative', () => {
    // NaN fails every comparison and a negative passes every "at most" one —
    // either way a corrupt row lands in the wrong filter results.
    const broken = session({ start_time: 'nonsense', last_activity: 'nonsense' })
    const reversed = session({
      start_time: '2026-08-09T11:00:00Z',
      last_activity: '2026-08-09T10:00:00Z',
    })
    for (const s of [broken, reversed]) {
      expect(sessionDurationMs(s)).toBe(0)
      expect(sessionDurationMinutes(s)).toBe(0)
    }
  })

  it('treats a missing usage or cost object as zero rather than throwing', () => {
    const bare = { session_id: 'x' } as ClaudeSessionSummary
    expect(sessionInputTokens(bare)).toBe(0)
    expect(sessionOutputTokens(bare)).toBe(0)
    expect(sessionTokens(bare)).toBe(0)
    expect(sessionCost(bare)).toBe(0)
  })
})
