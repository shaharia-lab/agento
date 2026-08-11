import { describe, it, expect, vi, beforeEach } from 'vitest'
import { render, screen, waitFor } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import DataAnalyticsTab from './DataAnalyticsTab'
import { settingsApi, claudeSessionsApi } from '@/lib/api'
import type { SettingsResponse } from '@/types'

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

const projects = [
  { encoded_name: '-home-me-agento', decoded_path: '/home/me/agento', session_count: 12 },
  { encoded_name: '-home-me-scratch', decoded_path: '/home/me/scratch', session_count: 3 },
]

describe('DataAnalyticsTab', () => {
  beforeEach(() => {
    // Calls accumulate across tests otherwise, and several assertions read the
    // first recorded call.
    vi.clearAllMocks()
    vi.mocked(settingsApi.get).mockResolvedValue(settingsResponse())
    vi.mocked(settingsApi.update).mockResolvedValue(settingsResponse())
    vi.mocked(claudeSessionsApi.projects).mockResolvedValue(projects)
  })

  // The tab must ask for hidden projects too, or an already hidden project
  // could never be unhidden: it is filtered out of every other listing.
  it('loads every project, including hidden ones', async () => {
    render(<DataAnalyticsTab />)

    expect(await screen.findByText('/home/me/agento')).toBeInTheDocument()
    expect(screen.getByText('/home/me/scratch')).toBeInTheDocument()
    expect(claudeSessionsApi.projects).toHaveBeenCalledWith(true)
  })

  it('shows an unset threshold as the default rather than as zero', async () => {
    render(<DataAnalyticsTab />)

    expect(await screen.findByLabelText('Idle Threshold')).toHaveValue(10)
  })

  it('checks visible projects and unchecks hidden ones', async () => {
    vi.mocked(settingsApi.get).mockResolvedValue(
      settingsResponse({ hidden_projects: ['/home/me/scratch'] }),
    )
    render(<DataAnalyticsTab />)

    const scratch = await screen.findByLabelText('Include /home/me/scratch in analytics')
    const agento = screen.getByLabelText('Include /home/me/agento in analytics')

    expect(scratch).not.toBeChecked()
    expect(agento).toBeChecked()
  })

  it('saves the projects the user unchecked', async () => {
    const user = userEvent.setup()
    render(<DataAnalyticsTab />)

    await user.click(await screen.findByLabelText('Include /home/me/scratch in analytics'))
    await user.click(screen.getByRole('button', { name: /save data settings/i }))

    await waitFor(() => expect(settingsApi.update).toHaveBeenCalled())
    expect(vi.mocked(settingsApi.update).mock.calls[0][0]).toMatchObject({
      hidden_projects: ['/home/me/scratch'],
      idle_gap_threshold_minutes: 10,
    })
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

  // Bulk actions apply to what is on screen. Hiding everything while a filter
  // is active must not sweep away projects the user cannot see.
  it('limits "Hide all" to the filtered projects', async () => {
    const user = userEvent.setup()
    render(<DataAnalyticsTab />)

    await user.type(await screen.findByLabelText('Filter projects'), 'scratch')
    await user.click(screen.getByRole('button', { name: /hide all/i }))
    await user.click(screen.getByRole('button', { name: /save data settings/i }))

    await waitFor(() => expect(settingsApi.update).toHaveBeenCalled())
    expect(vi.mocked(settingsApi.update).mock.calls[0][0]).toMatchObject({
      hidden_projects: ['/home/me/scratch'],
    })
  })
})
