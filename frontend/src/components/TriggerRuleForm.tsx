import { useState } from 'react'
import { X, Loader2 } from 'lucide-react'
import type { TriggerRule, Agent } from '@/types'

interface Props {
  rule: TriggerRule | null
  agents: Agent[]
  onSave: (data: Partial<TriggerRule>) => Promise<void>
  onCancel: () => void
}

export default function TriggerRuleForm({ rule, agents, onSave, onCancel }: Props) {
  const [name, setName] = useState(rule?.name ?? '')
  const [agentSlug, setAgentSlug] = useState(rule?.agent_slug ?? '')
  const [enabled, setEnabled] = useState(rule?.enabled ?? true)
  const [filterPrefix, setFilterPrefix] = useState(rule?.filter_prefix ?? '')
  const [filterKeywords, setFilterKeywords] = useState(rule?.filter_keywords?.join(', ') ?? '')
  const [filterChatIds, setFilterChatIds] = useState(rule?.filter_chat_ids?.join(', ') ?? '')
  const [saving, setSaving] = useState(false)
  const [error, setError] = useState<string | null>(null)

  const handleSubmit = async (e: React.FormEvent) => {
    e.preventDefault()
    if (!agentSlug) {
      setError('Please select an agent.')
      return
    }
    setSaving(true)
    setError(null)
    try {
      await onSave({
        name: name.trim(),
        agent_slug: agentSlug,
        enabled,
        filter_prefix: filterPrefix.trim(),
        filter_keywords: parseCommaSeparated(filterKeywords),
        filter_chat_ids: parseCommaSeparated(filterChatIds),
      })
    } catch (err) {
      setError((err as Error).message)
      setSaving(false)
    }
  }

  return (
    <div className="fixed inset-0 z-50 flex items-center justify-center bg-black/40">
      <div className="w-full max-w-lg rounded-lg border border-zinc-200 dark:border-zinc-700 bg-white dark:bg-zinc-800 shadow-xl">
        <div className="flex items-center justify-between border-b border-zinc-100 dark:border-zinc-700 px-5 py-3">
          <h3 className="text-sm font-semibold text-zinc-900 dark:text-zinc-100">
            {rule ? 'Edit Trigger Rule' : 'New Trigger Rule'}
          </h3>
          <button
            onClick={onCancel}
            className="text-zinc-400 hover:text-zinc-600 dark:hover:text-zinc-200 cursor-pointer"
          >
            <X className="h-4 w-4" />
          </button>
        </div>

        <form onSubmit={handleSubmit} className="p-5 space-y-4">
          {error && (
            <div className="rounded-md border border-red-200 dark:border-red-800/50 bg-red-50 dark:bg-red-900/20 px-3 py-2 text-xs text-red-700 dark:text-red-400">
              {error}
            </div>
          )}

          <div>
            <label
              htmlFor="rule-name"
              className="block text-xs font-medium text-zinc-700 dark:text-zinc-300 mb-1"
            >
              Rule Name
            </label>
            <input
              id="rule-name"
              type="text"
              value={name}
              onChange={e => setName(e.target.value)}
              placeholder="e.g. Support requests"
              className="w-full rounded-md border border-zinc-300 dark:border-zinc-600 bg-white dark:bg-zinc-900 px-3 py-2 text-sm text-zinc-900 dark:text-zinc-100 focus:outline-none focus:ring-2 focus:ring-zinc-900 dark:focus:ring-zinc-400"
            />
          </div>

          <div>
            <label
              htmlFor="rule-agent"
              className="block text-xs font-medium text-zinc-700 dark:text-zinc-300 mb-1"
            >
              Agent <span className="text-red-500">*</span>
            </label>
            <select
              id="rule-agent"
              value={agentSlug}
              onChange={e => setAgentSlug(e.target.value)}
              className="w-full rounded-md border border-zinc-300 dark:border-zinc-600 bg-white dark:bg-zinc-900 px-3 py-2 text-sm text-zinc-900 dark:text-zinc-100 focus:outline-none focus:ring-2 focus:ring-zinc-900 dark:focus:ring-zinc-400"
            >
              <option value="">Select an agent...</option>
              {agents.map(a => (
                <option key={a.slug} value={a.slug}>
                  {a.name}
                </option>
              ))}
            </select>
          </div>

          <div className="flex items-center gap-2">
            <input
              type="checkbox"
              id="rule-enabled"
              checked={enabled}
              onChange={e => setEnabled(e.target.checked)}
              className="h-4 w-4 rounded border-zinc-300 dark:border-zinc-600"
            />
            <label
              htmlFor="rule-enabled"
              className="text-sm text-zinc-700 dark:text-zinc-300 cursor-pointer"
            >
              Enabled
            </label>
          </div>

          <div className="border-t border-zinc-100 dark:border-zinc-700 pt-4">
            <p className="text-xs font-semibold uppercase tracking-widest text-zinc-400 mb-3">
              Filters (optional)
            </p>

            <div className="space-y-3">
              <div>
                <label
                  htmlFor="filter-prefix"
                  className="block text-xs font-medium text-zinc-700 dark:text-zinc-300 mb-1"
                >
                  Prefix
                </label>
                <input
                  id="filter-prefix"
                  type="text"
                  value={filterPrefix}
                  onChange={e => setFilterPrefix(e.target.value)}
                  placeholder="e.g. /ask"
                  className="w-full rounded-md border border-zinc-300 dark:border-zinc-600 bg-white dark:bg-zinc-900 px-3 py-2 text-sm text-zinc-900 dark:text-zinc-100 focus:outline-none focus:ring-2 focus:ring-zinc-900 dark:focus:ring-zinc-400"
                />
                <p className="text-xs text-zinc-400 mt-1">
                  Only trigger when the message starts with this prefix.
                </p>
              </div>

              <div>
                <label
                  htmlFor="filter-keywords"
                  className="block text-xs font-medium text-zinc-700 dark:text-zinc-300 mb-1"
                >
                  Keywords
                </label>
                <input
                  id="filter-keywords"
                  type="text"
                  value={filterKeywords}
                  onChange={e => setFilterKeywords(e.target.value)}
                  placeholder="e.g. help, support, question"
                  className="w-full rounded-md border border-zinc-300 dark:border-zinc-600 bg-white dark:bg-zinc-900 px-3 py-2 text-sm text-zinc-900 dark:text-zinc-100 focus:outline-none focus:ring-2 focus:ring-zinc-900 dark:focus:ring-zinc-400"
                />
                <p className="text-xs text-zinc-400 mt-1">
                  Comma-separated. Matches if any keyword is found.
                </p>
              </div>

              <div>
                <label
                  htmlFor="filter-chat-ids"
                  className="block text-xs font-medium text-zinc-700 dark:text-zinc-300 mb-1"
                >
                  Chat IDs
                </label>
                <input
                  id="filter-chat-ids"
                  type="text"
                  value={filterChatIds}
                  onChange={e => setFilterChatIds(e.target.value)}
                  placeholder="e.g. 12345, -100987654"
                  className="w-full rounded-md border border-zinc-300 dark:border-zinc-600 bg-white dark:bg-zinc-900 px-3 py-2 text-sm text-zinc-900 dark:text-zinc-100 focus:outline-none focus:ring-2 focus:ring-zinc-900 dark:focus:ring-zinc-400"
                />
                <p className="text-xs text-zinc-400 mt-1">
                  Comma-separated. Only trigger from these chats.
                </p>
              </div>
            </div>
          </div>

          <div className="flex items-center justify-end gap-2 pt-2">
            <button
              type="button"
              onClick={onCancel}
              className="rounded-md border border-zinc-200 dark:border-zinc-600 px-4 py-2 text-xs text-zinc-600 dark:text-zinc-300 hover:bg-zinc-50 dark:hover:bg-zinc-700 transition-colors cursor-pointer"
            >
              Cancel
            </button>
            <button
              type="submit"
              disabled={saving}
              className="flex items-center gap-1.5 rounded-md bg-zinc-900 dark:bg-zinc-100 text-white dark:text-zinc-900 px-4 py-2 text-xs hover:bg-zinc-700 dark:hover:bg-zinc-300 disabled:opacity-40 transition-colors cursor-pointer"
            >
              {saving && <Loader2 className="h-3.5 w-3.5 animate-spin" />}
              {rule ? 'Update Rule' : 'Create Rule'}
            </button>
          </div>
        </form>
      </div>
    </div>
  )
}

function parseCommaSeparated(input: string): string[] {
  return input
    .split(',')
    .map(s => s.trim())
    .filter(Boolean)
}
