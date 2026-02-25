import { useState, useEffect, useCallback, useMemo } from 'react'
import { useNavigate, useSearchParams } from 'react-router-dom'
import { chatsApi, agentsApi } from '@/lib/api'
import type { ChatSession, Agent } from '@/types'
import { formatRelativeTime, truncate } from '@/lib/utils'
import { Button } from '@/components/ui/button'
import { Badge } from '@/components/ui/badge'
import {
  Dialog,
  DialogContent,
  DialogHeader,
  DialogTitle,
  DialogDescription,
  DialogFooter,
} from '@/components/ui/dialog'
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from '@/components/ui/select'
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
import { Plus, MessageSquare, Trash2, Search } from 'lucide-react'

export default function ChatsPage() {
  const navigate = useNavigate()
  const [searchParams, setSearchParams] = useSearchParams()
  const [sessions, setSessions] = useState<ChatSession[]>([])
  const [agents, setAgents] = useState<Agent[]>([])
  const [loading, setLoading] = useState(true)
  const [error, setError] = useState<string | null>(null)
  const [newChatOpen, setNewChatOpen] = useState(false)
  const [selectedAgent, setSelectedAgent] = useState('')
  const [creating, setCreating] = useState(false)

  // Filters
  const [search, setSearch] = useState('')
  const [filterAgent, setFilterAgent] = useState('all')

  const load = useCallback(async () => {
    try {
      const [s, a] = await Promise.all([chatsApi.list(), agentsApi.list()])
      setSessions(s)
      setAgents(a)
    } catch (err) {
      setError(err instanceof Error ? err.message : 'Failed to load')
    } finally {
      setLoading(false)
    }
  }, [])

  useEffect(() => {
    void load()
  }, [load])

  // Auto-open new chat dialog if ?new=1
  useEffect(() => {
    if (searchParams.get('new') === '1') {
      setNewChatOpen(true)
      setSearchParams({}, { replace: true })
    }
  }, [searchParams, setSearchParams])

  const createChat = async () => {
    if (!selectedAgent) return
    setCreating(true)
    try {
      const session = await chatsApi.create(selectedAgent)
      navigate(`/chats/${session.id}`)
    } catch (err) {
      setError(err instanceof Error ? err.message : 'Failed to create chat')
    } finally {
      setCreating(false)
      setNewChatOpen(false)
    }
  }

  const deleteSession = async (id: string) => {
    try {
      await chatsApi.delete(id)
      setSessions(prev => prev.filter(s => s.id !== id))
    } catch (err) {
      setError(err instanceof Error ? err.message : 'Failed to delete')
    }
  }

  const getAgentName = (slug: string) =>
    agents.find(a => a.slug === slug)?.name ?? slug

  const filtered = useMemo(() => {
    return sessions.filter(s => {
      const matchesSearch = !search || s.title.toLowerCase().includes(search.toLowerCase())
      const matchesAgent = filterAgent === 'all' || s.agent_slug === filterAgent
      return matchesSearch && matchesAgent
    })
  }, [sessions, search, filterAgent])

  if (loading) {
    return (
      <div className="flex h-full items-center justify-center">
        <div className="text-sm text-zinc-400">Loading chats…</div>
      </div>
    )
  }

  return (
    <div className="flex flex-col h-full">
      {/* Header */}
      <div className="flex items-center justify-between border-b border-zinc-100 px-6 py-4 shrink-0">
        <div>
          <h1 className="text-base font-semibold text-zinc-900">Chats</h1>
          <p className="text-xs text-zinc-500 mt-0.5">
            {sessions.length} conversation{sessions.length !== 1 ? 's' : ''}
          </p>
        </div>
        <Button
          size="sm"
          className="gap-1.5 bg-zinc-900 hover:bg-zinc-800 text-white text-xs h-8"
          onClick={() => setNewChatOpen(true)}
        >
          <Plus className="h-3.5 w-3.5" />
          New Chat
        </Button>
      </div>

      {/* Filters */}
      {sessions.length > 0 && (
        <div className="flex items-center gap-3 px-6 py-3 border-b border-zinc-100 shrink-0">
          <div className="relative flex-1 max-w-xs">
            <Search className="absolute left-2.5 top-1/2 -translate-y-1/2 h-3.5 w-3.5 text-zinc-400" />
            <input
              value={search}
              onChange={e => setSearch(e.target.value)}
              placeholder="Search conversations…"
              className="w-full rounded-md border border-zinc-200 bg-white pl-8 pr-3 py-1.5 text-sm placeholder:text-zinc-400 focus:outline-none focus:ring-1 focus:ring-zinc-900 focus:border-zinc-900"
            />
          </div>
          {agents.length > 1 && (
            <Select value={filterAgent} onValueChange={setFilterAgent}>
              <SelectTrigger className="w-40 h-8 text-xs">
                <SelectValue placeholder="All agents" />
              </SelectTrigger>
              <SelectContent>
                <SelectItem value="all">All agents</SelectItem>
                {agents.map(a => (
                  <SelectItem key={a.slug} value={a.slug}>
                    {a.name}
                  </SelectItem>
                ))}
              </SelectContent>
            </Select>
          )}
        </div>
      )}

      {error && (
        <div className="mx-6 mt-3 rounded-md border border-red-200 bg-red-50 px-4 py-2.5 text-sm text-red-700">
          {error}
        </div>
      )}

      {/* Chat list */}
      <div className="flex-1 overflow-y-auto">
        {sessions.length === 0 ? (
          <div className="flex flex-col items-center justify-center py-20 text-center">
            <div className="flex h-12 w-12 items-center justify-center rounded-full bg-zinc-100 mb-4">
              <MessageSquare className="h-5 w-5 text-zinc-400" />
            </div>
            <h2 className="text-sm font-semibold text-zinc-900 mb-1">No chats yet</h2>
            <p className="text-xs text-zinc-500 mb-4 max-w-xs">
              Start a conversation with one of your agents.
            </p>
            <Button
              size="sm"
              className="gap-1.5 bg-zinc-900 hover:bg-zinc-800 text-white text-xs h-8"
              onClick={() => setNewChatOpen(true)}
            >
              <Plus className="h-3.5 w-3.5" />
              Start a chat
            </Button>
          </div>
        ) : filtered.length === 0 ? (
          <div className="flex flex-col items-center justify-center py-16 text-center">
            <p className="text-sm text-zinc-400">No conversations match your filters.</p>
          </div>
        ) : (
          <div className="divide-y divide-zinc-100">
            {filtered.map(session => (
              <ChatRow
                key={session.id}
                session={session}
                agentName={getAgentName(session.agent_slug)}
                onClick={() => navigate(`/chats/${session.id}`)}
                onDelete={() => deleteSession(session.id)}
              />
            ))}
          </div>
        )}
      </div>

      {/* New Chat Dialog */}
      <Dialog open={newChatOpen} onOpenChange={setNewChatOpen}>
        <DialogContent className="sm:max-w-sm">
          <DialogHeader>
            <DialogTitle>New Chat</DialogTitle>
            <DialogDescription>Choose an agent to start a conversation with.</DialogDescription>
          </DialogHeader>
          {agents.length === 0 ? (
            <p className="text-sm text-zinc-500 py-2">
              No agents available. Create one first.
            </p>
          ) : (
            <Select value={selectedAgent} onValueChange={setSelectedAgent}>
              <SelectTrigger>
                <SelectValue placeholder="Select an agent…" />
              </SelectTrigger>
              <SelectContent>
                {agents.map(a => (
                  <SelectItem key={a.slug} value={a.slug}>
                    {a.name}
                  </SelectItem>
                ))}
              </SelectContent>
            </Select>
          )}
          <DialogFooter>
            <Button variant="outline" size="sm" onClick={() => setNewChatOpen(false)}>
              Cancel
            </Button>
            <Button
              size="sm"
              className="bg-zinc-900 hover:bg-zinc-800 text-white"
              onClick={createChat}
              disabled={!selectedAgent || creating}
            >
              {creating ? 'Creating…' : 'Start Chat'}
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>
    </div>
  )
}

