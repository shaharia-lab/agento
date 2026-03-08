import { useEffect, useState, useCallback } from 'react'
import {
  Plus,
  Trash2,
  Loader2,
  AlertCircle,
  CheckCircle,
  XCircle,
  RefreshCw,
  Globe,
  Info,
} from 'lucide-react'
import { triggerRulesApi, webhookApi, agentsApi } from '@/lib/api'
import type { TriggerRule, WebhookStatus, Agent } from '@/types'
import TriggerRuleForm from './TriggerRuleForm'

interface Props {
  integrationId: string
}

export default function TriggerRulesPanel({ integrationId }: Props) {
  const [rules, setRules] = useState<TriggerRule[]>([])
  const [webhookStatus, setWebhookStatus] = useState<WebhookStatus | null>(null)
  const [agents, setAgents] = useState<Agent[]>([])
  const [loading, setLoading] = useState(true)
  const [error, setError] = useState<string | null>(null)
  const [success, setSuccess] = useState<string | null>(null)
  const [showForm, setShowForm] = useState(false)
  const [editingRule, setEditingRule] = useState<TriggerRule | null>(null)
  const [actionLoading, setActionLoading] = useState(false)

  const loadData = useCallback(async () => {
    try {
      const [rulesData, statusData, agentsData] = await Promise.all([
        triggerRulesApi.list(integrationId),
        webhookApi.status(integrationId),
        agentsApi.list(),
      ])
      setRules(rulesData)
      setWebhookStatus(statusData)
      setAgents(agentsData)
    } catch (err) {
      setError((err as Error).message)
    } finally {
      setLoading(false)
    }
  }, [integrationId])

  useEffect(() => {
    loadData()
  }, [loadData])

  const handleRegisterWebhook = async () => {
    setActionLoading(true)
    setError(null)
    try {
      await webhookApi.register(integrationId)
      setSuccess('Webhook registered successfully.')
      await loadData()
    } catch (err) {
      setError((err as Error).message)
    } finally {
      setActionLoading(false)
    }
  }

  const handleRemoveWebhook = async () => {
    setActionLoading(true)
    setError(null)
    try {
      await webhookApi.remove(integrationId)
      setSuccess('Webhook removed.')
      await loadData()
    } catch (err) {
      setError((err as Error).message)
    } finally {
      setActionLoading(false)
    }
  }

  const handleRegenerateSecret = async () => {
    setActionLoading(true)
    setError(null)
    try {
      await webhookApi.regenerateSecret(integrationId)
      setSuccess('Secret regenerated and webhook re-registered.')
      await loadData()
    } catch (err) {
      setError((err as Error).message)
    } finally {
      setActionLoading(false)
    }
  }

  const handleDeleteRule = async (ruleId: string) => {
    setError(null)
    try {
      await triggerRulesApi.delete(integrationId, ruleId)
      setRules(prev => prev.filter(r => r.id !== ruleId))
      setSuccess('Rule deleted.')
    } catch (err) {
      setError((err as Error).message)
    }
  }

  const handleSaveRule = async (data: Partial<TriggerRule>) => {
    setError(null)
    try {
      if (editingRule) {
        const updated = await triggerRulesApi.update(integrationId, editingRule.id, data)
        setRules(prev => prev.map(r => (r.id === updated.id ? updated : r)))
        setSuccess('Rule updated.')
      } else {
        const created = await triggerRulesApi.create(integrationId, data)
        setRules(prev => [...prev, created])
        setSuccess('Rule created.')
      }
      setShowForm(false)
      setEditingRule(null)
    } catch (err) {
      setError((err as Error).message)
    }
  }

  if (loading) {
    return (
      <div className="flex items-center justify-center py-8">
        <Loader2 className="h-5 w-5 animate-spin text-zinc-400" />
      </div>
    )
  }

  const isActive = webhookStatus?.status === 'active'
  const hasRules = rules.length > 0

  return (
    <div className="space-y-4">
      {/* Setup guide */}
      <div className="rounded-lg border border-blue-200 dark:border-blue-800/50 bg-blue-50 dark:bg-blue-900/20 p-4">
        <div className="flex items-start gap-2">
          <Info className="h-4 w-4 text-blue-500 shrink-0 mt-0.5" />
          <div className="text-sm text-blue-700 dark:text-blue-300">
            <p className="font-medium mb-1">Setup checklist</p>
            <ol className="list-decimal list-inside space-y-1 text-xs text-blue-600 dark:text-blue-400">
              <li className={isActive ? 'line-through opacity-50' : ''}>
                Set your <strong>Public URL</strong> in{' '}
                <a href="/settings" className="underline hover:no-underline">
                  Settings → General
                </a>{' '}
                (the externally reachable URL of this Agento instance).
              </li>
              <li className={isActive ? 'line-through opacity-50' : ''}>
                Click <strong>Register Webhook</strong> below — Agento will automatically notify
                Telegram of the webhook URL.
              </li>
              <li className={!isActive ? 'opacity-50' : hasRules ? 'line-through opacity-50' : ''}>
                Add at least one <strong>Trigger Rule</strong> to route incoming messages to an
                agent.
              </li>
              <li className={!isActive || !hasRules ? 'opacity-50' : ''}>
                Send a message to your bot in Telegram and the matching agent will reply.
              </li>
            </ol>
          </div>
        </div>
      </div>

      {error && (
        <div className="rounded-md border border-red-200 dark:border-red-800/50 bg-red-50 dark:bg-red-900/20 px-4 py-2.5 text-sm text-red-700 dark:text-red-400">
          {error}
        </div>
      )}
      {success && (
        <div className="rounded-md border border-green-200 dark:border-green-800/50 bg-green-50 dark:bg-green-900/20 px-4 py-2.5 text-sm text-green-700 dark:text-green-400">
          {success}
        </div>
      )}

      {/* Webhook Status Card */}
      <div className="rounded-lg border border-zinc-200 dark:border-zinc-700 bg-white dark:bg-zinc-800 p-4">
        <h3 className="text-xs font-semibold uppercase tracking-widest text-zinc-400 mb-3">
          Webhook Status
        </h3>
        <div className="space-y-3">
          <div className="flex items-center gap-2">
            {webhookStatus?.status === 'active' && (
              <span className="flex items-center gap-1.5 text-sm text-green-600 dark:text-green-400 font-medium">
                <CheckCircle className="h-4 w-4" />
                Active
              </span>
            )}
            {webhookStatus?.status === 'error' && (
              <span className="flex items-center gap-1.5 text-sm text-red-600 dark:text-red-400 font-medium">
                <AlertCircle className="h-4 w-4" />
                Error
              </span>
            )}
            {(!webhookStatus || webhookStatus.status === 'inactive') && (
              <span className="flex items-center gap-1.5 text-sm text-zinc-400 font-medium">
                <XCircle className="h-4 w-4" />
                Not registered
              </span>
            )}
          </div>

          {webhookStatus?.status === 'active' && webhookStatus.url && (
            <div className="flex items-center gap-2 text-xs text-zinc-500 dark:text-zinc-400">
              <Globe className="h-3.5 w-3.5 shrink-0" />
              <code className="bg-zinc-100 dark:bg-zinc-700 px-2 py-0.5 rounded text-xs break-all">
                {webhookStatus.url}
              </code>
            </div>
          )}

          {webhookStatus?.status === 'error' && webhookStatus.error && (
            <p className="text-xs text-red-500 dark:text-red-400">{webhookStatus.error}</p>
          )}

          <div className="flex items-center gap-2 pt-1">
            {(!webhookStatus || webhookStatus.status === 'inactive') && (
              <button
                onClick={handleRegisterWebhook}
                disabled={actionLoading}
                className="flex items-center gap-1.5 rounded-md bg-zinc-900 dark:bg-zinc-100 text-white dark:text-zinc-900 px-3 py-1.5 text-xs hover:bg-zinc-700 dark:hover:bg-zinc-300 disabled:opacity-40 transition-colors cursor-pointer"
              >
                {actionLoading ? <Loader2 className="h-3.5 w-3.5 animate-spin" /> : null}
                Register Webhook
              </button>
            )}
            {webhookStatus?.status === 'active' && (
              <>
                <button
                  onClick={handleRegenerateSecret}
                  disabled={actionLoading}
                  className="flex items-center gap-1.5 rounded-md border border-zinc-200 dark:border-zinc-600 px-3 py-1.5 text-xs text-zinc-600 dark:text-zinc-300 hover:bg-zinc-50 dark:hover:bg-zinc-800 disabled:opacity-40 transition-colors cursor-pointer"
                >
                  <RefreshCw className="h-3.5 w-3.5" />
                  Regenerate Secret
                </button>
                <button
                  onClick={handleRemoveWebhook}
                  disabled={actionLoading}
                  className="flex items-center gap-1.5 rounded-md border border-red-200 dark:border-red-800/50 px-3 py-1.5 text-xs text-red-600 dark:text-red-400 hover:bg-red-50 dark:hover:bg-red-900/20 disabled:opacity-40 transition-colors cursor-pointer"
                >
                  Remove Webhook
                </button>
              </>
            )}
            {webhookStatus?.status === 'error' && (
              <button
                onClick={handleRegisterWebhook}
                disabled={actionLoading}
                className="flex items-center gap-1.5 rounded-md bg-zinc-900 dark:bg-zinc-100 text-white dark:text-zinc-900 px-3 py-1.5 text-xs hover:bg-zinc-700 dark:hover:bg-zinc-300 disabled:opacity-40 transition-colors cursor-pointer"
              >
                {actionLoading ? <Loader2 className="h-3.5 w-3.5 animate-spin" /> : null}
                Retry Registration
              </button>
            )}
          </div>
        </div>
      </div>

      {/* Trigger Rules */}
      <div className="rounded-lg border border-zinc-200 dark:border-zinc-700 bg-white dark:bg-zinc-800 p-4">
        <div className="flex items-center justify-between mb-3">
          <h3 className="text-xs font-semibold uppercase tracking-widest text-zinc-400">
            Trigger Rules
          </h3>
          <button
            onClick={() => {
              setEditingRule(null)
              setShowForm(true)
            }}
            className="flex items-center gap-1.5 rounded-md bg-zinc-900 dark:bg-zinc-100 text-white dark:text-zinc-900 px-3 py-1.5 text-xs hover:bg-zinc-700 dark:hover:bg-zinc-300 transition-colors cursor-pointer"
          >
            <Plus className="h-3.5 w-3.5" />
            Add Rule
          </button>
        </div>

        {rules.length === 0 ? (
          <p className="text-sm text-zinc-400 py-4 text-center">
            No trigger rules configured. Add a rule to start processing inbound messages.
          </p>
        ) : (
          <div className="space-y-2">
            {rules.map(rule => {
              const agentName =
                agents.find(a => a.slug === rule.agent_slug)?.name || rule.agent_slug
              return (
                <div
                  key={rule.id}
                  className="flex items-center justify-between rounded-md border border-zinc-100 dark:border-zinc-700 px-3 py-2"
                >
                  <div className="min-w-0 flex-1">
                    <div className="flex items-center gap-2">
                      <span
                        className={`inline-block h-2 w-2 rounded-full shrink-0 ${
                          rule.enabled ? 'bg-green-500' : 'bg-zinc-300 dark:bg-zinc-600'
                        }`}
                      />
                      <span className="text-sm font-medium text-zinc-900 dark:text-zinc-100 truncate">
                        {rule.name || 'Unnamed rule'}
                      </span>
                    </div>
                    <div className="flex items-center gap-3 mt-1 text-xs text-zinc-400">
                      <span>Agent: {agentName}</span>
                      {rule.filter_prefix && <span>Prefix: {rule.filter_prefix}</span>}
                      {rule.filter_keywords?.length > 0 && (
                        <span>Keywords: {rule.filter_keywords.join(', ')}</span>
                      )}
                      {rule.filter_chat_ids?.length > 0 && (
                        <span>Chat IDs: {rule.filter_chat_ids.length}</span>
                      )}
                    </div>
                  </div>
                  <div className="flex items-center gap-1.5 shrink-0 ml-2">
                    <button
                      onClick={() => {
                        setEditingRule(rule)
                        setShowForm(true)
                      }}
                      className="text-xs text-zinc-500 hover:text-zinc-700 dark:hover:text-zinc-200 px-2 py-1 cursor-pointer"
                    >
                      Edit
                    </button>
                    <button
                      onClick={() => handleDeleteRule(rule.id)}
                      className="text-zinc-400 hover:text-red-500 cursor-pointer p-1"
                    >
                      <Trash2 className="h-3.5 w-3.5" />
                    </button>
                  </div>
                </div>
              )
            })}
          </div>
        )}
      </div>

      {/* Add/Edit Rule Modal */}
      {showForm && (
        <TriggerRuleForm
          rule={editingRule}
          agents={agents}
          onSave={handleSaveRule}
          onCancel={() => {
            setShowForm(false)
            setEditingRule(null)
          }}
        />
      )}
    </div>
  )
}
