import { useState, useEffect } from 'react'
import { BrowserRouter, Routes, Route, Navigate } from 'react-router-dom'
import Layout from '@/components/Layout'
import AgentsPage from '@/pages/AgentsPage'
import AgentCreatePage from '@/pages/AgentCreatePage'
import AgentEditPage from '@/pages/AgentEditPage'
import ChatsPage from '@/pages/ChatsPage'
import ChatSessionPage from '@/pages/ChatSessionPage'
import SettingsPage from '@/pages/SettingsPage'
import ClaudeSessionsPage from '@/pages/ClaudeSessionsPage'
import ClaudeSessionDetailPage from '@/pages/ClaudeSessionDetailPage'
import TokenUsagePage from '@/pages/TokenUsagePage'
import GeneralUsagePage from '@/pages/GeneralUsagePage'
import IntegrationsPage from '@/pages/IntegrationsPage'
import IntegrationGooglePage from '@/pages/IntegrationGooglePage'
import IntegrationDetailPage from '@/pages/IntegrationDetailPage'
import TasksPage from '@/pages/TasksPage'
import TaskCreatePage from '@/pages/TaskCreatePage'
import TaskEditPage from '@/pages/TaskEditPage'
import JobHistoriesPage from '@/pages/JobHistoriesPage'
import OnboardingWizard from '@/components/OnboardingWizard'
import { AppearanceProvider } from '@/contexts/ThemeContext'
import { settingsApi } from '@/lib/api'
import type { SettingsResponse } from '@/types'
import type { AppearanceSettings } from '@/contexts/ThemeContext'

export default function App() {
  const [settingsResp, setSettingsResp] = useState<SettingsResponse | null>(null)
  const [onboardingDone, setOnboardingDone] = useState(true)
  const [serverAppearance, setServerAppearance] = useState<Partial<AppearanceSettings>>({})

  useEffect(() => {
    settingsApi
      .get()
      .then(resp => {
        setSettingsResp(resp)
        setOnboardingDone(resp.settings.onboarding_complete)
        const s = resp.settings
        const ap: Partial<AppearanceSettings> = {}
        if (typeof s.appearance_dark_mode === 'boolean') ap.darkMode = s.appearance_dark_mode
        if (s.appearance_font_size && s.appearance_font_size >= 12 && s.appearance_font_size <= 24)
          ap.fontSize = s.appearance_font_size
        if (s.appearance_font_family) ap.fontFamily = s.appearance_font_family
        if (Object.keys(ap).length > 0) setServerAppearance(ap)
      })
      .catch(() => {
        // If settings can't be loaded, skip onboarding.
        setOnboardingDone(true)
      })
  }, [])

  const handleOnboardingComplete = () => {
    setOnboardingDone(true)
    settingsApi
      .get()
      .then(setSettingsResp)
      .catch(() => undefined)
  }

  return (
    <AppearanceProvider serverSettings={serverAppearance}>
      {settingsResp && !onboardingDone && (
        <OnboardingWizard
          defaultWorkingDir={settingsResp.settings.default_working_dir}
          defaultModel={settingsResp.settings.default_model}
          modelFromEnv={settingsResp.model_from_env}
          modelEnvVar={
            settingsResp.locked['default_model'] ??
            (settingsResp.model_from_env ? 'ANTHROPIC_DEFAULT_SONNET_MODEL' : undefined)
          }
          onComplete={handleOnboardingComplete}
        />
      )}
      <BrowserRouter>
        <Routes>
          <Route path="/" element={<Layout />}>
            <Route index element={<Navigate to="/chats" replace />} />
            <Route path="chats" element={<ChatsPage />} />
            <Route path="chats/:id" element={<ChatSessionPage />} />
            <Route path="agents" element={<AgentsPage />} />
            <Route path="agents/new" element={<AgentCreatePage />} />
            <Route path="agents/:slug/edit" element={<AgentEditPage />} />
            <Route path="claude-sessions" element={<ClaudeSessionsPage />} />
            <Route path="claude-sessions/:id" element={<ClaudeSessionDetailPage />} />
            <Route path="analytics/token-usage" element={<TokenUsagePage />} />
            <Route path="analytics/general-usage" element={<GeneralUsagePage />} />
            <Route path="integrations" element={<IntegrationsPage />} />
            <Route path="integrations/google" element={<IntegrationGooglePage />} />
            <Route path="integrations/:id" element={<IntegrationDetailPage />} />
            <Route path="tasks" element={<TasksPage />} />
            <Route path="tasks/new" element={<TaskCreatePage />} />
            <Route path="tasks/:id/edit" element={<TaskEditPage />} />
            <Route path="job-history" element={<JobHistoriesPage />} />
            <Route path="settings" element={<SettingsPage />} />
          </Route>
        </Routes>
      </BrowserRouter>
    </AppearanceProvider>
  )
}
