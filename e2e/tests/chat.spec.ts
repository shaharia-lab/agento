import fs from 'fs'
import { test, expect, type Page, request as playwrightRequest } from '@playwright/test'

/**
 * E2E tests for the Chat feature.
 *
 * Covers:
 *   - New Chat dialog (open, cancel, disabled state)
 *   - Full streaming journey: thinking blocks, tool call cards, completion dots
 *   - Follow-up message in an existing session
 *   - Stop generation mid-stream
 *   - Completed chat appears in the chat list
 */

const BASE_URL = 'http://localhost:8990'

/**
 * Files Claude may create when running the hello-world file-operations prompt.
 * Cleaned up after tests that use that prompt.
 */
const CLAUDE_ARTIFACTS = [
  '/tmp/hello-world.txt',
  '/tmp/hello_world.txt',
]

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

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

function cleanupArtifacts() {
  for (const f of CLAUDE_ARTIFACTS) {
    try {
      if (fs.existsSync(f)) fs.rmSync(f)
    } catch {
      // best-effort — file may not exist or may have a different path
    }
  }
}

/** Navigate to root and wait for the sidebar "New Chat" button to be ready. */
async function loadApp(page: Page) {
  await page.goto('/')
  await expect(page.getByRole('button', { name: 'New Chat' }).first()).toBeVisible({
    timeout: 10_000,
  })
}

/** Click the sidebar "New Chat" button and wait for the dialog. */
async function openNewChatDialog(page: Page) {
  await page.getByRole('button', { name: 'New Chat' }).first().click()
  await expect(page.getByRole('heading', { name: 'New Chat' })).toBeVisible({ timeout: 5_000 })
}

/**
 * Fill the first-message textarea and click Start Chat.
 * Returns after the URL changes to /chats/:id.
 */
async function startChat(page: Page, message: string) {
  await page.getByPlaceholder('Type your first message… (Enter to send)').fill(message)
  await page.getByRole('button', { name: 'Start Chat' }).click()
  await page.waitForURL(/\/chats\/[^/]+$/, { timeout: 15_000 })
}

/** Wait for streaming to start (Stop button appears) then finish (Stop disappears). */
async function waitForStreamingToComplete(page: Page, timeoutMs = 90_000) {
  const stopBtn = page.locator('button[title="Stop generation"]')
  await stopBtn.waitFor({ state: 'visible', timeout: 30_000 })
  await stopBtn.waitFor({ state: 'hidden', timeout: timeoutMs })
}

// ---------------------------------------------------------------------------
// Suite
// ---------------------------------------------------------------------------

