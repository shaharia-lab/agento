import { useEffect, useRef, useState } from 'react'
import { useNavigate } from 'react-router-dom'
import {
  ArrowLeft,
  ArrowRight,
  CheckCircle,
  Loader2,
  ExternalLink,
  AlertCircle,
} from 'lucide-react'
import { integrationsApi } from '@/lib/api'
import type { ServiceConfig } from '@/types'
import SlackIntegrationEditor from '@/components/integrations/SlackIntegrationEditor'

type AuthMode = 'bot_token' | 'oauth'
type Step = 1 | 2 | 3 | 4

export default function IntegrationSlackPage() {
  const navigate = useNavigate()
  const [step, setStep] = useState<Step>(1)
  const [integrationId, setIntegrationId] = useState<string | null>(null)

  // Step 1 form state
  const [name, setName] = useState('')
  const [authMode, setAuthMode] = useState<AuthMode>('bot_token')
  const [botToken, setBotToken] = useState('')
  const [clientId, setClientId] = useState('')
  const [clientSecret, setClientSecret] = useState('')

  // Step 2 service config
  const [services, setServices] = useState<Record<string, ServiceConfig>>({
    messaging: { enabled: false, tools: [] },
  })

  // Step 3 validation / OAuth state
  const [validating, setValidating] = useState(false)
  const [validated, setValidated] = useState(false)
  const [validationError, setValidationError] = useState<string | null>(null)
  const [oauthUrl, setOauthUrl] = useState<string | null>(null)
  const [pollingAuth, setPollingAuth] = useState(false)

  const [saving, setSaving] = useState(false)
  const [error, setError] = useState<string | null>(null)

  // Abort controller for the OAuth polling loop — cleaned up on unmount.
  const pollAbortRef = useRef<AbortController | null>(null)
  useEffect(() => {
    return () => {
      pollAbortRef.current?.abort()
    }
  }, [])

  const isStep1Valid = () => {
    if (!name) return false
    if (authMode === 'bot_token') return !!botToken
    return !!clientId && !!clientSecret
  }

  const buildCredentials = () => {
    if (authMode === 'bot_token') {
      return { auth_mode: 'bot_token', bot_token: botToken }
    }
    return { auth_mode: 'oauth', client_id: clientId, client_secret: clientSecret }
  }

  const handleSaveAndValidate = async () => {
    setSaving(true)
    setError(null)
    setValidationError(null)

    try {
      // Create the integration first (not yet authenticated).
      const created = await integrationsApi.create({
        name,
        type: 'slack',
        enabled: true,
        credentials: buildCredentials(),
        services,
      })
      setIntegrationId(created.id)
      setStep(3)

      if (authMode === 'bot_token') {
        // Validate the bot token.
        setValidating(true)
        const result = await integrationsApi.validateAuth(created.id)
        if (result.valid) {
          setValidated(true)
          setStep(4)
        } else {
          setValidationError('Bot token validation failed. Please check your token and try again.')
        }
      } else {
        // Start OAuth flow.
        setValidating(true)
        const result = await integrationsApi.startOAuth(created.id)
        setOauthUrl(result.auth_url)
        setValidating(false)
        window.open(result.auth_url, '_blank')
        // Poll for auth status.
        pollAuthStatus(created.id)
      }
    } catch (err) {
      const msg = (err as Error).message
      if (step === 3 || integrationId) {
        setValidationError(msg)
      } else {
        setError(msg)
      }
    } finally {
      setSaving(false)
      setValidating(false)
    }
  }

  const pollAuthStatus = async (id: string) => {
    const controller = new AbortController()
    pollAbortRef.current = controller

    setPollingAuth(true)
    const maxAttempts = 120 // 10 minutes at 5s intervals
    for (let i = 0; i < maxAttempts; i++) {
      if (controller.signal.aborted) return
      try {
        const status = await integrationsApi.getAuthStatus(id)
        if (status.authenticated) {
          setValidated(true)
          setPollingAuth(false)
          setStep(4)
          return
        }
      } catch {
        // Ignore polling errors, keep trying.
      }
      await new Promise(r => setTimeout(r, 5000))
    }
    setPollingAuth(false)
    setValidationError('OAuth flow timed out. Please try again.')
  }

  const handleRetryValidation = async () => {
    if (!integrationId) return
    setValidating(true)
    setValidationError(null)
    try {
      if (authMode === 'bot_token') {
        const result = await integrationsApi.validateAuth(integrationId)
        if (result.valid) {
          setValidated(true)
          setStep(4)
        } else {
          setValidationError('Bot token validation failed. Please check your token and try again.')
        }
      } else {
        const result = await integrationsApi.startOAuth(integrationId)
        setOauthUrl(result.auth_url)
        window.open(result.auth_url, '_blank')
        setValidating(false)
        pollAuthStatus(integrationId)
      }
    } catch (err) {
      setValidationError((err as Error).message)
    } finally {
      setValidating(false)
    }
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
        <h1 className="text-xl font-semibold text-zinc-900 dark:text-zinc-100">
          Slack Integration
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

        {/* Step 1: Auth mode + credentials */}
        {step === 1 && (
          <div>
            <h2 className="text-base font-medium text-zinc-900 dark:text-zinc-100 mb-1">
              Slack Credentials
            </h2>
            <p className="text-sm text-zinc-500 dark:text-zinc-400 mb-4">
              {'Create a Slack app at '}
              <a
                href="https://api.slack.com/apps"
                target="_blank"
                rel="noopener noreferrer"
                className="text-blue-500 dark:text-blue-400 hover:underline inline-flex items-center gap-0.5"
              >
                api.slack.com/apps
                <ExternalLink className="h-3 w-3" />
              </a>
              {' and configure your credentials below.'}
            </p>

            <div className="space-y-4 max-w-lg">
              <div>
                <label
                  htmlFor="slack-integration-name"
                  className="block text-sm font-medium text-zinc-700 dark:text-zinc-300 mb-1"
                >
                  Integration name
                </label>
                <input
                  id="slack-integration-name"
                  type="text"
                  value={name}
                  onChange={e => setName(e.target.value)}
                  placeholder="My Slack Workspace"
                  className="w-full rounded-md border border-zinc-300 dark:border-zinc-600 bg-white dark:bg-zinc-800 px-3 py-2 text-sm text-zinc-900 dark:text-zinc-100 placeholder-zinc-400 dark:placeholder-zinc-500 focus:outline-none focus:ring-2 focus:ring-zinc-900 dark:focus:ring-zinc-400"
                />
              </div>

              <fieldset className="border-0 p-0 m-0 min-w-0">
                <legend className="block text-sm font-medium text-zinc-700 dark:text-zinc-300 mb-2">
                  Authentication method
                </legend>
                <div className="flex gap-3">
                  <button
                    onClick={() => setAuthMode('bot_token')}
                    className={`flex-1 rounded-md border px-3 py-2.5 text-sm text-left transition-colors cursor-pointer ${
                      authMode === 'bot_token'
                        ? 'border-zinc-900 dark:border-zinc-100 bg-zinc-50 dark:bg-zinc-700'
                        : 'border-zinc-200 dark:border-zinc-700 hover:border-zinc-300 dark:hover:border-zinc-600'
                    }`}
                  >
                    <p className="font-medium text-zinc-900 dark:text-zinc-100">Bot Token</p>
                    <p className="text-xs text-zinc-500 dark:text-zinc-400 mt-0.5">
                      Use a bot token directly (xoxb-...)
                    </p>
                  </button>
                  <button
                    onClick={() => setAuthMode('oauth')}
                    className={`flex-1 rounded-md border px-3 py-2.5 text-sm text-left transition-colors cursor-pointer ${
                      authMode === 'oauth'
                        ? 'border-zinc-900 dark:border-zinc-100 bg-zinc-50 dark:bg-zinc-700'
                        : 'border-zinc-200 dark:border-zinc-700 hover:border-zinc-300 dark:hover:border-zinc-600'
                    }`}
                  >
                    <p className="font-medium text-zinc-900 dark:text-zinc-100">OAuth</p>
                    <p className="text-xs text-zinc-500 dark:text-zinc-400 mt-0.5">
                      Authenticate via OAuth 2.0 flow
                    </p>
                  </button>
                </div>
              </fieldset>

              {authMode === 'bot_token' && (
                <div>
                  <label
                    htmlFor="slack-bot-token"
                    className="block text-sm font-medium text-zinc-700 dark:text-zinc-300 mb-1"
                  >
                    Bot Token
                  </label>
                  <input
                    id="slack-bot-token"
                    type="password"
                    value={botToken}
                    onChange={e => setBotToken(e.target.value)}
                    placeholder="xoxb-..."
                    className="w-full rounded-md border border-zinc-300 dark:border-zinc-600 bg-white dark:bg-zinc-800 px-3 py-2 text-sm text-zinc-900 dark:text-zinc-100 placeholder-zinc-400 dark:placeholder-zinc-500 focus:outline-none focus:ring-2 focus:ring-zinc-900 dark:focus:ring-zinc-400 font-mono"
                  />
                  <p className="text-xs text-zinc-400 mt-1">
                    Found under OAuth & Permissions in your Slack app settings.
                  </p>
                </div>
              )}

              {authMode === 'oauth' && (
                <>
                  <div>
                    <label
                      htmlFor="slack-client-id"
                      className="block text-sm font-medium text-zinc-700 dark:text-zinc-300 mb-1"
                    >
                      Client ID
                    </label>
                    <input
                      id="slack-client-id"
                      type="text"
                      value={clientId}
                      onChange={e => setClientId(e.target.value)}
                      placeholder="123456789012.123456789012"
                      className="w-full rounded-md border border-zinc-300 dark:border-zinc-600 bg-white dark:bg-zinc-800 px-3 py-2 text-sm text-zinc-900 dark:text-zinc-100 placeholder-zinc-400 dark:placeholder-zinc-500 focus:outline-none focus:ring-2 focus:ring-zinc-900 dark:focus:ring-zinc-400 font-mono"
                    />
                  </div>
                  <div>
                    <label
                      htmlFor="slack-client-secret"
                      className="block text-sm font-medium text-zinc-700 dark:text-zinc-300 mb-1"
                    >
                      Client Secret
                    </label>
                    <input
                      id="slack-client-secret"
                      type="password"
                      value={clientSecret}
                      onChange={e => setClientSecret(e.target.value)}
                      placeholder="abc123def456..."
                      className="w-full rounded-md border border-zinc-300 dark:border-zinc-600 bg-white dark:bg-zinc-800 px-3 py-2 text-sm text-zinc-900 dark:text-zinc-100 placeholder-zinc-400 dark:placeholder-zinc-500 focus:outline-none focus:ring-2 focus:ring-zinc-900 dark:focus:ring-zinc-400 font-mono"
                    />
                  </div>
                </>
              )}
            </div>

            <div className="flex justify-end mt-6">
              <button
                onClick={() => setStep(2)}
                disabled={!isStep1Valid()}
                className="flex items-center gap-1.5 rounded-md bg-zinc-900 dark:bg-zinc-100 text-white dark:text-zinc-900 px-4 py-2 text-sm hover:bg-zinc-700 dark:hover:bg-zinc-300 disabled:opacity-40 disabled:cursor-not-allowed transition-colors cursor-pointer"
              >
                Next
                <ArrowRight className="h-4 w-4" />
              </button>
            </div>
          </div>
        )}

        {/* Step 2: Services & Tools */}
        {step === 2 && (
          <div>
            <h2 className="text-base font-medium text-zinc-900 dark:text-zinc-100 mb-1">
              Enable services & tools
            </h2>
            <p className="text-sm text-zinc-500 dark:text-zinc-400 mb-4">
              Choose which Slack tools agents can access.
            </p>

            <SlackIntegrationEditor services={services} onServicesChange={setServices} />

            <div className="flex justify-between mt-6">
              <button
                onClick={() => setStep(1)}
                className="flex items-center gap-1.5 text-sm text-zinc-500 dark:text-zinc-400 hover:text-zinc-700 dark:hover:text-zinc-200 transition-colors cursor-pointer"
              >
                <ArrowLeft className="h-4 w-4" />
                Back
              </button>
              <button
                onClick={handleSaveAndValidate}
                disabled={saving || !Object.values(services).some(s => s.enabled)}
                className="flex items-center gap-1.5 rounded-md bg-zinc-900 dark:bg-zinc-100 text-white dark:text-zinc-900 px-4 py-2 text-sm hover:bg-zinc-700 dark:hover:bg-zinc-300 disabled:opacity-40 disabled:cursor-not-allowed transition-colors cursor-pointer"
              >
                {saving ? <Loader2 className="h-4 w-4 animate-spin" /> : null}
                {authMode === 'bot_token' ? 'Save & Validate' : 'Save & Authorize'}
              </button>
            </div>
          </div>
        )}

        {/* Step 3: Validation / OAuth in progress */}
        {step === 3 && (
          <div className="text-center py-8">
            {(validating || pollingAuth) && (
              <>
                <Loader2 className="h-12 w-12 text-zinc-400 mx-auto mb-4 animate-spin" />
                <h2 className="text-base font-medium text-zinc-900 dark:text-zinc-100 mb-2">
                  {authMode === 'bot_token'
                    ? 'Validating bot token...'
                    : 'Waiting for OAuth authorization...'}
                </h2>
                <p className="text-sm text-zinc-500 dark:text-zinc-400">
                  {authMode === 'bot_token'
                    ? 'Connecting to Slack API to verify your bot token.'
                    : 'Complete the authorization in the browser window that opened.'}
                </p>
                {oauthUrl && (
                  <a
                    href={oauthUrl}
                    target="_blank"
                    rel="noopener noreferrer"
                    className="mt-3 inline-flex items-center gap-1 text-sm text-blue-500 dark:text-blue-400 hover:underline"
                  >
                    Open authorization page
                    <ExternalLink className="h-3 w-3" />
                  </a>
                )}
              </>
            )}

            {!validating && !pollingAuth && validationError && (
              <>
                <AlertCircle className="h-12 w-12 text-red-500 mx-auto mb-4" />
                <h2 className="text-base font-medium text-zinc-900 dark:text-zinc-100 mb-2">
                  {authMode === 'bot_token' ? 'Validation failed' : 'Authorization failed'}
                </h2>
                <p className="text-sm text-red-600 dark:text-red-400 mb-6">{validationError}</p>
                <div className="flex justify-center gap-3">
                  <button
                    onClick={() => navigate('/integrations')}
                    className="rounded-md border border-zinc-300 dark:border-zinc-600 px-4 py-2 text-sm text-zinc-700 dark:text-zinc-300 hover:bg-zinc-100 dark:hover:bg-zinc-800 transition-colors cursor-pointer"
                  >
                    Back to Integrations
                  </button>
                  <button
                    onClick={handleRetryValidation}
                    className="rounded-md bg-zinc-900 dark:bg-zinc-100 text-white dark:text-zinc-900 px-4 py-2 text-sm hover:bg-zinc-700 dark:hover:bg-zinc-300 transition-colors cursor-pointer"
                  >
                    {authMode === 'bot_token' ? 'Retry Validation' : 'Retry Authorization'}
                  </button>
                </div>
              </>
            )}
          </div>
        )}

        {/* Step 4: Success */}
        {step === 4 && validated && (
          <div className="text-center py-8">
            <CheckCircle className="h-12 w-12 text-green-500 mx-auto mb-4" />
            <h2 className="text-base font-medium text-zinc-900 dark:text-zinc-100 mb-2">
              Integration connected!
            </h2>
            <p className="text-sm text-zinc-500 dark:text-zinc-400 mb-6">
              Your Slack integration is ready. You can now assign its tools to agents.
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
