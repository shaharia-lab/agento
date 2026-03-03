import { useState } from 'react'
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
import GitHubIntegrationEditor from '@/components/integrations/GitHubIntegrationEditor'

type Step = 1 | 2 | 3

export default function IntegrationGitHubPage() {
  const navigate = useNavigate()
  const [step, setStep] = useState<Step>(1)
  const [integrationId, setIntegrationId] = useState<string | null>(null)

  // Step 1 form state
  const [name, setName] = useState('')
  const [personalAccessToken, setPersonalAccessToken] = useState('')

  // Step 2 service config
  const [services, setServices] = useState<Record<string, ServiceConfig>>({
    repos: { enabled: false, tools: [] },
    issues: { enabled: false, tools: [] },
    pull_requests: { enabled: false, tools: [] },
    actions: { enabled: false, tools: [] },
    releases: { enabled: false, tools: [] },
  })

  // Step 3 validation state
  const [validating, setValidating] = useState(false)
  const [validated, setValidated] = useState(false)
  const [validationError, setValidationError] = useState<string | null>(null)

  const [saving, setSaving] = useState(false)
  const [error, setError] = useState<string | null>(null)

  const handleSaveAndValidate = async () => {
    setSaving(true)
    setError(null)
    setValidationError(null)

    try {
      const payload = {
        name,
        type: 'github' as const,
        enabled: true,
        credentials: {
          auth_mode: 'pat',
          personal_access_token: personalAccessToken,
        },
        services,
      }

      // Create or update the integration depending on whether we already have an id.
      let id = integrationId
      if (id) {
        await integrationsApi.update(id, payload)
      } else {
        const created = await integrationsApi.create(payload)
        id = created.id
        setIntegrationId(id)
      }
      setStep(3)

      // Now validate the credentials.
      setValidating(true)
      const result = await integrationsApi.validateAuth(id)
      if (result.valid) {
        setValidated(true)
      } else {
        setValidationError('Credential validation failed. Please check your personal access token.')
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

  const handleRetryValidation = async () => {
    if (!integrationId) return
    setValidating(true)
    setValidationError(null)
    try {
      const result = await integrationsApi.validateAuth(integrationId)
      if (result.valid) {
        setValidated(true)
      } else {
        setValidationError('Credential validation failed. Please check your personal access token.')
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
          GitHub Integration
        </h1>
      </div>

      {/* Scrollable content */}
      <div className="flex-1 overflow-y-auto p-4 sm:p-6">
        {/* Step indicator */}
        <div className="flex items-center gap-2 mb-6">
          {([1, 2, 3] as Step[]).map(s => (
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
              GitHub Credentials
            </h2>
            <p className="text-sm text-zinc-500 dark:text-zinc-400 mb-4">
              {'Generate a Personal Access Token in your '}
              <a
                href="https://github.com/settings/tokens"
                target="_blank"
                rel="noopener noreferrer"
                className="text-blue-500 dark:text-blue-400 hover:underline inline-flex items-center gap-0.5"
              >
                GitHub developer settings
                <ExternalLink className="h-3 w-3" />
              </a>
              {' and paste it below. Use a fine-grained token with the scopes you need.'}
            </p>

            <div className="space-y-4 max-w-lg">
              <div>
                <label
                  htmlFor="github-integration-name"
                  className="block text-sm font-medium text-zinc-700 dark:text-zinc-300 mb-1"
                >
                  Integration name
                </label>
                <input
                  id="github-integration-name"
                  type="text"
                  value={name}
                  onChange={e => setName(e.target.value)}
                  placeholder="My GitHub"
                  className="w-full rounded-md border border-zinc-300 dark:border-zinc-600 bg-white dark:bg-zinc-800 px-3 py-2 text-sm text-zinc-900 dark:text-zinc-100 placeholder-zinc-400 dark:placeholder-zinc-500 focus:outline-none focus:ring-2 focus:ring-zinc-900 dark:focus:ring-zinc-400"
                />
              </div>
              <div>
                <label
                  htmlFor="github-pat"
                  className="block text-sm font-medium text-zinc-700 dark:text-zinc-300 mb-1"
                >
                  Personal Access Token
                </label>
                <input
                  id="github-pat"
                  type="password"
                  value={personalAccessToken}
                  onChange={e => setPersonalAccessToken(e.target.value)}
                  placeholder="ghp_xxxxxxxxxxxxxxxxxxxx"
                  className="w-full rounded-md border border-zinc-300 dark:border-zinc-600 bg-white dark:bg-zinc-800 px-3 py-2 text-sm text-zinc-900 dark:text-zinc-100 placeholder-zinc-400 dark:placeholder-zinc-500 focus:outline-none focus:ring-2 focus:ring-zinc-900 dark:focus:ring-zinc-400 font-mono"
                />
                <p className="text-xs text-zinc-400 mt-1">
                  Generate at: GitHub Settings &rarr; Developer settings &rarr; Personal access
                  tokens
                </p>
              </div>
            </div>

            <div className="flex justify-end mt-6">
              <button
                onClick={() => setStep(2)}
                disabled={!name || !personalAccessToken}
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
              Choose which GitHub tools agents can access.
            </p>

            <GitHubIntegrationEditor services={services} onServicesChange={setServices} />

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
                Save & Validate
              </button>
            </div>
          </div>
        )}

        {/* Step 3: Validation result */}
        {step === 3 && (
          <div className="text-center py-8">
            {validating && (
              <>
                <Loader2 className="h-12 w-12 text-zinc-400 mx-auto mb-4 animate-spin" />
                <h2 className="text-base font-medium text-zinc-900 dark:text-zinc-100 mb-2">
                  Validating credentials...
                </h2>
                <p className="text-sm text-zinc-500 dark:text-zinc-400">
                  Connecting to GitHub API to verify your personal access token.
                </p>
              </>
            )}

            {!validating && validated && (
              <>
                <CheckCircle className="h-12 w-12 text-green-500 mx-auto mb-4" />
                <h2 className="text-base font-medium text-zinc-900 dark:text-zinc-100 mb-2">
                  Integration connected!
                </h2>
                <p className="text-sm text-zinc-500 dark:text-zinc-400 mb-6">
                  Your GitHub integration is ready. You can now assign its tools to agents.
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
              </>
            )}

            {!validating && validationError && (
              <>
                <AlertCircle className="h-12 w-12 text-red-500 mx-auto mb-4" />
                <h2 className="text-base font-medium text-zinc-900 dark:text-zinc-100 mb-2">
                  Validation failed
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
                    onClick={() => {
                      setValidationError(null)
                      setStep(1)
                    }}
                    className="rounded-md border border-zinc-300 dark:border-zinc-600 px-4 py-2 text-sm text-zinc-700 dark:text-zinc-300 hover:bg-zinc-100 dark:hover:bg-zinc-800 transition-colors cursor-pointer"
                  >
                    Edit Credentials
                  </button>
                  <button
                    onClick={handleRetryValidation}
                    className="rounded-md bg-zinc-900 dark:bg-zinc-100 text-white dark:text-zinc-900 px-4 py-2 text-sm hover:bg-zinc-700 dark:hover:bg-zinc-300 transition-colors cursor-pointer"
                  >
                    Retry Validation
                  </button>
                </div>
              </>
            )}
          </div>
        )}
      </div>
    </div>
  )
}