function ChatRow({
  session,
  agentName,
  onClick,
  onDelete,
}: {
  session: ChatSession
  agentName: string
  onClick: () => void
  onDelete: () => void
}) {
  return (
    <div
      className="flex items-center gap-3 px-6 py-3.5 hover:bg-zinc-50 cursor-pointer group transition-colors"
      onClick={onClick}
    >
      <div className="flex h-8 w-8 items-center justify-center rounded-full bg-zinc-100 text-zinc-500 shrink-0">
        <MessageSquare className="h-3.5 w-3.5" />
      </div>
      <div className="flex-1 min-w-0">
        <p className="text-sm font-medium text-zinc-900 truncate">
          {truncate(session.title, 70)}
        </p>
        <div className="flex items-center gap-2 mt-0.5">
          <Badge
            variant="secondary"
            className="text-xs py-0 h-4 bg-zinc-100 text-zinc-600 hover:bg-zinc-100 border-0 font-normal"
          >
            {agentName}
          </Badge>
          <span className="text-xs text-zinc-400">{formatRelativeTime(session.updated_at)}</span>
        </div>
      </div>
      <AlertDialog>
        <AlertDialogTrigger asChild>
          <button
            className="opacity-0 group-hover:opacity-100 h-7 w-7 flex items-center justify-center rounded-md text-zinc-400 hover:text-red-500 hover:bg-red-50 transition-all shrink-0"
            onClick={e => e.stopPropagation()}
          >
            <Trash2 className="h-3.5 w-3.5" />
          </button>
        </AlertDialogTrigger>
        <AlertDialogContent>
          <AlertDialogHeader>
            <AlertDialogTitle>Delete chat?</AlertDialogTitle>
            <AlertDialogDescription>
              This will permanently delete this conversation. This action cannot be undone.
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
  )
}
