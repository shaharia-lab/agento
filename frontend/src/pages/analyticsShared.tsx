// Shared utilities and sub-components used by both TokenUsagePage and GeneralUsagePage.

import { Cell, Pie, PieChart, Tooltip } from 'recharts'

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

/**
 * Formats a Date as the YYYY-MM-DD the analytics API expects, using its **local**
 * calendar fields.
 *
 * toISOString would render the UTC day, and every caller here builds its Date
 * from local arithmetic (`new Date(y, m, 1)`, `setDate(...)`). Mixing the two
 * shifted every range edge by a day for anyone whose offset crosses midnight —
 * "this month" starting on the last day of the previous one.
 */
export function fmt(d: Date) {
  const pad = (n: number) => String(n).padStart(2, '0')
  return `${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())}`
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

/**
 * Abbreviates a token count.
 *
 * Billions get their own unit because cache-read traffic reaches them: without
 * it a month of cache reads rendered as "21274.5M", which is both hard to read
 * and wide enough to be clipped by the chart axes it labels.
 */
export function formatTokens(n: number): string {
  if (!n) return '0'
  if (n >= 1_000_000_000) return `${(n / 1_000_000_000).toFixed(1)}B`
  if (n >= 1_000_000) return `${(n / 1_000_000).toFixed(1)}M`
  if (n >= 1_000) return `${(n / 1_000).toFixed(1)}k`
  return String(n)
}

export function formatModelName(model: string): string {
  if (!model || model === 'unknown') return 'Unknown'
  const lower = model.toLowerCase()
  if (lower.includes('opus') || lower.includes('sonnet') || lower.includes('haiku'))
    return model
      .replaceAll(/claude-/gi, '') // regex needed for case-insensitive match
      .replaceAll('-', ' ')
      .replaceAll(/\b\w/g, (c: string) => c.toUpperCase())
  return model
}

/**
 * Renders a backend bucket key ("2026-08-08" or "2026-08-08T14") for display.
 *
 * The key is already in the browser's timezone — it is bucketed there — so the
 * date part is parsed without a `Z` (local) and the hour is printed verbatim.
 * Both halves therefore describe the same wall clock; before the backend took a
 * `tz`, the hour was UTC while the date was parsed locally.
 */
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
}: Readonly<{
  icon: React.ElementType
  label: string
  value: string
  sub?: string
  color?: string
}>) {
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

export function ChartCard({
  title,
  subtitle,
  children,
}: Readonly<{ title: string; subtitle?: string; children: React.ReactNode }>) {
  return (
    <div className="rounded-lg border border-zinc-200 dark:border-zinc-700/50 bg-white dark:bg-zinc-900 p-4">
      <h3 className="text-base font-semibold text-zinc-900 dark:text-zinc-100">{title}</h3>
      {/* A chart whose scope is not obvious states it here rather than in a
          tooltip: "tokens" that silently exclude cache, or a rate whose
          denominator is not the one a reader assumes, is how a dashboard
          misleads without ever being wrong. */}
      {subtitle && <p className="text-xs text-zinc-500 dark:text-zinc-400 mt-0.5">{subtitle}</p>}
      <div className="mt-4">{children}</div>
    </div>
  )
}

/**
 * Toggle for the previous-period ghost series.
 *
 * Off by default: the comparison doubles the request and only matters when a
 * reader is actually asking "versus last time", which the Insights page's Δ
 * badges answer for scalars but nothing answered for the time charts.
 */
export function CompareToggle({
  enabled,
  onChange,
  label,
}: Readonly<{ enabled: boolean; onChange: (v: boolean) => void; label: string }>) {
  return (
    <button
      onClick={() => onChange(!enabled)}
      className={`px-2.5 py-1 rounded-md text-xs font-medium transition-colors ${
        enabled
          ? 'bg-zinc-900 dark:bg-zinc-100 text-white dark:text-zinc-900'
          : 'bg-zinc-100 dark:bg-zinc-800 text-zinc-600 dark:text-zinc-400 hover:bg-zinc-200 dark:hover:bg-zinc-700'
      }`}
      title={label}
    >
      Compare to previous period
    </button>
  )
}

/** One slice of a DonutWithLegend: a labeled value with its share of the total. */
export interface DonutSlice {
  name: string
  value: number
  /** Share of the total, 0–100, as computed by the backend. */
  percentage: number
}

/**
 * A donut chart with the labels in a side legend rather than around the rim.
 *
 * Radial labels overlap unreadably as soon as a few slices are small — four
 * 0.0% entries used to render on top of each other — and the percentage they
 * showed was recomputed client-side from the rendered slice rather than taken
 * from the backend's own `percentage`, so a rounded chart and an exact table
 * could disagree. The legend has room for the full model name, the value and
 * the backend's share, and it stays readable however long the tail is.
 */
export function DonutWithLegend({
  data,
  formatValue,
  emptyLabel = 'No data in this range',
}: Readonly<{
  data: DonutSlice[]
  formatValue: (value: number) => string
  emptyLabel?: string
}>) {
  if (data.length === 0) {
    return <p className="text-sm text-zinc-400 dark:text-zinc-500 py-8 text-center">{emptyLabel}</p>
  }
  return (
    <div className="flex flex-col sm:flex-row items-center gap-4">
      {/* Explicitly sized rather than wrapped in a ResponsiveContainer: the
          donut is a fixed-size element beside a flexible legend, and measuring
          a flex item whose own width comes from its content is what collapsed
          this chart to a one-pixel line. */}
      <div className="shrink-0">
        <PieChart width={240} height={220}>
          <Pie
            data={data}
            dataKey="value"
            nameKey="name"
            cx="50%"
            cy="50%"
            innerRadius={52}
            outerRadius={86}
            paddingAngle={1}
            isAnimationActive={false}
          >
            {data.map((entry, i) => (
              <Cell key={entry.name} fill={MODEL_COLORS[i % MODEL_COLORS.length]} />
            ))}
          </Pie>
          <Tooltip
            formatter={(v, name) => [formatValue(Number(v ?? 0)), String(name ?? '')]}
            contentStyle={{ fontSize: 12, borderRadius: 6 }}
          />
        </PieChart>
      </div>
      <ul className="flex-1 w-full space-y-1.5">
        {data.map((slice, i) => (
          <li key={slice.name} className="flex items-center gap-2 text-xs">
            <span
              className="h-2.5 w-2.5 rounded-[2px] shrink-0"
              style={{ backgroundColor: MODEL_COLORS[i % MODEL_COLORS.length] }}
            />
            <span className="flex-1 truncate text-zinc-700 dark:text-zinc-300" title={slice.name}>
              {slice.name}
            </span>
            <span className="tabular-nums text-zinc-500 dark:text-zinc-400">
              {formatValue(slice.value)}
            </span>
            <span className="tabular-nums w-12 text-right font-medium text-zinc-900 dark:text-zinc-100">
              {slice.percentage.toFixed(1)}%
            </span>
          </li>
        ))}
      </ul>
    </div>
  )
}

/** One labeled segment of a StackedComposition bar. */
export interface CompositionPart {
  label: string
  value: number
  color: string
  hint?: string
}

/**
 * A single stacked bar showing how a total divides, with every part labeled.
 *
 * It exists for the token headline. "Total Tokens 996.7M" sat next to "Est.
 * Cost $20,232" on one KPI row while measuring different universes — the count
 * excluded 21.2B cache-read and 383M cache-write tokens that the cost included,
 * and cache read alone was more than half the money. Showing the parts at their
 * true relative size makes that impossible to misread.
 */
export function StackedComposition({
  parts,
  formatValue,
}: Readonly<{ parts: CompositionPart[]; formatValue: (value: number) => string }>) {
  const total = parts.reduce((sum, p) => sum + p.value, 0)
  const share = (v: number) => (total > 0 ? (v / total) * 100 : 0)

  return (
    <div>
      <div className="flex h-3 w-full overflow-hidden rounded-full bg-zinc-100 dark:bg-zinc-800">
        {parts.map(part => (
          <div
            key={part.label}
            className="h-full first:rounded-l-full last:rounded-r-full"
            style={{ width: `${share(part.value)}%`, backgroundColor: part.color }}
            title={`${part.label}: ${formatValue(part.value)} (${share(part.value).toFixed(1)}%)`}
          />
        ))}
      </div>
      <div className="mt-3 grid grid-cols-2 sm:grid-cols-4 gap-3">
        {parts.map(part => (
          <div key={part.label}>
            <div className="flex items-center gap-1.5">
              <span
                className="h-2 w-2 rounded-[2px] shrink-0"
                style={{ backgroundColor: part.color }}
              />
              <span className="text-xs text-zinc-500 dark:text-zinc-400">{part.label}</span>
            </div>
            <p className="text-base font-semibold text-zinc-900 dark:text-zinc-100 mt-0.5">
              {formatValue(part.value)}
            </p>
            <p className="text-[11px] text-zinc-400 dark:text-zinc-500">
              {share(part.value).toFixed(1)}%{part.hint ? ` · ${part.hint}` : ''}
            </p>
          </div>
        ))}
      </div>
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
}: Readonly<{
  preset: DatePreset
  from: string
  to: string
  onPreset: (p: DatePreset) => void
  onFrom: (v: string) => void
  onTo: (v: string) => void
  projects?: string[]
  project?: string
  onProject?: (v: string) => void
}>) {
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
