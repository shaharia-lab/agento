import { useState, useEffect, useRef, useCallback } from 'react'
import { useNavigate } from 'react-router-dom'
import { agentsApi, integrationsApi } from '@/lib/api'
import type { Agent, AvailableTool } from '@/types'
import { MODELS, BUILT_IN_TOOLS } from '@/types'
import { Button } from '@/components/ui/button'
import { Input } from '@/components/ui/input'
import { Label } from '@/components/ui/label'
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from '@/components/ui/select'
import { ChevronDown, ChevronRight } from 'lucide-react'

interface AgentFormProps {
  readonly agent?: Agent
  readonly isEdit?: boolean
}

function toSlug(name: string): string {
  return name
    .toLowerCase()
    .replaceAll(/[^a-z0-9]+/g, '-')
    .replaceAll(/^-+|-+$/g, '')
}

function CollapsibleSection({
  title,
  defaultOpen = false,
  children,
}: Readonly<{
  title: string
  defaultOpen?: boolean
  children: React.ReactNode
}>) {
  const [open, setOpen] = useState(defaultOpen)
  return (
    <div className="border border-border rounded-lg">
      <button
        type="button"
        onClick={() => setOpen(o => !o)}
        className="flex items-center justify-between w-full px-4 py-3 text-left"
      >
        <span className="text-sm font-medium">{title}</span>
        {open ? (
          <ChevronDown className="h-4 w-4 text-muted-foreground shrink-0" />
        ) : (
          <ChevronRight className="h-4 w-4 text-muted-foreground shrink-0" />
        )}
      </button>
      {open && <div className="px-4 pb-4 space-y-4">{children}</div>}
    </div>
  )
}

function LineNumberGutter({ value }: Readonly<{ value: string }>) {
  const lineCount = value.split('\n').length
  return (
    <div
      className="select-none text-right pr-3 pt-[9px] pb-[9px] text-xs font-mono text-muted-foreground/50 leading-[1.625] shrink-0"
      aria-hidden
    >
      {Array.from({ length: lineCount }, (_, i) => (
        <div key={i}>{i + 1}</div>
      ))}
    </div>
  )
}

function renderServiceTools(
  service: string,
  tools: AvailableTool[],
  integrationId: string,
  mcpTools: Record<string, string[]>,
  toggleMcpTool: (integrationId: string, toolName: string) => void,
) {
  return (
    <div key={service}>
      <p className="text-xs text-zinc-400 capitalize mb-1.5">{service}</p>
      <div className="grid grid-cols-2 gap-1.5">
        {tools.map(tool => {
          const selected = (mcpTools[integrationId] ?? []).includes(tool.tool_name)
          return (
            <label
              key={tool.tool_name}
              className="flex items-center gap-2 rounded-md border border-border px-2.5 py-1.5 text-sm cursor-pointer hover:bg-muted/50 transition-colors"
            >
              <input
                type="checkbox"
                checked={selected}
                onChange={() => toggleMcpTool(integrationId, tool.tool_name)}
                className="h-3.5 w-3.5 rounded border-gray-300"
              />
              <span className="font-mono text-xs">{tool.tool_name}</span>
            </label>
          )
        })}
      </div>
    </div>
  )
}

type MobileTab = 'prompt' | 'config'

