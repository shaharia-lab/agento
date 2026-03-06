import { useState, useEffect, useRef } from 'react'
import { chatsApi, agentsApi, settingsApi, claudeSettingsProfilesApi } from '@/lib/api'
import type { Agent, ChatSession, ClaudeSettingsProfile } from '@/types'
import { MODELS } from '@/types'
import { Button } from '@/components/ui/button'
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
import FilesystemBrowserModal from '@/components/FilesystemBrowserModal'
import { Send, FolderOpen, Lock } from 'lucide-react'

interface NewChatDialogProps {
  readonly open: boolean
  readonly onOpenChange: (open: boolean) => void
  /** Called with the newly created session. Caller decides where to navigate. */
  readonly onCreated: (session: ChatSession, firstMessage: string) => void
}

export default function NewChatDialog({ open, onOpenChange, onCreated }: NewChatDialogProps) {
  const [agents, setAgents] = useState<Agent[]>([])
  const [profiles, setProfiles] = useState<ClaudeSettingsProfile[]>([])
  const [selectedProfileId, setSelectedProfileId] = useState<string>('')
  const [selectedAgent, setSelectedAgent] = useState<string>('__none__')
  const [firstMessage, setFirstMessage] = useState('')
  const [creating, setCreating] = useState(false)
  const [workingDir, setWorkingDir] = useState('')
  const [selectedModel, setSelectedModel] = useState('')
  const [browserOpen, setBrowserOpen] = useState(false)
  const [error, setError] = useState<string | null>(null)
  const textareaRef = useRef<HTMLTextAreaElement>(null)

  useEffect(() => {
    if (!open) return
    let cancelled = false
    Promise.all([
      agentsApi.list(),
      settingsApi.get(),
      claudeSettingsProfilesApi.list().catch(() => [] as ClaudeSettingsProfile[]),
    ])
      .then(([a, settings, profileList]) => {
        if (cancelled) return
        setAgents(a)
        setWorkingDir(settings.settings.default_working_dir)
        setSelectedModel(settings.settings.default_model)
        setProfiles(profileList)
        const defaultProfile = profileList.find(p => p.is_default) ?? profileList[0]
        setSelectedProfileId(defaultProfile?.id ?? '')
      })
      .catch(() => {
        // silently ignore load errors
      })
    return () => {
      cancelled = true
    }
  }, [open])

  // Focus textarea when modal opens
  useEffect(() => {
    if (open) {
      setTimeout(() => textareaRef.current?.focus(), 50)
    }
  }, [open])

  const handleOpenChange = (nextOpen: boolean) => {
    onOpenChange(nextOpen)
    if (nextOpen) {
      const defaultProfile = profiles.find(p => p.is_default) ?? profiles[0]
      setSelectedProfileId(defaultProfile?.id ?? '')
    } else {
      setSelectedAgent('__none__')
      setFirstMessage('')
      setError(null)
    }
  }

  const selectedAgentObj = agents.find(a => a.slug === selectedAgent)
  const agentModelLocked = !!selectedAgentObj?.model
  const effectiveModel = agentModelLocked ? (selectedAgentObj?.model ?? '') : selectedModel

  const createChat = async () => {
    if (!firstMessage.trim() || creating) return
    setCreating(true)

    const agentSlug = selectedAgent === '__none__' ? '' : selectedAgent

    try {
      const session = await chatsApi.create(
        agentSlug,
        workingDir,
        effectiveModel,
        selectedProfileId,
      )
      handleOpenChange(false)
      setCreating(false)
      onCreated(session, firstMessage.trim())
    } catch (err) {
      setError(err instanceof Error ? err.message : 'Failed to create chat')
      setCreating(false)
    }
  }

  const handleModalKeyDown = (e: React.KeyboardEvent<HTMLTextAreaElement>) => {
    if (e.key === 'Enter' && !e.shiftKey) {
      e.preventDefault()
      createChat()
    }
  }

  return (
    <>
      <Dialog open={open} onOpenChange={handleOpenChange}>
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
                  placeholder="Default working directory"
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
                  className="font-mono text-xs h-8 bg-zinc-50 dark:bg-zinc-800"
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
                        {p.name}
                        {p.is_default ? ' (default)' : ''}
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
                placeholder="Type your first message... (Enter to send)"
                className="min-h-[100px] max-h-[220px] resize-none text-sm border-zinc-200 focus:border-zinc-900 focus:ring-zinc-900 pr-10"
                rows={4}
                disabled={creating}
              />
              <button
                onClick={() => createChat()}
                disabled={!firstMessage.trim() || creating}
                className="absolute right-2.5 bottom-2.5 h-7 w-7 flex items-center justify-center rounded-md transition-colors disabled:opacity-40 disabled:cursor-not-allowed bg-zinc-900 dark:bg-zinc-100 text-white dark:text-zinc-900 hover:bg-zinc-700 dark:hover:bg-zinc-300"
              >
                <Send className="h-3.5 w-3.5" />
              </button>
            </div>

            {error && <p className="text-xs text-red-600">{error}</p>}
          </div>

          <DialogFooter className="gap-2">
            <Button variant="outline" size="sm" onClick={() => handleOpenChange(false)}>
              Cancel
            </Button>
            <Button
              size="sm"
              className="bg-zinc-900 hover:bg-zinc-800 text-white"
              onClick={() => createChat()}
              disabled={!firstMessage.trim() || creating}
            >
              {creating ? 'Starting...' : 'Start Chat'}
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>

      <FilesystemBrowserModal
        open={browserOpen}
        onOpenChange={setBrowserOpen}
        initialPath={workingDir}
        onSelect={path => setWorkingDir(path)}
      />
    </>
  )
}
