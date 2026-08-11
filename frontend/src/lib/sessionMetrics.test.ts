import { readFileSync } from 'node:fs'
import { resolve } from 'node:path'
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

  it('measures duration as backend-computed active time, main thread plus delegated', () => {
    // Not the start/last span: a resumed session's span counts every idle day
    // between sittings, which reported a 6-hour session as 678 hours.
    const s = session({
      start_time: '2026-08-09T10:00:00Z',
      last_activity: '2026-09-06T11:30:00Z', // resumed a month later
      active_duration_ms: 60 * 60_000,
      subagent_active_duration_ms: 30 * 60_000,
    })
    expect(sessionDurationMs(s)).toBe(90 * 60_000)
    expect(sessionDurationMinutes(s)).toBe(90)
  })

  it('treats missing active durations as zero, never NaN', () => {
    // NaN fails every comparison and would silently hide the row from a filter
    // it should simply not have matched.
    const bare = { session_id: 'x' } as ClaudeSessionSummary
    expect(sessionDurationMs(bare)).toBe(0)
    expect(sessionDurationMinutes(bare)).toBe(0)
  })

  it('treats a missing usage or cost object as zero rather than throwing', () => {
    const bare = { session_id: 'x' } as ClaudeSessionSummary
    expect(sessionInputTokens(bare)).toBe(0)
    expect(sessionOutputTokens(bare)).toBe(0)
    expect(sessionTokens(bare)).toBe(0)
    expect(sessionCost(bare)).toBe(0)
  })
})

/**
 * The other half of the cross-language parity check.
 *
 * internal/claudesessions/session_page_test.go reads this same fixture and
 * asserts the SQL the server filters and sorts by; this asserts the TypeScript
 * the columns are rendered from. Together they are what stops a rendered figure
 * and the filter that hides its row from disagreeing — the bug this module was
 * extracted to prevent, which moving the filtering into SQL would otherwise
 * have reopened in a second language.
 */
describe('shared metric vectors — parity with the Go implementation', () => {
  interface Vector {
    name: string
    session: Record<string, number>
    expect: Record<string, number>
  }

  // Resolved from the Vitest root (frontend/) rather than import.meta.url,
  // which the jsdom environment reports as an http URL.
  const vectorsPath = resolve(
    process.cwd(),
    '../internal/claudesessions/testdata/session_metric_vectors.json',
  )
  const vectors: { cases: Vector[] } = JSON.parse(readFileSync(vectorsPath, 'utf8'))

  it('declares at least one case', () => {
    expect(vectors.cases.length).toBeGreaterThan(0)
  })

  it.each(vectors.cases)('$name', tc => {
    const s = session({
      usage: {
        ...zeroUsage,
        input_tokens: tc.session.input_tokens,
        output_tokens: tc.session.output_tokens,
      },
      subagent_usage: {
        ...zeroUsage,
        input_tokens: tc.session.subagent_input_tokens,
        output_tokens: tc.session.subagent_output_tokens,
      },
      cost: { ...zeroCost, total_usd: tc.session.total_cost_usd },
      subagent_cost: { ...zeroCost, total_usd: tc.session.subagent_cost_usd },
      active_duration_ms: tc.session.active_duration_ms,
      subagent_active_duration_ms: tc.session.subagent_active_duration_ms,
      message_count: tc.session.message_count,
    })

    expect(sessionInputTokens(s)).toBe(tc.expect.input_tokens)
    expect(sessionOutputTokens(s)).toBe(tc.expect.output_tokens)
    expect(sessionTokens(s)).toBe(tc.expect.tokens)
    expect(sessionCost(s)).toBeCloseTo(tc.expect.cost_usd, 9)
    expect(sessionDurationMs(s)).toBe(tc.expect.duration_ms)
    expect(sessionDurationMinutes(s)).toBeCloseTo(tc.expect.duration_minutes, 9)
    // Message count is deliberately main-thread only, matching the column.
    expect(s.message_count).toBe(tc.expect.messages)
  })
})