export default function AgentForm({ agent, isEdit = false }: AgentFormProps) {
  const navigate = useNavigate()
  const [saving, setSaving] = useState(false)
  const [error, setError] = useState<string | null>(null)
  const [mobileTab, setMobileTab] = useState<MobileTab>('prompt')

  const [name, setName] = useState(agent?.name ?? '')
  const [slug, setSlug] = useState(agent?.slug ?? '')
  const [slugTouched, setSlugTouched] = useState(isEdit)
  const [description, setDescription] = useState(agent?.description ?? '')
  const [model, setModel] = useState(agent?.model ?? '')
  const [thinking, setThinking] = useState<Agent['thinking']>(agent?.thinking ?? 'adaptive')
  const [permissionMode, setPermissionMode] = useState<Agent['permission_mode']>(
    agent?.permission_mode ?? 'default',
  )
  const [systemPrompt, setSystemPrompt] = useState(agent?.system_prompt ?? '')
  const [builtInTools, setBuiltInTools] = useState<string[]>(agent?.capabilities?.built_in ?? [])

  const [mcpTools, setMcpTools] = useState<Record<string, string[]>>(() => {
    const mcp = agent?.capabilities?.mcp ?? {}
    return Object.fromEntries(Object.entries(mcp).map(([id, v]) => [id, v.tools]))
  })
  const [availableTools, setAvailableTools] = useState<AvailableTool[]>([])

  const textareaRef = useRef<HTMLTextAreaElement>(null)
  const gutterRef = useRef<HTMLDivElement>(null)

  // Sync gutter scroll with textarea
  const handleTextareaScroll = useCallback(() => {
    if (textareaRef.current && gutterRef.current) {
      gutterRef.current.scrollTop = textareaRef.current.scrollTop
    }
  }, [])

  useEffect(() => {
    if (!slugTouched && name) {
      setSlug(toSlug(name))
    }
  }, [name, slugTouched])

  useEffect(() => {
    integrationsApi
      .availableTools()
      .then(setAvailableTools)
      .catch(() => {})
  }, [])

  const toggleTool = (tool: string) => {
    setBuiltInTools(prev => (prev.includes(tool) ? prev.filter(t => t !== tool) : [...prev, tool]))
  }

  const toggleMcpTool = (integrationId: string, toolName: string) => {
    setMcpTools(prev => {
      const current = prev[integrationId] ?? []
      const next = current.includes(toolName)
        ? current.filter(t => t !== toolName)
        : [...current, toolName]
      return { ...prev, [integrationId]: next }
    })
  }

  const toolsByIntegration = availableTools.reduce<
    Record<string, { name: string; byService: Record<string, AvailableTool[]> }>
  >((acc, tool) => {
    if (!acc[tool.integration_id]) {
      acc[tool.integration_id] = { name: tool.integration_name, byService: {} }
    }
    if (!acc[tool.integration_id].byService[tool.service]) {
      acc[tool.integration_id].byService[tool.service] = []
    }
    acc[tool.integration_id].byService[tool.service].push(tool)
    return acc
  }, {})

  const handleSubmit = async (e: React.FormEvent) => {
    e.preventDefault()
    setError(null)
    setSaving(true)
    try {
      const mcp: Record<string, { tools: string[] }> = {}
      for (const [id, tools] of Object.entries(mcpTools)) {
        if (tools.length > 0) mcp[id] = { tools }
      }
      const payload: Partial<Agent> = {
        name,
        slug,
        description,
        model,
        thinking,
        permission_mode: permissionMode,
        system_prompt: systemPrompt,
        capabilities: {
          built_in: builtInTools,
          ...(Object.keys(mcp).length > 0 ? { mcp } : {}),
        },
      }
      if (isEdit && agent) {
        await agentsApi.update(agent.slug, payload)
      } else {
        await agentsApi.create(payload)
      }
      navigate('/agents')
    } catch (err) {
      setError(err instanceof Error ? err.message : 'Failed to save agent')
    } finally {
      setSaving(false)
    }
  }

  const hasIntegrationTools = Object.keys(toolsByIntegration).length > 0

  // --- Shared sub-components for reuse in both layouts ---

  const systemPromptEditor = (
    <div className="flex flex-col flex-1 min-h-0">
      <Label htmlFor="system_prompt" className="mb-1.5 shrink-0">
        System Prompt
      </Label>
      <div className="flex flex-1 min-h-[400px] rounded-md border border-input bg-background overflow-hidden">
        <div
          ref={gutterRef}
          className="overflow-hidden shrink-0 bg-muted/30 border-r border-border"
        >
          <LineNumberGutter value={systemPrompt} />
        </div>
        <textarea
          ref={textareaRef}
          id="system_prompt"
          value={systemPrompt}
          onChange={e => setSystemPrompt(e.target.value)}
          onScroll={handleTextareaScroll}
          placeholder="You are a helpful assistant. Today is {{current_date}}."
          className="flex-1 resize-none bg-transparent px-3 py-2 text-sm font-mono leading-[1.625] placeholder:text-muted-foreground focus-visible:outline-none"
        />
      </div>
      <p className="text-xs text-muted-foreground mt-1.5 shrink-0">
        Use <code className="rounded bg-muted px-1 py-0.5">{'{{current_date}}'}</code> and{' '}
        <code className="rounded bg-muted px-1 py-0.5">{'{{current_time}}'}</code> as dynamic
        placeholders.
      </p>
    </div>
  )

  const submitLabel = isEdit ? 'Update Agent' : 'Create Agent'

  const configPanel = (
    <div className="space-y-3">
      {/* Basic Info — expanded by default */}
      <CollapsibleSection title="Basic Info" defaultOpen>
        <div className="space-y-1.5">
          <Label htmlFor="name">Name *</Label>
          <Input
            id="name"
            value={name}
            onChange={e => setName(e.target.value)}
            placeholder="My Helpful Agent"
            required
          />
        </div>
        <div className="space-y-1.5">
          <Label htmlFor="slug">Slug *</Label>
          <Input
            id="slug"
            value={slug}
            onChange={e => {
              setSlug(e.target.value)
              setSlugTouched(true)
            }}
            placeholder="my-helpful-agent"
            pattern="[a-z0-9]+(?:-[a-z0-9]+)*"
            title="Lowercase letters, numbers and hyphens only"
            disabled={isEdit}
            required
          />
          {isEdit && (
            <p className="text-xs text-muted-foreground">Slug cannot be changed after creation.</p>
          )}
        </div>
        <div className="space-y-1.5">
          <Label htmlFor="description">Description</Label>
          <Input
            id="description"
            value={description}
            onChange={e => setDescription(e.target.value)}
            placeholder="What does this agent do?"
          />
        </div>
      </CollapsibleSection>

      {/* Model & Behavior — collapsed */}
      <CollapsibleSection title="Model & Behavior">
        <div className="space-y-1.5">
          <Label>Model</Label>
          <Select
            value={model || '__default__'}
            onValueChange={v => setModel(v === '__default__' ? '' : v)}
          >
            <SelectTrigger>
              <SelectValue />
            </SelectTrigger>
            <SelectContent>
              <SelectItem value="__default__">Default (from settings)</SelectItem>
              {MODELS.map(m => (
                <SelectItem key={m.value} value={m.value}>
                  {m.label}
                </SelectItem>
              ))}
            </SelectContent>
          </Select>
        </div>
        <div className="space-y-1.5">
          <Label>Thinking Mode</Label>
          <Select value={thinking} onValueChange={v => setThinking(v as Agent['thinking'])}>
            <SelectTrigger>
              <SelectValue />
            </SelectTrigger>
            <SelectContent>
              <SelectItem value="adaptive">Adaptive (recommended)</SelectItem>
              <SelectItem value="enabled">Always enabled</SelectItem>
              <SelectItem value="disabled">Disabled</SelectItem>
            </SelectContent>
          </Select>
          <p className="text-xs text-muted-foreground">
            Adaptive lets Claude decide when extended thinking is helpful.
          </p>
        </div>
        <div className="space-y-1.5">
          <Label>Permission Mode</Label>
          <Select
            value={permissionMode}
            onValueChange={v => setPermissionMode(v as Agent['permission_mode'])}
          >
            <SelectTrigger>
              <SelectValue />
            </SelectTrigger>
            <SelectContent>
              <SelectItem value="default">Default — respect Claude Code settings</SelectItem>
              <SelectItem value="bypass">Bypass — auto-approve all tool calls</SelectItem>
            </SelectContent>
          </Select>
          <p className="text-xs text-muted-foreground">
            <strong>Default</strong> respects your Claude Code permission rules.{' '}
            <strong>Bypass</strong> skips all permission checks and auto-approves every tool call.
          </p>
        </div>
      </CollapsibleSection>

      {/* Built-in Tools — collapsed */}
      <CollapsibleSection title="Built-in Tools">
        <div className="grid grid-cols-2 gap-2">
          {BUILT_IN_TOOLS.map(tool => (
            <label
              key={tool}
              className="flex items-center gap-2 rounded-md border border-border px-3 py-2 text-sm cursor-pointer hover:bg-muted/50 transition-colors"
            >
              <input
                type="checkbox"
                checked={builtInTools.includes(tool)}
                onChange={() => toggleTool(tool)}
                className="h-3.5 w-3.5 rounded border-gray-300"
              />
              <span className="font-mono text-xs">{tool}</span>
            </label>
          ))}
        </div>
        <p className="text-xs text-muted-foreground">
          Leave all unchecked to allow all built-in tools.
        </p>
      </CollapsibleSection>

      {/* Integration Tools — collapsed, conditional */}
      {hasIntegrationTools && (
        <CollapsibleSection title="Integration Tools">
          <p className="text-xs text-muted-foreground">
            Tools from your connected integrations. Selected tools will be available to this agent.
          </p>
          <div className="space-y-3">
            {Object.entries(toolsByIntegration).map(
              ([integrationId, { name: integName, byService }]) => (
                <div key={integrationId} className="rounded-lg border border-border p-3">
                  <p className="text-xs font-semibold text-zinc-500 uppercase tracking-wider mb-2">
                    {integName}
                  </p>
                  <div className="space-y-2">
                    {Object.entries(byService).map(([service, tools]) =>
                      renderServiceTools(service, tools, integrationId, mcpTools, toggleMcpTool),
                    )}
                  </div>
                </div>
              ),
            )}
          </div>
        </CollapsibleSection>
      )}

      {/* Actions */}
      <div className="flex items-center gap-3 pt-2">
        <Button type="submit" disabled={saving}>
          {saving ? 'Saving...' : submitLabel}
        </Button>
        <Button type="button" variant="outline" onClick={() => navigate('/agents')}>
          Cancel
        </Button>
      </div>
    </div>
  )

  return (
    <form onSubmit={handleSubmit} className="flex flex-col flex-1 min-h-0">
      {error && (
        <div className="rounded-md bg-red-50 border border-red-200 px-4 py-3 text-sm text-red-700 mb-4 shrink-0">
          {error}
        </div>
      )}

      {/* Desktop: two-column layout (lg+) */}
      <div className="hidden lg:grid lg:grid-cols-[3fr_2fr] lg:gap-6 flex-1 min-h-0">
        {/* Left column: system prompt */}
        <div className="flex flex-col min-h-0">{systemPromptEditor}</div>

        {/* Right column: config sections, scrollable */}
        <div className="overflow-y-auto min-h-0 pr-1">{configPanel}</div>
      </div>

      {/* Mobile/Tablet: tabbed layout (<lg) */}
      <div className="flex flex-col flex-1 min-h-0 lg:hidden">
        {/* Tab bar */}
        <div className="flex border-b border-border mb-4 shrink-0">
          <button
            type="button"
            className={`px-4 py-2 text-sm font-medium border-b-2 transition-colors ${
              mobileTab === 'prompt'
                ? 'border-foreground text-foreground'
                : 'border-transparent text-muted-foreground hover:text-foreground'
            }`}
            onClick={() => setMobileTab('prompt')}
          >
            System Prompt
          </button>
          <button
            type="button"
            className={`px-4 py-2 text-sm font-medium border-b-2 transition-colors ${
              mobileTab === 'config'
                ? 'border-foreground text-foreground'
                : 'border-transparent text-muted-foreground hover:text-foreground'
            }`}
            onClick={() => setMobileTab('config')}
          >
            Configuration
          </button>
        </div>

        {/* Tab content */}
        <div className="flex-1 min-h-0 overflow-y-auto">
          {mobileTab === 'prompt' && systemPromptEditor}
          {mobileTab === 'config' && configPanel}
        </div>
      </div>
    </form>
  )
}
