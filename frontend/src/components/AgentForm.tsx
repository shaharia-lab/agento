import { useState, useEffect } from 'react'
import { useNavigate } from 'react-router-dom'
import { agentsApi } from '@/lib/api'
import type { Agent } from '@/types'
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
  const [systemPrompt, setSystemPrompt] = useState(agent?.system_prompt ?? '')
  const [builtInTools, setBuiltInTools] = useState<string[]>(
    agent?.capabilities?.built_in ?? [],
  )

  // Auto-generate slug from name
  useEffect(() => {
    if (!slugTouched && name) {
      setSlug(toSlug(name))
    }
  }, [name, slugTouched])

  const toggleTool = (tool: string) => {
    setBuiltInTools(prev =>
      prev.includes(tool) ? prev.filter(t => t !== tool) : [...prev, tool],
    )
  }

  const handleSubmit = async (e: React.FormEvent) => {
    e.preventDefault()
    setError(null)
    setSaving(true)
    try {
      const payload: Partial<Agent> = {
        name,
        slug,
        description,
        model,
        thinking,
        system_prompt: systemPrompt,
        capabilities: { built_in: builtInTools },
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
        <div className="grid grid-cols-3 gap-2">
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
