import { useState, useEffect } from 'react'
import { useNavigate } from 'react-router-dom'
import { Pencil, Trash2, CheckCircle, XCircle, Plug, Plus } from 'lucide-react'
import { integrationsApi } from '@/lib/api'
import type { Integration } from '@/types'
import {
  GoogleIcon,
  GmailIcon,
  GoogleCalendarIcon,
  GoogleDriveIcon,
} from '@/components/GoogleIcons'
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

const AVAILABLE_PROVIDERS = [
  {
    id: 'google',
    name: 'Google',
    description: 'Calendar, Gmail & Drive',
    icon: <GoogleIcon size={24} />,
  },
]

export default function IntegrationsPage() {
  const navigate = useNavigate()
  const [integrations, setIntegrations] = useState<Integration[]>([])
  const [loading, setLoading] = useState(true)
  const [error, setError] = useState<string | null>(null)

  const loadIntegrations = async () => {
    try {
      const data = await integrationsApi.list()
      setIntegrations(data)
    } catch (err) {
      setError(err instanceof Error ? err.message : 'Failed to load integrations')
    } finally {
      setLoading(false)
    }
  }

  useEffect(() => {
    void loadIntegrations()
  }, [])

  const handleDelete = async (id: string) => {
    try {
      await integrationsApi.delete(id)
      setIntegrations(prev => prev.filter(i => i.id !== id))
    } catch (err) {
      setError(err instanceof Error ? err.message : 'Failed to delete integration')
    }
  }

  if (loading) {
    return (
      <div className="flex h-full items-center justify-center">
        <div className="text-sm text-zinc-400">Loading integrations…</div>
      </div>
    )
  }

  return (
    <div className="flex flex-col h-full">
      {/* Header */}
      <div className="flex items-center justify-between border-b border-zinc-100 px-4 sm:px-6 py-4 shrink-0">
        <div>
          <h1 className="text-base font-semibold text-zinc-900">Integrations</h1>
          <p className="text-xs text-zinc-500 mt-0.5">
            Connect external services to make them available as tools to your agents
          </p>
        </div>
      </div>

      {error && (
        <div className="mx-6 mt-3 rounded-md border border-red-200 bg-red-50 px-4 py-2.5 text-sm text-red-700">
          {error}
        </div>
      )}

      {/* Content */}
      <div className="flex-1 overflow-y-auto p-4 sm:p-6 space-y-8">
        {/* Configured integrations */}
        {integrations.length > 0 && (
          <section>
            <h2 className="text-xs font-semibold uppercase tracking-widest text-zinc-400 mb-3">
              Your Integrations
            </h2>
            <div className="grid gap-3 grid-cols-1 sm:grid-cols-2 xl:grid-cols-3">
              {integrations.map(integration => (
                <IntegrationCard
                  key={integration.id}
                  integration={integration}
                  onEdit={() => navigate(`/integrations/${integration.id}`)}
                  onDelete={() => handleDelete(integration.id)}
                />
              ))}
            </div>
          </section>
        )}

        {/* Available providers */}
        <section>
          <h2 className="text-xs font-semibold uppercase tracking-widest text-zinc-400 mb-3">
            Add an Integration
          </h2>
          <div className="grid gap-3 grid-cols-1 sm:grid-cols-2 xl:grid-cols-3">
            {AVAILABLE_PROVIDERS.map(provider => (
              <button
                key={provider.id}
                onClick={() => navigate('/integrations/new')}
                className="flex items-center gap-3 rounded-lg border border-zinc-200 bg-white p-4 text-left hover:border-zinc-300 hover:bg-zinc-50 transition-colors group"
              >
                <div className="flex h-10 w-10 items-center justify-center rounded-lg border border-zinc-200 bg-white shrink-0">
                  {provider.icon}
                </div>
                <div className="flex-1 min-w-0">
                  <p className="text-sm font-medium text-zinc-900">{provider.name}</p>
                  <p className="text-xs text-zinc-500">{provider.description}</p>
                </div>
                <Plus className="h-4 w-4 text-zinc-300 group-hover:text-zinc-500 transition-colors shrink-0" />
              </button>
            ))}
          </div>
        </section>
      </div>
    </div>
  )
}

