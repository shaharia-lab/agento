import { test, expect, type Page, request as playwrightRequest } from '@playwright/test'

/**
 * E2E tests for the Claude Sessions list.
 *
 * The list used to ship every session to the browser and filter, sort, group
 * and render all of them there. Everything below is an assertion about the
 * replacement being genuinely server-side: the browser must hold one page, the
 * counters must describe the whole filtered set rather than the page, and the
 * predicates must run in SQL.
 *
 * These read the machine's real ~/.claude corpus — the binary scans
 * $HOME/.claude/projects and there is no override — so each test skips when the
 * machine has too few sessions to exercise what it is checking. That matches
 * the rest of this suite, which is documented as local-only.
 */

const BASE_URL = `http://localhost:${process.env.AGENTO_E2E_PORT ?? 8990}`

/** The page size the list requests; the sentinel appears above this. */
const PAGE_SIZE = 50

async function completeOnboardingViaApi() {
  const ctx = await playwrightRequest.newContext({ baseURL: BASE_URL })
  await ctx.put('/api/settings', {
    data: { default_working_dir: '/tmp', default_model: 'sonnet', onboarding_complete: true },
  })
  await ctx.dispose()
}

/**
 * Waits out the first scan.
 *
 * The suite starts against an empty data directory, so the first run reads the
 * whole corpus. Since that scan no longer blocks reads, a count taken while it
 * is running is a count of however much had been written by then — every
 * assertion about totals here has to be made after it settles.
 */
async function waitForScan() {
  const ctx = await playwrightRequest.newContext({ baseURL: BASE_URL })
  try {
    const deadline = Date.now() + 180_000
    for (;;) {
      const status = await (await ctx.get('/api/claude-sessions/status')).json()
      if (!status.scan_in_progress && status.last_scanned_at) return
      if (Date.now() > deadline) throw new Error('the initial scan did not finish in time')
      await new Promise(r => setTimeout(r, 1000))
    }
  } finally {
    await ctx.dispose()
  }
}

/** How many sessions the corpus holds, from the facet aggregate. */
async function totalSessions(): Promise<number> {
  const ctx = await playwrightRequest.newContext({ baseURL: BASE_URL })
  const res = await ctx.get('/api/claude-sessions/facets')
  const body = await res.json()
  await ctx.dispose()
  return body.total as number
}

/** Rows currently in the DOM. */
function sessionRows(page: Page) {
  return page.locator('[role="button"][aria-expanded]')
}

/**
 * Opens the list and waits for the first page.
 *
 * A cold cache no longer blocks the request, so the first scan of a large
 * corpus can still be running — the empty state says so, and the list reloads
 * itself when the status poll clears.
 */
async function openSessions(page: Page) {
  await page.goto('/claude-sessions')
  await expect(
    page.getByRole('heading', { name: 'Claude Sessions' }).or(page.getByText('Scanning ~/.claude')),
  ).toBeVisible()
  await expect(sessionRows(page).first()).toBeVisible({ timeout: 60_000 })
}

test.describe('Claude Sessions list', () => {
  test.beforeAll(async () => {
    await completeOnboardingViaApi()
    // The status endpoint reports files_done/files_total while this runs, which
    // is what the list's empty state shows instead of "no sessions".
    await waitForScan()
  })

  test('renders one page, not the corpus', async ({ page }) => {
    const total = await totalSessions()
    test.skip(total <= PAGE_SIZE, `needs more than ${PAGE_SIZE} sessions, corpus has ${total}`)

    await openSessions(page)
    // The wall this work exists for is DOM size: at 5,000 sessions the old list
    // rendered ~340k nodes. One page is the invariant.
    await expect(sessionRows(page)).toHaveCount(PAGE_SIZE)

    // …while the counter still describes everything the filter matches.
    await expect(page.getByText(`${total} session`).first()).toBeVisible()
    await expect(page.getByText(`showing ${PAGE_SIZE} of ${total}`)).toBeVisible()
  })

  test('loads the next page when the end scrolls into view', async ({ page }) => {
    const total = await totalSessions()
    test.skip(total <= PAGE_SIZE, `needs more than ${PAGE_SIZE} sessions, corpus has ${total}`)

    await openSessions(page)
    await expect(sessionRows(page)).toHaveCount(PAGE_SIZE)

    // Auto-loading on intersection rather than a click, because that is what
    // the list did before it was paged: the change is meant to bound the
    // browser's memory, not to add a click per fifty rows.
    await page.getByText(`showing ${PAGE_SIZE} of ${total}`).scrollIntoViewIfNeeded()

    const expected = Math.min(PAGE_SIZE * 2, total)
    await expect(sessionRows(page)).toHaveCount(expected, { timeout: 20_000 })
  })

  test('searches server-side and reports the matching total', async ({ page }) => {
    const total = await totalSessions()
    test.skip(total === 0, 'no sessions on this machine')

    await openSessions(page)
    const before = await sessionRows(page).count()

    // A string that cannot match anything: the empty state must say "no
    // matches", not "no sessions" — with a paged list an empty page means
    // nothing on its own, so the message is chosen from the filter.
    await page.getByPlaceholder('Search by ID or message…').fill('zzz-no-such-session-zzz')
    await expect(page.getByText('No sessions match your filters.')).toBeVisible({ timeout: 15_000 })
    await expect(page.getByText('0 sessions ·')).toBeVisible()

    await page.getByPlaceholder('Search by ID or message…').fill('')
    await expect(sessionRows(page)).toHaveCount(before, { timeout: 15_000 })
  })

  test('sorting by cost reorders server-side and drops the day headers', async ({ page }) => {
    const total = await totalSessions()
    test.skip(total <= 1, 'needs at least two sessions')

    await openSessions(page)
    // Day headers are meaningful under the recency sort and nowhere else: under
    // "highest cost" two adjacent rows can be weeks apart.
    const dayHeaders = page.getByText(/^\d+ sessions? · \d+ msgs/)
    await expect(dayHeaders.first()).toBeVisible()

    await page.getByRole('combobox').filter({ hasText: 'Most recent' }).click()
    await page.getByRole('option', { name: 'Highest cost' }).click()

    await expect(dayHeaders).toHaveCount(0, { timeout: 15_000 })

    // Costs descend. Parsed from the rendered column, so this checks what the
    // reader sees rather than what the API returned.
    const costs = await page.locator('[role="button"][aria-expanded] >> text=/^\\$/').allInnerTexts()
    const values = costs.map(c => Number(c.replace(/[$,]/g, ''))).filter(Number.isFinite)
    expect(values.length).toBeGreaterThan(1)
    for (let i = 1; i < values.length; i++) {
      expect(values[i]).toBeLessThanOrEqual(values[i - 1])
    }
  })
})