test.describe('Chat', () => {
  test.beforeAll(async () => {
    await completeOnboardingViaApi()
  })

  test.beforeEach(async ({ page }) => {
    await loadApp(page)
  })

  // ── 1. Full streaming journey ──────────────────────────────────────────────

  test('creates a new chat and renders streaming response with thinking and tool call blocks', async ({ page }) => {
    const prompt = 'Create file hello-world.txt somewhere and do the read/write/edit operations.'

    await openNewChatDialog(page)
    await expect(page.getByText('Type your first message. Optionally choose an agent')).toBeVisible()
    await startChat(page, prompt)

    // ── Chat session page opened ─────────────────────────────────────────────
    await expect(page).toHaveURL(/\/chats\/[^/]+$/)
    await expect(page.getByText(prompt)).toBeVisible({ timeout: 15_000 })

    // ── Stop button marks streaming started ──────────────────────────────────
    const stopBtn = page.locator('button[title="Stop generation"]')
    await stopBtn.waitFor({ state: 'visible', timeout: 30_000 })

    // ── At least one tool call card renders while streaming ──────────────────
    await page.locator('span.font-mono.font-semibold').first().waitFor({ state: 'visible', timeout: 30_000 })

    // ── Wait for streaming to finish ─────────────────────────────────────────
    await stopBtn.waitFor({ state: 'hidden', timeout: 90_000 })

    // ── Thinking blocks (if Claude emitted any) ──────────────────────────────
    // ThinkingAdaptive is the default — Claude decides whether to think.
    // If thinking blocks are present: verify each has a toggle and can be expanded.
    const thinkingToggles = page.getByRole('button', { name: 'Thinking' })
    const thinkingCount = await thinkingToggles.count()
    if (thinkingCount > 0) {
      // The toggle should be visible (collapsed by default)
      await expect(thinkingToggles.first()).toBeVisible()
      // Clicking it should expand the block and reveal thinking text
      await thinkingToggles.first().click()
      const thinkingContent = page.locator('div.font-mono.whitespace-pre-wrap').first()
      await expect(thinkingContent).toBeVisible({ timeout: 3_000 })
      expect((await thinkingContent.textContent())?.length ?? 0).toBeGreaterThan(0)
    }

    // ── At least one tool call card rendered ─────────────────────────────────
    // Claude is non-deterministic — tool names and order vary per run.
    const toolNameSpans = page.locator('span.font-mono.font-semibold')
    const toolCount = await toolNameSpans.count()
    expect(toolCount).toBeGreaterThan(0)

    // ── All completed tool calls have a green dot ─────────────────────────────
    // span.bg-emerald-400 appears on each ToolCallCard that received a result.
    const greenDots = page.locator('span.bg-emerald-400')
    const dotCount = await greenDots.count()
    expect(dotCount).toBeGreaterThan(0)
    expect(dotCount).toBeLessThanOrEqual(toolCount)

    // ── Final assistant response text is visible ──────────────────────────────
    const assistantContent = page.locator('p, div').filter({ hasText: /hello-world\.txt|operations|success/i })
    await expect(assistantContent.first()).toBeVisible({ timeout: 5_000 })

    // ── Input textarea re-enabled for follow-up ───────────────────────────────
    const inputTextarea = page.getByPlaceholder(
      'Message… (Enter to send, Shift+Enter for new line, drop/paste files)',
    )
    await expect(inputTextarea).toBeVisible()
    await expect(inputTextarea).toBeEnabled()
  })

  test.afterEach(async () => {
    cleanupArtifacts()
  })

  // ── 2. Follow-up message ───────────────────────────────────────────────────

  test('sends a follow-up message in an existing session', async ({ page }) => {
    // Start a short initial chat to open a session
    await openNewChatDialog(page)
    await startChat(page, 'Say "hello" and nothing else.')
    await waitForStreamingToComplete(page)

    // Verify first response arrived
    const inputTextarea = page.getByPlaceholder(
      'Message… (Enter to send, Shift+Enter for new line, drop/paste files)',
    )
    await expect(inputTextarea).toBeEnabled()

    // Send a follow-up
    await inputTextarea.fill('Now say "goodbye" and nothing else.')
    await inputTextarea.press('Enter')

    // Streaming must start and finish for the follow-up
    const stopBtn = page.locator('button[title="Stop generation"]')
    await stopBtn.waitFor({ state: 'visible', timeout: 30_000 })
    await stopBtn.waitFor({ state: 'hidden', timeout: 90_000 })

    // Input re-enabled after follow-up
    await expect(inputTextarea).toBeEnabled()

    // Both user messages visible in the conversation
    // .first() because the text also appears in the auto-generated chat title
    await expect(page.getByText('Say "hello" and nothing else.').first()).toBeVisible()
    await expect(page.getByText('Now say "goodbye" and nothing else.').first()).toBeVisible()
  })

  // ── 3. Stop generation ────────────────────────────────────────────────────

  test('stops generation mid-stream when Stop button is clicked', async ({ page }) => {
    await openNewChatDialog(page)
    // Use a prompt likely to produce a long response so we can stop it in time
    await startChat(page, 'Write a very long detailed essay about software engineering best practices. Include at least 10 sections.')

    // Wait for streaming to start
    const stopBtn = page.locator('button[title="Stop generation"]')
    await stopBtn.waitFor({ state: 'visible', timeout: 30_000 })

    // Click Stop
    await stopBtn.click()

    // Streaming must end: Stop button disappears and input becomes available
    await stopBtn.waitFor({ state: 'hidden', timeout: 15_000 })

    const inputTextarea = page.getByPlaceholder(
      'Message… (Enter to send, Shift+Enter for new line, drop/paste files)',
    )
    await expect(inputTextarea).toBeEnabled({ timeout: 10_000 })

    // No error banner visible after stopping
    await expect(page.locator('.text-red-600, .text-red-700').first()).not.toBeVisible()
  })

  // ── 4. Completed chat appears in the chat list ────────────────────────────

  test('completed chat appears in the chats list with a title', async ({ page }) => {
    // Create and complete a short chat
    await openNewChatDialog(page)
    await startChat(page, 'Say only the word "pineapple".')
    await waitForStreamingToComplete(page)

    // Navigate back to the chats list
    await page.getByRole('link', { name: 'Chats' }).first().click()
    await expect(page).toHaveURL(/\/chats$/)

    // The new chat should appear in the list (it may show "New Chat" as title
    // until Claude sets it — just assert at least one item exists)
    const chatList = page.locator('[class*="cursor-pointer"]').filter({ hasText: /pineapple|New Chat/i })
    await expect(chatList.first()).toBeVisible({ timeout: 10_000 })
  })

  // ── 5. Dialog UX ──────────────────────────────────────────────────────────

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