function IntegrationTypeIcon({ type, size }: { type: string; size: number }) {
  if (type === 'google') return <GoogleIcon size={size} />
  return <Plug className="h-4 w-4 text-zinc-400" />
}

function ServiceIcon({ service, size }: { service: string; size: number }) {
  if (service === 'calendar') return <GoogleCalendarIcon size={size} />
  if (service === 'gmail') return <GmailIcon size={size} />
  if (service === 'drive') return <GoogleDriveIcon size={size} />
  return null
}

function IntegrationCard({
  integration,
  onEdit,
  onDelete,
}: {
  integration: Integration
  onEdit: () => void
  onDelete: () => void
}) {
  const enabledServices = Object.entries(integration.services ?? {})
    .filter(([, svc]) => svc.enabled)
    .map(([name]) => name)

  return (
    <div className="flex flex-col rounded-lg border border-zinc-200 bg-white p-4 hover:border-zinc-300 transition-colors">
      {/* Icon + Name */}
      <div className="flex items-start gap-3 mb-3">
        <div className="flex h-8 w-8 items-center justify-center rounded-md bg-white border border-zinc-200 shrink-0">
          <IntegrationTypeIcon type={integration.type} size={20} />
        </div>
        <div className="flex-1 min-w-0">
          <h3 className="font-semibold text-sm text-zinc-900 truncate">{integration.name}</h3>
          <p className="text-xs text-zinc-400 font-mono capitalize">{integration.type}</p>
        </div>
      </div>

      {/* Auth status */}
      <div className="mb-3 flex items-center gap-1.5">
        {integration.authenticated ? (
          <>
            <CheckCircle className="h-3.5 w-3.5 text-green-500 shrink-0" />
            <span className="text-xs text-green-600 font-medium">Connected</span>
          </>
        ) : (
          <>
            <XCircle className="h-3.5 w-3.5 text-zinc-400 shrink-0" />
            <span className="text-xs text-zinc-400">Not connected</span>
          </>
        )}
      </div>

      {/* Enabled services */}
      {enabledServices.length > 0 && (
        <div className="flex flex-wrap gap-1 mb-3">
          {enabledServices.map(svc => (
            <span
              key={svc}
              className="inline-flex items-center gap-1 rounded-md bg-zinc-100 px-2 py-0.5 text-xs text-zinc-600 capitalize"
            >
              <ServiceIcon service={svc} size={11} />
              {svc}
            </span>
          ))}
        </div>
      )}

      {/* Actions */}
      <div className="flex items-center gap-1 pt-2 border-t border-zinc-100 mt-auto">
        <button
          onClick={onEdit}
          className="flex items-center gap-1.5 rounded-md px-2.5 py-1.5 text-xs text-zinc-600 hover:bg-zinc-100 hover:text-zinc-900 transition-colors"
        >
          <Pencil className="h-3 w-3" />
          Edit
        </button>
        <AlertDialog>
          <AlertDialogTrigger asChild>
            <button className="flex items-center gap-1.5 rounded-md px-2.5 py-1.5 text-xs text-zinc-400 hover:bg-red-50 hover:text-red-600 transition-colors">
              <Trash2 className="h-3 w-3" />
              Delete
            </button>
          </AlertDialogTrigger>
          <AlertDialogContent>
            <AlertDialogHeader>
              <AlertDialogTitle>Delete integration?</AlertDialogTitle>
              <AlertDialogDescription>
                This will permanently delete <strong>{integration.name}</strong> and stop its MCP
                server. This action cannot be undone.
              </AlertDialogDescription>
            </AlertDialogHeader>
            <AlertDialogFooter>
              <AlertDialogCancel>Cancel</AlertDialogCancel>
              <AlertDialogAction
                onClick={onDelete}
                className="bg-red-600 text-white hover:bg-red-700"
              >
                Delete
              </AlertDialogAction>
            </AlertDialogFooter>
          </AlertDialogContent>
        </AlertDialog>
      </div>
    </div>
  )
}
