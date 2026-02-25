import { useState, useEffect } from 'react'
import { useParams } from 'react-router-dom'
import { agentsApi } from '@/lib/api'
import type { Agent } from '@/types'
import AgentForm from '@/components/AgentForm'

export default function AgentEditPage() {
  const { slug } = useParams<{ slug: string }>()
  const [agent, setAgent] = useState<Agent | null>(null)
  const [loading, setLoading] = useState(true)
  const [error, setError] = useState<string | null>(null)

  useEffect(() => {
    if (!slug) return
    agentsApi
      .get(slug)
      .then(setAgent)
      .catch(err => setError(err instanceof Error ? err.message : 'Failed to load agent'))
      .finally(() => setLoading(false))
  }, [slug])

  if (loading) {
    return (
      <div className="flex h-full items-center justify-center">
        <div className="text-sm text-muted-foreground">Loading…</div>
      </div>
    )
  }

  if (error || !agent) {
    return (
      <div className="flex h-full items-center justify-center">
        <div className="text-sm text-red-600">{error ?? 'Agent not found'}</div>
      </div>
    )
  }

  return (
    <div className="flex flex-col h-full">
      <div className="border-b border-border px-4 sm:px-6 py-4">
        <h1 className="text-lg font-semibold">Edit Agent</h1>
        <p className="text-sm text-muted-foreground font-mono">{agent.slug}</p>
      </div>
      <div className="flex-1 overflow-y-auto p-4 sm:p-6">
        <AgentForm agent={agent} isEdit />
      </div>
    </div>
  )
}
