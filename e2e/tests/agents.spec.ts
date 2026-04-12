import { test, expect, type Page, request as playwrightRequest } from '@playwright/test'

/**
 * E2E tests for the Agent feature.
 *
 * Covers:
 *   - Create an agent via the UI form
 *   - Agent appears in the agents list with correct name/slug
 *   - Start a chat with the agent via the New Chat dialog
 *   - Chat header shows the agent label
 *   - Streaming response renders
 *   - Model field locks when an agent with a model is selected
 *   - Delete an agent via the UI
 */

const BASE_URL = 'http://localhost:8990'

// Agent used for chat tests — created via API in beforeAll so chat tests
// do not depend on the UI creation test succeeding first.
const CHAT_AGENT = {
  name: 'E2E Chat Agent',
  slug: 'e2e-chat-agent',
  systemPrompt: 'You are a simple test agent. Always respond concisely in one or two sentences.',
}

// Agent used only for the UI-creation test — different slug to avoid conflicts.
const UI_AGENT = {
  name: 'E2E UI Agent',
  slug: 'e2e-ui-agent',
  description: 'Created by the UI creation test',
  systemPrompt: 'You are a UI-created test agent.',
}

// Agent used for the model-lock test — pre-created via API in beforeAll.
const LOCKED_AGENT = {
  name: 'E2E Locked Model Agent',
  slug: 'e2e-locked-model-agent',
  model: 'sonnet',
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

async function completeOnboardingViaApi() {
  const ctx = await playwrightRequest.newContext({ baseURL: BASE_URL })
  await ctx.put('/api/settings', {
    data: { default_working_dir: '/tmp', default_model: 'sonnet', onboarding_complete: true },
  })
  await ctx.dispose()
}

async function createAgentViaApi(slug: string, name: string, systemPrompt: string, model = '') {
  const ctx = await playwrightRequest.newContext({ baseURL: BASE_URL })
  const res = await ctx.post('/api/agents', {
    data: { name, slug, model, system_prompt: systemPrompt, capabilities: { built_in: [] } },
  })
  await ctx.dispose()
  return res
}

async function deleteAgentViaApi(slug: string) {
  const ctx = await playwrightRequest.newContext({ baseURL: BASE_URL })
  await ctx.delete(`/api/agents/${slug}`).catch(() => {})
  await ctx.dispose()
}

async function loadApp(page: Page) {
  await page.goto('/')
  await expect(page.getByRole('button', { name: 'New Chat' }).first()).toBeVisible({
    timeout: 10_000,
  })
}

async function waitForStreamingToComplete(page: Page, timeoutMs = 90_000) {
  const stopBtn = page.locator('button[title="Stop generation"]')
  await stopBtn.waitFor({ state: 'visible', timeout: 30_000 })
  await stopBtn.waitFor({ state: 'hidden', timeout: timeoutMs })
}

// ---------------------------------------------------------------------------
// Suite
// ---------------------------------------------------------------------------

test.describe('Agents', () => {
  test.beforeAll(async () => {
    await completeOnboardingViaApi()
    // Ensure no stale agents from previous runs
    await deleteAgentViaApi(CHAT_AGENT.slug)
    await deleteAgentViaApi(UI_AGENT.slug)
    await deleteAgentViaApi(LOCKED_AGENT.slug)
    // Pre-create agents used for tests that should be independent of UI creation
    await createAgentViaApi(CHAT_AGENT.slug, CHAT_AGENT.name, CHAT_AGENT.systemPrompt)
    await createAgentViaApi(LOCKED_AGENT.slug, LOCKED_AGENT.name, 'Test.', LOCKED_AGENT.model)
  })

  test.afterAll(async () => {
    await deleteAgentViaApi(CHAT_AGENT.slug)
    await deleteAgentViaApi(UI_AGENT.slug)
    await deleteAgentViaApi(LOCKED_AGENT.slug)
  })

  test.beforeEach(async ({ page }) => {
    await loadApp(page)
  })

  // ── 1. Create agent via UI form ───────────────────────────────────────────

  test('creates an agent via the form and shows it in the agents list', async ({ page }) => {
    await page.getByRole('link', { name: 'Agents' }).click()
    await expect(page).toHaveURL(/\/agents$/)
    // Use exact:true — "No agents yet" also contains the word "agents"
    await expect(page.getByRole('heading', { name: 'Agents', exact: true })).toBeVisible()

    await page.getByRole('button', { name: 'New Agent' }).click()
    await expect(page).toHaveURL(/\/agents\/new$/)

    // Fill Basic Info (section is expanded by default)
    await page.getByLabel('Name *').fill(UI_AGENT.name)
    // Slug auto-derives from name
    await expect(page.getByLabel('Slug *')).toHaveValue(UI_AGENT.slug, { timeout: 3_000 })
    await page.getByLabel('Description').fill(UI_AGENT.description)

    // System Prompt (left column on desktop) — form renders dual layout (desktop + mobile),
    // both share the same id; use .first() to target the visible desktop one
    await page.locator('textarea#system_prompt').first().fill(UI_AGENT.systemPrompt)

    await page.getByRole('button', { name: 'Create Agent' }).click()
    await page.waitForURL(/\/agents$/, { timeout: 10_000 })

    // Agent card visible with correct name and slug
    await expect(page.getByRole('heading', { name: UI_AGENT.name })).toBeVisible({ timeout: 5_000 })
    await expect(page.getByText(UI_AGENT.slug)).toBeVisible()
  })

  // ── 2. Chat with an agent ─────────────────────────────────────────────────

  test('starts a chat with an agent and receives a streaming response', async ({ page }) => {
    // Open New Chat dialog
    await page.getByRole('button', { name: 'New Chat' }).first().click()
    await expect(page.getByRole('heading', { name: 'New Chat' })).toBeVisible({ timeout: 5_000 })

    // The agent selector defaults to "No agent (direct chat)".
    // Click it and pick our pre-created chat agent.
    // Scope to the dialog to avoid matching background page comboboxes (e.g. Directories filter).
    const dialog = page.getByRole('dialog')
    const agentCombobox = dialog.locator('[role="combobox"]').first()
    await agentCombobox.click()
    // Options render in a Radix portal — search the whole page
    await page.locator('[role="option"]').filter({ hasText: CHAT_AGENT.name }).click()

    // Trigger now shows the selected agent name
    await expect(agentCombobox).toContainText(CHAT_AGENT.name)

    // Type and submit the first message
    const firstMsg = 'Say hello.'
    await page.getByPlaceholder('Type your first message… (Enter to send)').fill(firstMsg)
    await page.getByRole('button', { name: 'Start Chat' }).click()

    await page.waitForURL(/\/chats\/[^/]+$/, { timeout: 15_000 })

    // ── Chat session ────────────────────────────────────────────────────────
    // Header shows the agent slug (replaces "Direct chat" label)
    await expect(page.getByText(CHAT_AGENT.slug).first()).toBeVisible({ timeout: 5_000 })

    // User message visible
    await expect(page.getByText(firstMsg).first()).toBeVisible({ timeout: 10_000 })

    // Streaming completes successfully
    await waitForStreamingToComplete(page)

    // Input re-enabled for follow-up
    await expect(
      page.getByPlaceholder('Message… (Enter to send, Shift+Enter for new line, drop/paste files)'),
    ).toBeEnabled()
  })

  // ── 3. Model locks when agent has a model ─────────────────────────────────

  test('model field is locked in New Chat dialog when agent has a fixed model', async ({ page }) => {
    // LOCKED_AGENT is pre-created in beforeAll — no API call or reload needed here
    await page.getByRole('button', { name: 'New Chat' }).first().click()
    await expect(page.getByRole('heading', { name: 'New Chat' })).toBeVisible({ timeout: 5_000 })

    // Scope to the dialog to avoid matching background page comboboxes (e.g. Directories filter).
    const dialog = page.getByRole('dialog')
    const agentTrigger = dialog.locator('[role="combobox"]').first()
    await agentTrigger.click()
    await page.locator('[role="option"]').filter({ hasText: LOCKED_AGENT.name }).waitFor({ state: 'visible', timeout: 5_000 })
    await page.locator('[role="option"]').filter({ hasText: LOCKED_AGENT.name }).click()

    // Model input becomes a disabled text field, not a select
    await expect(page.locator('input[disabled]')).toBeVisible({ timeout: 3_000 })
    await expect(page.getByText('Model set by agent configuration')).toBeVisible()
  })

  // ── 4. Delete agent via the UI ────────────────────────────────────────────

  test('deletes an agent via the agents list', async ({ page }) => {
    // Create a throwaway agent to delete
    const slug = 'e2e-delete-me'
    await createAgentViaApi(slug, 'E2E Delete Me', 'Temporary.')

    await page.goto('/agents')
    await expect(page.getByRole('heading', { name: 'E2E Delete Me' })).toBeVisible({ timeout: 5_000 })

    // Click Delete on that card — scope to the card element to avoid matching nested divs
    const card = page.locator('div.rounded-lg').filter({ hasText: 'E2E Delete Me' }).last()
    await card.getByRole('button', { name: 'Delete' }).first().click()

    // Confirm in the alert dialog
    await expect(page.getByRole('alertdialog')).toBeVisible()
    await page.getByRole('alertdialog').getByRole('button', { name: 'Delete' }).click()

    // Card disappears
    await expect(page.getByRole('heading', { name: 'E2E Delete Me' })).not.toBeVisible({
      timeout: 5_000,
    })
  })
})
