import { useState, useEffect } from 'react'
import { BrowserRouter, Routes, Route, Navigate } from 'react-router-dom'
import Layout from '@/components/Layout'
import AgentsPage from '@/pages/AgentsPage'
import AgentCreatePage from '@/pages/AgentCreatePage'
import AgentEditPage from '@/pages/AgentEditPage'
import ChatsPage from '@/pages/ChatsPage'
import ChatSessionPage from '@/pages/ChatSessionPage'
import SettingsPage from '@/pages/SettingsPage'
import OnboardingWizard from '@/components/OnboardingWizard'
import { settingsApi } from '@/lib/api'
import type { SettingsResponse } from '@/types'

export default function App() {
  const [settingsResp, setSettingsResp] = useState<SettingsResponse | null>(null)
  const [onboardingDone, setOnboardingDone] = useState(true)

  useEffect(() => {
    settingsApi
      .get()
      .then(resp => {
        setSettingsResp(resp)
        setOnboardingDone(resp.settings.onboarding_complete)
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
    <>
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
            <Route path="settings" element={<SettingsPage />} />
          </Route>
        </Routes>
      </BrowserRouter>
    </>
  )
}
