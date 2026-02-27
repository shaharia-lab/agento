import { useState } from 'react'
import { useNavigate } from 'react-router-dom'
import { ArrowLeft, ArrowRight, CheckCircle, Loader2, ExternalLink } from 'lucide-react'
import { integrationsApi } from '@/lib/api'
import type { ServiceConfig } from '@/types'

type Step = 1 | 2 | 3 | 4 | 5

const GOOGLE_TOOLS: Record<string, string[]> = {
  calendar: ['create_event', 'view_events'],
  gmail: ['send_email', 'read_email', 'search_email'],
  drive: ['list_files', 'create_file', 'download_file'],
}

export default function IntegrationNewPage() {
  const navigate = useNavigate()
  const [step, setStep] = useState<Step>(1)
  const [integrationId, setIntegrationId] = useState<string | null>(null)

  // Step 2 form state
  const [name, setName] = useState('')
  const [clientId, setClientId] = useState('')
  const [clientSecret, setClientSecret] = useState('')

  // Step 3 service config
  const [services, setServices] = useState<Record<string, ServiceConfig>>({
    calendar: { enabled: false, tools: [] },
    gmail: { enabled: false, tools: [] },
    drive: { enabled: false, tools: [] },
  })

  // Step 4 OAuth state
  const [authUrl, setAuthUrl] = useState<string | null>(null)
  const [polling, setPolling] = useState(false)
  const [authError, setAuthError] = useState<string | null>(null)

  const [saving, setSaving] = useState(false)
  const [error, setError] = useState<string | null>(null)

  const handleServiceToggle = (svcName: string) => {
    setServices(prev => {
      const svc = prev[svcName]
      const enabled = !svc.enabled
      return {
        ...prev,
        [svcName]: {
          enabled,
          tools: enabled ? [...GOOGLE_TOOLS[svcName]] : [],
        },
      }
    })
  }

  const handleToolToggle = (svcName: string, tool: string) => {
    setServices(prev => {
      const svc = prev[svcName]
      const tools = svc.tools.includes(tool)
        ? svc.tools.filter(t => t !== tool)
        : [...svc.tools, tool]
      return { ...prev, [svcName]: { ...svc, tools } }
    })
  }

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
      setStep(4)
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
          setStep(5)
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
      <div className="flex items-center gap-3 border-b border-zinc-100 px-4 sm:px-6 py-4 shrink-0">
        <button
          onClick={() => navigate('/integrations')}
          className="flex items-center gap-1.5 text-sm text-zinc-500 hover:text-zinc-700 transition-colors"
        >
          <ArrowLeft className="h-4 w-4" />
          Integrations
        </button>
        <span className="text-zinc-300">/</span>
        <h1 className="text-base font-semibold text-zinc-900">New Integration</h1>
      </div>

      {/* Scrollable content */}
      <div className="flex-1 overflow-y-auto">
        <div className="max-w-xl mx-auto px-4 sm:px-6 py-6">
          {/* Step indicator */}
          <div className="flex items-center gap-2 mb-6">
            {([1, 2, 3, 4, 5] as Step[]).map(s => (
              <div
                key={s}
                className={`h-1.5 flex-1 rounded-full transition-colors ${
                  step >= s ? 'bg-zinc-900' : 'bg-zinc-200'
                }`}
              />
            ))}
          </div>

          {error && (
            <div className="rounded-md border border-red-200 bg-red-50 px-4 py-3 text-sm text-red-700 mb-4">
              {error}
            </div>
          )}

          {/* Step 1: Choose provider */}
          {step === 1 && (
            <div>
              <h2 className="text-base font-medium text-zinc-900 mb-4">Choose a provider</h2>
              <button
                onClick={() => setStep(2)}
                className="w-full text-left rounded-lg border-2 border-blue-500 bg-blue-50 p-4 hover:border-blue-600 transition-colors"
              >
                <div className="flex items-center gap-3">
                  <div className="h-10 w-10 rounded-lg bg-white border border-zinc-200 flex items-center justify-center text-lg font-bold text-blue-600">
                    G
                  </div>
                  <div>
                    <p className="font-medium text-zinc-900">Google</p>
                    <p className="text-sm text-zinc-500">Calendar, Gmail, Drive</p>
                  </div>
                </div>
              </button>
            </div>
          )}

          {/* Step 2: Credentials */}
          {step === 2 && (
            <div>
              <h2 className="text-base font-medium text-zinc-900 mb-1">Google OAuth credentials</h2>
              <p className="text-sm text-zinc-500 mb-4">
                Create OAuth 2.0 credentials in the{' '}
                <a
                  href="https://console.cloud.google.com/apis/credentials"
                  target="_blank"
                  rel="noopener noreferrer"
                  className="text-blue-500 hover:underline inline-flex items-center gap-0.5"
                >
                  Google Cloud Console
                  <ExternalLink className="h-3 w-3" />
                </a>
                . Set the redirect URI to{' '}
                <code className="text-xs bg-zinc-100 px-1 py-0.5 rounded">
                  http://localhost:PORT/callback
                </code>
                .
              </p>

              <div className="space-y-4">
                <div>
                  <label className="block text-sm font-medium text-zinc-700 mb-1">
                    Integration name
                  </label>
                  <input
                    type="text"
                    value={name}
                    onChange={e => setName(e.target.value)}
                    placeholder="My Google integration"
                    className="w-full rounded-md border border-zinc-300 bg-white px-3 py-2 text-sm text-zinc-900 placeholder-zinc-400 focus:outline-none focus:ring-2 focus:ring-zinc-900"
                  />
                </div>
                <div>
                  <label className="block text-sm font-medium text-zinc-700 mb-1">Client ID</label>
                  <input
                    type="text"
                    value={clientId}
                    onChange={e => setClientId(e.target.value)}
                    placeholder="123456789-abc.apps.googleusercontent.com"
                    className="w-full rounded-md border border-zinc-300 bg-white px-3 py-2 text-sm text-zinc-900 placeholder-zinc-400 focus:outline-none focus:ring-2 focus:ring-zinc-900"
                  />
                </div>
                <div>
                  <label className="block text-sm font-medium text-zinc-700 mb-1">
                    Client secret
                  </label>
                  <input
                    type="password"
                    value={clientSecret}
                    onChange={e => setClientSecret(e.target.value)}
                    placeholder="GOCSPX-…"
                    className="w-full rounded-md border border-zinc-300 bg-white px-3 py-2 text-sm text-zinc-900 placeholder-zinc-400 focus:outline-none focus:ring-2 focus:ring-zinc-900"
                  />
                </div>
              </div>

              <div className="flex justify-between mt-6">
                <button
                  onClick={() => setStep(1)}
                  className="flex items-center gap-1.5 text-sm text-zinc-500 hover:text-zinc-700 transition-colors"
                >
                  <ArrowLeft className="h-4 w-4" />
                  Back
                </button>
                <button
                  onClick={() => setStep(3)}
                  disabled={!name || !clientId || !clientSecret}
                  className="flex items-center gap-1.5 rounded-md bg-zinc-900 text-white px-4 py-2 text-sm hover:bg-zinc-700 disabled:opacity-40 disabled:cursor-not-allowed transition-colors"
                >
                  Next
                  <ArrowRight className="h-4 w-4" />
                </button>
              </div>
            </div>
          )}

          {/* Step 3: Services */}
          {step === 3 && (
            <div>
              <h2 className="text-base font-medium text-zinc-900 mb-1">Enable services & tools</h2>
              <p className="text-sm text-zinc-500 mb-4">
                Choose which Google services to enable and which tools agents can access.
              </p>

              <div className="space-y-4">
                {Object.entries(GOOGLE_TOOLS).map(([svcName, toolNames]) => {
                  const svc = services[svcName]
                  return (
                    <div key={svcName} className="rounded-lg border border-zinc-200 p-4">
                      <div className="flex items-center gap-3 mb-3">
                        <input
                          type="checkbox"
                          id={`svc-${svcName}`}
                          checked={svc.enabled}
                          onChange={() => handleServiceToggle(svcName)}
                          className="h-4 w-4 rounded border-zinc-300"
                        />
                        <label
                          htmlFor={`svc-${svcName}`}
                          className="text-sm font-medium text-zinc-900 capitalize cursor-pointer"
                        >
                          Google {svcName.charAt(0).toUpperCase() + svcName.slice(1)}
                        </label>
                      </div>

                      {svc.enabled && (
                        <div className="ml-7 space-y-2">
                          {toolNames.map(tool => (
                            <div key={tool} className="flex items-center gap-2">
                              <input
                                type="checkbox"
                                id={`tool-${svcName}-${tool}`}
                                checked={svc.tools.includes(tool)}
                                onChange={() => handleToolToggle(svcName, tool)}
                                className="h-3.5 w-3.5 rounded border-zinc-300"
                              />
                              <label
                                htmlFor={`tool-${svcName}-${tool}`}
                                className="text-sm text-zinc-600 cursor-pointer font-mono"
                              >
                                {tool}
                              </label>
                            </div>
                          ))}
                        </div>
                      )}
                    </div>
                  )
                })}
              </div>

              <div className="flex justify-between mt-6">
                <button
                  onClick={() => setStep(2)}
                  className="flex items-center gap-1.5 text-sm text-zinc-500 hover:text-zinc-700 transition-colors"
                >
                  <ArrowLeft className="h-4 w-4" />
                  Back
                </button>
                <button
                  onClick={handleSaveAndAuth}
                  disabled={saving || !Object.values(services).some(s => s.enabled)}
                  className="flex items-center gap-1.5 rounded-md bg-zinc-900 text-white px-4 py-2 text-sm hover:bg-zinc-700 disabled:opacity-40 disabled:cursor-not-allowed transition-colors"
                >
                  {saving ? <Loader2 className="h-4 w-4 animate-spin" /> : null}
                  Save & Authenticate
                </button>
              </div>
            </div>
          )}

          {/* Step 4: OAuth flow */}
          {step === 4 && (
            <div className="text-center py-8">
              <h2 className="text-base font-medium text-zinc-900 mb-2">Authenticate with Google</h2>
              <p className="text-sm text-zinc-500 mb-6">
                A browser tab should have opened for Google sign-in. If not, click the button below.
              </p>

              {authUrl && (
                <a
                  href={authUrl}
                  target="_blank"
                  rel="noopener noreferrer"
                  className="inline-flex items-center gap-2 rounded-md border border-zinc-300 px-4 py-2 text-sm text-zinc-700 hover:bg-zinc-100 transition-colors mb-6"
                  onClick={() => window.open(authUrl, '_blank')}
                >
                  <ExternalLink className="h-4 w-4" />
                  Open Google sign-in
                </a>
              )}

              {polling && (
                <div className="flex items-center justify-center gap-2 text-sm text-zinc-500">
                  <Loader2 className="h-4 w-4 animate-spin" />
                  Waiting for Google authentication…
                </div>
              )}

              {authError && (
                <div className="rounded-md border border-red-200 bg-red-50 px-4 py-3 text-sm text-red-700 mt-4">
                  Authentication failed: {authError}
                </div>
              )}
            </div>
          )}

          {/* Step 5: Success */}
          {step === 5 && (
            <div className="text-center py-8">
              <CheckCircle className="h-12 w-12 text-green-500 mx-auto mb-4" />
              <h2 className="text-base font-medium text-zinc-900 mb-2">Integration connected!</h2>
              <p className="text-sm text-zinc-500 mb-6">
                Your Google integration is ready. You can now assign its tools to agents.
              </p>
              <div className="flex justify-center gap-3">
                <button
                  onClick={() => navigate('/integrations')}
                  className="rounded-md border border-zinc-300 px-4 py-2 text-sm text-zinc-700 hover:bg-zinc-100 transition-colors"
                >
                  Back to Integrations
                </button>
                {integrationId && (
                  <button
                    onClick={() => navigate(`/integrations/${integrationId}`)}
                    className="rounded-md bg-zinc-900 text-white px-4 py-2 text-sm hover:bg-zinc-700 transition-colors"
                  >
                    View Details
                  </button>
                )}
              </div>
            </div>
          )}
        </div>
      </div>
    </div>
  )
}
