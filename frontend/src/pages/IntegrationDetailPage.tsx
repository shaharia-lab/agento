import { useEffect, useState } from 'react'
import { useParams, useNavigate } from 'react-router-dom'
import { ArrowLeft, CheckCircle, XCircle, Loader2, RefreshCw, Trash2, Save } from 'lucide-react'
import { integrationsApi } from '@/lib/api'
import type { Integration, ServiceConfig } from '@/types'
import {
  AlertDialog,
  AlertDialogAction,
  AlertDialogCancel,
  AlertDialogContent,
  AlertDialogDescription,
  AlertDialogFooter,
  AlertDialogHeader,
  AlertDialogTitle,
  AlertDialogTrigger,
} from '@/components/ui/alert-dialog'

const GOOGLE_TOOLS: Record<string, string[]> = {
  calendar: ['create_event', 'view_events'],
  gmail: ['send_email', 'read_email', 'search_email'],
  drive: ['list_files', 'create_file', 'download_file'],
}

export default function IntegrationDetailPage() {
  const { id } = useParams<{ id: string }>()
  const navigate = useNavigate()

  const [integration, setIntegration] = useState<Integration | null>(null)
  const [loading, setLoading] = useState(true)
  const [saving, setSaving] = useState(false)
  const [deleting, setDeleting] = useState(false)
  const [reauthing, setReauthing] = useState(false)
  const [polling, setPolling] = useState(false)
  const [error, setError] = useState<string | null>(null)
  const [success, setSuccess] = useState<string | null>(null)

  const [name, setName] = useState('')
  const [enabled, setEnabled] = useState(false)
  const [services, setServices] = useState<Record<string, ServiceConfig>>({})

  useEffect(() => {
    if (!id) return
    integrationsApi
      .get(id)
      .then(data => {
        setIntegration(data)
        setName(data.name)
        setEnabled(data.enabled)
        setServices(
          data.services ?? {
            calendar: { enabled: false, tools: [] },
            gmail: { enabled: false, tools: [] },
            drive: { enabled: false, tools: [] },
          },
        )
      })
      .catch(err => setError(err.message))
      .finally(() => setLoading(false))
  }, [id])

  const handleSave = async () => {
    if (!id || !integration) return
    setSaving(true)
    setError(null)
    setSuccess(null)
    try {
      const updated = await integrationsApi.update(id, { ...integration, name, enabled, services })
      setIntegration(updated)
      setSuccess('Integration updated.')
    } catch (err) {
      setError((err as Error).message)
    } finally {
      setSaving(false)
    }
  }

  const handleDelete = async () => {
    if (!id) return
    setDeleting(true)
    try {
      await integrationsApi.delete(id)
      navigate('/integrations')
    } catch (err) {
      setError((err as Error).message)
      setDeleting(false)
    }
  }

  const handleReauth = async () => {
    if (!id) return
    setReauthing(true)
    setError(null)
    try {
      const { auth_url } = await integrationsApi.startOAuth(id)
      window.open(auth_url, '_blank')
      setPolling(true)
      const interval = setInterval(async () => {
        try {
          const { authenticated } = await integrationsApi.getAuthStatus(id)
          if (authenticated) {
            clearInterval(interval)
            setPolling(false)
            setReauthing(false)
            const updated = await integrationsApi.get(id)
            setIntegration(updated)
            setSuccess('Re-authentication successful.')
          }
        } catch {
          clearInterval(interval)
          setPolling(false)
          setReauthing(false)
        }
      }, 2000)
    } catch (err) {
      setError((err as Error).message)
      setReauthing(false)
    }
  }

  const handleServiceToggle = (svcName: string) => {
    setServices(prev => {
      const svc = prev[svcName] ?? { enabled: false, tools: [] }
      const nowEnabled = !svc.enabled
      return {
        ...prev,
        [svcName]: {
          enabled: nowEnabled,
          tools: nowEnabled ? [...(GOOGLE_TOOLS[svcName] ?? [])] : [],
        },
      }
    })
  }

  const handleToolToggle = (svcName: string, tool: string) => {
    setServices(prev => {
      const svc = prev[svcName] ?? { enabled: true, tools: [] }
      const tools = svc.tools.includes(tool)
        ? svc.tools.filter(t => t !== tool)
        : [...svc.tools, tool]
      return { ...prev, [svcName]: { ...svc, tools } }
    })
  }

  if (loading) {
    return (
      <div className="flex items-center justify-center h-full">
        <Loader2 className="h-6 w-6 animate-spin text-zinc-400" />
      </div>
    )
  }

  if (!integration) {
    return (
      <div className="flex flex-col h-full">
        <div className="border-b border-zinc-100 px-4 sm:px-6 py-4 shrink-0">
          <button
            onClick={() => navigate('/integrations')}
            className="flex items-center gap-1.5 text-sm text-zinc-500 hover:text-zinc-700 transition-colors"
          >
            <ArrowLeft className="h-4 w-4" />
            Integrations
          </button>
        </div>
        <div className="flex-1 flex items-center justify-center">
          <p className="text-sm text-zinc-500">Integration not found.</p>
        </div>
      </div>
    )
  }

  return (
    <div className="flex flex-col h-full">
      {/* Header */}
      <div className="flex items-center justify-between border-b border-zinc-100 px-4 sm:px-6 py-4 shrink-0 gap-4">
        {/* Left: breadcrumb + name + status */}
        <div className="flex items-center gap-2 min-w-0">
          <button
            onClick={() => navigate('/integrations')}
            className="flex items-center gap-1 text-sm text-zinc-400 hover:text-zinc-700 transition-colors shrink-0"
          >
            <ArrowLeft className="h-3.5 w-3.5" />
            Integrations
          </button>
          <span className="text-zinc-300 shrink-0">/</span>
          <h1 className="text-sm font-semibold text-zinc-900 truncate">{integration.name}</h1>
          <span className="text-xs bg-zinc-100 text-zinc-500 px-1.5 py-0.5 rounded capitalize shrink-0">
            {integration.type}
          </span>
          {integration.authenticated ? (
            <span className="flex items-center gap-1 text-xs text-green-600 font-medium shrink-0">
              <CheckCircle className="h-3.5 w-3.5" />
              Connected
            </span>
          ) : (
            <span className="flex items-center gap-1 text-xs text-zinc-400 shrink-0">
              <XCircle className="h-3.5 w-3.5" />
              Not connected
            </span>
          )}
        </div>

        {/* Right: actions */}
        <div className="flex items-center gap-2 shrink-0">
          <button
            onClick={handleReauth}
            disabled={reauthing || polling}
            className="flex items-center gap-1.5 rounded-md border border-zinc-200 px-3 py-1.5 text-xs text-zinc-600 hover:bg-zinc-50 disabled:opacity-40 transition-colors"
          >
            {polling ? (
              <Loader2 className="h-3.5 w-3.5 animate-spin" />
            ) : (
              <RefreshCw className="h-3.5 w-3.5" />
            )}
            {polling ? 'Waiting…' : 'Re-authenticate'}
          </button>
          <button
            onClick={handleSave}
            disabled={saving}
            className="flex items-center gap-1.5 rounded-md bg-zinc-900 text-white px-3 py-1.5 text-xs hover:bg-zinc-700 disabled:opacity-40 transition-colors"
          >
            {saving ? (
              <Loader2 className="h-3.5 w-3.5 animate-spin" />
            ) : (
              <Save className="h-3.5 w-3.5" />
            )}
            Save
          </button>
        </div>
      </div>

      {/* Scrollable content */}
      <div className="flex-1 overflow-y-auto p-4 sm:p-6 space-y-4">
        {error && (
          <div className="rounded-md border border-red-200 bg-red-50 px-4 py-2.5 text-sm text-red-700">
            {error}
          </div>
        )}
        {success && (
          <div className="rounded-md border border-green-200 bg-green-50 px-4 py-2.5 text-sm text-green-700">
            {success}
          </div>
        )}

        {/* Name + Enabled */}
        <div className="rounded-lg border border-zinc-200 bg-white p-4">
          <h2 className="text-xs font-semibold uppercase tracking-widest text-zinc-400 mb-4">
            General
          </h2>
          <div className="space-y-4">
            <div>
              <label className="block text-sm font-medium text-zinc-700 mb-1">Name</label>
              <input
                type="text"
                value={name}
                onChange={e => setName(e.target.value)}
                className="w-full max-w-sm rounded-md border border-zinc-300 bg-white px-3 py-2 text-sm text-zinc-900 focus:outline-none focus:ring-2 focus:ring-zinc-900"
              />
            </div>
            <div className="flex items-center gap-2">
              <input
                type="checkbox"
                id="enabled"
                checked={enabled}
                onChange={e => setEnabled(e.target.checked)}
                className="h-4 w-4 rounded border-zinc-300"
              />
              <label htmlFor="enabled" className="text-sm text-zinc-700 cursor-pointer">
                Enabled
              </label>
            </div>
          </div>
        </div>

        {/* Services */}
        <div className="rounded-lg border border-zinc-200 bg-white p-4">
          <h2 className="text-xs font-semibold uppercase tracking-widest text-zinc-400 mb-4">
            Services & Tools
          </h2>
          <div className="grid gap-3 grid-cols-1 sm:grid-cols-2 xl:grid-cols-3">
            {Object.keys(GOOGLE_TOOLS).map(svcName => {
              const svc = services[svcName] ?? { enabled: false, tools: [] }
              const toolNames = GOOGLE_TOOLS[svcName] ?? []
              return (
                <div
                  key={svcName}
                  className={`rounded-lg border p-3 transition-colors ${svc.enabled ? 'border-zinc-300 bg-zinc-50' : 'border-zinc-200 bg-white'}`}
                >
                  <div className="flex items-center gap-2 mb-2">
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
                  <div className="ml-6 space-y-1.5">
                    {toolNames.map(tool => (
                      <div key={tool} className="flex items-center gap-2">
                        <input
                          type="checkbox"
                          id={`tool-${svcName}-${tool}`}
                          checked={svc.tools.includes(tool)}
                          onChange={() => handleToolToggle(svcName, tool)}
                          disabled={!svc.enabled}
                          className="h-3.5 w-3.5 rounded border-zinc-300 disabled:opacity-40"
                        />
                        <label
                          htmlFor={`tool-${svcName}-${tool}`}
                          className={`text-xs font-mono cursor-pointer ${svc.enabled ? 'text-zinc-600' : 'text-zinc-400'}`}
                        >
                          {tool}
                        </label>
                      </div>
                    ))}
                  </div>
                </div>
              )
            })}
          </div>
        </div>

        {/* Danger zone */}
        <div className="rounded-lg border border-red-100 bg-white p-4">
          <h2 className="text-xs font-semibold uppercase tracking-widest text-red-400 mb-3">
            Danger Zone
          </h2>
          <div className="flex items-center justify-between">
            <div>
              <p className="text-sm font-medium text-zinc-900">Delete integration</p>
              <p className="text-xs text-zinc-500 mt-0.5">
                Permanently removes this integration and stops its MCP server.
              </p>
            </div>
            <AlertDialog>
              <AlertDialogTrigger asChild>
                <button
                  disabled={deleting}
                  className="flex items-center gap-1.5 rounded-md border border-red-200 px-3 py-1.5 text-xs text-red-600 hover:bg-red-50 disabled:opacity-40 transition-colors"
                >
                  {deleting ? (
                    <Loader2 className="h-3.5 w-3.5 animate-spin" />
                  ) : (
                    <Trash2 className="h-3.5 w-3.5" />
                  )}
                  Delete
                </button>
              </AlertDialogTrigger>
              <AlertDialogContent>
                <AlertDialogHeader>
                  <AlertDialogTitle>Delete integration?</AlertDialogTitle>
                  <AlertDialogDescription>
                    This will permanently delete <strong>{integration.name}</strong> and stop its
                    MCP server. This action cannot be undone.
                  </AlertDialogDescription>
                </AlertDialogHeader>
                <AlertDialogFooter>
                  <AlertDialogCancel>Cancel</AlertDialogCancel>
                  <AlertDialogAction
                    onClick={handleDelete}
                    className="bg-red-600 text-white hover:bg-red-700"
                  >
                    Delete
                  </AlertDialogAction>
                </AlertDialogFooter>
              </AlertDialogContent>
            </AlertDialog>
          </div>
        </div>
      </div>
    </div>
  )
}
