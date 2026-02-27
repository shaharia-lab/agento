// Shared utilities and sub-components used by both TokenUsagePage and GeneralUsagePage.

import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from '@/components/ui/select'

// ─── Constants ────────────────────────────────────────────────────────────────

export const MODEL_COLORS = [
  '#6366f1', // indigo
  '#22c55e', // green
  '#f59e0b', // amber
  '#ef4444', // red
  '#8b5cf6', // violet
  '#14b8a6', // teal
  '#f97316', // orange
  '#ec4899', // pink
]

export const DAY_NAMES = ['Sun', 'Mon', 'Tue', 'Wed', 'Thu', 'Fri', 'Sat']

export type DatePreset = '7d' | '30d' | '90d' | 'this-month' | 'last-month' | 'all-time' | 'custom'

export const PRESETS: { label: string; value: DatePreset }[] = [
  { label: '7d', value: '7d' },
  { label: '30d', value: '30d' },
  { label: '90d', value: '90d' },
  { label: 'This month', value: 'this-month' },
  { label: 'Last month', value: 'last-month' },
  { label: 'All time', value: 'all-time' },
  { label: 'Custom', value: 'custom' },
]

// ─── Utilities ────────────────────────────────────────────────────────────────

export function fmt(d: Date) {
  return d.toISOString().slice(0, 10)
}

export function subDays(d: Date, n: number): Date {
  const r = new Date(d)
  r.setDate(r.getDate() - n)
  return r
}

export function presetToRange(preset: DatePreset): { from: string; to: string } {
  const today = new Date()
  switch (preset) {
    case '7d':
      return { from: fmt(subDays(today, 7)), to: fmt(today) }
    case '30d':
      return { from: fmt(subDays(today, 30)), to: fmt(today) }
    case '90d':
      return { from: fmt(subDays(today, 90)), to: fmt(today) }
    case 'this-month': {
      const start = new Date(today.getFullYear(), today.getMonth(), 1)
      return { from: fmt(start), to: fmt(today) }
    }
    case 'last-month': {
      const start = new Date(today.getFullYear(), today.getMonth() - 1, 1)
      const end = new Date(today.getFullYear(), today.getMonth(), 0)
      return { from: fmt(start), to: fmt(end) }
    }
    case 'all-time':
      return { from: '2020-01-01', to: fmt(today) }
    default:
      return { from: fmt(subDays(today, 30)), to: fmt(today) }
  }
}

export function formatTokens(n: number): string {
  if (!n) return '0'
  if (n >= 1_000_000) return `${(n / 1_000_000).toFixed(1)}M`
  if (n >= 1_000) return `${(n / 1_000).toFixed(1)}k`
  return String(n)
}

export function formatModelName(model: string): string {
  if (!model || model === 'unknown') return 'Unknown'
  const lower = model.toLowerCase()
  if (lower.includes('opus') || lower.includes('sonnet') || lower.includes('haiku'))
    return model
      .replace(/claude-/i, '')
      .replace(/-/g, ' ')
      .replace(/\b\w/g, c => c.toUpperCase())
  return model
}

export function formatDateLabel(date: string): string {
  if (date.includes('T')) {
    const [d, h] = date.split('T')
    const parsed = new Date(d + 'T00:00:00')
    return `${parsed.toLocaleDateString('en-US', { month: 'short', day: 'numeric' })} ${h}:00`
  }
  const parsed = new Date(date + 'T00:00:00')
  return parsed.toLocaleDateString('en-US', { month: 'short', day: 'numeric' })
}

// ─── Sub-components ───────────────────────────────────────────────────────────

export function KPICard({
  icon: Icon,
  label,
  value,
  sub,
  color = 'text-zinc-900 dark:text-zinc-100',
}: {
  icon: React.ElementType
  label: string
  value: string
  sub?: string
  color?: string
}) {
  return (
    <div className="rounded-lg border border-zinc-200 dark:border-zinc-700/50 bg-white dark:bg-zinc-900 p-4">
      <div className="flex items-center gap-2 mb-2">
        <Icon className="h-4 w-4 text-zinc-400 dark:text-zinc-500 shrink-0" />
        <span className="text-xs text-zinc-500 dark:text-zinc-400">{label}</span>
      </div>
      <p className={`text-2xl font-semibold ${color}`}>{value}</p>
      {sub && <p className="text-xs text-zinc-400 dark:text-zinc-500 mt-0.5">{sub}</p>}
    </div>
  )
}

export function ChartCard({ title, children }: { title: string; children: React.ReactNode }) {
  return (
    <div className="rounded-lg border border-zinc-200 dark:border-zinc-700/50 bg-white dark:bg-zinc-900 p-4">
      <h3 className="text-sm font-semibold text-zinc-900 dark:text-zinc-100 mb-4">{title}</h3>
      {children}
    </div>
  )
}

export function DateRangePicker({
  preset,
  from,
  to,
  onPreset,
  onFrom,
  onTo,
  projects,
  project,
  onProject,
}: {
  preset: DatePreset
  from: string
  to: string
  onPreset: (p: DatePreset) => void
  onFrom: (v: string) => void
  onTo: (v: string) => void
  projects?: string[]
  project?: string
  onProject?: (v: string) => void
}) {
  return (
    <div className="flex flex-col sm:flex-row items-start sm:items-center gap-3">
      <div className="flex flex-wrap gap-1">
        {PRESETS.map(p => (
          <button
            key={p.value}
            onClick={() => onPreset(p.value)}
            className={`px-2.5 py-1 rounded-md text-xs font-medium transition-colors ${
              preset === p.value
                ? 'bg-zinc-900 dark:bg-zinc-100 text-white dark:text-zinc-900'
                : 'bg-zinc-100 dark:bg-zinc-800 text-zinc-600 dark:text-zinc-400 hover:bg-zinc-200 dark:hover:bg-zinc-700'
            }`}
          >
            {p.label}
          </button>
        ))}
      </div>
      {preset === 'custom' && (
        <div className="flex items-center gap-2">
          <input
            type="date"
            value={from}
            onChange={e => onFrom(e.target.value)}
            className="rounded-md border border-zinc-200 dark:border-zinc-600 bg-white dark:bg-zinc-800 text-zinc-900 dark:text-zinc-100 px-2 py-1 text-xs focus:outline-none focus:ring-1 focus:ring-zinc-900 dark:focus:ring-zinc-400"
          />
          <span className="text-xs text-zinc-400">to</span>
          <input
            type="date"
            value={to}
            onChange={e => onTo(e.target.value)}
            className="rounded-md border border-zinc-200 dark:border-zinc-600 bg-white dark:bg-zinc-800 text-zinc-900 dark:text-zinc-100 px-2 py-1 text-xs focus:outline-none focus:ring-1 focus:ring-zinc-900 dark:focus:ring-zinc-400"
          />
        </div>
      )}
      {(projects?.length ?? 0) > 1 && onProject && (
        <Select value={project} onValueChange={onProject}>
          <SelectTrigger className="w-full sm:w-56 h-7 text-xs">
            <SelectValue placeholder="All projects" />
          </SelectTrigger>
          <SelectContent>
            <SelectItem value="all">All projects</SelectItem>
            {projects!.map(p => (
              <SelectItem key={p} value={p} className="text-xs font-mono">
                {p.split('/').slice(-2).join('/')}
              </SelectItem>
            ))}
          </SelectContent>
        </Select>
      )}
    </div>
  )
}
