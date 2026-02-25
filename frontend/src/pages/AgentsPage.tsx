import { useState, useEffect } from 'react'
import { useNavigate } from 'react-router-dom'
import { agentsApi } from '@/lib/api'
import type { Agent } from '@/types'
import { Button } from '@/components/ui/button'
import { Badge } from '@/components/ui/badge'
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
import { Plus, Pencil, Trash2, Bot, Cpu } from 'lucide-react'

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
    void loadAgents()
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
        <div className="text-sm text-muted-foreground">Loading agents…</div>
      </div>
    )
  }

  return (
    <div className="flex flex-col h-full">
      {/* Header */}
      <div className="flex items-center justify-between border-b border-border px-6 py-4">
        <div>
          <h1 className="text-lg font-semibold">Agents</h1>
          <p className="text-sm text-muted-foreground">{agents.length} agent{agents.length !== 1 ? 's' : ''} defined</p>
        </div>
        <Button onClick={() => navigate('/agents/new')} size="sm" className="gap-1.5">
          <Plus className="h-3.5 w-3.5" />
          New Agent
        </Button>
      </div>

      {/* Error */}
      {error && (
        <div className="mx-6 mt-4 rounded-md bg-red-50 border border-red-200 px-4 py-3 text-sm text-red-700">
          {error}
        </div>
      )}

      {/* Content */}
      <div className="flex-1 overflow-y-auto p-6">
        {agents.length === 0 ? (
          <div className="flex flex-col items-center justify-center py-16 text-center">
            <div className="flex h-14 w-14 items-center justify-center rounded-full bg-gray-100 mb-4">
              <Bot className="h-7 w-7 text-gray-400" />
            </div>
            <h2 className="text-base font-semibold mb-1">No agents yet</h2>
            <p className="text-sm text-muted-foreground mb-4 max-w-xs">
              Create your first agent to start chatting. Agents are powered by Claude and can be
              customized with tools and system prompts.
            </p>
            <Button onClick={() => navigate('/agents/new')} size="sm" className="gap-1.5">
              <Plus className="h-3.5 w-3.5" />
              Create your first agent
            </Button>
          </div>
        ) : (
          <div className="grid gap-3 sm:grid-cols-2 lg:grid-cols-3">
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
}: {
  agent: Agent
  onEdit: () => void
  onDelete: () => void
}) {
  return (
    <div className="flex flex-col rounded-lg border border-border bg-card p-4 shadow-sm hover:shadow-md transition-shadow">
      {/* Icon + Name */}
      <div className="flex items-start gap-3 mb-3">
        <div className="flex h-9 w-9 items-center justify-center rounded-md bg-indigo-50 text-indigo-600 shrink-0">
          <Cpu className="h-4.5 w-4.5" />
        </div>
        <div className="flex-1 min-w-0">
          <h3 className="font-semibold text-sm truncate">{agent.name}</h3>
          <p className="text-xs text-muted-foreground font-mono">{agent.slug}</p>
        </div>
      </div>

      {/* Description */}
      {agent.description && (
        <p className="text-xs text-muted-foreground mb-3 line-clamp-2 flex-1">
          {agent.description}
        </p>
      )}

      {/* Model badge */}
      <div className="mb-3">
        <Badge variant="secondary" className="text-xs font-mono">
          {agent.model.replace('claude-', '')}
        </Badge>
      </div>

      {/* Actions */}
      <div className="flex items-center gap-2 pt-2 border-t border-border">
        <Button variant="ghost" size="sm" className="h-7 px-2 text-xs gap-1.5" onClick={onEdit}>
          <Pencil className="h-3 w-3" />
          Edit
        </Button>
        <AlertDialog>
          <AlertDialogTrigger asChild>
            <Button
              variant="ghost"
              size="sm"
              className="h-7 px-2 text-xs gap-1.5 text-destructive hover:text-destructive hover:bg-destructive/10"
            >
              <Trash2 className="h-3 w-3" />
              Delete
            </Button>
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
                className="bg-destructive text-destructive-foreground hover:bg-destructive/90"
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
