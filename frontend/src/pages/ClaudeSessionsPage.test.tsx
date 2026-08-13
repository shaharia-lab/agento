import { describe, it, expect, vi, beforeAll, beforeEach } from 'vitest'
import { render, screen, waitFor } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { MemoryRouter } from 'react-router-dom'
import ClaudeSessionsPage from './ClaudeSessionsPage'
import { claudeSessionsApi } from '@/lib/api'
import type { ClaudeSessionPage, ClaudeSessionFacets, ClaudeSessionStatus } from '@/types'

vi.mock('@/lib/api', () => ({
  claudeSessionsApi: {
    list: vi.fn(),
    facets: vi.fn(),
    projects: vi.fn(),
    status: vi.fn(),
    refresh: vi.fn(),
    toggleFavorite: vi.fn(),
  },
}))

// Two indexed accounts, so the account switcher renders at all — it only
// appears when the corpus spans more than one Claude config dir.
const ACCOUNT_A = '/home/me/.claude'
const ACCOUNT_B = '/home/me/.claude-personal'

const emptyPage: ClaudeSessionPage = { items: [], next_cursor: '', has_more: false }

const facets = (overrides: Partial<ClaudeSessionFacets> = {}): ClaudeSessionFacets => ({
  total: 0,
  total_tokens: 0,
  total_cost_usd: 0,
  token_p90: 0,
  models: [],
  permission_modes: [],
  config_dirs: [ACCOUNT_A, ACCOUNT_B],
  has_favorites: false,
  has_prs: false,
  ...overrides,
})

const idleStatus: ClaudeSessionStatus = {
  costs_stale: false,
  scan_in_progress: false,
  files_done: 0,
  files_total: 0,
  last_scanned_at: '2026-08-13T00:00:00Z',
}

// The config_dir carried by the most recent list request, or null if the list
// has never been asked to narrow to an account.
const lastRequestedConfigDir = (): string | null => {
  const calls = vi.mocked(claudeSessionsApi.list).mock.calls
  if (calls.length === 0) return null
  return calls[calls.length - 1][0]?.filters?.get('config_dir') ?? null
}

beforeAll(() => {
  // Radix Select and its Popper reach for browser APIs jsdom does not implement;
  // without these the trigger cannot open and the list's infinite-scroll
  // sentinel throws on mount.
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

describe('ClaudeSessionsPage account switcher', () => {
  beforeEach(() => {
    vi.clearAllMocks()
    vi.mocked(claudeSessionsApi.list).mockResolvedValue(emptyPage)
    vi.mocked(claudeSessionsApi.facets).mockResolvedValue(facets())
    vi.mocked(claudeSessionsApi.projects).mockResolvedValue([])
    vi.mocked(claudeSessionsApi.status).mockResolvedValue(idleStatus)
  })

  // The regression: selecting a different account set the dropdown value but
  // never reloaded the list, because `filterConfigDir` was missing from the
  // `filters` useMemo dependency array. The list only picked the account up
  // when some *other* filter (e.g. the project) changed and recomputed
  // `filters` for it. This asserts the account switch alone drives a refetch.
  it('reloads the session list when the account changes', async () => {
    const user = userEvent.setup({ pointerEventsCheck: 0 })

    render(
      <MemoryRouter>
        <ClaudeSessionsPage />
      </MemoryRouter>,
    )

    // Initial load narrows to no account ("All accounts").
    await waitFor(() => expect(claudeSessionsApi.list).toHaveBeenCalled())
    expect(lastRequestedConfigDir()).toBeNull()

    // Switch the account to the second config dir. Radix's SelectTrigger exposes
    // no accessible name in jsdom, so target it by the value it currently shows.
    const accountTrigger = screen
      .getAllByRole('combobox')
      .find(el => el.textContent?.includes('All accounts'))
    expect(accountTrigger).toBeDefined()
    await user.click(accountTrigger!)
    await user.click(await screen.findByText('~/.claude-personal'))

    // The list must refetch, now scoped to the chosen account.
    await waitFor(() => expect(lastRequestedConfigDir()).toBe(ACCOUNT_B))
  })
})
