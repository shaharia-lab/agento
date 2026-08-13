import { describe, it, expect, vi, beforeAll, beforeEach } from 'vitest'
import { render, screen, waitFor } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { MemoryRouter, Routes, Route } from 'react-router-dom'
import ClaudeSessionDetailPage from './ClaudeSessionDetailPage'
import { claudeSessionsApi, insightsApi } from '@/lib/api'
import type { ClaudeSessionDetail } from '@/types'

vi.mock('@/lib/api', () => ({
  claudeSessionsApi: {
    get: vi.fn(),
    refresh: vi.fn(),
    toggleFavorite: vi.fn(),
    updateTitle: vi.fn(),
    continue: vi.fn(),
  },
  insightsApi: {
    getSession: vi.fn(),
  },
}))

const SESSION_ID = 'sess-1'

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

const detailFixture = (over: Partial<ClaudeSessionDetail> = {}): ClaudeSessionDetail =>
  ({
    session_id: SESSION_ID,
    project_path: '/home/me/proj',
    preview: 'A session',
    start_time: '2026-08-13T08:00:00Z',
    last_activity: '2026-08-13T08:15:00Z',
    message_count: 3,
    event_count: 5,
    usage: { ...zeroUsage },
    subagent_count: 0,
    subagent_usage: { ...zeroUsage },
    compaction_count: 0,
    dropped_tokens: 0,
    cost: { ...zeroCost },
    subagent_cost: { ...zeroCost },
    messages: [],
    todos: [],
    subagents: [],
    ...over,
  }) as ClaudeSessionDetail

const renderPage = () =>
  render(
    <MemoryRouter initialEntries={[`/claude-sessions/${SESSION_ID}`]}>
      <Routes>
        <Route path="/claude-sessions/:id" element={<ClaudeSessionDetailPage />} />
      </Routes>
    </MemoryRouter>,
  )

beforeAll(() => {
  Element.prototype.scrollIntoView = vi.fn()
  Element.prototype.hasPointerCapture = vi.fn(() => false)
  Element.prototype.setPointerCapture = vi.fn()
  Element.prototype.releasePointerCapture = vi.fn()

  class Observer {
    observe() {}
    unobserve() {}
    disconnect() {}
    takeRecords() {
      return []
    }
  }
  vi.stubGlobal('IntersectionObserver', Observer)
  vi.stubGlobal('ResizeObserver', Observer)
  vi.stubGlobal(
    'matchMedia',
    vi.fn().mockImplementation((query: string) => ({
      matches: false,
      media: query,
      onchange: null,
      addEventListener: vi.fn(),
      removeEventListener: vi.fn(),
      addListener: vi.fn(),
      removeListener: vi.fn(),
      dispatchEvent: vi.fn(),
    })),
  )
})

describe('ClaudeSessionDetailPage refresh', () => {
  beforeEach(() => {
    vi.clearAllMocks()
    vi.mocked(claudeSessionsApi.get).mockResolvedValue(detailFixture())
    vi.mocked(claudeSessionsApi.refresh).mockResolvedValue(undefined as never)
    vi.mocked(insightsApi.getSession).mockRejectedValue(new Error('no insights'))
  })

  // The detail page had no way to pull fresh data from Claude — you had to go
  // back to the list, refresh there, and come back. This adds a Refresh button
  // that mirrors the list: trigger the server rescan, then reload the detail.
  it('rescans and reloads the session when Refresh is clicked', async () => {
    const user = userEvent.setup()
    renderPage()

    // Initial load.
    await waitFor(() => expect(claudeSessionsApi.get).toHaveBeenCalledTimes(1))

    await user.click(await screen.findByRole('button', { name: /refresh/i }))

    // Same behaviour as the list's refresh: kick off the background rescan that
    // re-reads Claude's JSONL, then re-fetch this session's detail.
    await waitFor(() => expect(claudeSessionsApi.refresh).toHaveBeenCalledTimes(1))
    await waitFor(
      () => expect(vi.mocked(claudeSessionsApi.get).mock.calls.length).toBeGreaterThanOrEqual(2),
      { timeout: 2000 },
    )
  })
})
