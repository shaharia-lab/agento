import { describe, it, expect, beforeEach, vi } from 'vitest'
import { render, screen } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import SunsetBanner from './SunsetBanner'
import {
  SUNSET_CUTOFF,
  DESKTOP_RELEASES_URL,
  SHARED_DB_PATH,
  SUNSET_DISMISS_STORAGE_KEY,
} from '@/lib/sunset'

describe('SunsetBanner', () => {
  beforeEach(() => {
    localStorage.clear()
  })

  it('states the cutoff, the download link and that the database is shared', () => {
    render(<SunsetBanner />)

    expect(screen.getByText(SUNSET_CUTOFF)).toBeInTheDocument()
    expect(screen.getByText(SHARED_DB_PATH)).toBeInTheDocument()

    const link = screen.getByRole('link', { name: /get agento desktop/i })
    expect(link).toHaveAttribute('href', DESKTOP_RELEASES_URL)
    expect(link).toHaveAttribute('rel', expect.stringContaining('noopener'))
  })

  it('says the app keeps working past the cutoff', () => {
    render(<SunsetBanner />)
    // The "no notice wall" decision: nothing about this release stops the app,
    // and the banner has to say so or it reads as a shutdown warning.
    expect(screen.getByText(/only updating stops/i)).toBeInTheDocument()
  })

  it('is never modal or blocking', () => {
    render(<SunsetBanner />)
    expect(screen.queryByRole('dialog')).not.toBeInTheDocument()
    expect(screen.queryByRole('alertdialog')).not.toBeInTheDocument()
  })

  it('dismisses and persists the dismissal', async () => {
    const user = userEvent.setup()
    render(<SunsetBanner />)

    await user.click(screen.getByRole('button', { name: /dismiss/i }))

    expect(screen.queryByText(SUNSET_CUTOFF)).not.toBeInTheDocument()
    expect(localStorage.getItem(SUNSET_DISMISS_STORAGE_KEY)).toBe('1')
  })

  it('stays dismissed on remount', async () => {
    const user = userEvent.setup()
    const first = render(<SunsetBanner />)
    await user.click(screen.getByRole('button', { name: /dismiss/i }))
    first.unmount()

    render(<SunsetBanner />)
    expect(screen.queryByText(SUNSET_CUTOFF)).not.toBeInTheDocument()
  })

  it('does not re-arm on a timer', async () => {
    vi.useFakeTimers()
    try {
      localStorage.setItem(SUNSET_DISMISS_STORAGE_KEY, '1')
      render(<SunsetBanner />)
      expect(screen.queryByText(SUNSET_CUTOFF)).not.toBeInTheDocument()

      // UpdateBanner re-checks hourly; this banner must have no such loop.
      await vi.advanceTimersByTimeAsync(25 * 60 * 60 * 1000)
      expect(screen.queryByText(SUNSET_CUTOFF)).not.toBeInTheDocument()
    } finally {
      vi.useRealTimers()
    }
  })

  it('shows the banner when localStorage is unavailable', () => {
    const getItem = vi.spyOn(Storage.prototype, 'getItem').mockImplementation(() => {
      throw new Error('denied')
    })
    try {
      render(<SunsetBanner />)
      // Failing closed here would mean a user in a restricted browsing context
      // never learns their install is being retired.
      expect(screen.getByText(SUNSET_CUTOFF)).toBeInTheDocument()
    } finally {
      getItem.mockRestore()
    }
  })

  it('still closes when the dismissal cannot be stored', async () => {
    const user = userEvent.setup()
    const setItem = vi.spyOn(Storage.prototype, 'setItem').mockImplementation(() => {
      throw new Error('denied')
    })
    try {
      render(<SunsetBanner />)
      await user.click(screen.getByRole('button', { name: /dismiss/i }))
      expect(screen.queryByText(SUNSET_CUTOFF)).not.toBeInTheDocument()
    } finally {
      setItem.mockRestore()
    }
  })
})
