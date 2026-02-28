import { useState, useEffect } from 'react'
import { useNavigate } from 'react-router-dom'
import { agentsApi } from '@/lib/api'
import type { Agent } from '@/types'
import { Button } from '@/components/ui/button'
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
import { Plus, Pencil, Trash2, Bot } from 'lucide-react'

function shortModel(model: string): string {
  // eu.anthropic.claude-X → claude-X, claude-sonnet-4-6 → sonnet-4-6
  const m = model.replace(/^.*anthropic\./i, '').replace(/^claude-/, '')
  return m.length > 28 ? m.slice(0, 28) + '…' : m
}

export default function AgentsPage() {
  const navigate = useNavigate()
  const [agents, setAgents] = useState<Agent[]>([])
  const [loading, setLoading] = useState(true)
  const [error, setError] = useState<string | null>(null)

  const loadAgents = async () => {
    try {
      const data = await agentsApi.list()
      setAgents(data)
    } catch (err) {
      setError(err instanceof Error ? err.message : 'Failed to load agents')
    } finally {
      setLoading(false)
    }
  }

  useEffect(() => {
    loadAgents()
  }, [])

  const handleDelete = async (slug: string) => {
    try {
      await agentsApi.delete(slug)
      setAgents(prev => prev.filter(a => a.slug !== slug))
    } catch (err) {
      setError(err instanceof Error ? err.message : 'Failed to delete agent')
    }
  }

  if (loading) {
    return (
      <div className="flex h-full items-center justify-center">
        <div className="text-sm text-zinc-400">Loading agents…</div>
      </div>
    )
  }

  return (
    <div className="flex flex-col h-full">
      {/* Header */}
      <div className="flex items-center justify-between border-b border-zinc-100 px-4 sm:px-6 py-4 shrink-0">
        <div>
          <h1 className="text-base font-semibold text-zinc-900">Agents</h1>
          <p className="text-xs text-zinc-500 mt-0.5">
            {agents.length} agent{agents.length === 1 ? '' : 's'} defined
          </p>
        </div>
        <Button
          onClick={() => navigate('/agents/new')}
          size="sm"
          className="gap-1.5 bg-zinc-900 hover:bg-zinc-800 text-white text-xs h-8"
        >
          <Plus className="h-3.5 w-3.5" />
          New Agent
        </Button>
      </div>

      {error && (
        <div className="mx-6 mt-3 rounded-md border border-red-200 bg-red-50 px-4 py-2.5 text-sm text-red-700">
          {error}
        </div>
      )}

      {/* Content */}
      <div className="flex-1 overflow-y-auto p-4 sm:p-6">
        {agents.length === 0 ? (
          <div className="flex flex-col items-center justify-center py-20 text-center">
            <div className="flex h-12 w-12 items-center justify-center rounded-full bg-zinc-100 mb-4">
              <Bot className="h-5 w-5 text-zinc-400" />
            </div>
            <h2 className="text-sm font-semibold text-zinc-900 mb-1">No agents yet</h2>
            <p className="text-xs text-zinc-500 mb-4 max-w-xs">
              Create your first agent to start chatting. Agents are powered by Claude and can be
              customized with tools and system prompts.
            </p>
            <Button
              onClick={() => navigate('/agents/new')}
              size="sm"
              className="gap-1.5 bg-zinc-900 hover:bg-zinc-800 text-white text-xs h-8"
            >
              <Plus className="h-3.5 w-3.5" />
              Create your first agent
            </Button>
          </div>
        ) : (
          <div className="grid gap-3 grid-cols-1 sm:grid-cols-2 xl:grid-cols-3">
            {agents.map(agent => (
              <AgentCard
                key={agent.slug}
                agent={agent}
                onEdit={() => navigate(`/agents/${agent.slug}/edit`)}
                onDelete={() => handleDelete(agent.slug)}
              />
            ))}
          </div>
        )}
      </div>
    </div>
  )
}

function AgentCard({
  agent,
  onEdit,
  onDelete,
}: Readonly<{
  agent: Agent
  onEdit: () => void
  onDelete: () => void
}>) {
  return (
    <div className="flex flex-col rounded-lg border border-zinc-200 bg-white p-4 hover:border-zinc-300 transition-colors">
      {/* Icon + Name */}
      <div className="flex items-start gap-3 mb-3">
        <div className="flex h-8 w-8 items-center justify-center rounded-md bg-zinc-900 text-white shrink-0">
          <Bot className="h-4 w-4" />
        </div>
        <div className="flex-1 min-w-0">
          <h3 className="font-semibold text-sm text-zinc-900 truncate">{agent.name}</h3>
          <p className="text-xs text-zinc-400 font-mono">{agent.slug}</p>
        </div>
      </div>

      {/* Description */}
      {agent.description && (
        <p className="text-xs text-zinc-500 mb-3 line-clamp-2 flex-1 leading-relaxed">
          {agent.description}
        </p>
      )}

      {/* Model */}
      <div className="mb-3">
        <span className="inline-flex items-center rounded-md bg-zinc-100 px-2 py-0.5 text-xs font-mono text-zinc-600">
          {shortModel(agent.model)}
        </span>
      </div>

      {/* Actions */}
      <div className="flex items-center gap-1 pt-2 border-t border-zinc-100">
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
              <AlertDialogTitle>Delete agent?</AlertDialogTitle>
              <AlertDialogDescription>
                This will permanently delete <strong>{agent.name}</strong>. This action cannot be
                undone.
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
