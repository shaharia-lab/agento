import { useState } from 'react'
import { useNavigate } from 'react-router-dom'
import { ArrowLeft, ArrowRight, CheckCircle, Loader2, ExternalLink } from 'lucide-react'
import { integrationsApi } from '@/lib/api'
import type { ServiceConfig } from '@/types'
import GoogleIntegrationEditor from '@/components/integrations/GoogleIntegrationEditor'

type Step = 1 | 2 | 3 | 4

export default function IntegrationGooglePage() {
  const navigate = useNavigate()
  const [step, setStep] = useState<Step>(1)
  const [integrationId, setIntegrationId] = useState<string | null>(null)

  // Step 1 form state
  const [name, setName] = useState('')
  const [clientId, setClientId] = useState('')
  const [clientSecret, setClientSecret] = useState('')

  // Step 2 service config
  const [services, setServices] = useState<Record<string, ServiceConfig>>({
    calendar: { enabled: false, tools: [] },
    gmail: { enabled: false, tools: [] },
    drive: { enabled: false, tools: [] },
  })

  // Step 3 OAuth state
  const [authUrl, setAuthUrl] = useState<string | null>(null)
  const [polling, setPolling] = useState(false)
  const [authError, setAuthError] = useState<string | null>(null)

  const [saving, setSaving] = useState(false)
  const [error, setError] = useState<string | null>(null)

  const handleSaveAndAuth = async () => {
    setSaving(true)
    setError(null)
    try {
      const created = await integrationsApi.create({
        name,
        type: 'google',
        enabled: true,
        credentials: { client_id: clientId, client_secret: clientSecret },
        services,
      })
      setIntegrationId(created.id)

      const { auth_url } = await integrationsApi.startOAuth(created.id)
      setAuthUrl(auth_url)
      setStep(3)
      startPolling(created.id)
    } catch (err) {
      setError((err as Error).message)
    } finally {
      setSaving(false)
    }
  }

  const startPolling = (id: string) => {
    setPolling(true)
    const interval = setInterval(async () => {
      try {
        const { authenticated } = await integrationsApi.getAuthStatus(id)
        if (authenticated) {
          clearInterval(interval)
          setPolling(false)
          setStep(4)
        }
      } catch (err) {
        clearInterval(interval)
        setPolling(false)
        setAuthError((err as Error).message)
      }
    }, 2000)
  }

  return (
    <div className="flex flex-col h-full">
      {/* Header */}
      <div className="flex items-center gap-3 border-b border-zinc-100 dark:border-zinc-700/50 px-4 sm:px-6 py-4 shrink-0">
        <button
          onClick={() => navigate('/integrations')}
          className="flex items-center gap-1.5 text-sm text-zinc-500 dark:text-zinc-400 hover:text-zinc-700 dark:hover:text-zinc-200 transition-colors cursor-pointer"
        >
          <ArrowLeft className="h-4 w-4" />
          Integrations
        </button>
        <span className="text-zinc-300 dark:text-zinc-600">/</span>
        <h1 className="text-base font-semibold text-zinc-900 dark:text-zinc-100">
          Google Integration
        </h1>
      </div>

      {/* Scrollable content */}
      <div className="flex-1 overflow-y-auto p-4 sm:p-6">
        {/* Step indicator */}
        <div className="flex items-center gap-2 mb-6">
          {([1, 2, 3, 4] as Step[]).map(s => (
            <div
              key={s}
              className={`h-1.5 flex-1 rounded-full transition-colors ${
                step >= s ? 'bg-zinc-900 dark:bg-zinc-100' : 'bg-zinc-200 dark:bg-zinc-700'
              }`}
            />
          ))}
        </div>

        {error && (
          <div className="rounded-md border border-red-200 dark:border-red-800/50 bg-red-50 dark:bg-red-900/20 px-4 py-3 text-sm text-red-700 dark:text-red-400 mb-4">
            {error}
          </div>
        )}

        {/* Step 1: Credentials */}
        {step === 1 && (
          <div>
            <h2 className="text-base font-medium text-zinc-900 dark:text-zinc-100 mb-1">
              Google OAuth credentials
            </h2>
            <p className="text-sm text-zinc-500 dark:text-zinc-400 mb-4">
              Create OAuth 2.0 credentials in the{' '}
              <a
                href="https://console.cloud.google.com/apis/credentials"
                target="_blank"
                rel="noopener noreferrer"
                className="text-blue-500 dark:text-blue-400 hover:underline inline-flex items-center gap-0.5"
              >
                Google Cloud Console
                <ExternalLink className="h-3 w-3" />
              </a>
              . Set the redirect URI to{' '}
              <code className="text-xs bg-zinc-100 dark:bg-zinc-800 px-1 py-0.5 rounded text-zinc-700 dark:text-zinc-300">
                http://localhost:PORT/callback
              </code>
              .
            </p>

            <div className="space-y-4 max-w-lg">
              <div>
                <label className="block text-sm font-medium text-zinc-700 dark:text-zinc-300 mb-1">
                  Integration name
                </label>
                <input
                  type="text"
                  value={name}
                  onChange={e => setName(e.target.value)}
                  placeholder="My Google integration"
                  className="w-full rounded-md border border-zinc-300 dark:border-zinc-600 bg-white dark:bg-zinc-800 px-3 py-2 text-sm text-zinc-900 dark:text-zinc-100 placeholder-zinc-400 dark:placeholder-zinc-500 focus:outline-none focus:ring-2 focus:ring-zinc-900 dark:focus:ring-zinc-400"
                />
              </div>
              <div>
                <label className="block text-sm font-medium text-zinc-700 dark:text-zinc-300 mb-1">
                  Client ID
                </label>
                <input
                  type="text"
                  value={clientId}
                  onChange={e => setClientId(e.target.value)}
                  placeholder="123456789-abc.apps.googleusercontent.com"
                  className="w-full rounded-md border border-zinc-300 dark:border-zinc-600 bg-white dark:bg-zinc-800 px-3 py-2 text-sm text-zinc-900 dark:text-zinc-100 placeholder-zinc-400 dark:placeholder-zinc-500 focus:outline-none focus:ring-2 focus:ring-zinc-900 dark:focus:ring-zinc-400"
                />
              </div>
              <div>
                <label className="block text-sm font-medium text-zinc-700 dark:text-zinc-300 mb-1">
                  Client secret
                </label>
                <input
                  type="password"
                  value={clientSecret}
                  onChange={e => setClientSecret(e.target.value)}
                  placeholder="GOCSPX-…"
                  className="w-full rounded-md border border-zinc-300 dark:border-zinc-600 bg-white dark:bg-zinc-800 px-3 py-2 text-sm text-zinc-900 dark:text-zinc-100 placeholder-zinc-400 dark:placeholder-zinc-500 focus:outline-none focus:ring-2 focus:ring-zinc-900 dark:focus:ring-zinc-400"
                />
              </div>
            </div>

            <div className="flex justify-end mt-6">
              <button
                onClick={() => setStep(2)}
                disabled={!name || !clientId || !clientSecret}
                className="flex items-center gap-1.5 rounded-md bg-zinc-900 dark:bg-zinc-100 text-white dark:text-zinc-900 px-4 py-2 text-sm hover:bg-zinc-700 dark:hover:bg-zinc-300 disabled:opacity-40 disabled:cursor-not-allowed transition-colors cursor-pointer"
              >
                Next
                <ArrowRight className="h-4 w-4" />
              </button>
            </div>
          </div>
        )}

        {/* Step 2: Services */}
        {step === 2 && (
          <div>
            <h2 className="text-base font-medium text-zinc-900 dark:text-zinc-100 mb-1">
              Enable services & tools
            </h2>
            <p className="text-sm text-zinc-500 dark:text-zinc-400 mb-4">
              Choose which Google services to enable and which tools agents can access.
            </p>

            <GoogleIntegrationEditor services={services} onServicesChange={setServices} />

            <div className="flex justify-between mt-6">
              <button
                onClick={() => setStep(1)}
                className="flex items-center gap-1.5 text-sm text-zinc-500 dark:text-zinc-400 hover:text-zinc-700 dark:hover:text-zinc-200 transition-colors cursor-pointer"
              >
                <ArrowLeft className="h-4 w-4" />
                Back
              </button>
              <button
                onClick={handleSaveAndAuth}
                disabled={saving || !Object.values(services).some(s => s.enabled)}
                className="flex items-center gap-1.5 rounded-md bg-zinc-900 dark:bg-zinc-100 text-white dark:text-zinc-900 px-4 py-2 text-sm hover:bg-zinc-700 dark:hover:bg-zinc-300 disabled:opacity-40 disabled:cursor-not-allowed transition-colors cursor-pointer"
              >
                {saving ? <Loader2 className="h-4 w-4 animate-spin" /> : null}
                Save & Authenticate
              </button>
            </div>
          </div>
        )}

        {/* Step 3: OAuth flow */}
        {step === 3 && (
          <div className="text-center py-8">
            <h2 className="text-base font-medium text-zinc-900 dark:text-zinc-100 mb-2">
              Authenticate with Google
            </h2>
            <p className="text-sm text-zinc-500 dark:text-zinc-400 mb-6">
              A browser tab should have opened for Google sign-in. If not, click the button below.
            </p>

            {authUrl && (
              <a
                href={authUrl}
                target="_blank"
                rel="noopener noreferrer"
                className="inline-flex items-center gap-2 rounded-md border border-zinc-300 dark:border-zinc-600 px-4 py-2 text-sm text-zinc-700 dark:text-zinc-300 hover:bg-zinc-100 dark:hover:bg-zinc-800 transition-colors mb-6"
                onClick={() => window.open(authUrl, '_blank')}
              >
                <ExternalLink className="h-4 w-4" />
                Open Google sign-in
              </a>
            )}

            {polling && (
              <div className="flex items-center justify-center gap-2 text-sm text-zinc-500 dark:text-zinc-400">
                <Loader2 className="h-4 w-4 animate-spin" />
                Waiting for Google authentication…
              </div>
            )}

            {authError && (
              <div className="rounded-md border border-red-200 dark:border-red-800/50 bg-red-50 dark:bg-red-900/20 px-4 py-3 text-sm text-red-700 dark:text-red-400 mt-4">
                Authentication failed: {authError}
              </div>
            )}
          </div>
        )}

        {/* Step 4: Success */}
        {step === 4 && (
          <div className="text-center py-8">
            <CheckCircle className="h-12 w-12 text-green-500 mx-auto mb-4" />
            <h2 className="text-base font-medium text-zinc-900 dark:text-zinc-100 mb-2">
              Integration connected!
            </h2>
            <p className="text-sm text-zinc-500 dark:text-zinc-400 mb-6">
              Your Google integration is ready. You can now assign its tools to agents.
            </p>
            <div className="flex justify-center gap-3">
              <button
                onClick={() => navigate('/integrations')}
                className="rounded-md border border-zinc-300 dark:border-zinc-600 px-4 py-2 text-sm text-zinc-700 dark:text-zinc-300 hover:bg-zinc-100 dark:hover:bg-zinc-800 transition-colors cursor-pointer"
              >
                Back to Integrations
              </button>
              {integrationId && (
                <button
                  onClick={() => navigate(`/integrations/${integrationId}`)}
                  className="rounded-md bg-zinc-900 dark:bg-zinc-100 text-white dark:text-zinc-900 px-4 py-2 text-sm hover:bg-zinc-700 dark:hover:bg-zinc-300 transition-colors cursor-pointer"
                >
                  View Details
                </button>
              )}
            </div>
          </div>
        )}
      </div>
    </div>
  )
}
