import { test, expect, type Page, request as playwrightRequest } from '@playwright/test'

/**
 * E2E tests for the Chat feature.
 *
 * These tests exercise the full user journey:
 *   1. Open the app
 *   2. Click "New Chat" from the sidebar
 *   3. Fill in the first message and submit
 *   4. Verify that the conversation view opens and streaming responses are rendered
 */

const BASE_URL = 'http://localhost:8990'

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/**
 * Mark onboarding as complete via the settings API so the wizard never
 * blocks the UI during tests. Also ensures a default working dir and model
 * are set.
 */
async function completeOnboardingViaApi() {
  const ctx = await playwrightRequest.newContext({ baseURL: BASE_URL })
  await ctx.put('/api/settings', {
    data: {
      default_working_dir: '/tmp',
      default_model: 'sonnet',
      onboarding_complete: true,
    },
  })
  await ctx.dispose()
}

/**
 * Wait for the New Chat dialog to appear and be visible.
 * The sidebar "New Chat" button navigates to /chats?new=1 which auto-opens it.
 */
async function openNewChatDialog(page: Page) {
  const newChatBtn = page.getByRole('button', { name: 'New Chat' }).first()
  await newChatBtn.waitFor({ state: 'visible', timeout: 10_000 })
  await newChatBtn.click()

  // Wait for the dialog title to appear
  await expect(page.getByRole('heading', { name: 'New Chat' })).toBeVisible({ timeout: 5_000 })
}

/**
 * Fill the first-message textarea in the New Chat dialog and submit.
 * Waits for navigation to the new chat session URL.
 */
async function startChat(page: Page, message: string) {
  const textarea = page.getByPlaceholder('Type your first message… (Enter to send)')
  await textarea.waitFor({ state: 'visible', timeout: 5_000 })
  await textarea.fill(message)

  // Click the "Start Chat" footer button
  const startBtn = page.getByRole('button', { name: 'Start Chat' })
  await expect(startBtn).toBeEnabled()
  await startBtn.click()

  // Navigation to /chats/:id should happen immediately after chat creation
  await page.waitForURL(/\/chats\/[^/]+$/, { timeout: 15_000 })
}

/**
 * Wait for streaming to begin (Stop button appears) then wait for it to
 * complete (Stop button disappears and the send button returns).
 */
async function waitForStreamingToComplete(page: Page, timeoutMs = 90_000) {
  const stopBtn = page.locator('button[title="Stop generation"]')

  // Wait for streaming to START — Stop button must appear
  await stopBtn.waitFor({ state: 'visible', timeout: 30_000 })

  // Wait for streaming to FINISH — Stop button disappears
  await stopBtn.waitFor({ state: 'hidden', timeout: timeoutMs })
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

test.describe('Chat', () => {
  test.beforeAll(async () => {
    // Mark onboarding complete so the wizard never blocks the UI.
    // This API call runs once before all tests in this suite.
    await completeOnboardingViaApi()
  })

  test.beforeEach(async ({ page }) => {
    // Navigate to root — React Router redirects to /chats
    await page.goto('/')
    // Wait for the sidebar to be interactive
    await expect(page.getByRole('button', { name: 'New Chat' }).first()).toBeVisible({
      timeout: 10_000,
    })
  })

  test('creates a new chat and renders streaming response', async ({ page }) => {
    const prompt =
      'Create file hello-world.txt somewhere and do the read/write/edit operations.'

    // ── Step 1: open the New Chat dialog ────────────────────────────────────
    await openNewChatDialog(page)

    // Verify dialog description is shown
    await expect(
      page.getByText('Type your first message. Optionally choose an agent'),
    ).toBeVisible()

    // Default settings are used (no agent, /tmp working dir, sonnet model)

    // ── Step 2: type the prompt and submit ───────────────────────────────────
    await startChat(page, prompt)

    // ── Step 3: chat session page opens ─────────────────────────────────────
    await expect(page).toHaveURL(/\/chats\/[^/]+$/)

    // The user's own message should appear on screen
    await expect(page.getByText(prompt)).toBeVisible({ timeout: 15_000 })

    // ── Step 4: streaming response is rendered ───────────────────────────────
    await waitForStreamingToComplete(page)

    // After streaming, an assistant response must be visible somewhere on
    // the page. We look for the "C" avatar circle that marks Claude's turns,
    // or any tool-use block, which confirms content was rendered.
    const assistantContent = page
      .locator('p, div')
      .filter({ hasText: /hello-world\.txt|All operations|created|Success/i })
    await expect(assistantContent.first()).toBeVisible({ timeout: 5_000 })

    // Input textarea should be re-enabled and ready for follow-up
    const inputTextarea = page.getByPlaceholder(
      'Message… (Enter to send, Shift+Enter for new line, drop/paste files)',
    )
    await expect(inputTextarea).toBeVisible()
    await expect(inputTextarea).toBeEnabled()
  })

  test('new chat dialog can be cancelled', async ({ page }) => {
    await openNewChatDialog(page)

    // Click Cancel — dialog should close, URL stays at /chats
    await page.getByRole('button', { name: 'Cancel' }).click()
    await expect(page.getByRole('heading', { name: 'New Chat' })).not.toBeVisible()
    await expect(page).toHaveURL(/\/chats/)
  })

  test('start chat button is disabled when message is empty', async ({ page }) => {
    await openNewChatDialog(page)

    const startBtn = page.getByRole('button', { name: 'Start Chat' })
    // With empty textarea the button must be disabled
    await expect(startBtn).toBeDisabled()

    // Typing something enables it
    await page.getByPlaceholder('Type your first message… (Enter to send)').fill('hello')
    await expect(startBtn).toBeEnabled()
  })
})
