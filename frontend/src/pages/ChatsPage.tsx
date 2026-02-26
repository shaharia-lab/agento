import { useState, useEffect, useCallback, useMemo, useRef } from 'react'
import { useNavigate, useSearchParams } from 'react-router-dom'
import { chatsApi, agentsApi, settingsApi, claudeSettingsProfilesApi } from '@/lib/api'
import type { ChatSession, Agent, ClaudeSettingsProfile } from '@/types'
import { MODELS } from '@/types'
import { formatRelativeTime, truncate } from '@/lib/utils'
import { Button } from '@/components/ui/button'
import { Badge } from '@/components/ui/badge'
import { Textarea } from '@/components/ui/textarea'
import { Input } from '@/components/ui/input'
import { Label } from '@/components/ui/label'
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
import FilesystemBrowserModal from '@/components/FilesystemBrowserModal'
import { Plus, MessageSquare, Trash2, Search, Send, FolderOpen, Lock } from 'lucide-react'

export default function ChatsPage() {
  const navigate = useNavigate()
  const [searchParams, setSearchParams] = useSearchParams()
  const [sessions, setSessions] = useState<ChatSession[]>([])
  const [agents, setAgents] = useState<Agent[]>([])
  const [profiles, setProfiles] = useState<ClaudeSettingsProfile[]>([])
  const [selectedProfileId, setSelectedProfileId] = useState<string>('')
  const [loading, setLoading] = useState(true)
  const [error, setError] = useState<string | null>(null)
  const [newChatOpen, setNewChatOpen] = useState(false)
  const [selectedAgent, setSelectedAgent] = useState<string>('__none__')
  const [firstMessage, setFirstMessage] = useState('')
  const [creating, setCreating] = useState(false)
  const [workingDir, setWorkingDir] = useState('/tmp/agento/work')
  const [selectedModel, setSelectedModel] = useState('')
  const [browserOpen, setBrowserOpen] = useState(false)
  const textareaRef = useRef<HTMLTextAreaElement>(null)

  // Filters
  const [search, setSearch] = useState('')
  const [filterAgent, setFilterAgent] = useState('all')

  const load = useCallback(async () => {
    try {
      const [s, a, settings, profileList] = await Promise.all([
        chatsApi.list(),
        agentsApi.list(),
        settingsApi.get(),
        claudeSettingsProfilesApi.list().catch(() => [] as ClaudeSettingsProfile[]),
      ])
      setSessions(s)
      setAgents(a)
      setWorkingDir(settings.settings.default_working_dir)
      setSelectedModel(settings.settings.default_model)
      setProfiles(profileList)
      const defaultProfile = profileList.find(p => p.is_default) ?? profileList[0]
      setSelectedProfileId(defaultProfile?.id ?? '')
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

  // Focus textarea when modal opens
  useEffect(() => {
    if (newChatOpen) {
      setTimeout(() => textareaRef.current?.focus(), 50)
    }
  }, [newChatOpen])

  const handleOpenChange = (open: boolean) => {
    setNewChatOpen(open)
    if (open) {
      // Reset profile to default each time the dialog opens.
      const defaultProfile = profiles.find(p => p.is_default) ?? profiles[0]
      setSelectedProfileId(defaultProfile?.id ?? '')
    } else {
      setSelectedAgent('__none__')
      setFirstMessage('')
    }
  }

  // When agent is selected, determine if model should be locked to agent's model.
  const selectedAgentObj = agents.find(a => a.slug === selectedAgent)
  const agentModelLocked = selectedAgentObj?.model ? true : false
  const effectiveModel = agentModelLocked ? (selectedAgentObj?.model ?? '') : selectedModel

  const createChat = async () => {
    if (!firstMessage.trim() || creating) return
    setCreating(true)

    const agentSlug = selectedAgent === '__none__' ? '' : selectedAgent

    try {
      const session = await chatsApi.create(agentSlug, workingDir, effectiveModel, selectedProfileId)
      setNewChatOpen(false)

      // Navigate immediately; send the first message in the background so the
      // chat page can display the streaming response right away.
      navigate(`/chats/${session.id}`, {
        state: {
          pendingMessage: firstMessage.trim(),
          workingDir: session.working_directory,
          model: session.model,
        },
      })
    } catch (err) {
      setError(err instanceof Error ? err.message : 'Failed to create chat')
      setCreating(false)
    }
  }

  const handleModalKeyDown = (e: React.KeyboardEvent<HTMLTextAreaElement>) => {
    if (e.key === 'Enter' && !e.shiftKey) {
      e.preventDefault()
      void createChat()
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

  const getAgentName = (slug: string) => agents.find(a => a.slug === slug)?.name ?? slug

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
      <div className="flex items-center justify-between border-b border-zinc-100 px-4 sm:px-6 py-4 shrink-0">
        <div>
          <h1 className="text-base font-semibold text-zinc-900">Chats</h1>
          <p className="text-xs text-zinc-500 mt-0.5">
            {sessions.length} conversation{sessions.length !== 1 ? 's' : ''}
          </p>
        </div>
        <Button
          size="sm"
          className="gap-1.5 bg-zinc-900 hover:bg-zinc-800 text-white text-xs h-8 cursor-pointer"
          onClick={() => setNewChatOpen(true)}
        >
          <Plus className="h-3.5 w-3.5" />
          New Chat
        </Button>
      </div>

      {/* Filters */}
      {sessions.length > 0 && (
        <div className="flex flex-col sm:flex-row items-stretch sm:items-center gap-2 sm:gap-3 px-4 sm:px-6 py-3 border-b border-zinc-100 shrink-0">
          <div className="relative flex-1 sm:max-w-xs">
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
              <SelectTrigger className="w-full sm:w-40 h-8 text-xs">
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
              Start a conversation — with or without an agent.
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
                agentName={session.agent_slug ? getAgentName(session.agent_slug) : null}
                onClick={() => navigate(`/chats/${session.id}`)}
                onDelete={() => deleteSession(session.id)}
              />
            ))}
          </div>
        )}
      </div>

      {/* New Chat Dialog */}
      <Dialog open={newChatOpen} onOpenChange={handleOpenChange}>
        <DialogContent className="sm:max-w-md">
          <DialogHeader>
            <DialogTitle>New Chat</DialogTitle>
            <DialogDescription>
              Type your first message. Optionally choose an agent — or chat directly without one.
            </DialogDescription>
          </DialogHeader>

          <div className="flex flex-col gap-3 py-1">
            {/* Agent selector (optional) */}
            <Select value={selectedAgent} onValueChange={setSelectedAgent}>
              <SelectTrigger className="h-9 text-sm">
                <SelectValue placeholder="No agent (direct chat)" />
              </SelectTrigger>
              <SelectContent>
                <SelectItem value="__none__">No agent (direct chat)</SelectItem>
                {agents.map(a => (
                  <SelectItem key={a.slug} value={a.slug}>
                    {a.name}
                  </SelectItem>
                ))}
              </SelectContent>
            </Select>

            {/* Working Directory */}
            <div className="flex flex-col gap-1">
              <Label className="text-xs text-zinc-600">Working Directory</Label>
              <div className="flex gap-2">
                <Input
                  value={workingDir}
                  onChange={e => setWorkingDir(e.target.value)}
                  className="flex-1 font-mono text-xs h-8"
                  placeholder="/tmp/agento/work"
                />
                <Button
                  variant="outline"
                  size="sm"
                  className="h-8 gap-1 text-xs shrink-0"
                  onClick={() => setBrowserOpen(true)}
                  type="button"
                >
                  <FolderOpen className="h-3 w-3" />
                  Browse
                </Button>
              </div>
            </div>

            {/* Model selector */}
            <div className="flex flex-col gap-1">
              <Label className="text-xs text-zinc-600 flex items-center gap-1">
                Model
                {agentModelLocked && <Lock className="h-3 w-3 text-zinc-400" />}
              </Label>
              {agentModelLocked ? (
                <Input
                  value={effectiveModel}
                  disabled
                  className="font-mono text-xs h-8 bg-zinc-50"
                />
              ) : (
                <Select value={selectedModel} onValueChange={setSelectedModel}>
                  <SelectTrigger className="h-8 text-xs">
                    <SelectValue placeholder="Select model" />
                  </SelectTrigger>
                  <SelectContent>
                    {MODELS.map(m => (
                      <SelectItem key={m.value} value={m.value} className="text-xs">
                        {m.label}
                      </SelectItem>
                    ))}
                  </SelectContent>
                </Select>
              )}
              {agentModelLocked && (
                <p className="text-xs text-zinc-400">Model set by agent configuration</p>
              )}
            </div>

            {/* Settings profile selector */}
            {profiles.length > 0 && (
              <div className="flex flex-col gap-1">
                <Label className="text-xs text-zinc-600">Claude Settings Profile</Label>
                <Select value={selectedProfileId} onValueChange={setSelectedProfileId}>
                  <SelectTrigger className="h-8 text-xs">
                    <SelectValue placeholder="Default profile" />
                  </SelectTrigger>
                  <SelectContent>
                    {profiles.map(p => (
                      <SelectItem key={p.id} value={p.id} className="text-xs">
                        {p.name}{p.is_default ? ' (default)' : ''}
                      </SelectItem>
                    ))}
                  </SelectContent>
                </Select>
              </div>
            )}

            {/* First message input */}
            <div className="relative">
              <Textarea
                ref={textareaRef}
                value={firstMessage}
                onChange={e => setFirstMessage(e.target.value)}
                onKeyDown={handleModalKeyDown}
                placeholder="Type your first message… (Enter to send)"
                className="min-h-[100px] max-h-[220px] resize-none text-sm border-zinc-200 focus:border-zinc-900 focus:ring-zinc-900 pr-10"
                rows={4}
                disabled={creating}
              />
              <button
                onClick={() => void createChat()}
                disabled={!firstMessage.trim() || creating}
                className="absolute right-2.5 bottom-2.5 h-7 w-7 flex items-center justify-center rounded-md transition-colors disabled:opacity-40 disabled:cursor-not-allowed bg-zinc-900 text-white hover:bg-zinc-700"
              >
                <Send className="h-3.5 w-3.5" />
              </button>
            </div>
          </div>

          <DialogFooter className="gap-2">
            <Button variant="outline" size="sm" onClick={() => handleOpenChange(false)}>
              Cancel
            </Button>
            <Button
              size="sm"
              className="bg-zinc-900 hover:bg-zinc-800 text-white"
              onClick={() => void createChat()}
              disabled={!firstMessage.trim() || creating}
            >
              {creating ? 'Starting…' : 'Start Chat'}
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>

      {/* Hidden component to send the first message after navigation */}
      {/* Actual sending is handled by ChatSessionPage via location.state */}

      <FilesystemBrowserModal
        open={browserOpen}
        onOpenChange={setBrowserOpen}
        initialPath={workingDir}
        onSelect={path => setWorkingDir(path)}
      />
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
  agentName: string | null
  onClick: () => void
  onDelete: () => void
}) {
  return (
    <div
      className="flex items-center gap-3 px-4 sm:px-6 py-3.5 hover:bg-zinc-50 cursor-pointer group transition-colors"
      onClick={onClick}
    >
      <div className="flex h-8 w-8 items-center justify-center rounded-full bg-zinc-100 text-zinc-500 shrink-0">
        <MessageSquare className="h-3.5 w-3.5" />
      </div>
      <div className="flex-1 min-w-0">
        <p className="text-sm font-medium text-zinc-900 truncate">{truncate(session.title, 70)}</p>
        <div className="flex items-center gap-2 mt-0.5">
          {agentName ? (
            <Badge
              variant="secondary"
              className="text-xs py-0 h-4 bg-zinc-100 text-zinc-600 hover:bg-zinc-100 border-0 font-normal"
            >
              {agentName}
            </Badge>
          ) : (
            <Badge
              variant="secondary"
              className="text-xs py-0 h-4 bg-zinc-50 text-zinc-400 hover:bg-zinc-50 border-0 font-normal"
            >
              Direct chat
            </Badge>
          )}
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