test.describe('Claude session transcript', () => {
  test.beforeAll(async () => {
    await completeOnboardingViaApi()
    await waitForScan()
  })

  /** The busiest session on this machine, which is the one worth windowing. */
  async function largestSessionId(): Promise<{ id: string; messages: number }> {
    const ctx = await playwrightRequest.newContext({ baseURL: BASE_URL })
    const res = await ctx.get('/api/claude-sessions?sort=messages&limit=1')
    const body = await res.json()
    await ctx.dispose()
    const top = body.items?.[0]
    return { id: top?.session_id ?? '', messages: top?.message_count ?? 0 }
  }

  test('renders a window of the transcript, not all of it', async ({ page }) => {
    const { id, messages } = await largestSessionId()
    test.skip(!id || messages < 250, `needs a session with 250+ messages, largest has ${messages}`)

    await page.goto(`/claude-sessions/${id}`)
    // The footer states what is on screen against what is there, so a windowed
    // list is never mistaken for the whole transcript.
    const footer = page.getByText(/Showing \d+ of \d+ events/)
    await expect(footer).toBeVisible({ timeout: 30_000 })

    const [shown, total] = (await footer.innerText()).match(/\d+/g)!.map(Number)
    expect(shown).toBeLessThan(total)
    expect(await page.locator('[id^="event-"]').count()).toBeLessThanOrEqual(shown)
  })

  test('the timeline reveals and scrolls to an event past the window', async ({ page }) => {
    const { id, messages } = await largestSessionId()
    test.skip(!id || messages < 250, `needs a session with 250+ messages, largest has ${messages}`)

    await page.goto(`/claude-sessions/${id}`)
    await expect(page.getByText(/Showing \d+ of \d+ events/)).toBeVisible({ timeout: 30_000 })

    const entries = page.locator('button').filter({ hasText: /^\d{2}:\d{2}/ })
    const count = await entries.count()
    test.skip(count < 2, 'needs a timeline with more than one entry')

    const before = await page.locator('[id^="event-"]').count()
    // The last entry is the one most likely to sit past the rendered window,
    // which is exactly the case that used to scroll to a node that did not
    // exist: the sidebar reached rows through document.getElementById.
    await entries.nth(count - 1).click()

    await expect
      .poll(async () => page.locator('[id^="event-"]').count(), { timeout: 15_000 })
      .toBeGreaterThanOrEqual(before)

    // Whatever the timeline pointed at is now on screen.
    await expect
      .poll(
        async () =>
          page.evaluate(() => {
            const rows = [...document.querySelectorAll('[id^="event-"]')]
            return rows.some(r => {
              const box = r.getBoundingClientRect()
              return box.top >= 0 && box.top < window.innerHeight && box.height > 0
            })
          }),
        { timeout: 15_000 },
      )
      .toBe(true)
    // And the page actually moved rather than staying at the top.
    await expect
      .poll(
        async () =>
          page.evaluate(() =>
            [...document.querySelectorAll('div')].some(
              d => d.className.includes('overflow-y-auto') && d.scrollTop > 200,
            ),
          ),
        { timeout: 15_000 },
      )
      .toBe(true)
  })

  test('searching the transcript narrows it without reloading', async ({ page }) => {
    const { id, messages } = await largestSessionId()
    test.skip(!id || messages < 20, 'needs a session with some messages')

    await page.goto(`/claude-sessions/${id}`)
    await expect(page.locator('[id^="event-"]').first()).toBeVisible({ timeout: 30_000 })
    const before = await page.locator('[id^="event-"]').count()

    // Debounced and matched against a precomputed index rather than
    // re-serializing every tool input per keystroke.
    await page.getByPlaceholder('Search this transcript…').fill('zzz-no-such-text-zzz')
    await expect(page.getByText('No events match the current filter.')).toBeVisible({
      timeout: 15_000,
    })

    await page.getByPlaceholder('Search this transcript…').fill('')
    await expect
      .poll(async () => page.locator('[id^="event-"]').count(), { timeout: 15_000 })
      .toBe(before)
  })
})
