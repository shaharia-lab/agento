import { useState, useEffect } from 'react'
import { useNavigate } from 'react-router-dom'
import { agentsApi, integrationsApi } from '@/lib/api'
import type { Agent, AvailableTool } from '@/types'
import { MODELS, BUILT_IN_TOOLS } from '@/types'
import { Button } from '@/components/ui/button'
import { Input } from '@/components/ui/input'
import { Textarea } from '@/components/ui/textarea'
import { Label } from '@/components/ui/label'
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from '@/components/ui/select'

interface AgentFormProps {
  agent?: Agent
  isEdit?: boolean
}

function toSlug(name: string): string {
  return name
    .toLowerCase()
    .replace(/[^a-z0-9]+/g, '-')
    .replace(/^-+|-+$/g, '')
}

export default function AgentForm({ agent, isEdit = false }: AgentFormProps) {
  const navigate = useNavigate()
  const [saving, setSaving] = useState(false)
  const [error, setError] = useState<string | null>(null)

  const [name, setName] = useState(agent?.name ?? '')
  const [slug, setSlug] = useState(agent?.slug ?? '')
  const [slugTouched, setSlugTouched] = useState(isEdit)
  const [description, setDescription] = useState(agent?.description ?? '')
  const [model, setModel] = useState(agent?.model ?? 'eu.anthropic.claude-sonnet-4-5-20250929-v1:0')
  const [thinking, setThinking] = useState<Agent['thinking']>(agent?.thinking ?? 'adaptive')
  const [permissionMode, setPermissionMode] = useState<Agent['permission_mode']>(
    agent?.permission_mode ?? 'default',
  )
  const [systemPrompt, setSystemPrompt] = useState(agent?.system_prompt ?? '')
  const [builtInTools, setBuiltInTools] = useState<string[]>(agent?.capabilities?.built_in ?? [])

  // Integration tools: { [integration_id]: string[] }
  const [mcpTools, setMcpTools] = useState<Record<string, string[]>>(() => {
    const mcp = agent?.capabilities?.mcp ?? {}
    return Object.fromEntries(Object.entries(mcp).map(([id, v]) => [id, v.tools]))
  })
  const [availableTools, setAvailableTools] = useState<AvailableTool[]>([])

  // Auto-generate slug from name
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

  // Group availableTools by integration_id → service
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
      // Build mcp capabilities — only include integrations with at least one tool selected
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

  return (
    <form onSubmit={handleSubmit} className="space-y-6 max-w-2xl">
      {error && (
        <div className="rounded-md bg-red-50 border border-red-200 px-4 py-3 text-sm text-red-700">
          {error}
        </div>
      )}

      {/* Name */}
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

      {/* Slug */}
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

      {/* Description */}
      <div className="space-y-1.5">
        <Label htmlFor="description">Description</Label>
        <Input
          id="description"
          value={description}
          onChange={e => setDescription(e.target.value)}
          placeholder="What does this agent do?"
        />
      </div>

      {/* Model */}
      <div className="space-y-1.5">
        <Label>Model</Label>
        <Select value={model} onValueChange={setModel}>
          <SelectTrigger>
            <SelectValue />
          </SelectTrigger>
          <SelectContent>
            {MODELS.map(m => (
              <SelectItem key={m.value} value={m.value}>
                {m.label}
              </SelectItem>
            ))}
          </SelectContent>
        </Select>
      </div>

      {/* Thinking */}
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

      {/* Permission Mode */}
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

      {/* System Prompt */}
      <div className="space-y-1.5">
        <Label htmlFor="system_prompt">System Prompt</Label>
        <Textarea
          id="system_prompt"
          value={systemPrompt}
          onChange={e => setSystemPrompt(e.target.value)}
          placeholder="You are a helpful assistant. Today is {{current_date}}."
          className="min-h-[120px] font-mono text-xs"
        />
        <p className="text-xs text-muted-foreground">
          Use <code className="rounded bg-muted px-1 py-0.5">{'{{current_date}}'}</code> and{' '}
          <code className="rounded bg-muted px-1 py-0.5">{'{{current_time}}'}</code> as dynamic
          placeholders.
        </p>
      </div>

      {/* Built-in Tools */}
      <div className="space-y-2">
        <Label>Built-in Tools</Label>
        <div className="grid grid-cols-2 sm:grid-cols-3 gap-2">
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
      </div>

      {/* Integration Tools */}
      {Object.keys(toolsByIntegration).length > 0 && (
        <div className="space-y-2">
          <Label>Integration Tools</Label>
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
                    {Object.entries(byService).map(([service, tools]) => (
                      <div key={service}>
                        <p className="text-xs text-zinc-400 capitalize mb-1.5">{service}</p>
                        <div className="grid grid-cols-2 sm:grid-cols-3 gap-1.5">
                          {tools.map(tool => {
                            const selected = (mcpTools[integrationId] ?? []).includes(
                              tool.tool_name,
                            )
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
                    ))}
                  </div>
                </div>
              ),
            )}
          </div>
        </div>
      )}

      {/* Actions */}
      <div className="flex items-center gap-3 pt-2">
        <Button type="submit" disabled={saving}>
          {saving ? 'Saving…' : isEdit ? 'Update Agent' : 'Create Agent'}
        </Button>
        <Button type="button" variant="outline" onClick={() => navigate('/agents')}>
          Cancel
        </Button>
      </div>
    </form>
  )
}
