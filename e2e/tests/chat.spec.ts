import { test, expect, type Page, request as playwrightRequest } from '@playwright/test'

/**
 * E2E tests for the Chat feature.
 *
 * These tests exercise the full user journey including:
 *   - New Chat dialog open / cancel / disabled-state
 *   - Streaming rendering: thinking blocks, tool call cards, completion dots
 */

const BASE_URL = 'http://localhost:8990'

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/**
 * Mark onboarding as complete via the settings API so the wizard never
 * blocks the UI during tests. Sets working dir to /tmp and model to sonnet.
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

/** Click the sidebar "New Chat" button and wait for the dialog to open. */
async function openNewChatDialog(page: Page) {
  const newChatBtn = page.getByRole('button', { name: 'New Chat' }).first()
  await newChatBtn.waitFor({ state: 'visible', timeout: 10_000 })
  await newChatBtn.click()
  await expect(page.getByRole('heading', { name: 'New Chat' })).toBeVisible({ timeout: 5_000 })
}

/**
 * Fill the first-message textarea in the New Chat dialog and submit.
 * Waits until the URL changes to /chats/:id.
 */
async function startChat(page: Page, message: string) {
  const textarea = page.getByPlaceholder('Type your first message… (Enter to send)')
  await textarea.waitFor({ state: 'visible', timeout: 5_000 })
  await textarea.fill(message)

  const startBtn = page.getByRole('button', { name: 'Start Chat' })
  await expect(startBtn).toBeEnabled()
  await startBtn.click()

  await page.waitForURL(/\/chats\/[^/]+$/, { timeout: 15_000 })
}

/**
 * Wait for streaming to begin (Stop button appears) then wait for it to
 * complete (Stop button disappears).
 */
async function waitForStreamingToComplete(page: Page, timeoutMs = 90_000) {
  const stopBtn = page.locator('button[title="Stop generation"]')
  await stopBtn.waitFor({ state: 'visible', timeout: 30_000 })
  await stopBtn.waitFor({ state: 'hidden', timeout: timeoutMs })
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

test.describe('Chat', () => {
  test.beforeAll(async () => {
    // Mark onboarding complete so the wizard never blocks the UI.
    await completeOnboardingViaApi()
  })

  test.beforeEach(async ({ page }) => {
    await page.goto('/')
    await expect(page.getByRole('button', { name: 'New Chat' }).first()).toBeVisible({
      timeout: 10_000,
    })
  })

  // ── Main journey ──────────────────────────────────────────────────────────

  test('creates a new chat and renders streaming response with thinking and tool call blocks', async ({ page }) => {
    const prompt =
      'Create file hello-world.txt somewhere and do the read/write/edit operations.'

    // ── 1. Open dialog and start chat ────────────────────────────────────────
    await openNewChatDialog(page)
    await expect(page.getByText('Type your first message. Optionally choose an agent')).toBeVisible()
    await startChat(page, prompt)

    await expect(page).toHaveURL(/\/chats\/[^/]+$/)

    // User message must appear in the conversation
    await expect(page.getByText(prompt)).toBeVisible({ timeout: 15_000 })

    // ── 2. During streaming: thinking block and first tool call appear ────────
    // The Stop button marks that streaming has started
    const stopBtn = page.locator('button[title="Stop generation"]')
    await stopBtn.waitFor({ state: 'visible', timeout: 30_000 })

    // At least one tool call card is rendered while streaming
    // (e.g. "Write hello-world.txt" collapses into span.font-mono.font-semibold)
    const firstToolName = page.locator('span.font-mono.font-semibold').first()
    await firstToolName.waitFor({ state: 'visible', timeout: 30_000 })

    // ── 3. Wait for streaming to finish ──────────────────────────────────────
    await stopBtn.waitFor({ state: 'hidden', timeout: 90_000 })

    // ── 4. After streaming: thinking blocks (if any) ─────────────────────────
    // Claude uses ThinkingAdaptive by default — thinking may or may not appear.
    // If it does appear, verify the toggle can be expanded to reveal content.
    const thinkingToggles = page.getByRole('button', { name: 'Thinking' })
    const thinkingCount = await thinkingToggles.count()
    if (thinkingCount > 0) {
      await thinkingToggles.first().click()
      const thinkingContent = page.locator('div.font-mono.whitespace-pre-wrap').first()
      await expect(thinkingContent).toBeVisible({ timeout: 3_000 })
      const thinkingText = await thinkingContent.textContent()
      expect(thinkingText?.length ?? 0).toBeGreaterThan(0)
    }

    // ── 5. After streaming: at least one tool call was rendered ──────────────
    // Claude is non-deterministic — it may use any combination of tools
    // (Write, Read, Edit, Bash, …). We only assert that at least one tool call
    // card rendered; we do not enforce specific names or ordering.
    const toolNameSpans = page.locator('span.font-mono.font-semibold')
    const toolNameCount = await toolNameSpans.count()
    expect(toolNameCount).toBeGreaterThan(0)

    // ── 6. After streaming: completed tool calls have green dots ─────────────
    // Every ToolCallCard that received a result renders a span.bg-emerald-400.
    // There must be at least one, and no more than the total number of tool calls.
    const completionDots = page.locator('span.bg-emerald-400')
    const dotCount = await completionDots.count()
    expect(dotCount).toBeGreaterThan(0)
    expect(dotCount).toBeLessThanOrEqual(toolNameCount)

    // ── 7. Input textarea re-enabled for follow-up ────────────────────────────
    const inputTextarea = page.getByPlaceholder(
      'Message… (Enter to send, Shift+Enter for new line, drop/paste files)',
    )
    await expect(inputTextarea).toBeVisible()
    await expect(inputTextarea).toBeEnabled()
  })

  // ── Dialog UX ─────────────────────────────────────────────────────────────

  test('new chat dialog can be cancelled', async ({ page }) => {
    await openNewChatDialog(page)
    await page.getByRole('button', { name: 'Cancel' }).click()
    await expect(page.getByRole('heading', { name: 'New Chat' })).not.toBeVisible()
    await expect(page).toHaveURL(/\/chats/)
  })

  test('start chat button is disabled when message is empty', async ({ page }) => {
    await openNewChatDialog(page)

    const startBtn = page.getByRole('button', { name: 'Start Chat' })
    await expect(startBtn).toBeDisabled()

    await page.getByPlaceholder('Type your first message… (Enter to send)').fill('hello')
    await expect(startBtn).toBeEnabled()
  })
})
