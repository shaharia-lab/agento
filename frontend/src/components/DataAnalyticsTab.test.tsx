import { describe, it, expect, vi, beforeEach } from 'vitest'
import { render, screen, waitFor, within } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import DataAnalyticsTab from './DataAnalyticsTab'
import { settingsApi, claudeSessionsApi } from '@/lib/api'
import type { SettingsResponse, ClaudeProject } from '@/types'

vi.mock('@/lib/api', () => ({
  settingsApi: { get: vi.fn(), update: vi.fn() },
  claudeSessionsApi: { projects: vi.fn() },
}))

const settingsResponse = (overrides = {}): SettingsResponse => ({
  settings: {
    default_working_dir: '/tmp/work',
    default_model: 'sonnet',
    onboarding_complete: true,
    hidden_projects: [],
    idle_gap_threshold_minutes: 0,
    ...overrides,
  },
  locked: {},
  model_from_env: false,
})

const projects: ClaudeProject[] = [
  { encoded_name: '-home-me-agento', decoded_path: '/home/me/agento', session_count: 12 },
  { encoded_name: '-home-me-scratch', decoded_path: '/home/me/scratch', session_count: 3 },
  { encoded_name: '-home-me-blog', decoded_path: '/home/me/blog', session_count: 1 },
]

// A corpus big enough that rendering all of it would be the bug: the picker
// must stay a set of suggestions, not an inventory.
const manyProjects: ClaudeProject[] = Array.from({ length: 500 }, (_, i) => ({
  encoded_name: `-home-me-p${i}`,
  decoded_path: `/home/me/p${i}`,
  session_count: 1,
}))

const suggestions = () => screen.queryAllByRole('option')

describe('DataAnalyticsTab', () => {
  beforeEach(() => {
    // Calls accumulate across tests otherwise, and several assertions read the
    // first recorded call.
    vi.clearAllMocks()
    vi.mocked(settingsApi.get).mockResolvedValue(settingsResponse())
    vi.mocked(settingsApi.update).mockResolvedValue(settingsResponse())
    vi.mocked(claudeSessionsApi.projects).mockResolvedValue(projects)
  })

  // The picker must know about every project, or one that is already excluded
  // could never be found and restored from here.
  it('asks for hidden projects too', async () => {
    render(<DataAnalyticsTab />)

    await screen.findByLabelText('Excluded Projects')
    expect(claudeSessionsApi.projects).toHaveBeenCalledWith(true)
  })

  it('shows an unset threshold as the default rather than as zero', async () => {
    render(<DataAnalyticsTab />)

    expect(await screen.findByLabelText('Idle Threshold')).toHaveValue(10)
  })

  it('lists what is excluded and says so when nothing is', async () => {
    vi.mocked(settingsApi.get).mockResolvedValue(
      settingsResponse({ hidden_projects: ['/home/me/scratch'] }),
    )
    render(<DataAnalyticsTab />)

    const list = await screen.findByRole('list')
    expect(within(list).getByText('/home/me/scratch')).toBeInTheDocument()
    expect(within(list).queryByText('/home/me/agento')).not.toBeInTheDocument()
  })

  it('excludes a project picked from the search results', async () => {
    const user = userEvent.setup()
    render(<DataAnalyticsTab />)

    await user.type(await screen.findByLabelText('Excluded Projects'), 'scratch')
    await user.click(screen.getByRole('option', { name: /scratch/ }))
    await user.click(screen.getByRole('button', { name: /save data settings/i }))

    await waitFor(() => expect(settingsApi.update).toHaveBeenCalled())
    expect(vi.mocked(settingsApi.update).mock.calls[0][0]).toMatchObject({
      hidden_projects: ['/home/me/scratch'],
      idle_gap_threshold_minutes: 10,
    })
  })

  it('removes an exclusion', async () => {
    const user = userEvent.setup()
    vi.mocked(settingsApi.get).mockResolvedValue(
      settingsResponse({ hidden_projects: ['/home/me/scratch', '/home/me/blog'] }),
    )
    render(<DataAnalyticsTab />)

    await user.click(await screen.findByLabelText('Stop excluding /home/me/scratch'))
    await user.click(screen.getByRole('button', { name: /save data settings/i }))

    await waitFor(() => expect(settingsApi.update).toHaveBeenCalled())
    expect(vi.mocked(settingsApi.update).mock.calls[0][0]).toMatchObject({
      hidden_projects: ['/home/me/blog'],
    })
  })

  // An already excluded project is not a candidate to exclude again.
  it('keeps excluded projects out of the suggestions', async () => {
    const user = userEvent.setup()
    vi.mocked(settingsApi.get).mockResolvedValue(
      settingsResponse({ hidden_projects: ['/home/me/scratch'] }),
    )
    render(<DataAnalyticsTab />)

    await user.click(await screen.findByLabelText('Excluded Projects'))

    const paths = suggestions().map(o => o.textContent)
    expect(paths.some(p => p?.includes('/home/me/agento'))).toBe(true)
    expect(paths.some(p => p?.includes('/home/me/scratch'))).toBe(false)
  })

  // The reason this is a search box rather than a checkbox per project: a
  // corpus can hold hundreds, and rendering them all is unusable. The list
  // scrolls, so the cap bounds rendering per keystroke rather than reach.
  it('caps the suggestions and says how many were left out', async () => {
    const user = userEvent.setup()
    vi.mocked(claudeSessionsApi.projects).mockResolvedValue(manyProjects)
    render(<DataAnalyticsTab />)

    await user.click(await screen.findByLabelText('Excluded Projects'))

    expect(suggestions()).toHaveLength(50)
    expect(screen.getByText(/450 more/)).toBeInTheDocument()

    await user.type(screen.getByLabelText('Excluded Projects'), 'p497')
    expect(suggestions()).toHaveLength(1)
    expect(screen.queryByText(/more\. Keep typing/)).not.toBeInTheDocument()
  })

  it('excludes the top match on Enter', async () => {
    const user = userEvent.setup()
    render(<DataAnalyticsTab />)

    await user.type(await screen.findByLabelText('Excluded Projects'), 'blog{Enter}')

    expect(screen.getByLabelText('Stop excluding /home/me/blog')).toBeInTheDocument()
  })

  it('saves a changed idle threshold', async () => {
    const user = userEvent.setup()
    render(<DataAnalyticsTab />)

    const input = await screen.findByLabelText('Idle Threshold')
    await user.clear(input)
    await user.type(input, '25')
    await user.click(screen.getByRole('button', { name: /save data settings/i }))

    await waitFor(() => expect(settingsApi.update).toHaveBeenCalled())
    expect(vi.mocked(settingsApi.update).mock.calls[0][0]).toMatchObject({
      idle_gap_threshold_minutes: 25,
    })
  })

  // Out-of-range values are rejected by the backend, so the form must not send
  // them: a failed save would lose the rest of the user's edits with it.
  it('blocks saving an out-of-range threshold', async () => {
    const user = userEvent.setup()
    render(<DataAnalyticsTab />)

    const input = await screen.findByLabelText('Idle Threshold')
    await user.clear(input)
    await user.type(input, '9000')

    expect(screen.getByText(/between 1 and 240 minutes/i)).toBeInTheDocument()
    expect(screen.getByRole('button', { name: /save data settings/i })).toBeDisabled()
    expect(settingsApi.update).not.toHaveBeenCalled()
  })
})
